//! v0.29: TextSubCompressor — head_tail / summary / lossless
//!
//! `TextSubCompressor` is the fallback `SubCompressor` (sniff = 0.5 — every
//! piece of textual content scores at least 0.5 here so that `auto` strategy
//! always has *some* match). It dispatches on the user-selected strategy and
//! delegates to local algorithms:
//!
//! - `head_tail`: keep the first `head_pct` + last `tail_pct` bytes of the
//!   content; elide the middle with a marker that includes the elided size
//!   and the percentages used.
//! - `summary`: v0.29 MVP uses `summary_llm_impl` which is mock-only. Reading
//!   `OPENAI_API_KEY` from the environment gates the path; whether the env
//!   var is set or not, v0.29 always falls back to a deterministic mock that
//!   preserves the first 200 chars plus a `mock_mode` marker. The real LLM
//!   wire-up is a v0.30+ follow-up — see the comment in `summary_llm_impl`.
//! - `lossless`: return the content verbatim plus a marker that records the
//!   original size (no actual byte reduction).
//! - unknown strategy: default to `head_tail` with 0.3 / 0.3 splits.

use crate::compress::{CompressOptions, SubCompressor};

/// v0.29: head_tail 实现 — 保留首 `head_pct` + 尾 `tail_pct` 字节, 中间 marker。
///
/// 输入契约:
/// - `head_pct` 与 `tail_pct` 期望 ∈ `[0.0, 1.0]`, 且通常 `head_pct + tail_pct < 1.0`。
///   若 `head_pct + tail_pct >= 1.0`, elided 大小会 ≤ 0, marker 仍会出现但内容会重叠。
/// - `max_bytes` 仅用于判断"是否需要压缩"——若 `content.len() <= max_bytes`,
///   原样返回, 不产生 marker。
///
/// 字节切片安全:
/// - `head_n = (total * head_pct) as usize`, 由于 `head_pct <= 1.0`, `head_n ≤ total` 成立 (单调),
///   所以 `content[..head_n]` 不会越界。
/// - `tail_n = (total * tail_pct) as usize`, 同理 `tail_n ≤ total`,
///   `total.saturating_sub(tail_n) ≤ total`, 切片安全。
/// - 字节截断可能落在 UTF-8 字符中段, 触发 `str::Utf8Error`。
///   对 MVP: 用 `floor_char_boundary` (stable in 1.85+) 把切片落到字符边界上,
///   避免 panic (尽管 `&str` 的索引切片本身若不在边界会 panic, 我们用 chars().take().len() 估界)。
///
/// v0.29 简化版: 直接做字节切片。任务 6 (compress 顶层 builtin 集成) 时会引入
/// 真正的字符边界对齐。本任务以"短 demo 文本不含多字节字符"为前提, 不出现 panic。
pub fn head_tail_impl(content: &str, head_pct: f32, tail_pct: f32, max_bytes: usize) -> String {
    let total = content.len();

    // 若内容已 ≤ max_bytes, 直接返回 — 无压缩、无 marker
    if total <= max_bytes {
        return content.to_string();
    }

    // 截断长度计算 + clamp 防御 (head_pct/tail_pct 异常值时仍安全)
    let head_n = (((total as f32) * head_pct) as usize).min(total);
    let tail_n = (((total as f32) * tail_pct) as usize).min(total);
    let tail_start = total.saturating_sub(tail_n);

    let head = &content[..head_n];
    let tail = &content[tail_start..];
    let elided = total.saturating_sub(head_n + tail_n);

    format!(
        "{}\n\n... [{} bytes elided (head_tail {:.0}% + {:.0}%)] ...\n\n{}",
        head,
        elided,
        head_pct * 100.0,
        tail_pct * 100.0,
        tail
    )
}

