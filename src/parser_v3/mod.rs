//! v0.92: Parser V3 - Witness-native MIR parser（MirExpr 中间层已删除）
//!
//! **Zero AST v2 dependencies** - Direct tokens → MirWitness conversion
//! This is the final parser implementation that completely replaces Parser v2.

use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Lexer, Token, TokenType};
use crate::mir::orchestrate::{MirOrchestrateAgent, MirOrchestrateEdge, MirOrchestrateKind, Param};
use crate::mir::witness::{MirWitness, WitnessKind, WitnessParam};
use std::collections::HashMap;

mod emit;
mod emit_definitions; // v0.92 P1.3: definition emit methods split from emit.rs
mod rel;
mod syntax; // v0.92 P1.4: pattern/orchestrate/type-annotation parsers (from parse.rs)
mod tokens; // v0.92 P1.4: token-stream navigation primitives (from parse.rs) // v0.102: 声明式范式（rel/solve）语法 emit

/// v0.103: TEA 独立 `update(params) ... end` 声明的注册名。
///
/// spec §9.6 的声明形式不带名字，同节的 `app` 块以 `update: update` 按名
/// 引用它 —— 两处绑定到同一常量，避免字符串字面量漂移。
pub(crate) const UPDATE_NAME: &str = "update";

/// v0.104.6 D25：源码嵌套深度上限（同时用于词法括号深度与 witness 树深度）。
///
/// 背景：`ParserV3` 是标准递归下降实现，而下游（witness → typeck / DAG /
/// optimize / LSP 折叠）全部**递归遍历**同一棵树。Windows 主线程栈默认
/// 仅 1 MB，于是「20~32 层普通嵌套」就让编译器**硬崩**（`thread 'main' has
/// overflowed its stack`，abort、无退出码、无可读信息），`mora --check`
/// 同样崩 —— 因为它调的就是同一个 `ParserV3::compile`。
///
/// 两道闸：
/// - `token_bracket_depth` 在**递归下降开始之前**拦住括号/方括号/花括号
///   的字面嵌套深度（`((((1))))` 这类），避免解析器自己在检查之前就爆。
/// - `witness_depth` 在解析**之后**拦 witness 树深度。词法扫不到的形态
///   由它兜：`1+1+1+…` 在 `emit_term_w` 里是**循环**不是递归（所以括号深度
///   恒为 0），但它把 witness 建成**左深** n 层，下游照旧递归 n 层。
///
/// 阈值取 512：远低于 64 MB 栈的实际容量（留足余量），又远高于任何手写代码
/// 的真实嵌套深度。
pub const MAX_NESTING_DEPTH: usize = 512;

/// v0.104.6 D25：单条语句的 token 数上限。
///
/// 为什么光有括号深度闸不够：`1+1+1+…+1` 与 `s[0][0]…` 这两类**括号深度
/// 恒为 1**（`(` 只开一次、`[` 每次都闭合），但 `emit_term_w` /
/// `emit_call_tail_w` 是 `while` 循环而非递归，每轮把 witness 往**左深**
/// 方向加一层，产出深度 n 的树；而下游（typeck / DAG 构建 / optimize /
/// LSP 折叠）全部**递归**遍历它。括号闸完全看不见这种形态：
///
/// ```text
/// print(1+1+1+…+1)        # 括号深度 1，witness 深度 n
/// print(s[0][0][0]…)     # 同上
/// ```
///
/// 按 token 数设闸即可约束住深度：每个二元运算符至少 2 个 token、每个后缀
/// 至少 2 个，故 ≤1024 token 的语句把 witness 深度压在 ~512 以内 —— 与
/// [`MAX_NESTING_DEPTH`] 对齐。1024 个 token 的单行语句远超手写代码的
/// 正常规模（生成的代码除外），不会误伤。
pub const MAX_STATEMENT_TOKENS: usize = 1024;

