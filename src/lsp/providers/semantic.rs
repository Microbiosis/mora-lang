//! v0.25: LSP semantic tokens provider（语义高亮）。

use std::collections::HashMap;

use super::parsed_doc_v3;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

/// semantic token 的 **legend**（客户端按 `legend.tokenTypes[type]` 取名）。
///
/// v0.104.6 D201：本文件此前**硬编码**了四个索引
/// （`FUNCTION=9.0` / `NUMBER=10.0` / `STRING=12.0` / `VARIABLE=13.0`），
/// 而 `server.rs` 声明的 legend 只有 **8** 项 —— 那些索引**既越界**、
/// 又和 legend 的含义**对不上**（9/10/12/13 恰是标准 LSP `SemanticTokenTypes`
/// 里的 property/enumMember/function/method）。
///
/// 实测：每个 token 的 `type` 都越界（实测 9、10、10、9、13、10），
/// 客户端按 legend 取名就会**取不到**。语义高亮对**每一个** token 都失效。
///
/// 现在 legend 与索引**同源**：改 legend 只需改这一个数组。
pub const TOKEN_TYPES: &[&str] = &[
    "keyword", "function", "variable", "string", "float", "comment", "type", "operator",
];

// 索引 = TOKEN_TYPES 里的位置（0 起）。**不得**手写别的数字。
//
// 目前 `collect` 只发出 FUNCTION / VARIABLE / STRING / FLOAT 四种；
// legend 里另外四项（keyword/comment/type/operator）先占位，
// 将来补上即可，**索引已按 legend 排好**。
const IDX_FUNCTION: f64 = 1.0;
const IDX_VARIABLE: f64 = 2.0;
const IDX_STRING: f64 = 3.0;
const IDX_FLOAT: f64 = 4.0;

/// 一个 token 的**绝对**位置（0-based 行、**char** 列）与类别。
///
/// v0.104.6 D212：此前直接在遍历中累加 delta，于是把「相对量」和
/// 「绝对量」混在一起 —— 详见 `push_token` 的历史注释。此处改为
/// **先收集全部绝对位置，再排序、再编码**，三件事各归各位。
struct SemToken {
    line: usize,
    col: usize,
    len: usize,
    kind: f64,
}

pub fn semantic_tokens_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Object(std::collections::BTreeMap::new()),
    };
    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Object(std::collections::BTreeMap::new()),
    };
    let lines: Vec<&str> = text.lines().collect();

    // ① 收集**绝对**位置（可能乱序）
    let mut toks: Vec<SemToken> = Vec::new();
    for expr in &exprs {
        collect(expr, &lines, &mut toks);
    }
    // ② **按文档顺序**排序 —— 规范要求 delta 编码必须建立在文档顺序上。
    //    深度优先遍历的产出顺序**不是**文档顺序（父节点的 span 起点
    //    可能晚于某些子节点），修前会出现 `deltaLine` 为负。
    toks.sort_by_key(|t| (t.line, t.col));

    // ③ 编码成 5 元组
    let mut data: Vec<f64> = Vec::new();
    let mut last_line = 0usize;
    let mut last_col = 0usize;
    let mut first = true;
    for t in &toks {
        if t.len == 0 {
            continue;
        }
        // v0.104.6 D212：`deltaStart` / `length` 的单位是 **UTF-16 码元**
        // （同 D211）。行内含星平面字符时 char 数与 UTF-16 数不同。
        let col_u16 =
            parsed_doc_v3::char_to_utf16_col(lines.get(t.line).copied().unwrap_or(""), t.col);
        let len_u16 = parsed_doc_v3::char_to_utf16_col(
            &lines
                .get(t.line)
                .copied()
                .unwrap_or("")
                .chars()
                .skip(t.col)
                .take(t.len)
                .collect::<String>(),
            usize::MAX,
        );
        let (dl, dc) = if first {
            first = false;
            (t.line as isize, col_u16 as isize)
        } else if t.line == last_line {
            (0, col_u16 as isize - last_col as isize)
        } else {
            (t.line as isize - last_line as isize, col_u16 as isize)
        };
        data.push(dl as f64);
        data.push(dc as f64);
        data.push(len_u16 as f64);
        data.push(t.kind);
        data.push(0.0); // tokenModifiers
        last_line = t.line;
        last_col = col_u16;
    }

    let mut m = std::collections::BTreeMap::new();
    m.insert(
        "data".to_string(),
        Value::Array(data.into_iter().map(Value::Number).collect()),
    );
    Value::Object(m)
}

