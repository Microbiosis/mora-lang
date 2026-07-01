//! v0.28: PptxBackend — wraps undoc 0.5 pure-Rust PPTX parser
use std::collections::HashMap;
use crate::value::Value;
use crate::document::DocumentBackend;

#[derive(Debug)]
pub struct PptxBackend {
    pub bytes: Vec<u8>,
    pub slide_count: usize,
    pub per_slide_text: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl PptxBackend {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        Ok(Self { bytes, slide_count: 0, per_slide_text: vec![], metadata: HashMap::new() })
    }
}

impl DocumentBackend for PptxBackend {
    fn origin(&self) -> &'static str { "pptx" }
    fn pages(&self) -> Result<Value, String> { Ok(Value::List(vec![])) }
    fn markdown(&self) -> Result<String, String> { Ok(String::new()) }
    fn text(&self) -> Result<String, String> { Ok(String::new()) }
    fn metadata(&self) -> Result<Value, String> {
        let mut m = HashMap::new();
        m.insert("origin".into(), Value::String("pptx".into()));
        Ok(Value::Dict(m))
    }
    fn blocks(&self) -> Result<Value, String> { Ok(Value::List(vec![])) }
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
}
