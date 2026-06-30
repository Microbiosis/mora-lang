use super::serialization::{esc, event_to_jsonl};
use super::*;

pub fn generate_report(
    events: &[Event],
    name: &str,
    note: Option<&str>,
    verify_cmd: Option<&str>,
    env_vars: &[(&str, &str)],
) -> String {
    let stats = compute_stats(events);
    let findings = audit_recording(events, &[]);
    let mut md = String::new();

    md.push_str(&format!("# Evidence Report: {}\n\n", name));
    md.push_str("---\n\n");

    // Metadata
    md.push_str("## Metadata\n\n");
    md.push_str(&format!("- **Recording**: {}\n", name));
    md.push_str(&format!("- **Generated**: {}\n", Recorder::now_ms()));
    if let Some(n) = note {
        md.push_str(&format!("- **Note**: {}\n", n));
    }
    if let Some(cmd) = verify_cmd {
        md.push_str(&format!("- **Verify**: `{}`\n", cmd));
    }
    if !env_vars.is_empty() {
        md.push_str("- **Environment**:\n");
        for (k, v) in env_vars {
            md.push_str(&format!("  - {}={}\n", k, v));
        }
    }
    md.push('\n');

    // Summary
    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Value |\n|--------|-------|\n");
    md.push_str(&format!("| Events | {} |\n", stats.total_events));
    md.push_str(&format!("| AI Calls | {} |\n", stats.ai_chat_count));
    md.push_str(&format!("| Web Calls | {} |\n", stats.web_fetch_count));
    md.push_str(&format!("| Errors | {} |\n", stats.error_count));
    md.push_str(&format!(
        "| Tokens (in+out) | {}+{}={} |\n",
        stats.total_tokens_in,
        stats.total_tokens_out,
        stats.total_tokens_in + stats.total_tokens_out
    ));
    md.push_str(&format!("| Duration | {}ms |\n", stats.duration_ms));
    md.push('\n');

    // Audit
    md.push_str("## Audit\n\n");
    if findings.is_empty() {
        md.push_str("✓ No secrets detected.\n\n");
    } else {
        md.push_str(&format!(
            "⚠ {} potential secret(s) found:\n\n",
            findings.len()
        ));
        md.push_str(
            "| Event | Field | Pattern | Preview |\n|-------|-------|---------|----------|\n",
        );
        for f in &findings {
            md.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                f.event_id, f.field, f.pattern, f.preview
            ));
        }
        md.push('\n');
    }

    // Timeline
    md.push_str("## Timeline\n\n");
    md.push_str("| # | Kind | Detail | Tokens | Lat(ms) | Status |\n");
    md.push_str("|---|------|--------|--------|---------|--------|\n");
    for row in &build_timeline(events) {
        let detail = if row.detail.len() > 50 {
            format!("{}…", &row.detail[..49])
        } else {
            row.detail.clone()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.seq, row.kind, detail, row.tokens, row.latency_ms, row.status
        ));
    }
    md.push('\n');

    // Redacted event log
    md.push_str("## Event Log (redacted)\n\n");
    md.push_str("```jsonl\n");
    for ev in events {
        let line = event_to_jsonl(ev);
        md.push_str(&redact_secrets(&line));
        md.push('\n');
    }
    md.push_str("```\n");

    md
}