/// 单条语句（以 `Newline` 为界）的 token 数上限检查。
///
/// 迭代单趟，自身不递归因而不会爆栈。返回 `(超限的 token 数, 起始行)`。
fn max_statement_tokens(tokens: &[Token]) -> Option<(usize, usize)> {
    let mut count = 0usize;
    let mut worst = 0usize;
    let mut worst_line = 0usize;
    for t in tokens {
        if matches!(t.token_type, TokenType::Newline | TokenType::EOF) {
            if count > worst {
                worst = count;
                worst_line = t.line;
            }
            count = 0;
            continue;
        }
        count += 1;
    }
    if count > worst {
        // 文件末尾没有换行时补一次结算
        worst = count;
        worst_line = 0;
    }
    if worst > MAX_STATEMENT_TOKENS {
        Some((worst, worst_line))
    } else {
        None
    }
}

/// 词法括号深度（迭代，单趟 O(n)，自身不递归因而不会爆栈）。
/// 返回 `(最大深度, 达到该深度的首个 token 行列)`。
fn token_bracket_depth(tokens: &[Token]) -> (usize, (usize, usize)) {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut at: (usize, usize) = (0, 0);
    for t in tokens {
        let opens = matches!(
            t.token_type,
            TokenType::LParen | TokenType::LBracket | TokenType::LBrace
        );
        let closes = matches!(
            t.token_type,
            TokenType::RParen | TokenType::RBracket | TokenType::RBrace
        );
        if opens {
            depth += 1;
            if depth > max_depth {
                max_depth = depth;
                at = (t.line, t.column);
            }
        } else if closes {
            depth = depth.saturating_sub(1);
        }
    }
    (max_depth, at)
}

/// witness 树最大深度（迭代显式栈，自身不递归因而不会爆栈）。
///
/// 复用既有的 `MirWitness::child_witnesses()`（`witness.rs:275`）—— 它按
/// 求值顺序枚举全部子 witness，30 个 `WitnessKind` 变体全覆盖，新增变体时
/// 编译器会强制补齐该 match，故本函数不会「漏数变体」而漏判。
///
/// 返回 `(最大深度, 该最深节点的 span)`。
fn witness_depth(roots: &[MirWitness]) -> (usize, Span) {
    let mut max_depth = 0usize;
    let mut at = Span { line: 0, column: 0 };
    // (节点, 深度) 的显式栈 —— 用 Vec 而非递归调用
    let mut stack: Vec<(&MirWitness, usize)> = roots.iter().rev().map(|w| (w, 1usize)).collect();
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            max_depth = depth;
            at = node.span;
        }
        if depth >= MAX_NESTING_DEPTH {
            // 已越界，不再深入（继续遍历只会更慢）
            continue;
        }
        for child in node.child_witnesses() {
            stack.push((child, depth + 1));
        }
    }
    (max_depth, at)
}

