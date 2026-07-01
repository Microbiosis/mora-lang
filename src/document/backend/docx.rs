//! v0.28: DocxBackend — wraps undoc 0.5.2 pure-Rust DOCX parser.
//!
//! ## undoc 0.5.2 API reconciliation (vs. spec §6.1 pseudocode)
//!
//! The spec §6.1 pseudocode assumes a `core_properties()` method and a per-
//! section `paragraphs()` / `to_markdown(Section)` surface that does NOT
//! exist in the published 0.5.2 API. The actual shape (verified by reading
//! `undoc-0.5.2/src/docx/parser.rs` and `model/document.rs`):
//!
//! - Entry: `undoc::docx::DocxParser::from_bytes(Vec<u8>) -> Result<Self>`
//!   (no separate `open` step — bytes go straight into the container).
//! - Drive: `parser.parse(&mut self) -> Result<Document>`.
//! - Sections: a normal DOCX yields exactly one `Section` (the whole body);
//!   page-section breaks are flattened into the same single section by the
//!   parser. So per-section content is `doc.sections[0].content`.
//! - Per-paragraph text: there is no free `to_markdown(Document) -> String`
//!   and no `Section::paragraphs()` iterator in this version. We build a
//!   best-effort markdown by joining `Plain(paragraph.plain_text())` for
//!   each `Block::Paragraph` in `section.content` (same algorithm
//!   `Document::plain_text` uses internally for the doc-level case).
//! - Core properties: lives on `doc.metadata` as a `Metadata` struct with
//!   `Option<String>` title/author/subject/description/created/modified/
//!   application/last_modified_by + a `keywords: Vec<String>` + optional
//!   `page_count: Option<u32>` and `word_count: Option<u32>`.
//!
//! Per project rule: errors must serialize as
//! `"document.parse: undoc docx <phase> error: <details>"`. We preserve that
//! contract through the 5 stages: `from_bytes`, `parse`, `parse_metadata`
//! (folded into `parse`), per-paragraph text extraction, `metadata()`
//! composition.
use std::collections::HashMap;
use crate::value::Value;
use crate::document::DocumentBackend;

/// v0.28: DOCX backend.
///
/// Caches the raw bytes (for downstream consumers), the paragraph count (so
/// `metadata()` can report it without re-parsing), per-paragraph text
/// (one entry per block-level paragraph, in document order), and the
/// best-effort metadata map extracted from the OOXML core properties.
///
/// DOCX has no real page concept in this MVP; `pages()` returns a single
/// synthetic page holding every block.
#[derive(Debug)]
pub struct DocxBackend {
    pub bytes: Vec<u8>,
    pub paragraph_count: usize,
    pub paragraphs: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl DocxBackend {
    /// Parse a DOCX byte stream into a `DocxBackend`.
    ///
    /// Phase failures all surface with the project-standard
    /// `document.parse: undoc docx <phase> error: <details>` prefix:
    ///   - phase `from_bytes`: container / ZIP / OOXML skeleton unreadable
    ///   - phase `parse`: section / metadata construction failed
    ///   - phase `metadata`: never fails (best-effort) — see `metadata()`
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        // 1. Build the parser. `DocxParser::from_bytes` consumes the Vec<u8>
        //    and returns the prepared container; this is the cheap step.
        let mut parser = undoc::docx::DocxParser::from_bytes(bytes.clone())
            .map_err(|e| {
                format!(
                    "document.parse: undoc docx from_bytes error: {}",
                    e
                )
            })?;

        // 2. Run `parse`. This is where the section tree and metadata are
        //    populated; it's the stage that touches user XML the most.
        let doc = parser
            .parse()
            .map_err(|e| format!("document.parse: undoc docx parse error: {}", e))?;

        // 3. Pull per-paragraph text. undoc 0.5.2 has no `to_markdown(Document)`;
        //    we approximate by reading every Section's Paragraph blocks and
        //    storing `paragraph.plain_text()` as one entry per paragraph.
        //    Tables / footnotes / section breaks are intentionally skipped at
        //    this stage (best-effort, matches `Document::plain_text`).
        let mut paragraphs: Vec<String> = Vec::new();
        for section in &doc.sections {
            for block in &section.content {
                if let undoc::Block::Paragraph(para) = block {
                    let t = para.plain_text();
                    if !t.is_empty() {
                        paragraphs.push(t);
                    }
                }
            }
        }
        let paragraph_count = paragraphs.len();

        // 4. Best-effort metadata extraction. Every field on `Metadata` is
        //    optional; missing fields just stay out of the map.
        let mut metadata: HashMap<String, String> = HashMap::new();
        let m = &doc.metadata;
        if let Some(t) = &m.title {
            metadata.insert("title".into(), t.clone());
        }
        if let Some(a) = &m.author {
            metadata.insert("author".into(), a.clone());
        }
        if let Some(s) = &m.subject {
            metadata.insert("subject".into(), s.clone());
        }
        if let Some(d) = &m.description {
            metadata.insert("description".into(), d.clone());
        }
        if let Some(c) = &m.created {
            metadata.insert("created".into(), c.clone());
        }
        if let Some(modified) = &m.modified {
            metadata.insert("modified".into(), modified.clone());
        }
        if let Some(app) = &m.application {
            metadata.insert("application".into(), app.clone());
        }
        if let Some(lmb) = &m.last_modified_by {
            metadata.insert("last_modified_by".into(), lmb.clone());
        }
        if !m.keywords.is_empty() {
            metadata.insert("keywords".into(), m.keywords.join(", "));
        }
        if let Some(pc) = m.page_count {
            metadata.insert("page_count".into(), pc.to_string());
        }
        if let Some(wc) = m.word_count {
            metadata.insert("word_count".into(), wc.to_string());
        }

        Ok(Self {
            bytes,
            paragraph_count,
            paragraphs,
            metadata,
        })
    }
}

