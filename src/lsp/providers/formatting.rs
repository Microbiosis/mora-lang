//! v0.110.0: LSP formatting / rangeFormatting provider —— 基于 lexer token 流的行级重排。
//!
//! 本 provider **不消费 AST**（仓库内已无 AST）：`simple_format` 直接消费
//! `crate::lexer::Lexer` 的 token 流（含注释），逐 token 重建整份文档
//! （行尾去空白 + 2 空格缩进 + token 间补空格）。
//!
//! 两条承重不变式：
//! - **保行数**：输出与输入的 `\n` 个数必须相等，否则区间切片会取错行；
//! - **不吞注释**：lexer 必须 `keep_comments(true)`，否则格式化会删光文件里的注释。
//!
//! 区间请求（`rangeFormatting`）先格式化**整份**文档、再从中切出请求的行区间，
//! 返回的 `newText` 恰好是该 range 的替换文本。

use super::parsed_doc_v3;
use crate::flow::json::JsonValue as Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

/// 一条 `TextEdit`：`range` 与 `newText` 在**同一个坐标系**里。
struct TextEdit {
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    new_text: String,
}

pub fn formatting(docs: &HashMap<String, DocumentState>, params: &Value, range: bool) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let text = match docs.get(uri) {
        Some(d) => d.text.clone(),
        None => return Value::Array(vec![]),
    };

    // 基础格式化：整份文档按 **lexer token 流** 重排（本仓已无 AST，
    // 不存在「AST-aware formatter」这条路径）。
    // 行级最小版本：行尾去空白 + 一致缩进（2 空格，按块开启处嵌套加 1 层，
    // `end` 闭合层级）+ token 间补空格；不重新决策换行位置。
    //
    // v0.104.6 D202/D203：**整份文档**只格式化一次，区间请求再从结果里切。
    //
    // 修前 `range` 参数被 `_range` 忽略 —— 客户端只要第 2–3 行，服务端却把
    // **整份文档**塞进一个**只覆盖第 2–3 行**的 `TextEdit.range` 里：
    //
    // ```text
    // 请求 range = 行 1..2（0-based，文档共 4 行）
    // 应答 newText = 4 行整份文档,  range = 行 1..2
    // ```
    //
    // 客户端照 `range` 应用（协议要求如此：「a text edit **replaces a range**」）
    // → 把 4 行内容**替换进那 2 行**，文件凭空多出 2 行、代码被复制。
    // 修前实测：`let a = 1 / let b = 2 / let c = 3 / print(...)` 四行文档
    // 请求格式化第 2–3 行，返回的 newText 是**全部四行**。
    //
    // 前提是 `simple_format` **保行数**（输出与源码的 `\n` 个数相等），
    // 否则「第 k 行」在两侧对不上，切出来的就不是被请求的那几行。
    let formatted = format_mora_source(&text);

    let edit = if range {
        let s = match params.get("range") {
            Some(v) => v,
            None => return Value::Array(vec![]),
        };
        let start = match s.get("start") {
            Some(v) => v,
            None => return Value::Array(vec![]),
        };
        let end = match s.get("end") {
            Some(v) => v,
            None => return Value::Array(vec![]),
        };
        // v0.104.6 D245：走唯一收口（同 `hover_v3`）。这里的后果更直接 ——
        // `l0` / `l1` 回绕成 `usize::MAX` 后被喂给 `slice_edit`。
        let (l0, l1) = super::parsed_doc_v3::pos_of(start.get("line"), end.get("line"));
        // 区间完全越界（客户端给了不存在的行）→ 没有可格式化的内容
        match slice_edit(&text, &formatted, l0, l1) {
            Some(e) => e,
            None => return Value::Array(vec![]),
        }
    } else {
        // 整份文档：range 取「从 (0,0) 到文档末尾」，于是这条 edit 的
        // `newText` **恰好**是它自己 range 的替换文本 —— 与区间分支同一条不变式。
        let last = text.split('\n').count() - 1;
        TextEdit {
            start_line: 0,
            start_char: 0,
            end_line: last,
            // v0.104.6 D211：出站 `character` 必须是 **UTF-16 码元**
            // （客户端照 range 应用 newText，列号错一位就改坏文件）
            end_char: parsed_doc_v3::char_to_utf16_col(
                text.split('\n').nth(last).unwrap_or(""),
                text.split('\n')
                    .nth(last)
                    .map(|l| l.chars().count())
                    .unwrap_or(0),
            ),
            new_text: formatted,
        }
    };

    let mut m = BTreeMap::new();
    m.insert(
        "range".to_string(),
        Value::Object({
            let mut r = BTreeMap::new();
            r.insert(
                "start".to_string(),
                Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(edit.start_line as f64));
                    p.insert(
                        "character".to_string(),
                        Value::Number(edit.start_char as f64),
                    );
                    p
                }),
            );
            r.insert(
                "end".to_string(),
                Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(edit.end_line as f64));
                    p.insert("character".to_string(), Value::Number(edit.end_char as f64));
                    p
                }),
            );
            r
        }),
    );
    m.insert("newText".to_string(), Value::String_(edit.new_text));
    Value::Array(vec![Value::Object(m)])
}