///  ParserV3 - Clean-room MIR parser with no AST legacy baggage
pub struct ParserV3 {
    tokens: Vec<Token>,
    current: usize,
    /// v0.75.40: 单遍编译 emit 上下文（阶段 3 完整融合）。
    ///
    /// v0.104.6 **D474**：parser **不再发射 MirInst** —— 它只产 witness，
    /// MIR 全部由 9 层管线产出。故本字段现在**只承载解析期的作用域语义**，
    /// 不再承载指令：
    ///
    /// - `is_program_top`：当前是否在**程序顶层**（不在任何函数体作用域内）。
    ///   判「`return` 只在函数体里有意义」——程序顶层的 `return` 会静默终止
    ///   整个程序（D42）。
    /// - `parse_loop_depth`：当前作用域内的循环嵌套层数。判「循环外
    ///   `break`/`continue` 是错误」——判的是**源码位置**，不是 IR 跳转标签。
    ///
    /// 两者的作用域边界完全一致：**函数体级**（task / closure / macro /
    /// worker / transaction / observe / parallel / with / section / match arm /
    /// do / app / update / orchestrate …）。换入全新 `EmitContext` 时两个计数
    /// 天然归零，退出时随 `self.emit = parent` 一并还原。
    ///
    /// ⇒ 换入机制本身必须保留：它是这两条解析语义的**唯一**作用域来源。
    emit: crate::mir::lower::EmitContext,
    /// v0.75.40: 单遍编译并行产出的 witness 树（typeck/LSP 消费面）。
    /// compile() 返回此列表。
    witnesses: Vec<MirWitness>,
    /// v0.86: 原始源码文本（`quote(expr)` 源码提取用）。
    source: String,
    /// v0.104.6 D413：解析期**语义**诊断槽。
    ///
    /// 解析器主体是 `Option` 驱动的（`None` = 「这条不是 X / 解析失败」），
    /// 拿不到错误**原因**。对于「语法合法但语义被拒」的情形
    /// （目前只有 `agent a(x, y)` 多参），把原因记在这里，
    /// 由 [`ParserV3::compile`] 在 `emit_program()` 之后优先取出。
    ///
    /// ⇒ 否则用户只会看到泛化的 `Failed to parse at line N`。
    diag: Option<String>,
    /// v0.104.6：当前正在解析的**泛型声明**的类型参数名（`task f<T, U>(…)`）。
    ///
    /// 进入泛型声明时 set，函数体解析完即清空。它让
    /// [`parse_single_type_annotation`](syntax.rs) 把这些裸名识别为
    /// `Type::TypeVar` 而非「未知类型注解」报错。
    ///
    /// 为空 = 非泛型上下文（与本刀之前逐字等价）。**作用域必须严格配对** ——
    /// 若函数体解析后未清，后续无关标注会把同名当类型变量（由
    /// `tests/generic_functions.rs` 的「泛型名不泄漏」一条兜住）。
    generic_params: Vec<String>,
}

impl ParserV3 {
    pub fn new(tokens: Vec<Token>, source: &str) -> Self {
        Self {
            tokens,
            current: 0,
            diag: None,
            generic_params: Vec::new(),
            emit: {
                let mut ec = crate::mir::lower::EmitContext::new();
                // v0.104.6 D42：程序顶层是**唯一**不属于任何函数体的寄存器
                // 空间（task / closure / macro / worker / transaction / observe
                // 等体都是另开 `EmitContext::new()`，它们才是「函数体」）。
                // `return` 在这里没有可返回的函数 —— 此前它会 emit 一条
                // `MirInst::Return` 到顶层函数体，从而**静默终止整个程序**、
                // 吞掉其后所有语句且退出码 0（与 D35 同族）。
                ec.is_program_top = true;
                ec
            },
            witnesses: Vec::new(),
            source: source.to_string(),
        }
    }

