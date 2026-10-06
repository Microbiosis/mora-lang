//! D232 判据：`HtmlBackend` 的 `blocks()` / `text()` 切块与内联拼接。
//!
//! ## 缺陷背景
//!
//! `HtmlBackend` 与 `MarkdownBackend`（D229）**同型**但独立实现，两处缺陷：
//!
//! ① **`text()` 按事件换行**：「每个 `Event::Text` 后补一个 `\n`」⇒
//!    `<h1>Head with <em>em</em> inside</h1>` 变成三行
//!    （`Head with` / `em` / `inside`），**词被切开**。
//!
//! ② **`blocks()` 内联粘连**：`Event::Text` 直接 `push_str` 拼接，而
//!    quick-xml 在 `trim_text(true)` 下把内联标签两侧的换行裁掉 ⇒
//!    `Head with` + `em` + `inside` 三个独立事件拼成 **`Head witheminside`**
//!    —— **词被粘在一起**。
//!
//! quick-xml 0.40 的实际事件流（外部 dump 实测，不凭记忆）：
//!
//! ```text
//! Start("h1") Text("Head with") Start("em") Text("em") End("em") Text("inside") End("h1")
//! ```
//!
//! ③ `text()` 与 `blocks()` **各自**跑一遍事件流、用**两套不同规则**
//!    ⇒ 二者对「一个文档有多少块」的判断**不一致**（与 D229 同款）。
//!
//! ④ `tag_kind` 与 `is_block_tag` 覆盖**同一个**标签集合（同一事实的两份
//!    手写清单，加标签要改两处）。

use mora::document::DocumentBackend;
use mora::document::backend::html::HtmlBackend;
use mora::value::Value;

const SAMPLE: &str = r#"<html><head><title>T</title></head><body>
<h1>Head with <em>em</em> inside</h1>
<p>A paragraph with <b>bold</b> and <a href="u">link</a> text.</p>
<p>Second paragraph.</p>
<script>alert(1);</script>
</body></html>"#;

fn block_texts(v: &Value) -> Vec<String> {
    match v {
        Value::List(list) => list
            .iter()
            .map(|b| {
                let d = match b {
                    Value::Dict(d) => d,
                    other => panic!("block must be dict: {other:?}"),
                };
                let spans = match d.get("spans") {
                    Some(Value::List(s)) => s,
                    other => panic!("block must have spans: {other:?}"),
                };
                let sd = match spans.first() {
                    Some(Value::Dict(sd)) => sd,
                    _ => return String::new(),
                };
                match sd.get("text") {
                    Some(Value::String(t)) => t.clone(),
                    _ => String::new(),
                }
            })
            .collect(),
        other => panic!("blocks must be a list: {other:?}"),
    }
}

fn block_kinds(v: &Value) -> Vec<String> {
    match v {
        Value::List(list) => list
            .iter()
            .map(|b| match b {
                Value::Dict(d) => match d.get("kind") {
                    Some(Value::String(s)) => s.clone(),
                    _ => panic!("block must have kind string"),
                },
                other => panic!("block must be dict: {other:?}"),
            })
            .collect(),
        other => panic!("blocks must be a list: {other:?}"),
    }
}

/// 判据 ①：内联标签不得把词**粘在一起**。
#[test]
fn d232_inline_tags_do_not_glue_words_together() {
    let b = HtmlBackend::new(SAMPLE);
    let texts = block_texts(&b.blocks().expect("blocks"));
    assert_eq!(
        texts,
        vec![
            "Head with em inside",
            "A paragraph with bold and link text.",
            "Second paragraph.",
        ],
        "D232: 内联标签边界丢分隔符 ⇒ 词被粘连。\n\
         quick-xml 在 trim_text(true) 下把 <em> 两侧的换行裁掉，\
         'Head with' / 'em' / 'inside' 是三个独立 Text 事件，直接拼接即粘连"
    );
}

/// 判据 ②：`text()` 不得按**事件**换行。
#[test]
fn d232_text_does_not_split_words_per_event() {
    let b = HtmlBackend::new(SAMPLE);
    let text = b.text().expect("text");
    assert_eq!(
        text, "Head with em inside\nA paragraph with bold and link text.\nSecond paragraph.",
        "D232: text() 修前是「每个 Event::Text 补一个换行」⇒ 一个句子被切成多行"
    );
}