/// 从**整份**格式化结果里切出第 `l0..=l1` 行，作为一条自洽的 `TextEdit`。
///
/// 返回 `None` 表示起始行在文档里根本不存在。
///
/// **本函数维护的不变式**：返回的 `newText` **恰好**是 `range` 所覆盖那几行
/// 在文档里的**替换文本**。LSP 对 `TextEdit` 的定义是
/// 「A text edit replaces a range」—— `newText` 是 range 的**内容**，
/// 不是「要插进 range 的东西」。
///
/// 区间**吸附到整行**：`start` 落到 `(l0, 0)`，`end` 落到第 `l1` 行的**行尾**
/// （即该行 `\n` 之前）。客户端常给半个行（`end.character` 在中间），
/// 本格式化器不处理半行，与其返回一个与 range 对不上的 `newText`，
/// 不如**扩大 range 到整行**、让两者严格对应。
fn slice_edit(text: &str, formatted: &str, l0: usize, l1: usize) -> Option<TextEdit> {
    let src_lines: Vec<&str> = text.split('\n').collect();
    let fmt_lines: Vec<&str> = formatted.split('\n').collect();
    if l0 >= src_lines.len() {
        return None;
    }
    let l1 = l1.clamp(l0, src_lines.len() - 1);
    Some(TextEdit {
        start_line: l0,
        start_char: 0,
        end_line: l1,
        // v0.104.6 D211：出站 `character` 必须是 **UTF-16 码元**
        // （客户端照 range 应用 newText，列号错一位就改坏文件）
        end_char: parsed_doc_v3::char_to_utf16_col(src_lines[l1], src_lines[l1].chars().count()),
        new_text: fmt_lines
            .get(l0..=l1)
            .map(|s| s.join("\n"))
            .unwrap_or_default(),
    })
}