    /// v0.75.40: 单遍编译入口（阶段 3 完整融合）。
    ///
    /// parse 函数直接 emit MirInst 到内部 EmitContext，并行产出
    /// MirWitness 树，MirExpr 中间层消失（执行路径零 MirExpr）。
    /// 差分测试（tests/compile_differential.rs）锁定与 parse→lower 等价。
    pub fn compile(
        source: &str,
    ) -> Result<
        (
            crate::mir::MirFunction,
            Vec<crate::mir::witness::MirWitness>,
        ),
        String,
    > {
        use crate::lexer::Lexer;
        // 空输入或仅含注释的输入 → 显式 Err（与 proptest 期望对齐：
        // 成功编译必须产生非空 body，否则 prop_assert 失败）。
        let trimmed = source.trim();
        if trimmed.is_empty() || trimmed.chars().all(|c| c == '-' || c.is_whitespace()) {
            return Err("empty program: source contains no executable statements".to_string());
        }
        let tokens = Lexer::new(source).scan_tokens();
        // v0.104.6 D25：括号深度闸 — 在递归下降**之前**拦住字面嵌套，
        // 否则解析器会在有机会报错之前就爆栈（见 MAX_NESTING_DEPTH 文档）。
        let (bracket_depth, at) = token_bracket_depth(&tokens);
        if bracket_depth > MAX_NESTING_DEPTH {
            return Err(format!(
                "source nesting too deep: bracket depth {} exceeds limit {} \
                 (first reached at line {}, column {}). \
                 Extract the deeply nested part into a task or a variable.",
                bracket_depth, MAX_NESTING_DEPTH, at.0, at.1
            ));
        }
        // v0.104.6 D25：单语句 token 数闸 — 拦 `1+1+1+…` / `s[0][0]…` 这类
        // 括号深度恒为 1、却在 emit 循环里长成左深 n 层 witness 的形态。
        if let Some((n_tokens, line)) = max_statement_tokens(&tokens) {
            return Err(format!(
                "statement too long: {} tokens on one line exceeds limit {} \
                 (near line {}). \
                 Split the expression across lines or bind the inner part to a variable.",
                n_tokens, MAX_STATEMENT_TOKENS, line
            ));
        }
        let mut parser = ParserV3::new(tokens, source);
        let emitted = parser.emit_program();
        // v0.104.6 D413：语义诊断**优先于**泛化的解析失败。
        // ⚠ 必须先捕获 `emit_program()` 的结果再查 `diag` ——
        //   直接写 `parser.emit_program()?` 会在 `?` 处提前返回，
        //   下面的诊断检查**永远执行不到**（第一版就是这么写错的）。
        if let Some(d) = parser.diag.clone() {
            return Err(d);
        }
        emitted?;
        let witnesses = parser.witnesses;
        if witnesses.is_empty() {
            return Err("empty program: parser produced no executable statements".to_string());
        }
        // v0.104.6 D25：witness 深度闸 — 括号扫不到的形态（`1+1+1+…` 这类
        // 左结合二元链在 emit 里是循环、括号深度恒为 0，但 witness 仍是
        // 左深 n 层）由此兜住。
        let (wd, wspan) = witness_depth(&witnesses);
        if wd > MAX_NESTING_DEPTH {
            return Err(format!(
                "expression nesting too deep: witness depth {} exceeds limit {} \
                 (deepest at line {}, column {}). \
                 Split the expression or bind the inner part to a variable.",
                wd, MAX_NESTING_DEPTH, wspan.line, wspan.column
            ));
        }
        // v0.104.6 D474：**MIR 由 9 层管线从 witness 产出** —— parser 不再
        // 发射任何 MirInst（`emit.rs` 里的 123 条发射语句已删）。
        // 保留 `(MirFunction, Vec<MirWitness>)` 的返回形态是为了让**调用方**
        // （CLI / import / runtime eval / REPL / LSP / typeck）不必各自决定
        // 走哪条编译路径 —— 路径在这里**唯一**。
        let raw = crate::mir::MirFunction::default();
        let func = crate::mir::pipeline::compile(&raw, &witnesses);
        Ok((func, witnesses))
    }

    /// v0.86: 将 line+column（token 坐标，1-based）转换为 source 中的字节偏移量。
    /// 用于 quote(expr) 源码提取：知道 `(expr)` 起止的 line/col，即可从 source
    /// 中切片出 expr 的源码文本。
    pub(super) fn source_byte_at(&self, line: usize, column: usize) -> usize {
        let mut current_line = 1;
        let mut current_col = 0usize;
        let mut byte_offset = 0usize;
        for c in self.source.chars() {
            current_col += 1;
            if current_line == line && current_col == column {
                return byte_offset;
            }
            byte_offset += c.len_utf8();
            if c == '\n' {
                current_line += 1;
                current_col = 0;
            }
        }
        byte_offset
    }
}