/// 快照基线格式 (简化: 只存事件摘要,不存完整响应)
#[derive(Clone, Debug)]
pub struct SnapshotBaseline {
    pub name: String,
    pub event_summaries: Vec<EventSummary>,
    pub created_ms: u128,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventSummary {
    pub kind: String,
    pub key: String, // model+prompt_hash 或 url
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub has_error: bool,
}

fn event_to_summary(ev: &Event) -> EventSummary {
    match ev {
        Event::AiChat {
            model,
            prompt_hash,
            tokens_in,
            tokens_out,
            error,
            ..
        } => EventSummary {
            kind: "ai.chat".to_string(),
            key: format!("{}|{}", model, prompt_hash),
            tokens_in: *tokens_in,
            tokens_out: *tokens_out,
            has_error: error.is_some(),
        },
        Event::WebFetch { url, error, .. } => EventSummary {
            kind: "web.fetch".to_string(),
            key: url.clone(),
            tokens_in: 0,
            tokens_out: 0,
            has_error: error.is_some(),
        },
        Event::Note { message, .. } => EventSummary {
            kind: "note".to_string(),
            key: message.clone(),
            tokens_in: 0,
            tokens_out: 0,
            has_error: false,
        },
    }
}

/// 从事件列表创建快照基线
pub fn create_snapshot(name: &str, events: &[Event]) -> SnapshotBaseline {
    SnapshotBaseline {
        name: name.to_string(),
        event_summaries: events.iter().map(event_to_summary).collect(),
        created_ms: Recorder::now_ms(),
    }
}

/// 快照基线序列化为 JSONL (简化格式)
pub fn snapshot_to_jsonl(snap: &SnapshotBaseline) -> String {
    let mut out = format!(
        "{{\"name\":\"{}\",\"created_ms\":{}}}\n",
        esc(&snap.name),
        snap.created_ms
    );
    for s in &snap.event_summaries {
        out.push_str(&format!(
            "{{\"kind\":\"{}\",\"key\":\"{}\",\"tokens_in\":{},\"tokens_out\":{},\"has_error\":{}}}\n",
            esc(&s.kind), esc(&s.key), s.tokens_in, s.tokens_out, s.has_error
        ));
    }
    out
}

/// 从 JSONL 解析快照基线
pub fn snapshot_from_jsonl(content: &str) -> Option<SnapshotBaseline> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let first = lines[0];
    // 提取 name 和 created_ms
    let name = extract_json_string(first, "name")?;
    let created_ms = extract_json_number(first, "created_ms").unwrap_or(0);
    let mut summaries = Vec::new();
    for line in &lines[1..] {
        let kind = extract_json_string(line, "kind")?;
        let key = extract_json_string(line, "key")?;
        let tokens_in = extract_json_number(line, "tokens_in").unwrap_or(0) as usize;
        let tokens_out = extract_json_number(line, "tokens_out").unwrap_or(0) as usize;
        let has_error = line.contains("\"has_error\":true");
        summaries.push(EventSummary {
            kind,
            key,
            tokens_in,
            tokens_out,
            has_error,
        });
    }
    Some(SnapshotBaseline {
        name,
        event_summaries: summaries,
        created_ms,
    })
}

fn extract_json_string(line: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_json_number(line: &str, key: &str) -> Option<u128> {
    let pattern = format!("\"{}\":", key);
    let start = line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 快照对比结果
#[derive(Clone, Debug)]
pub enum SnapshotDiff {
    /// 事件一致
    Match(usize),
    /// 事件数不同
    CountMismatch { expected: usize, actual: usize },
    /// 单个事件不同
    EventChanged {
        index: usize,
        expected: EventSummary,
        actual: EventSummary,
    },
    /// 新增事件
    EventAdded { index: usize, actual: EventSummary },
    /// 缺失事件
    EventMissing {
        index: usize,
        expected: EventSummary,
    },
}

/// 对比快照基线与当前事件
pub fn diff_snapshot(baseline: &SnapshotBaseline, current: &[Event]) -> Vec<SnapshotDiff> {
    let current_summaries: Vec<EventSummary> = current.iter().map(event_to_summary).collect();
    let mut diffs = Vec::new();
    let max = baseline.event_summaries.len().max(current_summaries.len());
    for i in 0..max {
        match (baseline.event_summaries.get(i), current_summaries.get(i)) {
            (Some(expected), Some(actual)) => {
                if expected == actual {
                    diffs.push(SnapshotDiff::Match(i));
                } else {
                    diffs.push(SnapshotDiff::EventChanged {
                        index: i,
                        expected: expected.clone(),
                        actual: actual.clone(),
                    });
                }
            }
            (Some(expected), None) => {
                diffs.push(SnapshotDiff::EventMissing {
                    index: i,
                    expected: expected.clone(),
                });
            }
            (None, Some(actual)) => {
                diffs.push(SnapshotDiff::EventAdded {
                    index: i,
                    actual: actual.clone(),
                });
            }
            (None, None) => {}
        }
    }
    diffs
}