/// Canonical Mora source formatter shared by `mora fmt` and `mora-lsp`
/// (`textDocument/formatting` & `textDocument/rangeFormatting`).
///
/// Preserved invariants:
/// - **Line count**: exact `\n` count matches input (required by `slice_edit` / `rangeFormatting`).
/// - **Comments & blank lines**: `keep_comments(true)` preserves every `--` comment verbatim and
///   keeps every blank line in place.
/// - **Idiomatic spacing & 2-space block indentation**: tight call/index/member/generic spacing
///   (`print(x)`, `a[0]`, `x.shape()`, `Tensor::ones(...)`, `list<string>`), unary operator
///   attachment (`-x`, `!x`, `@tag`), clean `else` / `else if` / `end` / `}` outdenting, and
///   strict idempotence (`format(format(s)) == format(s)`).
pub(crate) fn format_mora_source(text: &str) -> String {
    use crate::lexer::TokenType;

    let tokens = crate::lexer::Lexer::new(text)
        .keep_comments(true)
        .scan_tokens();

    // Group tokens by source line (preserving exact `\n` boundaries).
    let mut lines: Vec<Vec<&TokenType>> = Vec::new();
    let mut cur: Vec<&TokenType> = Vec::new();
    let mut trailing_newline = false;
    for tok in &tokens {
        match &tok.token_type {
            TokenType::Newline => {
                lines.push(std::mem::take(&mut cur));
                trailing_newline = true;
            }
            TokenType::EOF => {
                if !cur.is_empty() || !trailing_newline {
                    lines.push(std::mem::take(&mut cur));
                }
            }
            tt => {
                cur.push(tt);
                trailing_newline = false;
            }
        }
    }
    if lines.is_empty() {
        return String::new();
    }

    fn first_code_tok<'a>(
        line: &[&'a crate::lexer::TokenType],
    ) -> Option<&'a crate::lexer::TokenType> {
        line.iter()
            .copied()
            .find(|t| !matches!(t, crate::lexer::TokenType::Comment(_)))
    }

    let next_code_line_first = |from_idx: usize| -> Option<&TokenType> {
        for l in lines.iter().skip(from_idx + 1) {
            if let Some(tok) = first_code_tok(l) {
                return Some(tok);
            }
        }
        None
    };

    let mut out = String::with_capacity(text.len() + 32);
    let mut depth: usize = 0;
    let mut trait_depths: Vec<usize> = Vec::new();

    for (line_idx, line) in lines.iter().enumerate() {
        if line_idx > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            continue;
        }

        let first_code = first_code_tok(line);
        let leading_outdent = match first_code {
            Some(TokenType::End | TokenType::RBrace) => true,
            Some(TokenType::Identifier(s)) if matches!(s.as_str(), "else" | "elif" | "elseif") => {
                true
            }
            _ => false,
        };
        let line_depth = if leading_outdent {
            depth.saturating_sub(1)
        } else {
            depth
        };

        push_indent(&mut out, line_depth);

        let mut prev_prev: Option<&TokenType> = None;
        let mut prev: Option<&TokenType> = None;
        let mut angle_depth: usize = 0;
        let mut bracket_depth: usize = 0;
        let in_trait_body = trait_depths.last().copied() == Some(depth);
        let has_end_on_line = line.iter().any(|t| matches!(t, TokenType::End));

        for (i, &tt) in line.iter().enumerate() {
            let next = line.get(i + 1).copied();
            let opens_generic = matches!(tt, TokenType::Less)
                && matches!(
                    prev,
                    Some(TokenType::Identifier(s))
                        if matches!(
                            s.as_str(),
                            "list"
                                | "dict"
                                | "set"
                                | "channel"
                                | "Tensor"
                                | "Result"
                                | "Option"
                                | "Box"
                                | "Arc"
                        )
                );
            let closes_generic = matches!(tt, TokenType::Greater) && angle_depth > 0;

            let is_at_tag = matches!(prev, Some(TokenType::At))
                && matches!(tt, TokenType::Identifier(s) if s == "tag")
                && matches!(next, Some(TokenType::LParen));

            if !is_at_tag
                && needs_space(
                    prev_prev,
                    prev,
                    tt,
                    angle_depth,
                    bracket_depth,
                    opens_generic,
                    closes_generic,
                )
            {
                out.push(' ');
            }

            out.push_str(&token_text(tt));

            if opens_generic {
                angle_depth += 1;
            } else if closes_generic {
                angle_depth -= 1;
            }
            match tt {
                TokenType::LBracket => bracket_depth += 1,
                TokenType::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                _ => {}
            }

            // Update block indentation depth for subsequent lines.
            let after_else = matches!(prev, Some(TokenType::Identifier(s)) if s == "else");
            match tt {
                TokenType::End | TokenType::RBrace => {
                    depth = depth.saturating_sub(1);
                    if trait_depths.last().copied().is_some_and(|td| depth < td) {
                        trait_depths.pop();
                    }
                }
                TokenType::LBrace => {
                    depth += 1;
                }
                TokenType::If if after_else => {
                    // `else if ...` shares the enclosing `if`'s single `end`.
                }
                TokenType::Task if in_trait_body && !has_end_on_line => {
                    // Bodyless trait method signature (`task foo(self): T`) is followed directly
                    // by `end` or another `task`, so it does not open an indented block.
                    let is_signature_only = matches!(
                        next_code_line_first(line_idx),
                        Some(TokenType::End | TokenType::Task)
                    );
                    if !is_signature_only {
                        depth += 1;
                    }
                }
                _ if is_block_opener_ctx(tt, prev, next) => {
                    depth += 1;
                    if matches!(tt, TokenType::Identifier(s) if s == "trait") {
                        trait_depths.push(depth);
                    }
                }
                _ => {}
            }

            if !matches!(tt, TokenType::Comment(_)) {
                prev_prev = prev;
                prev = Some(tt);
            }
        }
        trim_trailing_spaces(&mut out);
    }

    if trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    // Guarantee exact newline count preservation (D202/D203 load-bearing invariant).
    let src_newlines = text.matches('\n').count();
    while out.matches('\n').count() < src_newlines {
        out.push('\n');
    }
    out
}

