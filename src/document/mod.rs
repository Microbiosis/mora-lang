//! v0.27: Document unified IR + DocumentBackend trait
use std::collections::HashMap;
use crate::value::Value;

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

    /// 完整 IR: List<Page dict> — 与 MinerU middle_json pages 对齐
    /// Page = {page_no, width, height, blocks: [Block]}
    /// Block = {kind, bbox, spans: [Span]}
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
        "pdf" => Err("document.parse: PdfBackend not yet implemented (Task 5)".into()),
        "md" | "markdown" => Err("document.parse: MarkdownBackend not yet implemented (Task 6)".into()),
        "html" | "htm" => Err("document.parse: HtmlBackend not yet implemented (Task 7)".into()),
        other => Err(format!("document.parse: unsupported extension '.{}'", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pdf_yields_not_yet_implemented_error() {
        let r = parse_document("/tmp/x.pdf");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("PdfBackend not yet implemented"));
    }

    #[test]
    fn parse_md_yields_not_yet_implemented_error() {
        let r = parse_document("/tmp/x.md");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("MarkdownBackend not yet implemented"));
    }

    #[test]
    fn parse_html_yields_not_yet_implemented_error() {
        let r = parse_document("/tmp/x.html");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("HtmlBackend not yet implemented"));
    }

    #[test]
    fn parse_unsupported_ext_errors() {
        let r = parse_document("/tmp/x.xyz");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unsupported extension '.xyz'"));
    }
}