/// 遍历一棵 witness 子树，把可高亮的 token 收集进 `out`。
///
/// ⚠ **只在 `walk_witness` 的闭包里发** —— `walk_witness` **包含** `expr`
/// 自身，所以「先单独发根、再遍历整棵子树」会把**每个根节点发两遍**
/// （`print` 在同一位置出现两次 token 就是这么来的）。
///
/// v0.104.6 D212 一并修掉的其余三处：
/// ① **长度取 token 自身的字符数** —— 修前硬编码 `1.0`，
///    于是 `total` 只高亮 1 个字符；
/// ② token 的**起点靠「在该行找它的原文」**确定 —— `span.column` 是
///    **整个表达式**的起点（`let a = 1` 落在 `l` 上），不是 token 自己的；
/// ③ 找不到原文就**不发**这个 token —— 宁可不高亮，也不要高亮到错的地方。
fn collect(expr: &crate::mir::witness::MirWitness, lines: &[&str], out: &mut Vec<SemToken>) {
    parsed_doc_v3::walk_witness(expr, &mut |e| {
        let Some((tok_text, kind)) = token_of(e) else {
            return;
        };
        if tok_text.is_empty() {
            return;
        }
        let line0 = e.span.line.saturating_sub(1);
        let span_col = e.span.column.saturating_sub(1);
        if let Some(line) = lines.get(line0)
            && let Some(col) = locate_token(line, &tok_text, span_col)
        {
            out.push(SemToken {
                line: line0,
                col,
                len: tok_text.chars().count(),
                kind,
            });
        }
    });
}

/// 这个 witness 自己对应的**高亮 token 原文**与类别；不可高亮则 `None`。
fn token_of(e: &crate::mir::witness::MirWitness) -> Option<(String, f64)> {
    use crate::mir::witness::{WitnessCallee, WitnessKind};
    match &e.kind {
        WitnessKind::Variable(n) => Some((n.clone(), IDX_VARIABLE)),
        WitnessKind::Literal(crate::common::Literal::String(s, _)) => Some((s.clone(), IDX_STRING)),
        WitnessKind::Literal(crate::common::Literal::Int(n, _)) => Some((n.to_string(), IDX_FLOAT)),
        WitnessKind::Literal(crate::common::Literal::Float(n, _)) => {
            Some((format!("{}", n), IDX_FLOAT))
        }
        WitnessKind::Call { callee, .. } => match callee {
            WitnessCallee::Name(n) | WitnessCallee::Var(n) => Some((n.clone(), IDX_FUNCTION)),
            WitnessCallee::Method(_, m) => Some((m.clone(), IDX_FUNCTION)),
            _ => None,
        },
        _ => None,
    }
}

/// 在一行里找 `needle` 的**完整词**出现位置，尽量靠近 `hint`。
///
/// 双向就近查找：`span.column`（整个表达式的起点）在不同构造里落点不同 ——
/// 有的落在 token **之前**（`let a = 1` 的 span 起点在 `l`），有的落在**之后**。
/// 找不到就返回 `None`（调用方选择不发这个 token）。
fn locate_token(line: &str, needle: &str, hint: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let key: Vec<char> = needle.chars().collect();
    if key.is_empty() || key.len() > chars.len() {
        return None;
    }
    let from = hint.min(chars.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let hit = |i: usize| {
        chars[i..i + key.len()] == key[..]
            && !i
                .checked_sub(1)
                .and_then(|k| chars.get(k))
                .is_some_and(|c| is_word(*c))
            && !chars.get(i + key.len()).is_some_and(|c| is_word(*c))
    };
    (from..=chars.len() - key.len())
        .find(|&i| hit(i))
        .or_else(|| (0..from).rev().find(|&i| hit(i)))
}
