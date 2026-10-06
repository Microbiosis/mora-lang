//! v0.27: HtmlBackend — quick-xml pull-parser based,
//! extracts `<p>` / `<h1-h6>` / `<pre>` / `<code>` blocks plus `<title>` and `<meta name="author">`.
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
    bd.insert(
        "bbox".into(),
        Value::List(vec![Value::Float(0.0); 4].into()),
    );
    let mut span: HashMap<String, Value> = HashMap::new();
    span.insert("text".into(), Value::String(text.into()));
    span.insert(
        "bbox".into(),
        Value::List(vec![Value::Float(0.0); 4].into()),
    );
    span.insert("score".into(), Value::Nil);
    bd.insert("spans".into(), Value::List(vec![Value::Dict(span)].into()));
    Value::Dict(bd)
}

/// v0.104.6 D232：块级标签判定（`tag_kind` / `is_block_tag` 的**唯一**真值源）。
///
/// 修前这两个函数覆盖**同一个**集合（`tag_kind` 返回 `Some` 的 ⇔ `is_block_tag`
/// 为 true），是同一事实的两份手写清单 —— 加标签要改两处，漏一处就漂移。
/// 现按 `tag_kind` 推导 `is_block_tag`，不可能不一致。
fn tag_kind(name: &str) -> Option<&'static str> {
    match name {
        "p" => Some("text"),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some("heading"),
        "pre" | "code" => Some("code"),
        _ => None,
    }
}

/// 块级标签闭合时收尾一个块（与 `tag_kind` 同一集合）。
fn is_block_tag(name: &str) -> bool {
    tag_kind(name).is_some()
}

/// v0.104.6 D232：`<script>` / `<style>` 的正文（两者都不是渲染内容）。
fn is_skip_tag(name: &str) -> bool {
    matches!(name, "script" | "style")
}

impl DocumentBackend for HtmlBackend {
    fn origin(&self) -> &'static str {
        "html"
    }

    fn pages(&self) -> Result<Value, String> {
        let blocks = self.blocks()?;
        let mut pd: HashMap<String, Value> = HashMap::new();
        pd.insert("page_no".into(), Value::Float(1.0));
        pd.insert("width".into(), Value::Float(0.0));
        pd.insert("height".into(), Value::Float(0.0));
        pd.insert("blocks".into(), blocks);
        Ok(Value::List(vec![Value::Dict(pd)].into()))
    }

    fn markdown(&self) -> Result<String, String> {
        self.text()
    }

    /// 纯文本：按**块**拼接，每块一行。
    ///
    /// v0.104.6 D232：修前是「每个 `Event::Text` 后补一个 `\n`」——
    /// 即**按事件**换行（与 D229 修前的 `MarkdownBackend::text()` 同型）。
    /// `<h1>Head with <em>em</em> inside</h1>` 变成三行：
    ///
    /// ```text
    /// Head with
    /// em
    /// inside
    /// ```
    ///
    /// 「纯文本」把词切开。改为复用 `blocks()` 的块结构。
    fn text(&self) -> Result<String, String> {
        let mut out = String::new();
        for (_kind, block_text) in self.block_texts() {
            out.push_str(block_text.trim_end());
            out.push('\n');
        }
        Ok(out.trim_end().to_string())
    }

    fn metadata(&self) -> Result<Value, String> {
        let mut m: HashMap<String, Value> = HashMap::new();
        m.insert("origin".into(), Value::String("html".into()));
        m.insert("pages".into(), Value::Float(1.0));
        m.insert("size".into(), Value::Float(self.source.len() as f64));
        if let Some(t) = &self.title {
            m.insert("title".into(), Value::String(t.clone()));
        }
        if let Some(a) = &self.author {
            m.insert("author".into(), Value::String(a.clone()));
        }
        Ok(Value::Dict(m))
    }

    fn blocks(&self) -> Result<Value, String> {
        let blocks: Vec<Value> = self
            .block_texts()
            .into_iter()
            .map(|(kind, text)| make_block(kind, &text))
            .collect();
        Ok(Value::List(blocks.into()))
    }
}