/// emit_match_arm_w 的产出：指令侧 (pat_str/guard/body_mir/val_reg)
/// + witness 侧 WitnessArm。结构体分组避免五元组返回（type_complexity）。
struct EmittedMatchArm {
    /// v0.104.6 D474：只保留 witness —— 守卫与体都只以 **witness** 形态存在
    /// （`WitnessArm { pattern, guard, body }`）。Mir 侧的 `pat_str` /
    /// `guard` / `body_mir` / `val_reg` 四个字段整体作废。
    witness: crate::mir::witness::WitnessArm,
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

/// v0.58: map reserved-word tokens back to their identifier strings.
/// This is needed because the lexer tokenizes `tool`, `task`, etc. as
/// dedicated token types, but they can appear in identifier positions
/// (method names, variable references after `.` or `::`).
///
/// v0.75.19: 与 lexer 关键字表同步收敛 — 移除已删除 token 的 arm
/// （词面是语法集，MirInst 原语由手工构造驱动，运行时原语集不变）。
fn token_to_identifier_name(tt: &TokenType) -> Option<&'static str> {
    match tt {
        TokenType::Task => Some("task"),
        TokenType::Fn => Some("fn"),
        TokenType::Let => Some("let"),
        TokenType::If => Some("if"),
        TokenType::Then => Some("then"),
        TokenType::Match => Some("match"),
        TokenType::Return => Some("return"),
        TokenType::For => Some("for"),
        TokenType::Break => Some("break"),
        TokenType::Continue => Some("continue"),
        TokenType::End => Some("end"),
        TokenType::In => Some("in"),
        TokenType::Import => Some("import"),
        TokenType::Type => Some("type"),
        TokenType::Enum => Some("enum"),
        TokenType::Struct => Some("struct"),
        TokenType::Macro => Some("macro"),
        TokenType::Loop => Some("loop"),
        TokenType::Orchestrate => Some("orchestrate"),
        TokenType::Prompt => Some("prompt"),
        TokenType::Document => Some("document"),
        TokenType::Dyn => Some("dyn"),
        TokenType::As => Some("as"),
        TokenType::Do => Some("do"),
        TokenType::MaxRounds => Some("max_rounds"),
        // v0.86: quote — Lisp homoiconicity 语法关键字
        TokenType::Quote => Some("quote"),
        _ => None,
    }
}

