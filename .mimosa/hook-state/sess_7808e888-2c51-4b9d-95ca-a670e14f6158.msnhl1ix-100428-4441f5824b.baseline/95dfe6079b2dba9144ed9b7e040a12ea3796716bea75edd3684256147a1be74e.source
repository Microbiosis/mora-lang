//! v0.29: HtmlSubCompressor — 复用 v0.27 HtmlBackend 的 quick-xml 切块逻辑
//!
//! 算法 (与 v0.27 `HtmlBackend::text` 一致):
//! - `sniff`: 每行平均 `<` 字符数 ≥ 0.5 → 返回 0.8; 否则 0.0
//! - `compress`: quick-xml pull-parser, 跳过 `<script>` / `<style>` 子树
//!   (`skip_depth` 计数器), 把所有 `Event::Text` 解码后拼接。
//!
//! quick-xml 0.40 API 注意:
//! - v0.28 的实现里 `BytesText::unescape()` 在 0.40 已被移除, 改用 `decode()`。
//!   这与 v0.27 `HtmlBackend` (已运行中) 的实际用法一致。

use crate::compress::{CompressOptions, SubCompressor};
use quick_xml::Reader;
use quick_xml::events::Event;

/// v0.29: `SubCompressor` trait impl for HTML content.
#[derive(Debug)]
pub struct HtmlSubCompressor;

impl SubCompressor for HtmlSubCompressor {
    /// 嗅探 HTML: 行均 `<` 字符数 ≥ 0.5 → 0.8
    fn sniff(&self, content: &str) -> f32 {
        let n_tags = content.matches('<').count();
        let n_lines = content.lines().count().max(1);
        if (n_tags as f32) / (n_lines as f32) >= 0.5 {
            0.8
        } else {
            0.0
        }
    }

    /// 压缩: quick-xml pull-parser 提取文本, 跳过 `<script>` / `<style>`。
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        _options: &CompressOptions,
    ) -> Result<String, String> {
        let mut reader = Reader::from_str(content);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut out = String::new();
        let mut skip_depth: usize = 0;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "script" || name == "style" {
                        skip_depth += 1;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if (name == "script" || name == "style") && skip_depth > 0 {
                        skip_depth -= 1;
                    }
                }
                Ok(Event::Text(t)) => {
                    if skip_depth == 0 {
                        let un = t.decode().unwrap_or_default().to_string();
                        if !un.trim().is_empty() {
                            out.push_str(&un);
                            out.push('\n');
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
            if out.len() >= max_bytes {
                break;
            }
        }
        out.push_str(&format!(
            "\n<compressed:method=html original_size={}>\n",
            content.len()
        ));
        Ok(out)
    }

    fn origin(&self) -> &'static str {
        "html"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_sniff_detects_tag_density() {
        let c = HtmlSubCompressor;
        let html = "<html><body><h1>x</h1><p>y</p></body></html>";
        let score = c.sniff(html);
        assert!(score >= 0.6, "expected sniff >= 0.6, got {score}");
    }

    #[test]
    fn test_html_strips_script() {
        let c = HtmlSubCompressor;
        let html = "<html><head><script>alert(1);</script></head><body><p>Hello</p></body></html>";
        let opts = CompressOptions::default();
        let out = c
            .compress(html, 1000, &opts)
            .expect("compress should not error");
        assert!(
            !out.contains("alert"),
            "script body must be stripped: {out}"
        );
        assert!(out.contains("Hello"), "body text must survive: {out}");
    }
}
