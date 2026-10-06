//! v0.27: MarkdownBackend — uses pulldown-cmark event iterator.
//!
//! Strategy:
//!   1. Cache the source string at construction time.
//!   2. `markdown()` returns the source as-is (no transformation).
//!   3. `text()` walks the pulldown-cmark event iterator and concatenates only
//!      `Event::Text` payloads — markers like `#`, `**`, fences are dropped.
//!   4. `blocks()` walks the same iterator but tracks `Tag::Heading`,
//!      `Tag::CodeBlock`, `Tag::Paragraph` Start/End pairs to emit one block
//!      per logical unit with the correct `kind` and accumulated text.
//!   5. `metadata()` returns `{origin: "markdown", pages: 1, size: source.len()}`
//!      matching the v0.27 contract from Task 4.
//!   6. `pages()` returns a single-page mock whose `blocks` list is the
//!      flattened `blocks()` output.

use std::collections::HashMap;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::document::DocumentBackend;
use crate::value::Value;

/// v0.27: Markdown backend.
#[derive(Debug)]
pub struct MarkdownBackend {
    pub source: String,
}

impl MarkdownBackend {
    /// Parse an in-memory markdown string.
    pub fn new(s: &str) -> Self {
        Self {
            source: s.to_string(),
        }
    }
}

/// v0.104.6 D229：块级标签的 `Start` 决定 `kind`。
///
/// **不含** `BlockQuote` / `List` / `Item` 的父容器之外的标签 —— 见
/// [`block_kind`]。这里列的是「会开启一个独立块」的标签。
fn kind_of(tag: &Tag) -> Option<&'static str> {
    match tag {
        Tag::Heading { .. } => Some("heading"),
        Tag::CodeBlock(_) => Some("code"),
        Tag::Paragraph => Some("text"),
        Tag::Item => Some("text"),
        _ => None,
    }
}

/// v0.104.6 D229：块级 `End` —— 只有这些标签闭合时才收尾一个块。
///
/// 修前是 `Event::End(_) if !current_text.is_empty()` —— **任何** `End`
/// 都收尾，于是内联标记（`**bold**` 的 `End(Strong)`、链接的 `End(Link)`、
/// 强调的 `End(Emphasis)`）把一个段落**劈成多块**，实测 6 块变 11 块。
///
/// 正确的闭合集合由 pulldown-cmark 0.13 的事件流实测得出（外部探针
/// dump 全部事件，见 CHANGELOG D229），不是凭记忆写的。
///
/// ⚠ `BlockQuote` **不在**此列：引用块内部还有 `Paragraph`，
/// `End(Paragraph)` 已经收过一次，再对 `End(BlockQuote)` 收一次会
/// 切出一个空块。空块由 `flush` 的 `t.trim().is_empty()` 挡掉，
/// 但靠过滤而不是靠「不重复收尾」，是能避免的额外一步。
fn is_block_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::CodeBlock | TagEnd::Item
    )
}

impl DocumentBackend for MarkdownBackend {
    fn origin(&self) -> &'static str {
        "markdown"
    }

    /// Single-page mock — markdown has no notion of "page" so we emit one
    /// page whose blocks are the full `blocks()` list.
    fn pages(&self) -> Result<Value, String> {
        let mut page_dict: HashMap<String, Value> = HashMap::new();
        page_dict.insert("page_no".into(), Value::Float(1.0));
        page_dict.insert("width".into(), Value::Float(0.0));
        page_dict.insert("height".into(), Value::Float(0.0));
        page_dict.insert("blocks".into(), self.blocks()?);
        Ok(Value::List(vec![Value::Dict(page_dict)].into()))
    }

    /// MVP: return the source verbatim. Real Markdown→HTML→text round-trip is
    /// out of scope for v0.27 — the caller is expected to feed the source to
    /// an LLM or renderer.
    fn markdown(&self) -> Result<String, String> {
        Ok(self.source.clone())
    }

    /// 纯文本：按**块**拼接，每块一行。
    ///
    /// v0.104.6 D229：修前是「每个 `Event::Text` 后补一个 `\n`」——
    /// 即**按事件**换行。段内的内联标记各产生一个 `Text` 事件，于是
    /// `Title with **bold** inside` 变成三行：
    ///
    /// ```text
    /// Title with
    /// bold
    ///  inside
    /// ```
    ///
    /// 「纯文本」被拆成了碎片，词被切开。改为复用 `blocks()` 的块结构，
    /// 每块追加**一个**换行。
    ///
    /// 块文本**先 `trim_end` 再补换行**：代码块的 `Event::Text` 自带尾
    /// 换行（`"fn main() { … }\n"`），不 trim 会多出一行空行 ——
    /// 实测 `text()` 9 行 vs `blocks()` 8 块，往返不变式立刻变红。
    fn text(&self) -> Result<String, String> {
        let mut out = String::new();
        for (_kind, block_text) in self.block_texts() {
            out.push_str(block_text.trim_end());
            out.push('\n');
        }
        Ok(out)
    }

    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("markdown".into()));
        m.insert("pages".into(), Value::Float(1.0));
        m.insert("size".into(), Value::Float(self.source.len() as f64));
        Ok(Value::Dict(m))
    }

    /// v0.104.6 D229：按**块级** Start/End 切分，并携带正确的 `kind`。
    ///
    /// 修前有两个互相独立的缺陷：
    ///
    /// ① **切块点错**：`End(_)` 无条件收尾 ⇒ 内联标记把块劈开。
    ///    实测 `# Title with **bold** inside` 变成两块
    ///    （`Title with bold` + ` inside`）。
    ///
    /// ② **kind 继承**：`current_kind` 只在 Heading / CodeBlock / Paragraph
    /// 的 `Start` 里被赋值，但 `List`/`Item` 的 `Start` **不重置它**。
    ///    于是 `## Sub heading` 之后的 `- item one` / `- item two`
    ///    继承了上一个 heading 的 `kind="heading"` ——
    ///    **两个列表项被标成标题**。
    fn blocks(&self) -> Result<Value, String> {
        let blocks: Vec<Value> = self
            .block_texts()
            .into_iter()
            .map(|(kind, text)| make_block(kind, &text))
            .collect();
        Ok(Value::List(blocks.into()))
    }
}