impl DocumentBackend for DocxBackend {
    fn origin(&self) -> &'static str {
        "docx"
    }

    /// DOCX has no real page model in our IR. Return a single synthetic page
    /// with width/height = 0 (placeholder geometry) wrapping every paragraph
    /// as a `text` block whose single span carries the paragraph text. There
    /// is no per-text-run bbox in the undoc DOCX output, so spans use a zero
    /// bbox and `score = nil` (matches the PPTX backend's MVP shape).
    fn pages(&self) -> Result<Value, String> {
        let mut blocks: Vec<Value> = Vec::with_capacity(self.paragraphs.len());
        for p in &self.paragraphs {
            let mut block_dict: HashMap<String, Value> = HashMap::new();
            block_dict.insert("kind".into(), Value::String("text".into()));
            block_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(0.0),
                ]),
            );
            let mut span_dict: HashMap<String, Value> = HashMap::new();
            span_dict.insert("text".into(), Value::String(p.clone()));
            span_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(0.0),
                ]),
            );
            span_dict.insert("score".into(), Value::Nil);
            block_dict.insert("spans".into(), Value::List(vec![Value::Dict(span_dict)]));
            blocks.push(Value::Dict(block_dict));
        }

        let mut page_dict: HashMap<String, Value> = HashMap::new();
        page_dict.insert("page_no".into(), Value::Number(1.0));
        page_dict.insert("width".into(), Value::Number(0.0));
        page_dict.insert("height".into(), Value::Number(0.0));
        page_dict.insert("blocks".into(), Value::List(blocks));
        Ok(Value::List(vec![Value::Dict(page_dict)]))
    }

    /// Paragraph text joined with `\n\n` (blank line between paragraphs).
    fn markdown(&self) -> Result<String, String> {
        Ok(self.paragraphs.join("\n\n"))
    }

    /// Same as `markdown()` here — plain concatenated transcript. Kept as a
    /// separate method to mirror the PdfBackend / PptxBackend shapes.
    fn text(&self) -> Result<String, String> {
        Ok(self.paragraphs.join("\n\n"))
    }

    /// `{origin, pages, size}` plus any metadata fields we managed to read.
    /// `pages` is always `1` here because we collapse to a synthetic page;
    /// `size` is the raw byte length. All other entries are strings; absent
    /// optional fields simply aren't present in the dict.
    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("docx".into()));
        m.insert("pages".into(), Value::Number(1.0));
        m.insert("size".into(), Value::Number(self.bytes.len() as f64));
        for (k, v) in &self.metadata {
            m.insert(k.clone(), Value::String(v.clone()));
        }
        Ok(Value::Dict(m))
    }

    /// Flatten every paragraph into a single block list (the only `page` is
    /// synthetic). Mirrors `PptxBackend::blocks` shape: one block per text
    /// run, span text = paragraph text.
    fn blocks(&self) -> Result<Value, String> {
        let pages = self.pages()?;
        let mut blocks: Vec<Value> = Vec::new();
        if let Value::List(pl) = pages {
            for page in pl {
                if let Value::Dict(pd) = page
                    && let Some(Value::List(bs)) = pd.get("blocks")
                {
                    blocks.extend(bs.clone());
                }
            }
        }
        Ok(Value::List(blocks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_docx() {
        // minimal: 构造空 struct,直接验 trait method
        let backend = DocxBackend {
            bytes: vec![],
            paragraph_count: 0,
            paragraphs: vec![],
            metadata: HashMap::new(),
        };
        assert_eq!(backend.origin(), "docx");
    }

    #[test]
    fn parses_real_docx() {
        let path = format!("{}/tests/fixtures/sample.docx", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).expect("fixture file");
        let backend = DocxBackend::from_bytes(bytes).expect("parse");
        assert_eq!(backend.origin(), "docx");
        assert!(
            backend.paragraph_count >= 2,
            "expected ≥2 paragraphs, got {}",
            backend.paragraph_count
        );
        let md = backend.markdown().unwrap();
        assert!(!md.is_empty(), "markdown non-empty, got empty: {:?}", md);
    }
}
