//! D229 判据：`MarkdownBackend` 的 `blocks()` / `text()` 切块正确性。
//!
//! ## 缺陷背景（D229）
//!
//! 修前有两个互相独立的缺陷：
//!
//! ① **切块点错**：`blocks()` 用 `Event::End(_) if !current_text.is_empty()`
//!    —— **任何** `End` 都收尾。于是内联标记（`**bold**` 的 `End(Strong)`、
//!    `[link](url)` 的 `End(Link)`、`*em*` 的 `End(Emphasis)`）把一个段落
//!    **劈成多块**。实测 `# Title with **bold** inside` 变成两块。
//!
//! ② **kind 继承**：`current_kind` 只在 Heading / CodeBlock / Paragraph 的
//!    `Start` 里赋值，`List`/`Item` 的 `Start` **不重置**它。实测
//!    `## Sub heading` 之后的 `- item one` / `- item two`
//!    继承了 `kind="heading"` —— **两个列表项被标成标题**。
//!
//! ③ **text() 换行粒度**：`text()` 修前是「每个 `Event::Text` 补一个 `\n`」，
//!    即**按事件**换行 ⇒ `Title with **bold** inside` 变成三行
//!    （`Title with` / `bold` / ` inside`），词被切开。
//!
//! ## 判据形态
//!
//! **外部判据**（不重实现产品逻辑）：用 `pulldown_cmark` 事件流**独立**数出
//! 顶层块的数量与类型，再与 `blocks()` 的输出比对。
//! 这样「块数对不对」不由产品自己的切块代码回答。
//!
//! **往返不变式**：`text()` 的行数 == `blocks()` 的块数（修前二者对
//! 「一个文档有多少块」的判断**不一致**，这条不变式会立刻变红）。

use mora::document::DocumentBackend;
use mora::document::backend::markdown::MarkdownBackend;
use pulldown_cmark::{Event, Parser, Tag};

/// 与产品实现**无关**的参考切块：只认块级标签。
///
/// 第一版把 `List` / `BlockQuote` 当成「容器」并用 `depth` 跳过内部，
/// 结果容器内的 `Item` / `Paragraph` 全都不计，参考只数出 5 块而产品给出
/// 8 块 —— **是判据错了，不是产品错了**。已按下面的理由改正：
/// `List` 的每个 `Item`、以及 `BlockQuote` 内的每个 `Paragraph`，
/// 都是**独立的块**（它们各自有自己的文本），不是「容器内的附属物」。
/// 因此参考实现应当**只**认 Heading / CodeBlock / Paragraph / Item
/// 四种块级标签，与容器标签无关。
fn reference_blocks(md: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for ev in Parser::new(md) {
        if let Event::Start(tag) = ev {
            let kind = match &tag {
                Tag::Heading { .. } => "heading",
                Tag::CodeBlock(_) => "code",
                Tag::Paragraph | Tag::Item => "text",
                _ => continue,
            };
            out.push(kind);
        }
    }
    out
}

/// 从 `blocks()` 的 `Value` 提取每个块的 span 文本。
///
/// 判据只需要「块文本」，不关心其余字段；集中在一处提取，
/// 避免三处判据各写一遍嵌套 match（那种重复本身就是 bug 温床）。
fn block_texts(blocks: &mora::value::Value) -> Vec<String> {
    match blocks {
        mora::value::Value::List(v) => v
            .iter()
            .map(|b| {
                let d = match b {
                    mora::value::Value::Dict(d) => d,
                    other => panic!("block must be dict: {other:?}"),
                };
                let spans = match d.get("spans") {
                    Some(mora::value::Value::List(s)) => s,
                    other => panic!("block must have spans: {other:?}"),
                };
                let first = match spans.first() {
                    Some(s) => s,
                    None => return String::new(),
                };
                let sd = match first {
                    mora::value::Value::Dict(sd) => sd,
                    other => panic!("span must be dict: {other:?}"),
                };
                match sd.get("text") {
                    Some(mora::value::Value::String(t)) => t.clone(),
                    _ => String::new(),
                }
            })
            .collect(),
        other => panic!("blocks must be a list: {other:?}"),
    }
}

/// 从 `blocks()` 的 `Value` 提取每个块的 `kind`。
fn block_kinds(blocks: &mora::value::Value) -> Vec<String> {
    match blocks {
        mora::value::Value::List(v) => v
            .iter()
            .map(|b| {
                let d = match b {
                    mora::value::Value::Dict(d) => d,
                    other => panic!("block must be dict: {other:?}"),
                };
                match d.get("kind") {
                    Some(mora::value::Value::String(s)) => s.clone(),
                    _ => panic!("block must have kind string"),
                }
            })
            .collect(),
        other => panic!("blocks must be a list: {other:?}"),
    }
}

const SAMPLE: &str = "\
# Title with **bold** inside