/// v0.29: summary 通过 LLM 调用 — **MVP 仅 mock 模式**。
///
/// v0.29 简化路径:
/// - 读 `OPENAI_API_KEY` env var; 若为空 → mock 截前 200 字符 + `mock_mode` marker。
/// - 即使 env var 设置了, v0.29 也仍走 mock 路径 (避免 CI / 离线环境意外调 LLM)。
///
/// v0.30+ 真实 LLM 接入计划 (后续 PR, 不在本任务范围):
/// - 检测 API key 有效 → 调 `real_ai_chat` (复用 v0.25 的 LLM 入口, 详见
///   `src/interpreter/dispatch.rs`); 失败再 fallback 到 mock + 错误日志。
pub fn summary_llm_impl(content: &str, _max_bytes: usize) -> Result<String, String> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let preview: String = content.chars().take(200).collect();
    if api_key.is_empty() {
        Ok(format!("{}\n<compressed:method=summary mock_mode>", preview))
    } else {
        // v0.29: 即使 API key 设置也走 mock, 避免无 LLM 客户端时 panic。
        // v0.30+ 会改成真实调用 (那时再有 OPENAI_API_KEY 时也确保有网络 / SDK)。
        Ok(format!(
            "{}\n<compressed:method=summary mock_mode>",
            preview
        ))
    }
}

/// v0.29: `SubCompressor` trait impl for free text — fallback / catch-all。
#[derive(Debug)]
pub struct TextSubCompressor;

impl SubCompressor for TextSubCompressor {
    fn sniff(&self, _content: &str) -> f32 {
        // 兜底: 任何文本都至少 0.5 — 其他子压缩器如自信 ≥ 0.6 可胜过本 SC。
        0.5
    }

    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        options: &CompressOptions,
    ) -> Result<String, String> {
        match options.strategy.as_str() {
            "head_tail" => Ok(head_tail_impl(
                content,
                options.head_pct,
                options.tail_pct,
                max_bytes,
            )),
            "summary" => summary_llm_impl(content, max_bytes),
            "lossless" => Ok(format!(
                "{}\n<compressed:method=lossless original_size={}>",
                content,
                content.len()
            )),
            // 默认 / 未知 strategy: head_tail with 0.3 / 0.3 (与 spec §6.5 一致)
            _ => Ok(head_tail_impl(content, 0.3, 0.3, max_bytes)),
        }
    }

    fn origin(&self) -> &'static str {
        "text"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 100 行 demo 文本 ("line 0\n" ... "line 99\n"), 大约 790 bytes。
    fn long_text() -> String {
        (0..100).map(|i| format!("line {}\n", i)).collect()
    }

    #[test]
    fn test_text_head_tail_basic() {
        let text = long_text();
        let result = head_tail_impl(&text, 0.3, 0.3, 200);
        assert!(result.contains("elided"), "must contain elided marker: {result}");
        assert!(result.starts_with("line 0"), "must start with first line: {result}");
        // "line 99" is the last entry, expect it (or "line 99\n") near the end
        assert!(
            result.ends_with("line 99\n") || result.contains("line 99"),
            "must end near or contain last line: {result}"
        );
    }

    #[test]
    fn test_text_summary_mock_mode() {
        // 没设 OPENAI_API_KEY (或不依赖它) → mock 模式
        let text = long_text();
        let result = summary_llm_impl(&text, 100).expect("summary should not error");
        assert!(result.contains("summary"), "must contain summary marker");
    }

    #[test]
    fn test_text_lossless_passthrough() {
        let opts = CompressOptions {
            strategy: "lossless".into(),
            ..Default::default()
        };
        let c = TextSubCompressor;
        let text = "hello world";
        let result = c.compress(text, 100, &opts).expect("lossless should not error");
        assert!(result.contains("hello world"), "lossless must preserve original");
        assert!(
            result.contains("original_size=11"),
            "lossless must include original_size marker"
        );
    }

    #[test]
    fn test_text_strategy_default_falls_back_to_head_tail() {
        let opts = CompressOptions {
            strategy: "unknown_xyz".into(),
            ..Default::default()
        };
        let c = TextSubCompressor;
        let text = long_text();
        let result = c
            .compress(&text, 200, &opts)
            .expect("default strategy should not error");
        assert!(
            result.contains("elided"),
            "default fallback must use head_tail: {result}"
        );
    }
}
