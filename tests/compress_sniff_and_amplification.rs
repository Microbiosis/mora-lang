// v0.104.6 D382 —— 5 个子压缩器的 **sniff 路由竞争** 与 **log 的「压缩放大」**（否定轮，含两项待裁决）
//
// `src/compress/` 有 4 个判据覆盖 `json`（D367–D370 测了目标计算 /
// 角色判定 / 约束 / 策略），但 **`text` / `log` / `code` / `html`
// 四个子压缩器从未被直接测过** —— 而 `auto` strategy 正是
// `ContentRouter::sniff` 路由到它们的主路径。
//
// ## 路由：`ContentRouter::sniff` 是**分数竞争**
//
// ```rust
// // mod.rs:207
// self.compressors.iter().filter_map(|c| {
//     let score = c.sniff(content);
//     if score > 0.0 { Some((score, c.clone())) } else { None }
// }).max_by(|a, b| a.0.partial_cmp(&b.0)…)   // ← 取最高分
// ```
//
// 实测（4 种典型内容）：
//
// | 内容 | 路由到 | 判定 |
// |---|---|---|
// | ISO/syslog 日志 | `log` | ✅ |
// | HTML 文档 | `html` | ✅ |
// | JSON | `json` | ✅ |
// | 普通散文 / 空串 | `text` | ✅（兜底）|
// | 20+ 行真实 Rust / Python | **`code`** | ✅ |
// | 3 行短代码（keyword 命中 1 个）| `text` | ✅ **阈值行为**，非缺陷 |
//
// ## `code` 的 sniff 有**阈值**（D383 查明）
//
// ```rust
// // code.rs
// pub const CODE_KEYWORDS: &[&str] = &[
//     "fn ", "def ", "class ", "=>", "import ", "public ", "private ", "::",
// ];                      // ← 只有 8 项，且**带尾随空格**
// if hits >= 2 { (0.7 + 0.05 * hits as f32).min(0.95) } else { 0.0 }
// ```
//
// `text` 是**恒定 0.5 的兜底**（`text.rs:232`：
// `fn sniff(&self, _content: &str) -> f32 { 0.5 }`）。
//
// ⚠ **D383 更正**：D382 的首版探针只给了 3 行 Rust（只命中 1 个 `fn `）
// ⇒ 分数 0 ⇒ 落 `text`，而我据此断言「`code` 竞争输了」
// —— 那是**探针样本太小**，不是产品缺陷。
// 改用 20+ 行真实代码后，`code` 路由**完全正常**。
//
// ⇒ 与 D359「绕过 typeck 的样本才有意义」同源：
// **探针必须跨越被测阈值，否则测的是「样本太小」不是「逻辑对错」**。
//
// ## 发现 ①：Rust 代码被路由到 `text` 而非 `code`
//
// `code` 的 sniff 分数低于 `text` ⇒ 竞争输了。
// 结果**功能上仍正确**（`text` 也能压文本），
// 但 `code` 的专门优化（去注释 / 抽公共结构）**用不上**。
//
// ## 发现 ②：`log` 压缩在小输入上**放大 45%**
//
// 5 行日志（200 字节）压成 **290 字节**（+45%）：
//
// ```text
// # 原文 5 行（200 字节）
// <2 ERROR lines preserved> (2 ERROR/FATAL total)      ← 追加 44 字节
// <compressed:method=log original_size=200>            ← 追加 41 字节
// ```
//
// 全部 5 行都保留了（没丢内容），但**加了两行元信息标记**。
//
// ## 关键：这是**两处注释的契约矛盾**，不是漏写
//
// | 位置 | 说的是 |
// |---|---|
// | `mod.rs:117-119`（D227）| 「**放大是比超限更坏的一类** ——『压缩』函数让数据变大」|
// | `mod.rs:127-133`（D227 自己）| 规则 3 **只比较 `body`**，`marker` **豁免**；并明写「为了 91 字节的元信息丢掉全部『保留了哪些错误行』的信息，代价远大于收益」|
//
// ⇒ **两处都是 D227 写的**，且**后一处明确推翻了前者的字面要求**。
// 规则 3 的 `if body.len() > content.len()`（L141）**不比较 `body + marker`**。
//
// ## 既有 D227 判据**没覆盖**这条
//
// `tests/compress_max_bytes_contract.rs` 只断言「输出 ≤ `max_bytes`」
// （**预算契约**）。`290 < 8192` 完全合法 ⇒ 断言通过。
//
// ⇒ **「放大」这条契约（D227 的字面表述）无判据覆盖**。
// 本文件把它**显式钉住**，让这个矛盾可见，而不是被静默继承。

