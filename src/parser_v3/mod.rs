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
mod syntax; // v0.92 P1.4: pattern/orchestrate/type-annotation parsers (from parse.rs)
mod tokens; // v0.92 P1.4: token-stream navigation primitives (from parse.rs)
mod rel; // v0.102: 声明式范式（rel/solve）语法 emit

/// v0.103: TEA 独立 `update(params) ... end` 声明的注册名。
///
/// spec §9.6 的声明形式不带名字，同节的 `app` 块以 `update: update` 按名
/// 引用它 —— 两处绑定到同一常量，避免字符串字面量漂移。
pub(crate) const UPDATE_NAME: &str = "update";

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
}

impl ParserV3 {
    pub fn new(tokens: Vec<Token>, source: &str) -> Self {
        Self {
            tokens,
            current: 0,
            emit: crate::mir::lower::EmitContext::new(),
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
        let mut parser = ParserV3::new(tokens, source);
        parser.emit_program()?;
        let func = parser.emit.finish();
        let witnesses = parser.witnesses;
        if func.body.is_empty() {
            return Err("empty program: parser produced no executable instructions".to_string());
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