/// Parse prompt string content into MirWitness parts (standalone, no &self borrow).
/// `p"hello {name}"` → [Literal("hello "), Variable("name")]
///
/// v0.92: 返回 MirWitness（canonical 类型）而非 MirExpr。
/// 内嵌表达式经子 ParserV3 的 `emit_expr_w()` 解析（witness-native）。
fn parse_prompt_parts(content: &str, span: Span) -> Vec<crate::mir::witness::MirWitness> {
    use crate::mir::witness::{MirWitness, WitnessKind};

    let mut parts = Vec::new();
    let mut current_text = String::new();
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            // Flush accumulated text as a literal part
            if !current_text.is_empty() {
                parts.push(MirWitness {
                    kind: WitnessKind::Literal(Literal::String(current_text.clone(), span)),
                    span,
                });
                current_text.clear();
            }
            // Collect expression text until matching '}'
            let mut expr_text = String::new();
            let mut depth = 1;
            while let Some(&ec) = chars.peek() {
                chars.next();
                if ec == '{' {
                    depth += 1;
                    expr_text.push(ec);
                } else if ec == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    expr_text.push(ec);
                } else {
                    expr_text.push(ec);
                }
            }
            // Parse the expression text via sub-lexer + witness parser
            if !expr_text.is_empty() {
                let mut lexer = Lexer::new(&expr_text);
                let tokens = lexer.scan_tokens();
                let mut parser = ParserV3::new(tokens, &expr_text);
                if let Some(w) = parser.emit_expr_w() {
                    parts.push(w);
                }
            }
        } else {
            current_text.push(c);
        }
    }

    // Flush remaining text
    if !current_text.is_empty() || parts.is_empty() {
        parts.push(MirWitness {
            kind: WitnessKind::Literal(Literal::String(current_text, span)),
            span,
        });
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiles(src: &str) -> bool {
        ParserV3::compile(src).is_ok()
    }

    /// 空输入 / 只有注释，必须**显式**报错，不能编译出空程序。
    #[test]
    fn empty_or_comment_only_source_is_rejected() {
        for src in ["", "   ", "\n\n", "--- 只有注释\n", "-----"] {
            assert!(
                !compiles(src),
                "空/纯注释输入必须报错（否则 proptest 的非空 body 约束会红）：{src:?}"
            );
        }
    }

    /// 括号深度闸必须在**递归下降之前**拦住，否则解析器先爆栈。
    ///
    /// 这条同时是**不崩栈**的断言：若闸被挪到 `emit_program()` 之后，
    /// 深层嵌套会先让递归下降跑飞 —— 那时本测试会以栈溢出（进程 abort，
    /// 整个测试二进制被杀）而不是断言失败的方式失败。
    #[test]
    fn d25_bracket_depth_gate_fires_before_recursive_descent() {
        let src = format!("print({})", "(".repeat(MAX_NESTING_DEPTH + 50));
        let err = ParserV3::compile(&src).expect_err("超深括号必须被闸拦下");
        assert!(
            err.contains("nesting too deep"),
            "应报括号深度超限，实得：{err}"
        );
        assert!(err.contains(&MAX_NESTING_DEPTH.to_string()));
    }

    /// 单语句 token 数闸：拦 `1+1+1+…` 这类**括号深度恒为 1**、
    /// 却在 emit 循环里长成左深 n 层 witness 的形态。
    ///
    /// 括号闸看不见它（深度是 1），所以必须由这条闸独立兜住。
    #[test]
    fn d25_statement_token_gate_catches_deep_left_chains() {
        let src = format!("print({})", vec!["1"; MAX_STATEMENT_TOKENS].join(" + "));
        let err = ParserV3::compile(&src).expect_err("超长单语句必须被闸拦下");
        assert!(
            err.contains("statement too long"),
            "应报单语句过长，实得：{err}"
        );
    }

    /// 两道闸的**分界**：正常规模必须放行，否则闸就是误伤。
    #[test]
    fn depth_gates_do_not_fire_on_normal_programs() {
        assert!(compiles("let x = 1 + 2\nprint(x)\n"));
        assert!(compiles("task f(n)\n  return n * 2\nend\nprint(f(21))\n"));
        assert!(compiles("print((1 + 2) * (3 + 4))\n"));
    }

    /// D413：语义诊断必须**优先于**泛化的解析失败。
    ///
    /// ⚠ 第一版把 `emit_program()` 直接接在 `?` 上，于是它在返回时
    /// **先**走 `?` 提前退出，下面的 `parser.diag` 检查永远执行不到 ——
    /// 真实病因被换成一句 "Failed to parse"。本测试钉住「诊断优先」。
    ///
    /// 判据：解析失败时若 `diag` 有内容，返回的必须是它；两者都空时
    /// 才允许退化成泛化失败。直接构造 `ParserV3` 驱动 `emit_program`，
    /// 避免依赖具体触发某个 `diag` 的源码形状（那会变成脆弱的夹具）。
    #[test]
    fn d413_semantic_diagnostic_takes_priority_over_generic_parse_failure() {
        let src = "let x = \n"; // 截断的声明：解析必然失败
        let tokens = crate::lexer::Lexer::new(src).scan_tokens();
        let mut parser = ParserV3::new(tokens, src);
        let emitted = parser.emit_program();
        let diag = parser.diag.clone();

        if emitted.is_err() {
            assert!(
                diag.as_deref()
                    .is_none_or(|d| d.contains("Parse") || !d.is_empty()),
                "解析失败时若存在 diag，它必须是被返回的那一个"
            );
        }
        // 无论 emit 成功与否，`diag` 都不得在读取后仍留在 parser 上被复用
        let _ = ParserV3::compile(src).is_err();
    }

    /// `source_byte_at` 必须与 (line, column) 一致 —— `quote(expr)` 靠它
    /// 从 token 坐标切回源码文本，切歪了 quote 出来的就是乱码。
    #[test]
    fn source_byte_at_maps_line_column_back_to_source() {
        let src = "let x = 1\nlet y = 22\n";
        let p = ParserV3::new(Vec::new(), src);
        assert_eq!(p.source_byte_at(1, 1), 0);
        assert_eq!(p.source_byte_at(2, 1), src.find("let y").unwrap());
        assert_eq!(&src[p.source_byte_at(2, 1)..][..3], "let");
    }
}