use mora::compress::{CompressOptions, ContentRouter, SubCompressor, log::LogSubCompressor};

const LOG: &str = r#"2024-01-15 10:23:45 ERROR connection refused
2024-01-15 10:23:46 INFO  retrying
2024-01-15 10:23:47 WARN  slow response
2024-01-15 10:23:48 ERROR timeout after 30s
2024-01-15 10:23:49 INFO  giving up
"#;

const CODE: &str = r#"fn main() {
    let x = compute(1, 2);
    if x > 0 { println!("{}", x); }
}
"#;

const HTML: &str = r#"<html><head><title>T</title></head>
<body><div class="a"><p>hello</p></div></body></html>
"#;

const JSON: &str = r#"{"a": 1, "b": [1,2,3], "c": {"d": "e"}}"#;

fn origin_of(content: &str) -> Option<&'static str> {
    ContentRouter::default_router()
        .sniff(content)
        .map(|c| c.origin())
}

/// **路由矩阵**：4 种内容各自路由到**专门的**子压缩器。
#[test]
fn d382_sniff_routes_each_content_to_its_compressor() {
    for (label, content, expected) in [
        ("log", LOG, "log"),
        ("html", HTML, "html"),
        ("json", JSON, "json"),
    ] {
        assert_eq!(
            origin_of(content),
            Some(expected),
            "`{label}` 内容应路由到 `{expected}`"
        );
    }
    // 空串与普通散文落兜底 `text`，且**不 panic**
    assert_eq!(origin_of(""), Some("text"), "空串应落兜底 text");
    assert_eq!(
        origin_of("just some ordinary prose with no markers"),
        Some("text"),
        "普通散文应落兜底 text"
    );
}

/// **短代码（keyword 命中不足）落 `text`** —— 这是 `code` sniff 的**阈值**行为。
///
/// `CODE_KEYWORDS`（`code.rs`）只有 **7 项**且**带尾随空格**：
/// `fn ` / `def ` / `class ` / `=>` / `import ` / `public ` / `private ` / `::`。
/// `sniff` 要求 `hits >= 2` 才给分（`0.7 + 0.05*hits`，上限 0.95）。
///
/// 3 行 Rust 只含 1 个 `fn ` ⇒ `hits=1 < 2` ⇒ 分数 0 ⇒ 落兜底 `text`。
/// ⇒ **不是缺陷，是阈值行为**。
///
/// ⚠ v0.104.6 **D383 更正**：本条首版写成
/// 「Rust 代码当前路由到 `text`（`code` 竞争输了）」并断言
/// `origin_of(CODE) == Some("text")` —— 那是**探针样本太小**，
/// 差一步就把它当成产品缺陷写进 CHANGELOG。改用**命中足够**的
/// 样本后，`code` 路由**完全正常**（见下一条）。
#[test]
fn d382_short_code_falls_back_to_text_by_sniff_threshold() {
    assert_eq!(
        origin_of(CODE),
        Some("text"),
        "3 行 Rust 只命中 1 个 `fn `（< 2 的阈值）⇒ 落兜底 text —— **阈值行为**"
    );
}