A paragraph with *em* and `code` and [a link](http://x).

## Sub heading

- item one
- item two

```rust
fn main() { println!(\"hi\"); }
```

> a blockquote

Last paragraph.
";

/// 判据 ①：块数与参考实现一致。
#[test]
fn d229_block_count_matches_independent_reference() {
    let backend = MarkdownBackend::new(SAMPLE);
    let blocks = backend.blocks().expect("blocks must succeed");
    let got = block_kinds(&blocks);
    let want = reference_blocks(SAMPLE);

    assert!(!want.is_empty(), "参考实现必须真的数出块（否则判据空转）");
    // 人工核对：标题 / 段落 / 标题 / 列表项 / 列表项 / 代码 / 引用内段落 / 末段
    assert_eq!(
        want,
        vec![
            "heading", "text", "heading", "text", "text", "code", "text", "text"
        ],
        "参考实现的预期形状变了 —— 先确认 pulldown-cmark 行为再改判据"
    );
    assert_eq!(
        got.len(),
        want.len(),
        "块数应与参考一致；\n  got  = {got:?}\n  want = {want:?}\n\
         修前 blocks() 在每个 End(_) 都切一次，内联标记把块劈开（实测 11 块 vs 8 块）"
    );
}

/// 判据 ②：每个块的 `kind` 与参考一致。
///
/// 修前列表项继承了上一个 heading 的 `kind="heading"` ——
/// 「两个列表项被标成标题」。
#[test]
fn d229_list_items_do_not_inherit_previous_heading_kind() {
    let backend = MarkdownBackend::new(SAMPLE);
    let blocks = backend.blocks().expect("blocks must succeed");
    let kinds = block_kinds(&blocks);
    let want = reference_blocks(SAMPLE);
    assert_eq!(kinds, want, "每个块的 kind 都应与参考一致");

    // 针对性断言：列表项绝不能是 heading
    let texts = block_texts(&blocks);
    for (i, k) in kinds.iter().enumerate() {
        if k == "heading" {
            assert!(
                !texts[i].contains("item one") && !texts[i].contains("item two"),
                "D229: 列表项被标成 heading（继承了上一个标题的 kind）: {:?}",
                texts[i]
            );
        }
    }
}

/// 判据 ③：内联标记不把块劈开。
///
/// 修前 `# Title with **bold** inside` 产出两块，第二块是 ` inside`
/// —— 块文本与源文不再对应。
#[test]
fn d229_inline_markers_do_not_split_blocks() {
    let backend = MarkdownBackend::new("# Title with **bold** inside");
    let b = backend.blocks().expect("blocks");
    let list = match &b {
        mora::value::Value::List(v) => v.clone(),
        _ => panic!("blocks must be list"),
    };
    assert_eq!(
        list.len(),
        1,
        "含内联 **bold** 的标题应是**一个**块；修前被 End(Strong) 劈成两块"
    );
    assert_eq!(
        block_texts(&b),
        vec!["Title with bold inside".to_string()],
        "块文本应是把内联标记剥掉后的完整句子"
    );
}

/// 判据 ④（往返不变式）：`text()` 的行数 == `blocks()` 的块数。
///
/// 修前二者对「一个文档有多少块」的判断**不一致**：
/// `blocks()` 按 `End(_)` 切（11 块），`text()` 按每个 `Event::Text` 切
/// （行数更多，且词被切开）。合并成一份 `block_texts` 后不可能漂移。
#[test]
fn d229_text_line_count_equals_block_count() {
    for md in [SAMPLE, "# H\n\npara\n\n- a\n- b\n", "just a paragraph"] {
        let backend = MarkdownBackend::new(md);
        let blocks = match backend.blocks().expect("blocks") {
            mora::value::Value::List(v) => v,
            _ => panic!(),
        };
        let text = backend.text().expect("text");
        let lines = text.lines().count();
        assert_eq!(
            lines,
            blocks.len(),
            "D229: text() 行数 {lines} != blocks() 块数 {}（md={md:?}）\n\
             text=\n{text}\n两者必须来自同一份切块实现",
            blocks.len()
        );
    }
}

/// 判据 ⑤：内联标记不会在 `text()` 里把词切开。
///
/// 修前 `Title with **bold** inside` → 三行（`Title with` / `bold` / ` inside`）。
#[test]
fn d229_text_does_not_split_words_on_inline_markers() {
    let backend = MarkdownBackend::new("# Title with **bold** inside");
    let text = backend.text().expect("text");
    assert_eq!(
        text.trim(),
        "Title with bold inside",
        "D229: 内联标记把词/句拆成了多行；text() 应按块拼接"
    );
    assert!(
        !text.lines().any(|l| l.trim() == "bold"),
        "D229: 'bold' 独占一行 —— 这是按 Event::Text 换行的痕迹"
    );
}
