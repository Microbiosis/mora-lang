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

use crate::compress::{CompressOptions, SubCompressor, finish_within_budget};
use crate::config::{AI_API_KEY_ENV, AI_BASE_URL_DEFAULT, AI_BASE_URL_ENV};
use crate::flow::json_to_value;
use crate::value::Value;

/// v0.29: 把字节索引向下对齐到最近的 UTF-8 字符边界。
///
/// 当 `idx` 落在某个多字节 UTF-8 字符的中间字节上时, 向左回退直到字符起点。
/// 这样 `&s[..idx]` 和 `&s[idx..]` 都是合法的字符串切片, 不会 panic。
///
/// v0.29 final review BLOCKER fix — 由 `examples/compact_demo.mora` 的中文文本触发。
///
/// v0.104.6 D227：改为转发到 `compress::floor_char_boundary`（已提为共享），
/// 本文件保留这个名字是因为 4 处调用点 + 单测都按它写。
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    crate::compress::floor_char_boundary(s, idx)
}

/// v0.29: head_tail 实现 — 保留首 `head_pct` + 尾 `tail_pct` 字节, 中间 marker。
///
/// 输入契约:
/// - `head_pct` 与 `tail_pct` 期望 ∈ `[0.0, 1.0]`, 且通常 `head_pct + tail_pct < 1.0`。
///   **v0.104.6 D227**: 此前二者**完全无校验** —— 和 > 1 时 head 段与 tail 段
///   **重叠**, 同一段内容被输出两次, 结果比原文更长 (实测 6500 → 7850)。
///   现在钳制到 `[0.0, 1.0]` 并要求 `head_pct + tail_pct <= 1.0`。
/// - `max_bytes` 现在是**硬上限**: head/tail 两段的字节预算由它反推
///   (修前只当「要不要压缩」的开关, 结果 max_bytes=10 与 max_bytes=2000
///   输出**完全一样**的 2003 字节)。
///
/// 字节切片安全:
/// - 预算反推保证 `head_n + tail_n <= total`, 两个区间不重叠, 切片不越界。
/// - 字节截断可能落在 UTF-8 字符中段 → 用 `floor_char_boundary` 对齐。
pub fn head_tail_impl(content: &str, head_pct: f32, tail_pct: f32, max_bytes: usize) -> String {
    let total = content.len();

    // 若内容已 ≤ max_bytes, 直接返回 — 无压缩、无 marker
    if total <= max_bytes {
        return content.to_string();
    }

    // v0.104.6 D227：pct 钳制。越界值（负数 / > 1 / 和 > 1）此前全部直接
    // 参与切片，导致重叠或空输出。现按「和 ≤ 1」等比缩放。
    let head_pct = head_pct.clamp(0.0, 1.0);
    let tail_pct = tail_pct.clamp(0.0, 1.0);
    let sum = head_pct + tail_pct;
    let (head_pct, tail_pct) = if sum > 1.0 {
        (head_pct / sum, tail_pct / sum)
    } else {
        (head_pct, tail_pct)
    };

    // v0.104.6 D227：预算从 max_bytes 反推。
    //
    // 预留量按**真实** marker 的保守长度算（marker 里嵌了 elided 字节数，
    // 位数不定，故按 usize 上界估），而不是拍一个常数。拍常数会让小预算
    // 场景（max_bytes=10）输出退化成 2 字节这种无意义的残片。
    // 收尾时由 `finish_within_budget` 兜底（它才是契约的保证者）。
    const MARKER_RESERVE: usize = 64;
    let budget_body = max_bytes.saturating_sub(MARKER_RESERVE);
    let take = ((total as f64) * (head_pct + tail_pct) as f64).min(budget_body as f64) as usize;

    // 按 head:tail 的比例把 take 拆成两段（比例已保证和 ≤ 1，两段不重叠）
    let head_n = if sum > 0.0 {
        ((take as f64) * (head_pct as f64 / sum as f64)) as usize
    } else {
        0
    };
    let tail_n = take.saturating_sub(head_n);

    // UTF-8 边界对齐: 避免字节切片落在多字节字符中段触发 panic
    let head_n = floor_char_boundary(content, head_n.min(total));
    let tail_n = floor_char_boundary(content, tail_n.min(total.saturating_sub(head_n)));
    let tail_start = floor_char_boundary(content, total.saturating_sub(tail_n));

    let head = &content[..head_n];
    let tail = &content[tail_start..];
    let elided = total.saturating_sub(head_n + tail_n);

    // v0.104.6 D227：marker 报**实际**保留比例，不是请求的 pct。
    //
    // 修前报的是 `head_pct` / `tail_pct` 原值。预算饱和时（max_bytes 远小于
    // total × pct）二者分叉：实测 pct=0.5+0.5、max_bytes=1000、total=6500 时，
    // marker 写「head_tail 50% + 50%」，实况只留了 975/6500 ≈ 15%。
    // marker 是压缩结果的**唯一**自述，报请求值等于让输出对自身撒谎。
    let (real_head_pct, real_tail_pct) = if total == 0 {
        (0.0, 0.0)
    } else {
        (
            head_n as f64 * 100.0 / total as f64,
            tail_n as f64 * 100.0 / total as f64,
        )
    };
    let marker = format!(
        "\n\n... [{} bytes elided (head_tail {:.0}% + {:.0}%)] ...\n\n",
        elided, real_head_pct, real_tail_pct
    );
    let body = format!("{head}\n\n{tail}");
    finish_within_budget(content, body, &marker, max_bytes)
}

