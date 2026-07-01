//! v0.28: ImageBackend — pure-Rust OCR via ocrs + image
//!
//! Task 5: skeleton only. No OCR yet, no image decode yet — those land in
//! Task 6 (which will populate `width`/`height`/`lines` via `image` and
//! `ocrs`). For now `from_bytes` is a trivial constructor that records
//! the raw bytes, the caller's `format`, and reserves placeholders for
//! `width`/`height`/`lines`/`ocr_engine`.
use std::collections::HashMap;
use crate::value::Value;
use crate::document::DocumentBackend;

#[derive(Debug)]
pub struct ImageBackend {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String,     // "png" only in v0.28
    pub lines: Vec<String>, // OCR output (empty until OCR pipeline implemented in Task 6)
    pub ocr_engine: String, // "rten" (set in Task 6)
}

impl ImageBackend {
    /// Task 5: minimal — no OCR yet, no image decode yet
    pub fn from_bytes(bytes: Vec<u8>, format: String) -> Result<Self, String> {
        Ok(Self {
            bytes,
            width: 0,
            height: 0,
            format,
            lines: vec![],
            ocr_engine: "rten".into(),
        })
    }
}

impl DocumentBackend for ImageBackend {
    fn origin(&self) -> &'static str { "image" }
    fn pages(&self) -> Result<Value, String> {
        let mut pd = HashMap::new();
        pd.insert("page_no".into(), Value::Number(1.0));
        pd.insert("width".into(), Value::Number(self.width as f64));
        pd.insert("height".into(), Value::Number(self.height as f64));
        pd.insert("blocks".into(), Value::List(vec![]));
        Ok(Value::List(vec![Value::Dict(pd)]))
    }
    fn markdown(&self) -> Result<String, String> { Ok(self.lines.join("\n")) }
    fn text(&self) -> Result<String, String> { Ok(self.lines.join("\n")) }
    fn metadata(&self) -> Result<Value, String> {
        let mut m = HashMap::new();
        m.insert("origin".into(), Value::String("image".into()));
        m.insert("pages".into(), Value::Number(1.0));
        m.insert("format".into(), Value::String(self.format.clone()));
        m.insert("width".into(), Value::Number(self.width as f64));
        m.insert("height".into(), Value::Number(self.height as f64));
        m.insert("ocr_engine".into(), Value::String(self.ocr_engine.clone()));
        m.insert("size".into(), Value::Number(self.bytes.len() as f64));
        Ok(Value::Dict(m))
    }
    fn blocks(&self) -> Result<Value, String> { Ok(Value::List(vec![])) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_is_image() {
        let backend = ImageBackend {
            bytes: vec![], width: 0, height: 0, format: "png".into(),
            lines: vec![], ocr_engine: "rten".into(),
        };
        assert_eq!(backend.origin(), "image");
    }
    #[test]
    fn metadata_has_ocr_engine() {
        let backend = ImageBackend {
            bytes: vec![], width: 800, height: 600, format: "png".into(),
            lines: vec![], ocr_engine: "rten".into(),
        };
        let meta = backend.metadata().unwrap();
        if let Value::Dict(m) = meta {
            assert_eq!(m.get("ocr_engine"), Some(&Value::String("rten".into())));
            assert_eq!(m.get("format"), Some(&Value::String("png".into())));
        } else { panic!("metadata should be dict"); }
    }
}
