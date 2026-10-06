//! v0.25: record diff — diff_recordings（两录制事件对比，DiffLine 渲染）。

use super::*;

pub fn diff_recordings(a_events: &[Event], b_events: &[Event]) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let max = a_events.len().max(b_events.len());
    for i in 0..max {
        match (a_events.get(i), b_events.get(i)) {
            (Some(a), Some(b)) => {
                let summary_a = summarize_event(a);
                let summary_b = summarize_event(b);
                if summary_a == summary_b {
                    out.push(DiffLine::Identical(i + 1, summary_a));
                } else {
                    out.push(DiffLine::Changed(i + 1, summary_a, summary_b));
                }
            }
            (Some(a), None) => out.push(DiffLine::OnlyInA(i + 1, summarize_event(a))),
            (None, Some(b)) => out.push(DiffLine::OnlyInB(i + 1, summarize_event(b))),
            (None, None) => {} // unreachable (max computed)
        }
    }
    out
}

#[derive(Clone, Debug)]
pub enum DiffLine {
    Identical(usize, String),
    Changed(usize, String, String),
    OnlyInA(usize, String),
    OnlyInB(usize, String),
}

impl DiffLine {
    pub fn render(&self) -> String {
        match self {
            DiffLine::Identical(n, s) => format!("  [#{}] {}", n, s),
            DiffLine::Changed(n, a, b) => {
                format!("~ [#{}]-\n        {}\n~ [#{}]+\n        {}", n, a, n, b)
            }
            DiffLine::OnlyInA(n, s) => format!("- [#{}] {}", n, s),
            DiffLine::OnlyInB(n, s) => format!("+ [#{}] {}", n, s),
        }
    }
}

fn summarize_event(ev: &Event) -> String {
    match ev {
        Event::AiChat {
            model,
            tokens_in,
            tokens_out,
            latency_ms,
            response,
            error,
            ..
        } => {
            let resp_preview: String = response.chars().take(60).collect();
            if let Some(e) = error {
                format!("ai.chat model={} ERROR={}", model, e)
            } else {
                format!(
                    "ai.chat model={} tokens={}+{} latency={}ms resp={:?}",
                    model, tokens_in, tokens_out, latency_ms, resp_preview
                )
            }
        }
        Event::WebFetch {
            url,
            method,
            status,
            body_len,
            latency_ms,
            error,
            ..
        } => {
            if let Some(e) = error {
                format!("web.fetch {} {} ERROR={}", method, url, e)
            } else {
                format!(
                    "web.fetch {} {} -> {} ({}B, {}ms)",
                    method, url, status, body_len, latency_ms
                )
            }
        }
        Event::Note { message, .. } => format!("note: {}", message),
        // v0.83: Msg + StateMutation 简化为基本信息
        Event::Msg {
            channel, payload, ..
        } => format!("msg channel={} payload={:?}", channel, payload),
        // v0.104.6 D176：原先摘要**只取 `var`（变量名），把 `old` / `new` 两个值
        // 整个丢掉**。后果是「同一变量名、值不同」被判成 identical ——
        // 两次运行 `let score = 42` vs `let score = 99999`，
        // `mora diff` 报 `identical=2 changed=0`。
        //
        // 而 JSONL 里**数据是齐的**（`"new":42.0` vs `"new":99999.0`），
        // 即：不是没录到，是比对时自己扔了。state mutation 记录的是
        // agent 的状态/记忆变化，**值变化本身就是结果** ——
        // 丢它等于让这份录像的 diff 面对最该发现的那类差异失明。
        Event::StateMutation { var, old, new, .. } => {
            // 与上面 AiChat 的 `resp` 同一套截断口径（60 字符），
            // 免得长 dict/list 把对齐的 diff 输出冲垮。
            let old_s: String = old.to_string().chars().take(40).collect();
            let new_s: String = new.to_string().chars().take(40).collect();
            format!("state_mutation var={} {} -> {}", var, old_s, new_s)
        }
    }
}

// ===================================================================
// v0.15: CLI 辅助函数 (list / stats / timeline / export / audit / report)
// ===================================================================
