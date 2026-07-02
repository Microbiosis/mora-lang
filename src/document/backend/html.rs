//! v0.27: HtmlBackend — quick-xml pull-parser based,
//! extracts <p>/<h1-h6>/<pre>/<code> blocks plus <title> and <meta name="author">.
//!
//! Strategy:
//!   1. Cache the source string at construction time.
//!   2. On construction, run a single pass over the document to extract
//!      the optional `<title>` text and `<meta name="author" content="...">`
//!      attribute pair. These are exposed via `metadata()`.
//!   3. `text()` walks all `Event::Text` payloads, skipping `<script>` and
//!      `<style>` bodies via a `skip_depth` counter, and joins them with newlines.
//!   4. `blocks()` does the same pass but emits one `Block` dict per
//!      `<p>` / `<h1>`-`<h6>` / `<pre>` / `<code>` start-end pair, tagging
//!      `kind` as `text` / `heading` / `code`.
//!   5. `markdown()` returns the plain-text dump for v0.27 MVP (full HTML→MD
//!      conversion is out of scope).
//!   6. `metadata()` returns `{origin: "html", pages: 1, size, title?, author?}`.
//!   7. `pages()` returns a single-page mock whose `blocks` list is `blocks()`.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::document::DocumentBackend;
use crate::value::Value;

/// v0.27: HTML backend.
#[derive(Debug)]
pub struct HtmlBackend {
    pub source: String,
    pub title: Option<String>,
    pub author: Option<String>,
}

impl HtmlBackend {
    pub fn new(s: &str) -> Self {
        let (title, author) = extract_meta(s);
        Self {
            source: s.to_string(),
            title,
            author,
        }
    }
}

fn extract_meta(s: &str) -> (Option<String>, Option<String>) {
    let mut title: Option<String> = None;
    let mut author: Option<String> = None;
    let mut reader = Reader::from_str(s);
    reader.config_mut().trim_text(true);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "title" {
                    buf.clear();
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf)
                        && let Ok(un) = t.decode()
                    {
                        title = Some(un.to_string());
                    }
                } else if name == "meta" {
                    let mut aname: Option<String> = None;
                    let mut acontent: Option<String> = None;
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let v = String::from_utf8_lossy(&attr.value).to_string();
                        if k == "name" {
                            aname = Some(v);
                        } else if k == "content" {
                            acontent = Some(v);
                        }
                    }
                    if aname.as_deref() == Some("author") {
                        author = acontent;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (title, author)
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

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "code"
    )
}

fn tag_kind(name: &str) -> Option<&'static str> {
    match name {
        "p" => Some("text"),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some("heading"),
        "pre" | "code" => Some("code"),
        _ => None,
    }
}

impl DocumentBackend for HtmlBackend {
    fn origin(&self) -> &'static str {
        "html"
    }

    fn pages(&self) -> Result<Value, String> {
        let blocks = self.blocks()?;
        let mut pd: HashMap<String, Value> = HashMap::new();
        pd.insert("page_no".into(), Value::Number(1.0));
        pd.insert("width".into(), Value::Number(0.0));
        pd.insert("height".into(), Value::Number(0.0));
        pd.insert("blocks".into(), blocks);
        Ok(Value::List(vec![Value::Dict(pd)]))
    }

    fn markdown(&self) -> Result<String, String> {
        self.text()
    }

    fn text(&self) -> Result<String, String> {
        let mut reader = Reader::from_str(&self.source);
        reader.config_mut().trim_text(true);
        reader.config_mut().check_end_names = false;
        let mut buf = Vec::new();
        let mut out = String::new();
        let mut skip_depth: usize = 0;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
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
        }
        Ok(out.trim_end().to_string())
    }

    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("html".into()));
        m.insert("pages".into(), Value::Number(1.0));
        m.insert("size".into(), Value::Number(self.source.len() as f64));
        if let Some(t) = &self.title {
            m.insert("title".into(), Value::String(t.clone()));
        }
        if let Some(a) = &self.author {
            m.insert("author".into(), Value::String(a.clone()));
        }
        Ok(Value::Dict(m))
    }

    fn blocks(&self) -> Result<Value, String> {
        let mut reader = Reader::from_str(&self.source);
        reader.config_mut().trim_text(true);
        reader.config_mut().check_end_names = false;
        let mut buf = Vec::new();
        let mut blocks: Vec<Value> = Vec::new();
        let mut current_kind: Option<String> = None;
        let mut current_text = String::new();
        let mut skip_depth: usize = 0;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "script" || name == "style" {
                        skip_depth += 1;
                        continue;
                    }
                    if current_kind.is_none()
                        && let Some(k) = tag_kind(&name)
                    {
                        current_kind = Some(k.to_string());
                    }
                }
                Ok(Event::Text(t)) => {
                    if current_kind.is_some() && skip_depth == 0 {
                        let un = t.decode().unwrap_or_default().to_string();
                        current_text.push_str(&un);
                    }
                }
                Ok(Event::End(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "script" || name == "style" {
                        skip_depth = skip_depth.saturating_sub(1);
                        continue;
                    }
                    if current_kind.is_some() && is_block_tag(&name) {
                        let text = current_text.trim();
                        if !text.is_empty() {
                            let kind = current_kind.take().unwrap();
                            blocks.push(make_block(&kind, text));
                        } else {
                            current_kind = None;
                        }
                        current_text.clear();
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        Ok(Value::List(blocks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_html() {
        let html = r#"
            <html><head><title>Test Page</title>
            <meta name="author" content="Alice"></head>
            <body><h1>Hello</h1><p>World.</p>
            <script>alert(1)</script>
            <pre>code</pre></body></html>
        "#;
        let backend = HtmlBackend::new(html);
        assert_eq!(backend.origin(), "html");

        let meta = backend.metadata().unwrap();
        if let Value::Dict(m) = meta {
            assert_eq!(m.get("title"), Some(&Value::String("Test Page".into())));
            assert_eq!(m.get("author"), Some(&Value::String("Alice".into())));
        } else {
            panic!("metadata should be a dict");
        }

        let text = backend.text().unwrap();
        assert!(
            !text.contains("alert"),
            "script body should be stripped, got: {:?}",
            text
        );
        assert!(
            text.contains("Hello"),
            "should contain heading text, got: {:?}",
            text
        );
        assert!(
            text.contains("World"),
            "should contain paragraph text, got: {:?}",
            text
        );

        let blocks = backend.blocks().unwrap();
        if let Value::List(bs) = blocks {
            assert!(
                bs.len() >= 3,
                "should produce heading+paragraph+code blocks, got {}",
                bs.len()
            );
        } else {
            panic!("blocks should be a list");
        }
    }
}
