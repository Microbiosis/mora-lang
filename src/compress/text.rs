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

use std::time::Duration;

use crate::compress::{CompressOptions, SubCompressor};
use crate::config::{AI_API_KEY_ENV, AI_BASE_URL_DEFAULT, AI_BASE_URL_ENV};
use crate::flow::json_to_value;
use crate::value::Value;

/// v0.29: 把字节索引向下对齐到最近的 UTF-8 字符边界。
///
/// 当 `idx` 落在某个多字节 UTF-8 字符的中间字节上时, 向左回退直到字符起点。
/// 这样 `&s[..idx]` 和 `&s[idx..]` 都是合法的字符串切片, 不会 panic。
///
/// v0.29 final review BLOCKER fix — 由 `examples/compact_demo.mora` 的中文文本触发。
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

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
/// - 字节截断可能落在 UTF-8 字符中段, 触发 `slice` panic。
///   修复: 用 `floor_char_boundary` 把切片落到最近的字符边界, 避免 panic
///   (v0.29 final review BLOCKER fix — `examples/compact_demo.mora` 含中文文本)。
pub fn head_tail_impl(content: &str, head_pct: f32, tail_pct: f32, max_bytes: usize) -> String {
    let total = content.len();

    // 若内容已 ≤ max_bytes, 直接返回 — 无压缩、无 marker
    if total <= max_bytes {
        return content.to_string();
    }

    // 截断长度计算 + clamp 防御 (head_pct/tail_pct 异常值时仍安全)
    let head_n = (((total as f32) * head_pct) as usize).min(total);
    let tail_n = (((total as f32) * tail_pct) as usize).min(total);

    // UTF-8 边界对齐: 避免字节切片落在多字节字符中段触发 panic
    // (v0.29 final review BLOCKER — 触发源: examples/compact_demo.mora 中文文本)
    let head_n = floor_char_boundary(content, head_n);
    let tail_start = floor_char_boundary(content, total.saturating_sub(tail_n));

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

/// v0.29: summary 通过 LLM 调用。
///
/// 路径:
/// - `OPENAI_API_KEY` 为空 → mock 截前 200 字符 + `mock_mode` marker。
/// - `OPENAI_API_KEY` 已设置 → 调 Chat Completions API (复用 ureq，保持零 serde 依赖);
///   调用失败时 eprintln 错误并 fallback 到 mock。
pub fn summary_llm_impl(content: &str, _max_bytes: usize) -> Result<String, String> {
    let api_key = std::env::var(AI_API_KEY_ENV).unwrap_or_default();
    let preview: String = content.chars().take(200).collect();

    if api_key.is_empty() {
        return Ok(format!(
            "{}\n<compressed:method=summary mock_mode>",
            preview
        ));
    }

    // 有 API key: 尝试真实 LLM 调用
    let base_url =
        std::env::var(AI_BASE_URL_ENV).unwrap_or_else(|_| AI_BASE_URL_DEFAULT.to_string());
    let prompt_len = content.len().min(4000);
    let prompt = format!(
        "Summarize the following text concisely:\n\n{}",
        &content[..prompt_len]
    );
    if let Ok(summary) = summary_via_llm(&prompt, &api_key, &base_url) {
        Ok(format!("{}\n<compressed:method=summary llm>", summary))
    } else {
        eprintln!("compress.summary: LLM call failed (OPENAI_API_KEY set), falling back to mock");
        Ok(format!(
            "{}\n<compressed:method=summary mock_mode>",
            preview
        ))
    }
}

/// v0.29: 通过 Chat Completions API 执行摘要。
///
/// - 手写 JSON 请求体（保持零 serde 依赖原则）
/// - 用 `json_to_value` 解析响应，提取 `choices[0].message.content`
/// - 30s 读超时（LLM 推理可能慢）
fn summary_via_llm(prompt: &str, api_key: &str, base_url: &str) -> Result<String, String> {
    let escaped_prompt = prompt
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let body = format!(
        r#"{{"model":"gpt-4o-mini","messages":[{{"role":"user","content":"{}"}}]}}"#,
        escaped_prompt
    );
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();

    match agent
        .post(&url)
        .header("Authorization", &format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .send(&body)
    {
        Ok(mut resp) => {
            let status = resp.status();
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("LLM response read error: {}", e))?;
            if status.as_u16() >= 400 {
                return Err(format!(
                    "LLM API error ({}): {}",
                    status,
                    &text[..200.min(text.len())]
                ));
            }
            // 解析响应: choices[0].message.content
            let root =
                json_to_value(&text).map_err(|e| format!("LLM response parse error: {}", e))?;
            if let Value::Dict(map) = root
                && let Some(Value::List(choices)) = map.get("choices")
                && let Some(Value::Dict(choice_map)) = choices.first()
                && let Some(Value::Dict(msg_map)) = choice_map.get("message")
                && let Some(Value::String(content)) = msg_map.get("content")
            {
                return Ok(content.clone());
            }
            Err("LLM response: could not extract content".to_string())
        }
        Err(e) => Err(format!("LLM API request error: {}", e)),
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
        assert!(
            result.contains("elided"),
            "must contain elided marker: {result}"
        );
        assert!(
            result.starts_with("line 0"),
            "must start with first line: {result}"
        );
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
        let result = c
            .compress(text, 100, &opts)
            .expect("lossless should not error");
        assert!(
            result.contains("hello world"),
            "lossless must preserve original"
        );
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

    /// v0.29 final review BLOCKER regression test:
    /// `head_tail_impl` 在含多字节 UTF-8 字符的文本上不应 panic。
    /// 无 fix 时, head_pct=0.3 的 head_n 落在一个中文字符中间字节, 触发 slice panic。
    #[test]
    fn test_text_head_tail_utf8_boundary() {
        // 中文 + ASCII 混合; 故意长到 max_bytes 8 强制触发 elision
        let s = "中文测试 abc 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试";
        let result = head_tail_impl(s, 0.3, 0.3, 8);
        assert!(
            result.contains("elided"),
            "must contain elided marker: {result}"
        );
        // No panic is the main assertion — UTF-8 boundary safety
    }
}
