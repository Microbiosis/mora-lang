//! v0.104.6 D384 —— 压缩 sniff 的**三个阈值边界**成对验证（否定轮，无产品变更）
//!
//! D383 的教训是「探针必须跨越被测阈值」，但**没人系统做过**这件事。
//! 本轮把 `src/compress/` 里的三个阈值型判定用**成对样本**
//! （恰好达阈值 / 恰好差一个）逐个压测。
//!
//! | 阈值 | 位置 | 表达式 |
//! |---|---|---|
//! | `>= 0.5` | `html.rs:25` | `<` 的**总个数** / **行数** |
//! | `>= 0.4` | `log.rs:58` | 命中 syslog/ISO 的**行数** / 总行数 |
//! | `>= 2` | `code.rs:43` | `CODE_KEYWORDS` 命中数 |
//!
//! **全部精确，无 off-by-one。**
//!
//! ## 探针必须按**实现用的那个量**构造
//!
//! `html.rs:23-25` 的判据是
//! `content.matches('<').count() / content.lines().count()` ——
//! 分子是 **`<` 字符的总个数**，不是「含标签的行数」。
//!
//! 首版探针每行放 2 个 `<`（`<div>` + `<p>`）⇒ 实测 ratio 是我心算的 2 倍，
//! 一度以为「阈值 0.5 以下也命中」。改成每行 1 个 `<` 后，
//! `0.50 → html` / `0.40 → text` 完全吻合。
//!
//! ⇒ 与 D383 同源，但更深一层：
//! **不仅样本要跨越阈值，样本的构造方式还必须与实现的判据同构。**

use mora::compress::ContentRouter;

fn origin_of(c: &str) -> Option<&'static str> {
    ContentRouter::default_router().sniff(c).map(|c| c.origin())
}

/// 造 n 行文本，其中前 `tagged` 行**各含恰好 1 个 `<`**。
fn html_lines(n: usize, tagged: usize) -> String {
    (0..n)
        .map(|i| if i < tagged { "<x>" } else { "y" })
        .collect::<Vec<_>>()
        .join("\n")
}

/// **`html` 阈值 `>= 0.5`** —— 成对压测。
#[test]
fn d384_html_sniff_threshold_is_exactly_half() {
    for (n, tagged, expect_html) in [
        (10usize, 10usize, true), // 1.00
        (10, 6, true),            // 0.60
        (10, 5, true),            // 0.50 ← 恰好达阈值
        (10, 4, false),           // 0.40 ← 恰好不过
        (10, 1, false),           // 0.10
        (10, 0, false),           // 0.00
        (4, 2, true),             // 0.50
        (4, 1, false),            // 0.25
    ] {
        let c = html_lines(n, tagged);
        let ratio = c.matches('<').count() as f32 / c.lines().count() as f32;
        assert_eq!(
            origin_of(&c),
            Some(if expect_html { "html" } else { "text" }),
            "n={n} '<'={tagged} ratio={ratio:.2}：期望 {}",
            if expect_html { "html" } else { "text" }
        );
    }
}

/// 造 n 行日志，前 `hits` 行含 syslog 级别词，**其余行不含**。
///
/// ⚠ 首版每行都以 `2024-01-15 10:00:` 开头 ⇒ `looks_like_iso_prefix`
/// **每行都命中** ⇒ `line_hits = n` ⇒ ratio 恒 1.0，阈值根本没被压到。
/// 这里用**无时间前缀**的普通行，才能真正压 `>= 0.4` 的边界。
fn log_lines(n: usize, hits: usize) -> String {
    (0..n)
        .map(|i| {
            if i < hits {
                format!("ERROR something bad happened at step {i}")
            } else {
                format!("plain boring line number {i} with nothing special")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// **`log` 阈值 `>= 0.4`** —— 成对压测。
#[test]
fn d384_log_sniff_threshold_is_exactly_two_fifths() {
    for (n, hits, expect_log) in [
        (10usize, 10usize, true), // 1.00
        (10, 5, true),            // 0.50
        (10, 4, true),            // 0.40 ← 恰好达阈值
        (10, 3, false),           // 0.30 ← 恰好不过
        (10, 1, false),           // 0.10
        (5, 2, true),             // 0.40
        (5, 1, false),            // 0.20
        (100, 40, true),          // 0.40
        (100, 39, false),         // 0.39 ← 恰好不过
    ] {
        let c = log_lines(n, hits);
        let ratio = hits as f32 / n as f32;
        let got = origin_of(&c);
        if expect_log {
            assert_eq!(
                got,
                Some("log"),
                "n={n} hits={hits} ratio={ratio:.2}：期望 log"
            );
        } else {
            assert_ne!(
                got,
                Some("log"),
                "n={n} hits={hits} ratio={ratio:.2}：**不该**是 log（阈值 0.4）"
            );
        }
    }
}

/// **`code` 阈值 `>= 2`** —— 成对压测。
#[test]
fn d384_code_sniff_threshold_is_exactly_two_keywords() {
    // 1 个 `fn ` ⇒ 分数 0 ⇒ 落 text
    assert_eq!(
        origin_of("fn f() {}\n"),
        Some("text"),
        "只命中 1 个 `fn `（< 2）⇒ 落 text"
    );
    // 2 个 `fn ` ⇒ 分数 0.7 > text 的 0.5 ⇒ code
    assert_eq!(
        origin_of("fn f() {}\nfn g() {}\n"),
        Some("code"),
        "命中 2 个 ⇒ code"
    );
    // 0 个 ⇒ text
    assert_eq!(
        origin_of("let x = 1;\nlet y = 2;\n"),
        Some("text"),
        "0 个 keyword ⇒ text"
    );
    // 混合命中（`import ` + `class ` + `def ` 三个）⇒ code
    assert_eq!(
        origin_of("import os\nclass A:\n    def f(self):\n        pass\n"),
        Some("code"),
        "Python 三关键字 ⇒ code"
    );
}

/// **纯文本落兜底**且 `sniff` **不 panic**（空串、单字符、超长单行）。
#[test]
fn d384_sniff_never_panics_on_degenerate_input() {
    for content in ["", " ", "\n\n\n", "a", "<", "x".repeat(10_000).leak()] {
        let got = origin_of(content);
        assert!(
            got.is_some(),
            "任何输入都应被路由到某个子压缩器（含空串）; content_len={}",
            content.len()
        );
    }
}