impl HtmlBackend {
    /// v0.104.6 D232：`blocks()` 与 `text()` 的**唯一**切块实现。
    ///
    /// 修前二者**各自**跑一遍 quick-xml 事件流、用**两套不同规则**：
    /// `blocks()` 按块级标签切（正确），`text()` 按每个 `Event::Text` 切
    /// （错误，且与 blocks 的块数不一致）。合并后不可能漂移。
    ///
    /// 返回 `(kind, text)` 序列；空块不产出。
    ///
    /// v0.104.6 D232：`trim_text(true)` 是「粘连」缺陷的真正来源 ——
    /// 它把内联标签之间的**换行/空格裁掉**，于是
    /// `Head with <em>em</em> inside` 变成三个独立 `Text` 事件
    /// （`"Head with"` / `"em"` / `"inside"`），直接拼接即
    /// **`Head witheminside`**（实测）。
    ///
    /// 改用 `trim_text(false)` + 自行跳过纯空白节点：
    /// ① 源里**原有**的空白被保留 ⇒ 不粘连；
    /// ② 源里**本无**空白的地方不会被凭空插入 ⇒ 不改内容
    ///    （`x<b>Y</b>Z` 仍是 `xYZ`，与浏览器渲染一致）。
    ///
    /// ⚠ 先前一版修法在内联标签 Start/End 各补一个空格，判据
    /// `d232_inline_spacing_does_not_invent_words` 测出它会把
    /// `x<b>Y</b>Z` 变成 `x Y Z` —— **凭空造词**。已改为本方案。
    fn block_texts(&self) -> Vec<(&'static str, String)> {
        let mut reader = Reader::from_str(&self.source);
        // v0.104.6 D232：关掉 trim，空白由本函数自己处理（见上）
        reader.config_mut().trim_text(false);
        reader.config_mut().check_end_names = false;
        let mut buf = Vec::new();
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let mut current_kind: Option<&'static str> = None;
        let mut current_text = String::new();
        let mut skip_depth: usize = 0;

        let flush = |out: &mut Vec<(&'static str, String)>,
                     kind: Option<&'static str>,
                     text: &mut String| {
            let t = std::mem::take(text);
            if let Some(k) = kind
                && !t.trim().is_empty()
            {
                // `trim()` 去掉块首缩进空白与块间换行（D232）
                out.push((k, t.trim().to_string()));
            }
        };

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if is_skip_tag(&name) {
                        skip_depth += 1;
                        continue;
                    }
                    if skip_depth == 0
                        && current_kind.is_none()
                        && let Some(k) = tag_kind(&name)
                    {
                        flush(&mut out, current_kind, &mut current_text);
                        current_kind = Some(k);
                    }
                }
                Ok(Event::CData(c)) => {
                    if skip_depth == 0 && current_kind.is_some() {
                        current_text.push_str(&String::from_utf8_lossy(&c));
                    }
                }
                Ok(Event::Text(t)) => {
                    if skip_depth == 0 && current_kind.is_some() {
                        // v0.104.6 D232：`trim_text(false)` 下，块内的空白
                        // **已经内含在相邻文本节点里**（实测事件流：
                        // `Text("Head with ")` 尾部自带空格），
                        // 纯空白节点只出现在块**之间**。
                        // 因此这里原样拼接即可 —— 既不粘连（空格在）、
                        // 也不造词（紧贴处本来就没空格），
                        // 块首缩进空白由 `flush` 的 `trim()` 去掉。
                        current_text.push_str(&t.decode().unwrap_or_default());
                    }
                }
                Ok(Event::End(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if is_skip_tag(&name) {
                        skip_depth = skip_depth.saturating_sub(1);
                        continue;
                    }
                    if skip_depth == 0 && is_block_tag(&name) && current_kind.is_some() {
                        flush(&mut out, current_kind.take(), &mut current_text);
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        flush(&mut out, current_kind, &mut current_text);
        out
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
