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

use pulldown_cmark::{Event, Parser, Tag};

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

impl DocumentBackend for MarkdownBackend {
    fn origin(&self) -> &'static str {
        "markdown"
    }

    /// Single-page mock — markdown has no notion of "page" so we emit one
    /// page whose blocks are the full `blocks()` list.
    fn pages(&self) -> Result<Value, String> {
        let mut page_dict: HashMap<String, Value> = HashMap::new();
        page_dict.insert("page_no".into(), Value::Number(1.0));
        page_dict.insert("width".into(), Value::Number(0.0));
        page_dict.insert("height".into(), Value::Number(0.0));
        page_dict.insert("blocks".into(), self.blocks()?);
        Ok(Value::List(vec![Value::Dict(page_dict)]))
    }

    /// MVP: return the source verbatim. Real Markdown→HTML→text round-trip is
    /// out of scope for v0.27 — the caller is expected to feed the source to
    /// an LLM or renderer.
    fn markdown(&self) -> Result<String, String> {
        Ok(self.source.clone())
    }

    /// Strip markdown markers by walking the pulldown-cmark event iterator
    /// and emitting only the textual payload.
    fn text(&self) -> Result<String, String> {
        let mut out = String::new();
        for ev in Parser::new(&self.source) {
            if let Event::Text(t) = ev {
                out.push_str(&t);
                out.push('\n');
            }
        }
        Ok(out)
    }

    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("markdown".into()));
        m.insert("pages".into(), Value::Number(1.0));
        m.insert("size".into(), Value::Number(self.source.len() as f64));
        Ok(Value::Dict(m))
    }

    /// Iterate the parser events, accumulating text between Start/End pairs
    /// and emitting one block per heading / paragraph / code-block.
    fn blocks(&self) -> Result<Value, String> {
        let mut blocks: Vec<Value> = Vec::new();
        let mut current_kind = String::from("text");
        let mut current_text = String::new();

        for ev in Parser::new(&self.source) {
            match ev {
                Event::Start(Tag::Heading { .. }) => {
                    if !current_text.is_empty() {
                        blocks.push(make_block(&current_kind, &current_text));
                        current_text.clear();
                    }
                    current_kind = "heading".into();
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    if !current_text.is_empty() {
                        blocks.push(make_block(&current_kind, &current_text));
                        current_text.clear();
                    }
                    current_kind = "code".into();
                }
                Event::Start(Tag::Paragraph) => {
                    if !current_text.is_empty() {
                        blocks.push(make_block(&current_kind, &current_text));
                        current_text.clear();
                    }
                    current_kind = "text".into();
                }
                Event::Text(t) => current_text.push_str(&t),
                Event::Code(c) => current_text.push_str(&c),
                Event::End(_) if !current_text.is_empty() => {
                    blocks.push(make_block(&current_kind, &current_text));
                    current_text.clear();
                }
                _ => {}
            }
        }
        if !current_text.is_empty() {
            blocks.push(make_block(&current_kind, &current_text));
        }
        Ok(Value::List(blocks))
    }
}

fn make_block(kind: &str, text: &str) -> Value {
    let mut bd: HashMap<String, Value> = HashMap::new();
    bd.insert("kind".into(), Value::String(kind.into()));
    bd.insert("bbox".into(), Value::List(vec![Value::Number(0.0); 4]));

    let mut span: HashMap<String, Value> = HashMap::new();
    span.insert("text".into(), Value::String(text.into()));
    span.insert("bbox".into(), Value::List(vec![Value::Number(0.0); 4]));
    span.insert("score".into(), Value::Nil);
    bd.insert("spans".into(), Value::List(vec![Value::Dict(span)]));

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
