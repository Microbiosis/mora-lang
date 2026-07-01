//! v0.27: PdfBackend — synchronous PDF backend (lopdf + pdf-extract).
//!
//! Strategy:
//!   1. `lopdf::Document::load_mem` parses the bytes and exposes the page count
//!      and `info_dict` (which we skip for the MVP — extracting user-facing
//!      Title/Author requires deep Object traversal).
//!   2. `pdf_extract::extract_text_from_mem` returns the concatenated text of
//!      all pages. `pdf-extract 0.12` does NOT expose per-page boundaries in
//!      its top-level API, so we treat the whole document as one page-worth
//!      of text (matching the MVP contract from Task 4).
//!   3. The returned text is cached so subsequent `text()` / `markdown()` calls
//!      don't re-run OCR or layout analysis.
//!
//! All fields are `pub` so unit tests can construct instances directly and
//! inspect internals.
use std::collections::HashMap;
use crate::value::Value;
use crate::document::DocumentBackend;

/// v0.27: PDF backend.
///
/// Caches raw bytes, page count, full extracted text and the (currently empty)
/// info-dict view. Construction is fallible because both `lopdf` and
/// `pdf-extract` can fail on malformed PDFs.
#[derive(Debug)]
pub struct PdfBackend {
    pub bytes: Vec<u8>,
    pub page_count: usize,
    pub per_page_text: Vec<String>,
    pub info: HashMap<String, String>,
}

impl PdfBackend {
    /// Parse a PDF byte stream into a PdfBackend.
    ///
    /// Errors propagate as a `String` so the value-level `parse_document`
    /// factory doesn't need a custom error type.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let doc = lopdf::Document::load_mem(&bytes)
            .map_err(|e| format!("document.parse: lopdf load error: {}", e))?;
        let page_count = doc.get_pages().len();
        // v0.27 MVP: `info_dict()` returns `Option<Object>` and extracting the
        // user-facing fields (Title/Author/...) would require deep Object
        // traversal. We expose an empty map; metadata() still returns
        // origin/pages/size which is the v0.27 contract.
        let info = HashMap::new();
        let full_text = pdf_extract::extract_text_from_mem(&bytes)
            .map_err(|e| format!("document.parse: pdf-extract error: {}", e))?;
        // pdf-extract 0.12 does not separate pages in its top-level API, so we
        // treat the whole document as one page-worth of text. When `page_count`
        // is 0 (malformed but loadable), produce no per-page entries.
        let per_page_text = if page_count == 0 {
            Vec::new()
        } else {
            vec![full_text]
        };
        Ok(Self {
            bytes,
            page_count,
            per_page_text,
            info,
        })
    }
}

impl DocumentBackend for PdfBackend {
    fn origin(&self) -> &'static str {
        "pdf"
    }

    /// Build the unified IR: `List<Page dict>` where each page has
    /// `{page_no, width, height, blocks: [Block]}` and each block has
    /// `{kind, bbox, spans: [Span]}` with one span containing the full
    /// extracted page text.
    ///
    /// MVP block-level layout detection is intentionally out of scope; each
    /// page gets a single placeholder `text` block that spans the full page.
    fn pages(&self) -> Result<Value, String> {
        let mut out: Vec<Value> = Vec::with_capacity(self.page_count);
        for (i, text) in self.per_page_text.iter().enumerate() {
            let mut page_dict: HashMap<String, Value> = HashMap::new();
            page_dict.insert("page_no".into(), Value::Number((i as f64) + 1.0));
            page_dict.insert("width".into(), Value::Number(595.0));
            page_dict.insert("height".into(), Value::Number(842.0));

            // One placeholder text block per page.
            let mut block_dict: HashMap<String, Value> = HashMap::new();
            block_dict.insert("kind".into(), Value::String("text".into()));
            block_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(595.0),
                    Value::Number(842.0),
                ]),
            );
            let mut span_dict: HashMap<String, Value> = HashMap::new();
            span_dict.insert("text".into(), Value::String(text.clone()));
            span_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(595.0),
                    Value::Number(842.0),
                ]),
            );
            span_dict.insert("score".into(), Value::Nil);
            block_dict.insert("spans".into(), Value::List(vec![Value::Dict(span_dict)]));
            page_dict.insert("blocks".into(), Value::List(vec![Value::Dict(block_dict)]));
            out.push(Value::Dict(page_dict));
        }
        Ok(Value::List(out))
    }

    /// MVP: returns the cached page text. With pdf-extract 0.12 collapsing
    /// every page into one string, this is just that single entry (or empty
    /// if the document had zero parseable pages).
    fn markdown(&self) -> Result<String, String> {
        // No pseudo-headers heuristic at this stage; markdown() == text() for v0.27.
        Ok(self.per_page_text.join("\n\n"))
    }

    fn text(&self) -> Result<String, String> {
        Ok(self.per_page_text.join("\n\n"))
    }

    /// v0.27 contract: `{origin, pages, size}` plus any future info-dict keys.
    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("pdf".into()));
        m.insert("pages".into(), Value::Number(self.page_count as f64));
        m.insert("size".into(), Value::Number(self.bytes.len() as f64));
        // Merge info-dict keys (currently always empty for MVP).
        for (k, v) in &self.info {
            m.insert(k.clone(), Value::String(v.clone()));
        }
        Ok(Value::Dict(m))
    }

    /// Flatten every page's blocks into one list. Mirrors `pages()[i].blocks`
    /// concatenated across pages.
    fn blocks(&self) -> Result<Value, String> {
        let mut out: Vec<Value> = Vec::new();
        for text in &self.per_page_text {
            let mut block_dict: HashMap<String, Value> = HashMap::new();
            block_dict.insert("kind".into(), Value::String("text".into()));
            block_dict.insert(
                "bbox".into(),
                Value::List(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(595.0),
                    Value::Number(842.0),
                ]),
            );
            let mut span_dict: HashMap<String, Value> = HashMap::new();
            span_dict.insert("text".into(), Value::String(text.clone()));
            span_dict.insert("score".into(), Value::Nil);
            block_dict.insert("spans".into(), Value::List(vec![Value::Dict(span_dict)]));
            out.push(Value::Dict(block_dict));
        }
        Ok(Value::List(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.pdf");
        p
    }

    #[test]
    fn parses_real_pdf() {
        let path = fixture();
        if !path.exists() {
            eprintln!("skipping: fixture missing at {:?}", path);
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let backend = PdfBackend::from_bytes(bytes).expect("parse ok");
        assert_eq!(backend.origin(), "pdf");
        let meta = backend.metadata().expect("metadata");
        if let Value::Dict(m) = meta {
            assert_eq!(m.get("origin"), Some(&Value::String("pdf".into())));
        } else {
            panic!("metadata should be dict");
        }
    }
}