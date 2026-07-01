//! v0.27: Document unified IR + DocumentBackend trait
use std::collections::HashMap;
use crate::value::Value;

pub mod backend;

/// v0.27: Backend trait — every document format (PDF / Markdown / HTML)
/// implements this to expose a unified IR surface.
pub trait DocumentBackend: std::fmt::Debug + Send + Sync {
    /// Format identity: "pdf" | "markdown" | "html"
    fn origin(&self) -> &'static str;
    // Full methods (pages / markdown / text / metadata / blocks) added in Task 3.
}

/// v0.27: Convenience — make a Document::value from any backend.
/// Called by backends and tests.
pub fn make_document(
    backend: std::sync::Arc<dyn DocumentBackend>,
    metadata: HashMap<String, Value>,
) -> Value {
    Value::Document { backend, metadata }
}