/// Decide whether a single space separates `prev` and `curr` on the same source line.
fn needs_space(
    prev_prev: Option<&crate::lexer::TokenType>,
    prev: Option<&crate::lexer::TokenType>,
    curr: &crate::lexer::TokenType,
    angle_depth: usize,
    bracket_depth: usize,
    opens_generic: bool,
    closes_generic: bool,
) -> bool {
    use crate::lexer::TokenType;

    let Some(prev) = prev else {
        return false;
    };

    // Always separate inline comments from preceding code.
    if matches!(curr, TokenType::Comment(_)) {
        return true;
    }

    // Tokens that attach tightly to the preceding token.
    match curr {
        TokenType::RParen
        | TokenType::RBracket
        | TokenType::Comma
        | TokenType::Dot
        | TokenType::DotDot
        | TokenType::DotDotDot
        | TokenType::ColonColon
        | TokenType::Colon
        | TokenType::Question => return false,
        TokenType::RBrace => return false,
        TokenType::LParen => {
            if matches!(
                prev,
                TokenType::Fn
                    | TokenType::RParen
                    | TokenType::RBracket
                    | TokenType::Self_
                    | TokenType::LParen
                    | TokenType::LBracket
            ) {
                return false;
            }
            if let TokenType::Identifier(s) = prev
                && !is_spaced_before_paren(s)
            {
                return false;
            }
        }
        TokenType::LBracket => {
            if matches!(
                prev,
                TokenType::RParen | TokenType::RBracket | TokenType::Self_
            ) {
                return false;
            }
            if let TokenType::Identifier(s) = prev
                && !is_spaced_before_bracket(s)
            {
                return false;
            }
        }
        _ => {}
    }

    if opens_generic || closes_generic {
        return false;
    }

    // Tokens that attach tightly to the following token.
    match prev {
        TokenType::LParen
        | TokenType::LBracket
        | TokenType::LBrace
        | TokenType::Dot
        | TokenType::DotDot
        | TokenType::DotDotDot
        | TokenType::ColonColon
        | TokenType::Bang
        | TokenType::Backtick
        | TokenType::Amp => return false,
        TokenType::Less if angle_depth > 0 => return false,
        TokenType::Colon if angle_depth > 0 || bracket_depth > 0 => return false,
        TokenType::Minus if is_unary_prefix_context(prev_prev) => return false,
        TokenType::At if prev_prev.is_none() => return false,
        _ => {}
    }

    true
}

