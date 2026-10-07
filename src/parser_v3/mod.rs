//! v0.92: Parser V3 - Witness-native MIR parser（MirExpr 中间层已删除）
//!
//! **Zero AST v2 dependencies** - Direct tokens → MirWitness conversion
//! This is the final parser implementation that completely replaces Parser v2.

use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Lexer, Token, TokenType};
use crate::mir::orchestrate::{MirOrchestrateAgent, MirOrchestrateEdge, MirOrchestrateKind, Param};
use crate::mir::witness::{MirWitness, WitnessKind, WitnessParam};
use crate::mir::{MirFunction, MirInst, Reg};
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
    /// parse 函数在构造语法树的同时 emit MirInst 到此处；compile() 取走
    /// 指令序列。
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
}

impl ParserV3 {
    pub fn new(tokens: Vec<Token>, source: &str) -> Self {
        Self {
            tokens,
            current: 0,
            diag: None,
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
        let func = parser.emit.finish();
        let witnesses = parser.witnesses;
        if func.body.is_empty() {
            return Err("empty program: parser produced no executable instructions".to_string());
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
    pat_str: String,
    /// v0.104.3: 守卫是**延迟求值**的 MirFunction（在模式绑定之后由
    /// `h_match_expr` 调用），不再是外层寄存器 —— 守卫引用模式绑定变量，
    /// 那些绑定只在匹配成功时才存在于 env。
    guard: Option<Box<MirFunction>>,
    body_mir: Box<MirFunction>,
    val_reg: Reg,
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
                if let Some((_reg, w)) = parser.emit_expr_w() {
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
