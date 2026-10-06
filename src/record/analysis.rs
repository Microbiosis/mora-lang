//! v0.25: record 分析 — 录制列表/统计（list_recordings/compute_stats 等）。

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
            // v0.104.6 D184：事件数改用**解析出的**事件数，不再用 `count_lines`
            // （原始行数）。
            //
            // 修前 `list` 与 `stats` 对同一个文件给出**两个数** —— 实测一份
            // 首行被截断的录像：
            //
            // ```text
            // $ mora record list      →  r2 … EVENTS 3
            // $ mora record stats r2  →  [warn] 1 of 3 line(s) could not be parsed…
            //                           →  Events: 2 total
            // ```
            //
            // 且 `list` 是 D178 之后**唯一**仍对跳过行沉默的消费��。
            //
            // ⚠ 零额外成本：`load_time_range`（原第 28 行）**本来就**调用
            // `load_jsonl` 把整个文件解析一遍再丢掉除时间戳外的一切。
            // 现在从**同一次**加载里顺带取事件数与跳过行 —— 解析次数不变。
            let (event_count, first_ts, last_ts, skipped) = load_summary(&path);
            if !skipped.is_empty() {
                eprintln!(
                    "[warn] {}: {}/{} line(s) could not be parsed and were SKIPPED — \
                     the EVENTS column below counts only the {} readable one(s).",
                    path.display(),
                    skipped.len(),
                    event_count + skipped.len(),
                    event_count
                );
            }
            // v0.104.6 D224：另存**文件 mtime**。此前 `list` 的
            // 「LAST MODIFIED」列与排序键都用 `last_ts_ms`（最后一条**事件**的
            // 时间戳），与表头语义不符：
            //   实测一份**刚写**的录制（事件 ts 指向 2023-11）显示 `1053d ago`，
            //   且排序把「内容时间最新」当成「最近录制」。
            // 事件时间来自被录的程序，与「这份录制什么时候存在」是**两件事**
            // （跨机器、导入旧录制、时钟偏移都会让两者分叉）。
            let modified_ms: u128 = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            infos.push(RecordingInfo {
                name,
                path,
                size_bytes,
                event_count,
                first_ts_ms: first_ts,
                last_ts_ms: last_ts,
                modified_ms,
            });
        }
    }
    // v0.104.6 D224：按**文件 mtime** 排（表头说的是「最近修改」）
    infos.sort_by_key(|b| std::cmp::Reverse(b.modified_ms));
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
    /// v0.104.6 D224：**文件**最后修改时间（epoch ms）。
    /// 与 `last_ts_ms`（最后一条**事件**的时间戳）是两件事。
    pub modified_ms: u128,
}

/// 一次加载同时拿到：可读事件数 / 首末时间戳 / 跳过的行。
///
/// v0.104.6 D184。原先 `list_recordings` 做**两次**独立工作 ——
/// `count_lines`（原始行数）与 `load_time_range`（整文件解析后只留时间戳）——
/// 前者与后者对「事件数」给出**不同答案**。现在合并成一次，
/// 两者对同一文件**必然一致**。
fn load_summary(path: &Path) -> (usize, u128, u128, Vec<super::SkippedLine>) {
    match load_jsonl(path) {
        Ok((events, skipped)) => {
            let first = events.first().map(event_ts).unwrap_or(0);
            let last = events.last().map(event_ts).unwrap_or(0);
            (events.len(), first, last, skipped)
        }
        Err(_) => (0, 0, 0, Vec::new()),
    }
}

fn event_ts(ev: &Event) -> u128 {
    match ev {
        Event::AiChat { ts_ms, .. } => *ts_ms,
        Event::WebFetch { ts_ms, .. } => *ts_ms,
        Event::Note { ts_ms, .. } => *ts_ms,
        // v0.83: 新增 Msg + StateMutation 变体也支持 ts 提取
        Event::Msg { ts_ms, .. } => *ts_ms,
        Event::StateMutation { ts_ms, .. } => *ts_ms,
    }
}

/// 统计信息
#[derive(Clone, Debug)]
pub struct RecordingStats {
    pub total_events: usize,
    pub ai_chat_count: usize,
    pub web_fetch_count: usize,
    pub note_count: usize,
    /// v0.104.6 D225：补齐**其余两类**事件的计数。
    ///
    /// 修前 `Events: N total` 下面只列 `ai.chat` / `web.fetch` / `notes`
    /// 三行，而 `Event` 有**五个**变体 —— `Msg` 与 `StateMutation`
    /// 被计入 `total_events` 却不显示在任何一行：
    ///
    /// ```text
    /// $ mora record stats baseline     # 18 条全是 state_mutation
    /// Events:        18 total
    ///   ai.chat:     0
    ///   web.fetch:   0
    ///   notes:       0          ← 0+0+0 ≠ 18，且零提示
    /// ```
    ///
    /// 分解读起来像**穷尽**分类，实则不是 —— 用户据此判断「录了 18 次调用
    /// 却一次都没成功」之类，是**误导**。加上这两行后子类之和恒等于 total。
    pub msg_count: usize,
    pub state_mutation_count: usize,
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
        msg_count: 0,
        state_mutation_count: 0,
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
            // v0.83: Msg + StateMutation 不进 latency/tokens 统计
            // v0.104.6 D225：但**必须计数** —— 否则 `total_events` 与子类之和对不上。
            Event::Msg { .. } => stats.msg_count += 1,
            Event::StateMutation { .. } => stats.state_mutation_count += 1,
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
            // v0.83: Msg + StateMutation 简化为 timeline 行
            Event::Msg { channel, .. } => TimelineRow {
                seq: i + 1,
                kind: "msg".to_string(),
                detail: channel.clone(),
                tokens: "-".to_string(),
                latency_ms: 0,
                status: "-".to_string(),
            },
            Event::StateMutation { var, .. } => TimelineRow {
                seq: i + 1,
                kind: "state_mutation".to_string(),
                detail: var.clone(),
                tokens: "-".to_string(),
                latency_ms: 0,
                status: "-".to_string(),
            },
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
    // v0.104.6 D225：markdown 报告此前连 `notes` 都没有，分解更不穷尽。
    md.push_str(&format!("- Notes: {}\n", stats.note_count));
    md.push_str(&format!("- Messages: {}\n", stats.msg_count));
    md.push_str(&format!(
        "- State mutations: {}\n",
        stats.state_mutation_count
    ));
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
        // v0.104.6 D179：原为 `row.detail.len() > 40` + `&row.detail[..39]`
        // （字节数判断 + 字节数切片）→ 中文内容必 panic。改走
        // `truncate_display`（按字符）。
        let detail = truncate_display(&row.detail, 40);
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}ms | {} |\n",
            row.seq, row.kind, detail, row.tokens, row.latency_ms, row.status
        ));
    }
    md
}
