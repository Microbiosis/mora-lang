use super::serialization::{event_to_jsonl, load_jsonl};
use super::*;
use std::path::Path;

pub fn list_recordings(dir: &Path) -> Result<Vec<RecordingInfo>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries =
        fs::read_dir(dir).map_err(|e| format!("list: failed to read {}: {}", dir.display(), e))?;
    let mut infos = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("list: read_dir error: {}", e))?;
        let path = entry.path();
        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let metadata =
                fs::metadata(&path).map_err(|e| format!("list: metadata error: {}", e))?;
            let size_bytes = metadata.len();
            // 快速计数事件数
            let event_count = count_lines(&path).unwrap_or(0);
            // 加载首尾事件获取时间范围
            let (first_ts, last_ts) = load_time_range(&path);
            infos.push(RecordingInfo {
                name,
                path,
                size_bytes,
                event_count,
                first_ts_ms: first_ts,
                last_ts_ms: last_ts,
            });
        }
    }
    infos.sort_by_key(|b| std::cmp::Reverse(b.last_ts_ms)); // 最新在前
    Ok(infos)
}

/// 录制文件元信息
#[derive(Clone, Debug)]
pub struct RecordingInfo {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub event_count: usize,
    pub first_ts_ms: u128,
    pub last_ts_ms: u128,
}

fn count_lines(path: &Path) -> Result<usize, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    Ok(reader
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count())
}

fn load_time_range(path: &Path) -> (u128, u128) {
    let events = load_jsonl(path).unwrap_or_default();
    let first = events.first().map(event_ts).unwrap_or(0);
    let last = events.last().map(event_ts).unwrap_or(0);
    (first, last)
}

fn event_ts(ev: &Event) -> u128 {
    match ev {
        Event::AiChat { ts_ms, .. } => *ts_ms,
        Event::WebFetch { ts_ms, .. } => *ts_ms,
        Event::Note { ts_ms, .. } => *ts_ms,
    }
}

/// 统计信息
#[derive(Clone, Debug)]
pub struct RecordingStats {
    pub total_events: usize,
    pub ai_chat_count: usize,
    pub web_fetch_count: usize,
    pub note_count: usize,
    pub error_count: usize,
    pub total_tokens_in: usize,
    pub total_tokens_out: usize,
    pub total_latency_ms: u128,
    pub min_latency_ms: u128,
    pub max_latency_ms: u128,
    pub models: Vec<String>,
    pub duration_ms: u128, // 首尾事件时间差
}

/// 计算录制的统计信息
pub fn compute_stats(events: &[Event]) -> RecordingStats {
    let mut stats = RecordingStats {
        total_events: events.len(),
        ai_chat_count: 0,
        web_fetch_count: 0,
        note_count: 0,
        error_count: 0,
        total_tokens_in: 0,
        total_tokens_out: 0,
        total_latency_ms: 0,
        min_latency_ms: u128::MAX,
        max_latency_ms: 0,
        models: Vec::new(),
        duration_ms: 0,
    };
    let mut model_set = std::collections::HashSet::new();
    for ev in events {
        match ev {
            Event::AiChat {
                model,
                tokens_in,
                tokens_out,
                latency_ms,
                error,
                ..
            } => {
                stats.ai_chat_count += 1;
                stats.total_tokens_in += tokens_in;
                stats.total_tokens_out += tokens_out;
                stats.total_latency_ms += latency_ms;
                stats.min_latency_ms = stats.min_latency_ms.min(*latency_ms);
                stats.max_latency_ms = stats.max_latency_ms.max(*latency_ms);
                model_set.insert(model.clone());
                if error.is_some() {
                    stats.error_count += 1;
                }
            }
            Event::WebFetch {
                latency_ms, error, ..
            } => {
                stats.web_fetch_count += 1;
                stats.total_latency_ms += latency_ms;
                stats.min_latency_ms = stats.min_latency_ms.min(*latency_ms);
                stats.max_latency_ms = stats.max_latency_ms.max(*latency_ms);
                if error.is_some() {
                    stats.error_count += 1;
                }
            }
            Event::Note { .. } => {
                stats.note_count += 1;
            }
        }
    }
    stats.models = model_set.into_iter().collect();
    stats.models.sort();
    if stats.min_latency_ms == u128::MAX {
        stats.min_latency_ms = 0;
    }
    // 首尾时间差
    if let (Some(first), Some(last)) = (events.first(), events.last()) {
        stats.duration_ms = event_ts(last).saturating_sub(event_ts(first));
    }
    stats
}