fn is_spaced_before_paren(ident: &str) -> bool {
    matches!(
        ident,
        "and"
            | "or"
            | "not"
            | "while"
            | "else"
            | "elif"
            | "elseif"
            | "select"
            | "spawn"
            | "perform"
            | "handle"
            | "trait"
            | "impl"
            | "kernel"
            | "export"
            | "assign"
    )
}

fn is_spaced_before_bracket(ident: &str) -> bool {
    is_spaced_before_paren(ident)
}

fn is_unary_prefix_context(prev_prev: Option<&crate::lexer::TokenType>) -> bool {
    use crate::lexer::TokenType;
    match prev_prev {
        None => true,
        Some(
            TokenType::LParen
            | TokenType::LBracket
            | TokenType::LBrace
            | TokenType::Comma
            | TokenType::Colon
            | TokenType::Assign
            | TokenType::Plus
            | TokenType::Minus
            | TokenType::Star
            | TokenType::StarStar
            | TokenType::Slash
            | TokenType::Percent
            | TokenType::Equal
            | TokenType::NotEqual
            | TokenType::Less
            | TokenType::Greater
            | TokenType::LessEqual
            | TokenType::GreaterEqual
            | TokenType::Arrow
            | TokenType::FatArrow
            | TokenType::Pipe
            | TokenType::Or
            | TokenType::At
            | TokenType::Return
            | TokenType::If
            | TokenType::Then
            | TokenType::In
            | TokenType::Do,
        ) => true,
        Some(TokenType::Identifier(s))
            if matches!(
                s.as_str(),
                "and" | "or" | "not" | "else" | "elif" | "elseif"
            ) =>
        {
            true
        }
        _ => false,
    }
}

/// Contextual block opener detection: keyword openers (`task`, `for`, `if`, `match`, `fn`,
/// `macro`, `with`, `struct`, `enum`, `loop`) plus contextual identifier keywords (`while`,
/// `worker`, `handle`, `observe`, `parallel`, `transaction`, `trait`, `impl`, `select`) when
/// NOT used as a member/method/variable name (e.g. `self.handle(req, stream)`).
fn is_block_opener_ctx(
    tt: &crate::lexer::TokenType,
    prev: Option<&crate::lexer::TokenType>,
    next: Option<&crate::lexer::TokenType>,
) -> bool {
    use crate::lexer::TokenType;
    match tt {
        TokenType::Task
        | TokenType::For
        | TokenType::If
        | TokenType::Match
        | TokenType::Fn
        | TokenType::Macro
        | TokenType::With
        | TokenType::Struct
        | TokenType::Enum
        | TokenType::Loop => true,
        TokenType::Identifier(s)
            if matches!(
                s.as_str(),
                "while"
                    | "worker"
                    | "handle"
                    | "observe"
                    | "parallel"
                    | "transaction"
                    | "trait"
                    | "impl"
                    | "kernel"
                    | "select"
            ) =>
        {
            let prev_blocks = matches!(
                prev,
                Some(TokenType::Dot | TokenType::ColonColon | TokenType::Let)
            ) || matches!(prev, Some(TokenType::Identifier(p)) if p == "assign");
            let next_blocks = matches!(
                next,
                Some(
                    TokenType::Assign
                        | TokenType::Colon
                        | TokenType::Dot
                        | TokenType::Comma
                        | TokenType::RParen
                        | TokenType::RBracket
                )
            ) || (s == "handle" && matches!(next, Some(TokenType::LParen)));
            !prev_blocks && !next_blocks
        }
        _ => false,
    }
}

