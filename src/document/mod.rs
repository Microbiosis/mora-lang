//! v0.27: Document unified IR + DocumentBackend trait
use crate::value::Value;
use std::collections::HashMap;

pub mod backend;

/// v0.27: Document unified IR backend — every document format
/// (PDF / Markdown / HTML) implements this to expose a unified surface.
///
/// 实现方应该:
/// 1. 在构造时一次性解析 + 缓存结果 (e.g. extracted text, parsed DOM)
/// 2. `pages()` / `markdown()` / `text()` / `blocks()` / `metadata()` 在调用时才构造 Value
pub trait DocumentBackend: std::fmt::Debug + Send + Sync {
    /// Format identity string: "pdf" | "markdown" | "html"
    fn origin(&self) -> &'static str;

    /// 完整 IR: `List<Page dict>` — 与 MinerU middle_json pages 对齐
    /// Page = {page_no, width, height, blocks: \[Block\]}
    /// Block = {kind, bbox, spans: \[Span\]}
    fn pages(&self) -> Result<Value, String>;

    /// 渲染为完整 markdown 字符串
    fn markdown(&self) -> Result<String, String>;

    /// 渲染为纯文本
    fn text(&self) -> Result<String, String>;

    /// 元信息 Dict — {origin, pages, size, title?, author?, ...}
    fn metadata(&self) -> Result<Value, String>;

    /// 跨页合并的 block 列表
    fn blocks(&self) -> Result<Value, String>;
}

/// v0.27: Convenience — make a Document::value from any backend.
/// Called by backends and tests.
pub fn make_document(
    backend: std::sync::Arc<dyn DocumentBackend>,
    metadata: HashMap<String, Value>,
) -> Value {
    Value::Document { backend, metadata }
}

/// v0.27: 解析文件路径,根据扩展名分发到对应 backend。
/// Tasks 5–7 会分别实现 PdfBackend / MarkdownBackend / HtmlBackend。
pub fn parse_document(path: &str) -> Result<Value, String> {
    use std::path::Path;
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend = crate::document::backend::pdf::PdfBackend::from_bytes(bytes)?;
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        "md" | "markdown" => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend = crate::document::backend::markdown::MarkdownBackend::new(&text);
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        "html" | "htm" => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend = crate::document::backend::html::HtmlBackend::new(&text);
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        "pptx" => {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend = crate::document::backend::pptx::PptxBackend::from_bytes(bytes)?;
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        "docx" => {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend = crate::document::backend::docx::DocxBackend::from_bytes(bytes)?;
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        "png" => {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("document.parse: cannot read '{}': {}", path, e))?;
            let backend =
                crate::document::backend::image::ImageBackend::from_bytes(bytes, "png".into())?;
            let meta = backend.metadata()?;
            let mut metadata = match meta {
                Value::Dict(m) => m,
                _ => HashMap::new(),
            };
            metadata
                .entry("path".to_string())
                .or_insert(Value::String(path.to_string()));
            Ok(make_document(std::sync::Arc::new(backend), metadata))
        }
        other => Err(format!(
            "document.parse: unsupported extension '.{}' (this v0.28 release supports pdf, md, markdown, html, htm, pptx, docx, png)",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pdf_now_implemented_via_pdf_backend() {
        // v0.27 Task 5: PdfBackend is implemented; factory now reads the file
        // and wraps the parsed bytes. We point at our 1-page test fixture and
        // assert the dispatch yields a Value::Document (instead of an Err).
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.pdf");
        let p_str = p.to_string_lossy().to_string();
        let r = parse_document(&p_str);
        assert!(
            r.is_ok(),
            "parse_document for sample.pdf should succeed, got: {:?}",
            r.err()
        );
        match r.unwrap() {
            Value::Document { .. } => {}
            other => panic!("expected Value::Document, got: {:?}", other),
        }
    }

    #[test]
    fn parse_md_now_implemented_via_markdown_backend() {
        // v0.27 Task 6: MarkdownBackend is implemented; factory now reads the
        // file and wraps the source string. We point at our fixture and assert
        // dispatch yields a Value::Document (instead of an Err).
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.md");
        let p_str = p.to_string_lossy().to_string();
        let r = parse_document(&p_str);
        assert!(
            r.is_ok(),
            "parse_document for sample.md should succeed, got: {:?}",
            r.err()
        );
        match r.unwrap() {
            Value::Document { .. } => {}
            other => panic!("expected Value::Document, got: {:?}", other),
        }
    }

    #[test]
    fn parse_html_now_implemented_via_html_backend() {
        // v0.27 Task 7: HtmlBackend is implemented; factory now reads the file
        // and wraps the parsed source. We point at our 1-page test fixture and
        // assert dispatch yields a Value::Document (instead of an Err).
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.html");
        let p_str = p.to_string_lossy().to_string();
        let r = parse_document(&p_str);
        assert!(
            r.is_ok(),
            "parse_document for sample.html should succeed, got: {:?}",
            r.err()
        );
        match r.unwrap() {
            Value::Document { .. } => {}
            other => panic!("expected Value::Document, got: {:?}", other),
        }
    }

    #[test]
    fn parse_unsupported_ext_errors() {
        let r = parse_document("/tmp/x.xyz");
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(
            msg.contains("unsupported extension '.xyz'"),
            "expected 'unsupported extension '.xyz'' in error, got: {}",
            msg
        );
        // v0.28: error mentions the v0.28 release and the supported extensions.
        assert!(
            msg.contains("v0.28") && msg.contains("supports"),
            "expected v0.28 release support list in error, got: {}",
            msg
        );
    }
}