/// Timeline 行: 一行一调用
#[derive(Clone, Debug)]
pub struct TimelineRow {
    pub seq: usize,
    pub kind: String,
    pub detail: String,
    pub tokens: String,
    pub latency_ms: u128,
    pub status: String,
}

/// 生成 timeline 行
pub fn build_timeline(events: &[Event]) -> Vec<TimelineRow> {
    events
        .iter()
        .enumerate()
        .map(|(i, ev)| match ev {
            Event::AiChat {
                model,
                tokens_in,
                tokens_out,
                latency_ms,
                response,
                error,
                ..
            } => {
                let status = if let Some(e) = error {
                    format!("ERR:{}", &e[..e.len().min(30)])
                } else {
                    "ok".to_string()
                };
                let resp_preview: String = response.chars().take(40).collect();
                TimelineRow {
                    seq: i + 1,
                    kind: "ai.chat".to_string(),
                    detail: format!("{} → {:?}", model, resp_preview),
                    tokens: format!("{}+{}", tokens_in, tokens_out),
                    latency_ms: *latency_ms,
                    status,
                }
            }
            Event::WebFetch {
                url,
                method,
                status: s,
                latency_ms,
                error,
                ..
            } => {
                let status = if let Some(e) = error {
                    format!("ERR:{}", &e[..e.len().min(30)])
                } else {
                    s.to_string()
                };
                let url_short: String = url.chars().take(50).collect();
                TimelineRow {
                    seq: i + 1,
                    kind: "web.fetch".to_string(),
                    detail: format!("{} {}", method, url_short),
                    tokens: "-".to_string(),
                    latency_ms: *latency_ms,
                    status,
                }
            }
            Event::Note { message, .. } => {
                let msg_preview: String = message.chars().take(50).collect();
                TimelineRow {
                    seq: i + 1,
                    kind: "note".to_string(),
                    detail: msg_preview,
                    tokens: "-".to_string(),
                    latency_ms: 0,
                    status: "-".to_string(),
                }
            }
        })
        .collect()
}

/// 导出格式
#[derive(Clone, Debug)]
pub enum ExportFormat {
    /// 完整 JSONL (默认已脱敏)
    Jsonl,
    /// Markdown 报告
    Markdown,
}

/// 导出录制到字符串
pub fn export_recording(events: &[Event], format: &ExportFormat, name: &str) -> String {
    match format {
        ExportFormat::Jsonl => {
            let mut out = String::new();
            for ev in events {
                out.push_str(&event_to_jsonl(ev));
                out.push('\n');
            }
            out
        }
        ExportFormat::Markdown => export_markdown(events, name),
    }
}

fn export_markdown(events: &[Event], name: &str) -> String {
    let stats = compute_stats(events);
    let mut md = String::new();
    md.push_str(&format!("# Recording: {}\n\n", name));
    md.push_str("## Summary\n\n");
    md.push_str(&format!("- Events: {}\n", stats.total_events));
    md.push_str(&format!("- AI calls: {}\n", stats.ai_chat_count));
    md.push_str(&format!("- Web calls: {}\n", stats.web_fetch_count));
    md.push_str(&format!("- Errors: {}\n", stats.error_count));
    md.push_str(&format!(
        "- Tokens: {} in + {} out\n",
        stats.total_tokens_in, stats.total_tokens_out
    ));
    md.push_str(&format!("- Duration: {}ms\n\n", stats.duration_ms));

    md.push_str("## Timeline\n\n");
    md.push_str("| # | Kind | Detail | Tokens | Latency | Status |\n");
    md.push_str("|---|------|--------|--------|---------|--------|\n");
    let rows = build_timeline(events);
    for row in &rows {
        let detail = if row.detail.len() > 40 {
            format!("{}…", &row.detail[..39])
        } else {
            row.detail.clone()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}ms | {} |\n",
            row.seq, row.kind, detail, row.tokens, row.latency_ms, row.status
        ));
    }
    md
}
