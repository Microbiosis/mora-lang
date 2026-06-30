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
    }
}

// ===================================================================
// v0.15: CLI 辅助函数 (list / stats / timeline / export / audit / report)
// ===================================================================