impl MarkdownBackend {
    /// v0.104.6 D229：`blocks()` 与 `text()` 的**唯一**切块实现。
    ///
    /// 两者修前各自遍历一遍事件流、用**两套不同的切块规则**（`blocks()`
    /// 按 `End(_)`，`text()` 按每个 `Event::Text`），于是二者对
    /// 「一个文档有多少块」的判断**不一致**。合并成一份后不可能漂移。
    ///
    /// 返回 `(kind, text)` 序列；空块不产出。
    fn block_texts(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let mut current_kind: &'static str = "text";
        let mut current_text = String::new();

        // 只在块级标签的 Start 开启新块时切分；`BlockQuote` / `List`
        // 是容器，其内部还要靠 Paragraph/Item 切，故不在此列。
        let flush =
            |out: &mut Vec<(&'static str, String)>, kind: &'static str, text: &mut String| {
                let t = std::mem::take(text);
                if !t.trim().is_empty() {
                    out.push((kind, t));
                }
            };

        for ev in Parser::new(&self.source) {
            match ev {
                Event::Start(tag) => {
                    if let Some(kind) = kind_of(&tag) {
                        flush(&mut out, current_kind, &mut current_text);
                        current_kind = kind;
                    }
                }
                Event::End(tag) if is_block_end(&tag) => {
                    flush(&mut out, current_kind, &mut current_text);
                }
                // 内联文本 / 代码 / 软硬换行 / 数学 / HTML —— 一律并入当前块
                Event::Text(t) => current_text.push_str(&t),
                Event::Code(c) => current_text.push_str(&c),
                Event::SoftBreak | Event::HardBreak => current_text.push('\n'),
                _ => {}
            }
        }
        flush(&mut out, current_kind, &mut current_text);
        out
    }
}

fn make_block(kind: &str, text: &str) -> Value {
    let mut bd: HashMap<String, Value> = HashMap::new();
    bd.insert("kind".into(), Value::String(kind.into()));
    bd.insert(
        "bbox".into(),
        Value::List(vec![Value::Float(0.0); 4].into()),
    );

    let mut span: HashMap<String, Value> = HashMap::new();
    span.insert("text".into(), Value::String(text.into()));
    span.insert(
        "bbox".into(),
        Value::List(vec![Value::Float(0.0); 4].into()),
    );
    span.insert("score".into(), Value::Nil);
    bd.insert("spans".into(), Value::List(vec![Value::Dict(span)].into()));

    Value::Dict(bd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_markdown() {
        let md = "# Title\n\nSome text here.\n\n```rust\nlet x = 1;\n```\n";
        let backend = MarkdownBackend::new(md);
        assert_eq!(backend.origin(), "markdown");
        assert_eq!(backend.markdown().unwrap(), md);
        let blocks = backend.blocks().unwrap();
        if let Value::List(bs) = blocks {
            assert!(
                bs.len() >= 3,
                "should produce heading + paragraph + code blocks, got {}",
                bs.len()
            );
        } else {
            panic!("blocks should be list");
        }
    }

    #[test]
    fn text_strips_markdown() {
        let md = "# Title\n\nSome **bold** text.";
        let backend = MarkdownBackend::new(md);
        let text = backend.text().unwrap();
        assert!(
            !text.contains("#"),
            "should strip heading marker, got: {:?}",
            text
        );
        assert!(text.contains("Title"));
        assert!(text.contains("bold"));
    }
}