/// v0.29: summary 通过 LLM 调用。
///
/// 路径:
/// - `OPENAI_API_KEY` 为空 → mock 截前 200 字符 + `mock_mode` marker。
/// - `OPENAI_API_KEY` 已设置 → 调 Chat Completions API (复用 ureq，保持零 serde 依赖);
///   调用失败时 eprintln 错误并 fallback 到 mock。
pub fn summary_llm_impl(content: &str, max_bytes: usize) -> Result<String, String> {
    let api_key = std::env::var(AI_API_KEY_ENV).unwrap_or_default();

    if api_key.is_empty() {
        // v0.104.6 D227：mock 模式此前固定取前 **200 字符**并无视 max_bytes
        // （max_bytes=16 时输出 240+ 字节）。现在预览长度由 max_bytes 定，
        // 并经 `finish_within_budget` 收口。
        return Ok(mock_summary(content, max_bytes));
    }

    // 有 API key: 尝试真实 LLM 调用
    let base_url =
        std::env::var(AI_BASE_URL_ENV).unwrap_or_else(|_| AI_BASE_URL_DEFAULT.to_string());
    let prompt_len = floor_char_boundary(content, content.len().min(4000));
    let prompt = format!(
        "Summarize the following text concisely:\n\n{}",
        &content[..prompt_len]
    );
    if let Ok(summary) = summary_via_llm(&prompt, &api_key, &base_url) {
        // v0.104.6 D227：真实 LLM 的摘要长度同样不受控，经收口保证上限。
        Ok(crate::compress::finish_within_budget(
            content,
            summary,
            "\n<compressed:method=summary llm>",
            max_bytes,
        ))
    } else {
        eprintln!("compress.summary: LLM call failed (OPENAI_API_KEY set), falling back to mock");
        Ok(mock_summary(content, max_bytes))
    }
}

/// v0.104.6 D227：mock 摘要 —— 预览长度由 `max_bytes` 决定。
///
/// 修前固定 `content.chars().take(200)`，与 `max_bytes` 无关：
/// 短内容 + 小上限时输出必然超限。
fn mock_summary(content: &str, max_bytes: usize) -> String {
    const MARKER: &str = "\n<compressed:method=summary mock_mode>";
    let room = max_bytes.saturating_sub(MARKER.len());
    let take = floor_char_boundary(content, room.min(content.len()));
    crate::compress::finish_within_budget(content, content[..take].to_string(), MARKER, max_bytes)
}

/// v0.29: 通过 Chat Completions API 执行摘要。
///
/// - 手写 JSON 请求体（保持零 serde 依赖原则）
/// - 用 `json_to_value` 解析响应，提取 `choices[0].message.content`
/// - 30s 读超时（LLM 推理可能慢）
fn summary_via_llm(prompt: &str, api_key: &str, base_url: &str) -> Result<String, String> {
    // v0.104.6 D209：改用共享的 RFC 8259 转义器
    let escaped_prompt = crate::flow::escape_json_string(prompt);
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
            // v0.104.6 D227：lossless 此前**无条件**返回 content + marker，
            // 完全不看 max_bytes —— 原样输出必然超限（实测 6500 → 6560，
            // 而 max_bytes 可以是 16）。lossless 的语义本就是「不丢内容」，
            // 所以正确行为是：装得下就原样返回，装不下就**如实报错**，
            // 而不是静默返回一个超限结果。
            "lossless" => {
                let marker = format!(
                    "\n<compressed:method=lossless original_size={}>",
                    content.len()
                );
                if content.len() + marker.len() <= max_bytes {
                    Ok(format!("{content}{marker}"))
                } else {
                    Err(format!(
                        "compress.lossless: content is {} bytes but max_bytes is {} \
                         — lossless keeps all content, so it cannot fit; \
                         use head_tail or raise max_bytes",
                        content.len(),
                        max_bytes
                    ))
                }
            }
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
        // 中文 + ASCII 混合; 故意长到 max_bytes 64 强制触发 elision
        //
        // v0.104.6 D227：预算从 8 提到 64。原先 `max_bytes: 8` 让本判据的
        // `contains("elided")` 与 D227 的字节上限契约正面冲突 —— 8 字节
        // 装不下 `... [N bytes elided (head_tail H% + T%)] ...` marker，
        // 收口函数把输出截到 0 字节。64 字节既能触发省略（原文 ~300 字节），
        // 又装得下 marker，两条判据同时成立。
        let s = "中文测试 abc 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试 中文测试";
        let result = head_tail_impl(s, 0.3, 0.3, 64);
        assert!(
            result.contains("elided"),
            "must contain elided marker: {result}"
        );
        // D227: 输出必须仍是合法 UTF-8（截断点落在字符边界上），
        // 且不超过 max_bytes
        assert!(
            result.len() <= 64,
            "D227: 输出不得超过 max_bytes; 实得 {} 字节: {result}",
            result.len()
        );
        // No panic is the main assertion — UTF-8 boundary safety
    }
}