/// 判据 ③（往返不变式）：`text()` 的**行结构**必须由 `blocks()` 决定。
///
/// 修前二者**各自**遍历事件流、用**两套规则**（blocks 按块级标签切、
/// text 按每个 Text 事件切）⇒ 对「有多少块」的判断不一致。
///
/// ⚠ 第一版断言「`text()` 行数 == `blocks()` 块数」**过强**：多行块
/// （`<pre>line1\nline2</pre>`）在 `text()` 里本就该占 2 行、在 `blocks()`
/// 里是 1 块。加了 `<pre>` 语料后立刻暴露。
///
/// 正确形态：`text()` 的行序列 == 把每个块文本按**块内换行**展开后的
/// 序列。修前 `text()` 按事件切（`Head with` / `em` / `inside` 三行），
/// 与块文本（`Head with em inside` 一行）**对不上** —— 这才是缺陷。
#[test]
fn d232_text_lines_are_exactly_the_block_texts() {
    for html in [
        SAMPLE,
        "<h1>only heading</h1>",
        "<p>a</p><p>b</p><p>c</p>",
        "<div>no block tags at all</div>",
        "<pre>line1\nline2\n</pre>",
        "<pre>a</pre><pre>b</pre>",
        "<code>x</code><p>y</p>",
        "",
    ] {
        let b = HtmlBackend::new(html);
        let blocks_value = b.blocks().expect("blocks");
        let texts = block_texts(&blocks_value);
        let text = b.text().expect("text");
        let expected: Vec<&str> = texts.iter().flat_map(|t| t.trim_end().lines()).collect();
        let got: Vec<&str> = if text.is_empty() {
            vec![]
        } else {
            text.lines().collect()
        };
        assert_eq!(
            got, expected,
            "D232: text() 的行序列应恰为各块文本按块内换行展开。\n\
             blocks = {texts:?}\ntext =\n{text}\n\
             修前 text() 按每个 Event::Text 换行，与块边界不一致"
        );
    }
}

/// 判据 ④：`<script>` / `<style>` 内容仍被剥离。
#[test]
fn d232_script_and_style_are_stripped() {
    let b = HtmlBackend::new(SAMPLE);
    let texts = block_texts(&b.blocks().expect("blocks"));
    for t in &texts {
        assert!(!t.contains("alert"), "D232: script 内容泄漏进块文本: {t:?}");
    }
    let text = b.text().expect("text");
    assert!(
        !text.contains("alert"),
        "script 内容泄漏进 text(): {text:?}"
    );

    // style 也要剥
    let b2 = HtmlBackend::new("<p>a</p><style>.x{color:red}</style><p>b</p>");
    let t2 = b2.text().expect("text");
    assert!(!t2.contains("color"), "style 内容泄漏: {t2:?}");
}

/// 判据 ⑤：`kind` 正确（`h1`→heading，`p`→text）。
#[test]
fn d232_block_kinds_are_correct() {
    let b = HtmlBackend::new(SAMPLE);
    assert_eq!(
        block_kinds(&b.blocks().expect("blocks")),
        vec!["heading", "text", "text"],
        "D232: h1 应为 heading，p 应为 text"
    );
}

/// 判据 ⑥：内联拼接**不得凭空造出空格**（也不得粘连）。
///
/// ⚠ 判据的第一版期望值被我写成了 `x Y Z` —— 那等于**把当时修法的
/// （错误的）行为当成期望**。标准 HTML 语义下 `x<b>Y</b>Z` 渲染为
/// `xYZ`（无空格），凭空插入空格是**改内容**（造出一个不存在的词边界）。
///
/// 正解：关掉 `trim_text`、保留源里**原有**的空白。
#[test]
fn d232_inline_spacing_matches_browser_semantics() {
    // 源里内联标签与文字**紧贴**，无空格 → 渲染为 xYZ
    let b = HtmlBackend::new("<p>x<b>Y</b>Z</p>");
    assert_eq!(
        block_texts(&b.blocks().expect("blocks")),
        vec!["xYZ"],
        "内联拼接凭空造空格：浏览器把 'x<b>Y</b>Z' 渲染为 'xYZ'"
    );

    // 源里内联标签**两侧有空格** → 空白必须保留，否则粘连
    let b2 = HtmlBackend::new("<p>x <b>Y</b> Z</p>");
    assert_eq!(
        block_texts(&b2.blocks().expect("blocks")),
        vec!["x Y Z"],
        "源里原有的空格被丢弃 ⇒ 粘连（浏览器渲染为 'x Y Z'）"
    );
}