/// 去掉行尾空格。格式化器不该在**每一行**末尾留下一个空格。
///
/// 安全：字符串 / 注释 token 的文本自带定界符（`"…"` / `--…`），
/// 永远不会以裸空格收尾，故不会误伤字面量里的内容。
fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Mora `"…"` 字面量的**正文**转义（不含外层引号）。
///
/// v0.104.6 D210：转义 `\` `"` 与**全部控制字符**。
///
/// lexer 把源码里的 `\n` `\t` `\x0c` … **解码成真字符**后才交给
/// `token_text`，所以重排时若不把它们重新转义回去，输出里就会出现
/// **裸控制字符**（尤其是裸换行）—— 那会凭空多出一行，
/// 破坏 `simple_format` 的保行数不变式。
///
/// 规则与 JSON 字符串转义一致（`flow::escape_json_string` 的同款语义），
/// 但**这是 Mora 源码字面量**、不是 JSON，故单列一份。
fn mora_string_body(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

/// Mora `'…'` 字符字面量的正文转义。
///
/// v0.104.6 D210：同理，且还要转义 `'` 本身 ——
/// 修前是 `format!("'{}'", c)`，一个字都没转义。
fn mora_char_body(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\'' => "\\'".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        c if (c as u32) < 0x20 || c as u32 == 0x7F => {
            format!("\\u{{{:x}}}", c as u32)
        }
        c => c.to_string(),
    }
}

