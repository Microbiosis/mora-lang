//! v0.28: PptxBackend — wraps undoc 0.5.2 pure-Rust PPTX parser.
//!
//! ## undoc 0.5.2 API reconciliation (vs. spec §6.1 pseudocode)
//!
//! The spec §6.1 pseudocode assumes a doc-slides-iter / per-slide `to_markdown`
//! surface that does NOT exist in the published 0.5.2 API. The actual shape:
//!
//! - Entry: `undoc::pptx::PptxParser::from_bytes(Vec<u8>) -> Result<Self>`
//!   (no separate `open` step; bytes go straight into the container).
//! - Drive: `parser.parse(&mut self) -> Result<Document>`.
//! - Slides: there is **no** `doc.slides()` / `slide(i)` accessor. The PPTX
//!   parser represents each slide as a `Section` and appends them to
//!   `Document.sections` in `parse()`. So per-slide content is
//!   `doc.sections[i]` (a `Section` whose `name` carries the slide title).
//! - Per-slide markdown: there is no free `to_markdown(Document) -> String`
//!   in this version. `to_markdown` only takes a path. So we build a
//!   best-effort markdown per section by joining `Plain(paragraph.plain_text())`
//!   for each `Block::Paragraph` in `section.content`. This is the same
//!   algorithm `Document::plain_text` uses internally for the doc-level case.
//! - Core properties: lives on `doc.metadata` as a `Metadata` struct with
//!   `Option<String>` title/author/subject/description and a few scalar
//!   counters. No `core_properties()` method — direct field access.
//!
//! Per project rule: errors must serialize as
//! `"document.parse: undoc pptx <phase> error: <details>"`. We preserve that
//! contract through the 5 stages: `from_bytes`, `parse`, `parse_metadata`
//! (none — folded into `parse`), per-slide text extraction, `metadata()`
//! composition.
use std::collections::HashMap;
use crate::value::Value;
use crate::document::DocumentBackend;

/// v0.28: PPTX backend.
///
/// Caches the raw bytes (for downstream consumers), the slide count (so
/// `metadata()` can report it without re-parsing), per-slide markdown text
/// (one entry per slide, in slide order), and the best-effort metadata map
/// extracted from the OOXML core properties.
#[derive(Debug)]
pub struct PptxBackend {
    pub bytes: Vec<u8>,
    pub slide_count: usize,
    pub per_slide_text: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl PptxBackend {
    /// Parse a PPTX byte stream into a `PptxBackend`.
    ///
    /// Phase failures all surface with the project-standard
    /// `document.parse: undoc pptx <phase> error: <details>` prefix:
    ///   - phase `from_bytes`: container / ZIP / OOXML skeleton unreadable
    ///   - phase `parse`: section / metadata construction failed
    ///   - phase `metadata`: never fails (best-effort) — see `metadata()`
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        // 1. Build the parser. `PptxParser::from_bytes` consumes the Vec<u8>
        //    and returns the prepared container; this is the cheap step.
        let mut parser = undoc::pptx::PptxParser::from_bytes(bytes.clone())
            .map_err(|e| {
                format!(
                    "document.parse: undoc pptx from_bytes error: {}",
                    e
                )
            })?;

        // 2. Run `parse`. This is where slides → Sections and metadata are
        //    populated; it's the stage that touches user XML the most.
        let doc = parser
            .parse()
            .map_err(|e| format!("document.parse: undoc pptx parse error: {}", e))?;

        // 3. Pull per-slide text. undoc 0.5.2 has no `to_markdown(Document)`;
        //    we approximate by reading each Section's Paragraph blocks and
        //    concatenating `paragraph.plain_text()`. Tables / notes are
        //    ignored at this stage (best-effort, matches `Document::plain_text`).
        let mut per_slide_text: Vec<String> = Vec::with_capacity(doc.sections.len());
        for section in &doc.sections {
            let mut buf = String::new();
            for block in &section.content {
                if let undoc::model::Block::Paragraph(para) = block {
                    let t = para.plain_text();
                    if !t.is_empty() {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(&t);
                    }
                }
            }
            per_slide_text.push(buf);
        }

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
        if !m.keywords.is_empty() {
            metadata.insert("keywords".into(), m.keywords.join(", "));
        }

        Ok(Self {
            bytes,
            slide_count: doc.sections.len(),
            per_slide_text,
            metadata,
        })
    }
}

