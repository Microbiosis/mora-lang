//! v0.25: LSP formatting provider（格式化）。

use super::parsed_doc_v3;
use crate::lsp::json::Value;
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

    // 基础格式化：每行按 token 简单重排。
    // 完整实现需要 AST-aware formatter；这里给一个能工作的最小版本——
    // 行首/行末去空白 + 一致缩进（2 空格，按 then / for body / task body / do-end 嵌套加 1 层）。
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
    let formatted = simple_format(&text);

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

/// 简单 formatter：trim 行尾空白 + 缩进。基础但能跑。
///
/// **保行数不变式**（v0.104.6 D202）：输出的 `\n` 个数**必须**与输入相等。
/// 两条依赖：
/// ① 注释不能被吞（lexer 靠 `keep_comments(true)` 把 `--` 发成 token）；
/// ② 空行不能被吞（`Newline` **无条件**换行，不再去重连续空行）。
/// 少了任何一条，输出就会比输入**短**，`slice_edit` 的「第 k 行对第 k 行」
/// 也就跟着失效。
fn simple_format(text: &str) -> String {
    // 扫描 token，按 indent 规则重新组装
    //
    // v0.104.6 D202：必须 `keep_comments(true)` —— 否则 lexer 静默丢弃 `--`，
    // 而本函数是**从 token 流重建整份文档**：lexer 不发的东西就等于不存在，
    // 于是格式化会**删光文件里所有注释**（实测 5 行含 3 处 `--` 的源码
    // → 4 行 0 注释，而服务器声明了 `documentFormattingProvider: true`，
    // 编辑器「保存时格式化」一按，用户的注释就没了）。
    let tokens = crate::lexer::Lexer::new(text)
        .keep_comments(true)
        .scan_tokens();
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut needs_indent = true;

    use crate::lexer::TokenType;
    for tok in &tokens {
        match &tok.token_type {
            // v0.104.6 D202：**无条件**换行。
            //
            // 修前是 `if !last_was_newline { … }` —— 连续两个 `Newline`
            // （即空行）只出**一个**换行，于是空行被吞：实测 6 行源码
            // （含 2 个空行 + 1 个纯空白行）格式化成 4 行。
            //
            // 保留连续换行还让「输出第 k 行 ↔ 输入第 k 行」成立，
            // `slice_edit` 的区间切片才可靠（见上）。
            TokenType::Newline => {
                trim_trailing_spaces(&mut out);
                out.push('\n');
                needs_indent = true;
            }
            TokenType::EOF => {
                trim_trailing_spaces(&mut out);
                // v0.104.6 D202：只补**源码本来就有**的收尾换行 ——
                // 无条件补会凭空多出一个 `\n`，破坏上面的保行数不变式
                // （`"a"` 会变成 `"a \n"`）。
                if !out.is_empty() && !out.ends_with('\n') && text.ends_with('\n') {
                    out.push('\n');
                }
            }
            TokenType::LBrace => {
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                out.push_str(&token_text(&tok.token_type));
                // v0.104.6 D196：花括号块**开启**一层
                // （修前只有 RBrace 在减、开括号从不加 → 花括号块永远缩进 0）。
                depth += 1;
            }
            // `(` / `[` 是**表达式**分组，不是块 —— 不参与层级
            // （修前 LParen 也不加，但 RParen 却在减，见下）。
            TokenType::LBracket | TokenType::LParen => {
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                out.push_str(&token_text(&tok.token_type));
            }
            // v0.104.6 D196：`end` 是**闭合**关键字。
            // 修前它 `depth += 1`（当作开启）—— 于是**循环/条件之外的语句
            // 被缩进、循环体反而顶格**（实测 `for … end` + 一行顶层语句：
            // 体顶格、顶层语句缩进 2）。此处改为先减后打印。
            TokenType::End => {
                depth = depth.saturating_sub(1);
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                out.push_str("end");
            }
            TokenType::RBrace => {
                depth = depth.saturating_sub(1);
                out.push_str(&token_text(&tok.token_type));
            }
            // v0.104.6 D196：`)` / `]` **不改**层级（修前与 `}` 一起减，
            // 于是 `print(x)` 会把层级减 1）。
            TokenType::RBracket | TokenType::RParen => {
                out.push_str(&token_text(&tok.token_type));
            }
            _ => {
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                // v0.104.6 D202：注释走这条**普通 token** 路径 ——
                // `token_text` 原样带出 `--` + 正文，于是
                // 「行尾注释」仍留在行尾、「独立注释行」仍是独立的一行，
                // 且不影响层级（`is_block_opener` 对 `Comment` 为假）。
                out.push_str(&token_text(&tok.token_type));
                out.push(' ');
                // v0.104.6 D196：块的开端在这里**进入一层**。
                // 修前只有 `End` / `Then` 会 `depth += 1`，而 `End` 是闭合方 ——
                // 于是没有任何开启方，`depth` 只会朝错误方向走。
                if is_block_opener(&tok.token_type) {
                    depth += 1;
                }
            }
        }
    }
    out
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

/// v0.104.6 D196：这个 token 是否**开启**一个缩进层级。
///
/// 分两类来源，因为本语言的块开头一半是关键字、一半不是：
/// - 关键字 token：`task` / `for` / `if` / `match` / `fn` / `macro` / `with`；
/// - **标识符** token：`while` / `worker` / `handle` / `observe` /
///   `parallel` / `transaction` —— 它们在 `lexer.rs` 的 `TokenType` 枚举里
///   **根本不是关键字**（与 `else` 同类，见 `token_text` 的注释），
///   只能按文本判断。
///
/// ⚠ `else` / `then` **不开**层级：`if x then … else … end` 里
/// 开启方是 `if`，`then` / `else` 只是分隔符。
fn is_block_opener(tt: &crate::lexer::TokenType) -> bool {
    use crate::lexer::TokenType;
    match tt {
        TokenType::Task
        | TokenType::For
        | TokenType::If
        | TokenType::Match
        | TokenType::Fn
        | TokenType::Macro
        | TokenType::With => true,
        TokenType::Identifier(s) => matches!(
            s.as_str(),
            "while" | "worker" | "handle" | "observe" | "parallel" | "transaction"
        ),
        _ => false,
    }
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Mora `"…"` / `p"…"` 字面量的**正文**转义（不含外层引号）。
///
/// v0.104.6 D210：转义 `\` `"` 与**全部控制字符**。
///
/// lexer 把源码里的 `\n` `\t` `\x0c` … **解码成真字符**后才交给
/// `token_text`，所以重排时若不把它们重新转义回去，输出里就会出现
/// **裸控制字符**（尤其是裸换行）—— 那会凭空多出一行，
/// 破坏 `simple_format` 的保行数不变式。
///
/// 规则与 JSON 字符串转义一致（`flow::escape_json_string` 的同款语义），
/// 但**这是 Mora 源码字面量**、不是 JSON，故单列一份而不是复用 ——
/// 两者的定界符规则将来可能分道扬镳（`p"…"` 的转义规则本就与 `"…"` 不同）。
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
        TokenType::Orchestrate => "orchestrate".into(),
        TokenType::Loop => "loop".into(),
        TokenType::MaxRounds => "max_rounds".into(),
        TokenType::Prompt => "prompt".into(),
        TokenType::Document => "document".into(),
        TokenType::With => "with".into(),
        TokenType::App => "app".into(),
        TokenType::Rel => "rel".into(),
        TokenType::Solve => "solve".into(),
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
        TokenType::PromptString(s) => format!("p\"{}\"", mora_string_body(s)),
        // ⚠ D102 之前**没有**这两条：`Int` 会变成 `"int"`、
        // `BigInt` 会变成 `"bigint(123)"`。
        TokenType::Int(n) => n.to_string(),
        TokenType::Float(n) => n.to_string(),
        TokenType::BigInt(n) => format!("{}n", n),
        TokenType::Lifetime(s) => format!("'{}'", s),
        // v0.104.6 D202：注释**原样**带出。正文里已经含 `--` 之后的
        // 那个空格（`-- foo` 的正文是 `" foo"`），所以这里只补 `--` 前缀，
        // 拼出的文本与源码**逐字相同** —— 格式化绝不改写注释内容。
        TokenType::Comment(s) => format!("--{}", s),

        // ── 运算符与标点 ──
        TokenType::Plus => "+".into(),
        TokenType::Minus => "-".into(),
        TokenType::Star => "*".into(),
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