/// token → **源码拼写**。
///
/// v0.104.6 D102：兜底分支原先是 `format!("{:?}", tt).to_lowercase()`，
/// 那输出的是**枚举变体名**而不是源码文本，于是
/// `let x=1` 被格式化成 `let x assign 1`、`print( x +y )` 被格式化成
/// `print lparenx plus y rparen` —— **产物根本解析不了**
/// （实测 `Parse error: Expected '=' in let binding at line 1`，而原文能跑出 `3.0`）。
/// 编辑器一旦接受这份 `newText` 并存盘，用户的代码就被毁了。
///
/// 改为**穷举每个变体的源码拼写**。宁可多写几行，也不能让
/// 「格式化」产出不可解析的代码。
fn token_text(tt: &crate::lexer::TokenType) -> String {
    use crate::lexer::TokenType;
    match tt {
        // ── 关键字（变体名大写、源码小写，故必须逐个列）──
        TokenType::Let => "let".into(),
        TokenType::Task => "task".into(),
        TokenType::If => "if".into(),
        TokenType::Then => "then".into(),
        // 注：没有 `TokenType::Else` —— `else` 按 `Identifier` 走（见 `Identifier` 臂）。
        TokenType::End => "end".into(),
        TokenType::Return => "return".into(),
        TokenType::True => "true".into(),
        TokenType::False => "false".into(),
        TokenType::Nil => "nil".into(),
        TokenType::For => "for".into(),
        TokenType::In => "in".into(),
        TokenType::Import => "import".into(),
        TokenType::Match => "match".into(),
        TokenType::Fn => "fn".into(),
        TokenType::As => "as".into(),
        TokenType::Do => "do".into(),
        TokenType::Break => "break".into(),
        TokenType::Continue => "continue".into(),
        TokenType::Macro => "macro".into(),
        TokenType::Quote => "quote".into(),
        TokenType::Loop => "loop".into(),
        TokenType::With => "with".into(),
        TokenType::Type => "type".into(),
        TokenType::Enum => "enum".into(),
        TokenType::Struct => "struct".into(),
        TokenType::Dyn => "dyn".into(),
        TokenType::Self_ => "Self".into(),

        // ── 字面量 ──
        TokenType::Identifier(s) => s.clone(),
        // 转义内层的 `"` 与 `\`，否则含引号的字符串会被改坏。
        //
        // v0.104.6 D210：**还必须转义控制字符**。
        //
        // 修前只处理 `\` 与 `"`，于是源码里的 `"x\ny"`（lexer 已把它解码成
        // 「x + 真换行 + y」）被原样吐出 —— 输出里就有了一个**裸换行**：
        //
        // ```text
        // 源码（4 行 / 3 换行）   let a = "x\ny"
        // 修后（5 行 / 4 换行）   let a = "x
        //                               y"
        // ```
        //
        // 后果有两条，都实测过：
        // ① **保行数不变式被破坏**（D202 立的，D203 的承重前提）；
        // ② **`rangeFormatting` 因此取错行** —— 客户端要格式化源码第 2–3 行，
        //    服务端按错位后的映射返回 `y"` / `let p = 1`，应用即损坏文件。
        //
        // （格式化结果**仍能跑**、二次格式化也**稳定** —— 那是据实记录的边界，
        //  不是本条的缺陷。）
        TokenType::String(s) => format!("\"{}\"", mora_string_body(s)),
        TokenType::Char(c) => format!("'{}'", mora_char_body(*c)),
        TokenType::Int(n) => n.to_string(),
        TokenType::TypedInt(n, dt) => format!("{n}{}", dt.short_name()),
        TokenType::Float(n) => {
            let s = n.to_string();
            if n.is_finite() && !s.contains(['.', 'e', 'E']) {
                format!("{s}.0")
            } else {
                s
            }
        }
        TokenType::TypedFloat(n, dt) => {
            let s = n.to_string();
            if n.is_finite() && !s.contains(['.', 'e', 'E']) {
                format!("{s}.0{}", dt.short_name())
            } else {
                format!("{s}{}", dt.short_name())
            }
        }
        TokenType::Lifetime(s) => format!("'{}'", s),
        // v0.104.6 D202：注释**原样**带出。正文里已经含 `--` 之后的
        // 那个空格（`-- foo` 的正文是 `" foo"`），所以这里只补 `--` 前缀，
        // 拼出的文本与源码**逐字相同** —— 格式化绝不改写注释内容。
        TokenType::Comment(s) => format!("--{}", s),

        // ── 运算符与标点 ──
        TokenType::Plus => "+".into(),
        TokenType::Minus => "-".into(),
        TokenType::Star => "*".into(),
        TokenType::StarStar => "**".into(),
        TokenType::Slash => "/".into(),
        TokenType::Percent => "%".into(),
        TokenType::Assign => "=".into(),
        TokenType::Equal => "==".into(),
        TokenType::NotEqual => "!=".into(),
        TokenType::Greater => ">".into(),
        TokenType::Less => "<".into(),
        TokenType::GreaterEqual => ">=".into(),
        TokenType::LessEqual => "<=".into(),
        TokenType::Pipe => "|>".into(),
        TokenType::Or => "|".into(),
        TokenType::Bang => "!".into(),
        TokenType::At => "@".into(),
        TokenType::Arrow => "->".into(),
        TokenType::FatArrow => "=>".into(),
        TokenType::Question => "?".into(),
        TokenType::ColonColon => "::".into(),
        TokenType::Backtick => "`".into(),
        TokenType::CommaComma => ",,".into(),
        TokenType::LParen => "(".into(),
        TokenType::RParen => ")".into(),
        TokenType::LBracket => "[".into(),
        TokenType::RBracket => "]".into(),
        TokenType::LBrace => "{".into(),
        TokenType::RBrace => "}".into(),
        TokenType::Dot => ".".into(),
        TokenType::DotDot => "..".into(),
        TokenType::DotDotDot => "...".into(),
        TokenType::Comma => ",".into(),
        TokenType::Colon => ":".into(),
        TokenType::Amp => "&".into(),
        TokenType::AmpMut => "&mut".into(),

        // 词法错误：原样带出，格式化不该掩盖问题
        TokenType::Error(e) => format!("/* lexer error: {} */", e),
        // 这两个由 `simple_format` 单独处理（不落到 token_text）
        TokenType::Newline | TokenType::EOF => String::new(),
        // ⚠ 故意**没有** `_ =>` 兜底：D102 的根因正是兜底分支用
        // `format!("{:?}", tt)` 输出枚举变体名。编译器会强制本函数
        // **穷尽所有 TokenType 变体** —— 新增变体时这里编译不过，
        // 于是「新增 token 忘了加拼写映射」这个缺陷在**编译期**就被挡住。
        // （v0.104.6 D202 的 `Comment` 变体就是这么被强制加上的。）
    }
}

// ===================================================================
// Rename
// ===================================================================