impl DocumentBackend for PptxBackend {
    fn origin(&self) -> &'static str {
        "pptx"
    }

    /// One IR page per slide. Page geometry is a fixed 16:9 at 96 dpi
    /// (960×540 — PowerPoint's default widescreen in points×dpi). Each slide
    /// gets exactly one placeholder `text` block whose single span carries
    /// the slide's concatenated plain text. There's no per-text-run bbox in
    /// the undoc PPTX output, so spans use the slide bbox and `score = nil`
    /// (matches the PDF backend's MVP shape).
    fn pages(&self) -> Result<Value, String> {
        let mut out: Vec<Value> = Vec::with_capacity(self.per_slide_text.len());
        for (i, text) in self.per_slide_text.iter().enumerate() {
            let mut page_dict: HashMap<String, Value> = HashMap::new();
            page_dict.insert("page_no".into(), Value::Number((i as f64) + 1.0));
            page_dict.insert("width".into(), Value::Number(960.0));
            page_dict.insert("height".into(), Value::Number(540.0));

            let mut block_dict: HashMap<String, Value> = HashMap::new();
            block_dict.insert("kind".into(), Value::String("text".into()));
            block_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(960.0),
                    Value::Number(540.0),
                ]),
            );
            let mut span_dict: HashMap<String, Value> = HashMap::new();
            span_dict.insert("text".into(), Value::String(text.clone()));
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
            page_dict.insert("blocks".into(), Value::List(vec![Value::Dict(block_dict)]));
            out.push(Value::Dict(page_dict));
        }
        Ok(Value::List(out))
    }

    /// Slide text joined with `\n\n---\n\n` so callers can split on the rule
    /// if they need to recover slide boundaries.
    fn markdown(&self) -> Result<String, String> {
        Ok(self.per_slide_text.join("\n\n---\n\n"))
    }

    /// Slide text joined with blank lines (no rule) so the output looks like
    /// a plain concatenated transcript.
    fn text(&self) -> Result<String, String> {
        Ok(self.per_slide_text.join("\n\n"))
    }

    /// `{origin, pages, size}` plus any metadata fields we managed to read.
    /// All entries are strings (or numbers for `pages`/`size`); absent
    /// optional fields simply aren't present in the dict.
    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("pptx".into()));
        m.insert("pages".into(), Value::Number(self.slide_count as f64));
        m.insert("size".into(), Value::Number(self.bytes.len() as f64));
        for (k, v) in &self.metadata {
            m.insert(k.clone(), Value::String(v.clone()));
        }
        Ok(Value::Dict(m))
    }

    /// Same shape as `PdfBackend::blocks`: one block per slide, each block
    /// carrying a single span with the slide's text. (Top-level accessors
    /// are easier to reason about when each slide has a flat representation.)
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
    fn origin_is_pptx() {
        // minimal: 构造空 struct,直接验 trait method
        let backend = PptxBackend {
            bytes: vec![],
            slide_count: 0,
            per_slide_text: vec![],
            metadata: HashMap::new(),
        };
        assert_eq!(backend.origin(), "pptx");
    }

    #[test]
    fn parses_real_pptx() {
        let path = format!("{}/tests/fixtures/sample.pptx", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).expect("fixture file");
        let backend = PptxBackend::from_bytes(bytes).expect("parse");
        assert_eq!(backend.origin(), "pptx");
        assert!(
            backend.slide_count >= 2,
            "expected ≥2 slides, got {}",
            backend.slide_count
        );
        let md = backend.markdown().unwrap();
        assert!(
            md.contains("Sample Slide") || md.contains("Hello") || !md.is_empty(),
            "markdown non-empty: got {:?}",
            md
        );
    }
}