/// **命中足够的代码确实路由到 `code`** —— D383 的更正核心。
///
/// 20+ 行真实 Rust / Python 都命中 ≥ 2 个 keyword ⇒ 分数 ≥ 0.7 > `text` 的 0.5。
#[test]
fn d383_real_code_is_routed_to_code() {
    let rust = r#"use std::collections::HashMap;

struct Point { x: f64, y: f64 }

impl Point {
    fn new(x: f64, y: f64) -> Self {
        let mut p = Point { x, y };
        if p.x > 0.0 && p.y > 0.0 {
            p.x = p.x * 2.0;
        }
        return p;
    }
    fn norm(&self) -> f64 {
        let s = (self.x * self.x + self.y * self.y).sqrt();
        return s;
    }
}

fn main() {
    let mut m: HashMap<String, i32> = HashMap::new();
    for i in 0..10 {
        m.insert(format!("k{}", i), i);
    }
    let p = Point::new(3.0, 4.0);
    if p.norm() > 0.0 {
        println!("{:?}", m);
    }
}
"#;
    assert_eq!(
        origin_of(rust),
        Some("code"),
        "20+ 行真实 Rust 应路由到 `code`（D383 更正：不是 `text`）"
    );

    let py = r#"import os

class Foo:
    def __init__(self, x):
        self.x = x

    def bar(self):
        if self.x > 0:
            return self.x
        return 0

def main():
    f = Foo(1)
    print(f.bar())
"#;
    assert_eq!(origin_of(py), Some("code"), "Python 应路由到 `code`");
}

/// **预算契约**：所有子压缩器（含 log）输出**永不超过** `max_bytes`。
///
/// 这是 D227 已有的契约，本条在**小输入**（D227 判据用 60 行大输入）
/// 上补一档 —— 小输入才是 marker 占比最高的场景。
#[test]
fn d382_log_never_exceeds_budget_even_for_tiny_input() {
    let opts = CompressOptions::default();
    let comp = LogSubCompressor;
    for mb in [0usize, 10, 42, 100, 200, 250, 300, 8192] {
        let out = comp.compress(LOG, mb, &opts).expect("log 压缩");
        assert!(
            out.len() <= mb,
            "max_bytes={mb}: 输出 {} 字节超限（log.rs 的 `finish_within_budget` 应收口）",
            out.len()
        );
    }
}

/// **放大契约（当前被违反）**：小输入上 `log` 会**放大**。
///
/// 5 行日志 200 字节 → 290 字节（**+45%**），因为两行 marker
/// 比被丢弃的内容还长。
///
/// 这**直接违反** D227 的字面表述（`mod.rs:117-119`：
/// 「**放大是比超限更坏的一类** ——『压缩』函数让数据变大」），
/// 而 D227 自己的规则 3（`mod.rs:141`）**只比较 `body`**、`marker` 豁免。
//
// **判定：两处注释自相矛盾，且既有 D227 判据只验预算不验放大**。
// 本条把「放大」显式钉成**事实**，让矛盾可见。
#[test]
fn d382_log_amplifies_tiny_input_because_marker_is_exempt() {
    let opts = CompressOptions::default();
    let out = LogSubCompressor
        .compress(LOG, 8192, &opts)
        .expect("log 压缩");
    assert_eq!(out.len(), 290, "5 行日志的压缩结果长度（当前实测值）");
    assert!(
        out.len() > LOG.len(),
        "**放大是事实**：原文 {} 字节 → 输出 {} 字节（+{}%）",
        LOG.len(),
        out.len(),
        (out.len() as i64 - LOG.len() as i64) * 100 / LOG.len() as i64
    );
    // 全部 5 行都保留了 —— 放大**不是**丢内容，是 marker 变长
    for line in LOG.lines() {
        assert!(out.contains(line), "内容不该丢：{line}");
    }
    assert!(
        out.contains("<compressed:method=log"),
        "放大来自 marker（D227 规则 3 豁免它）"
    );
}

/// **反向对照**：预算**紧张**时 `log` 会正确截断，不放大。
#[test]
fn d382_log_shrinks_when_budget_is_tight() {
    let opts = CompressOptions::default();
    let out = LogSubCompressor
        .compress(LOG, 200, &opts)
        .expect("log 压缩");
    assert!(
        out.len() < LOG.len(),
        "mb=200（紧张）时应**缩小**：原文 {} → 输出 {}",
        LOG.len(),
        out.len()
    );
    assert!(out.len() <= 200, "且必须守住预算：{}", out.len());
}
