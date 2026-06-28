//! Mora v0.14: 录制 / 重放 / 对比 —— AI agent 飞行记录仪
//!
//! 受 [FlightBox](https://github.com/he-yufeng/FlightBox) 启发:
//! 当 AI agent 失败时,证据应被完整、结构化、可重放地捕获。
//!
//! 三种模式:
//! - `Off`      —— 不录制 (默认)
//! - `Record`   —— 录制 ai.chat / web.fetch 到 JSONL
//! - `Replay`   —— 重放已录制响应 (deterministic)
//!
//! 存储格式: JSONL (`.mora/recordings/<name>.jsonl`),每行一个 Event。
//!
//! Example:
//!   $ mora record script.mora demo-001
//!   $ mora replay script.mora demo-001
//!   $ mora diff demo-001 demo-002

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Recorder 运行模式
#[derive(Clone, Debug)]
pub enum Mode {
    /// 不录制也不重放 (默认)
    Off,
    /// 录制所有事件到 `path` (JSONL)
    Record(PathBuf),
    /// 从 `path` 重放,匹配 (kind, key) 返回录制响应
    Replay(PathBuf),
}

impl Mode {
    pub fn is_off(&self) -> bool {
        matches!(self, Mode::Off)
    }
    pub fn is_record(&self) -> bool {
        matches!(self, Mode::Record(_))
    }
    pub fn is_replay(&self) -> bool {
        matches!(self, Mode::Replay(_))
    }
}

/// 单个录制事件 —— JSONL 一行
#[derive(Clone, Debug)]
pub enum Event {
    /// ai.chat 调用
    AiChat {
        id: u64,
        ts_ms: u128,
        model: String,
        prompt_hash: String,
        prompt_preview: String,
        response: String,
        tokens_in: usize,
        tokens_out: usize,
        latency_ms: u128,
        error: Option<String>,
    },
    /// web.fetch 调用
    WebFetch {
        id: u64,
        ts_ms: u128,
        url: String,
        method: String,
        status: u16,
        body_len: usize,
        latency_ms: u128,
        error: Option<String>,
    },
    /// 用户/系统 note
    Note {
        id: u64,
        ts_ms: u128,
        message: String,
    },
}

/// 重放时匹配的响应
#[derive(Clone, Debug)]
pub struct RecordedResponse {
    pub response: String,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub latency_ms: u128,
    pub status: Option<u16>,     // for web.fetch
    pub body_len: Option<usize>, // for web.fetch
}

/// Recorder 主结构 —— 持有 mode + 累积事件 + 索引 (重放用)
pub struct Recorder {
    mode: Mode,
    events: Vec<Event>,
    next_id: u64,
    // 重放时按 (kind, key) → 第一个匹配的响应
    // kind: "ai.chat" | "web.fetch"
    // key: model+prompt_hash (ai) 或 url (web)
    index: HashMap<(String, String), RecordedResponse>,
}

impl Recorder {
    pub fn new_off() -> Self {
        Self {
            mode: Mode::Off,
            events: Vec::new(),
            next_id: 1,
            index: HashMap::new(),
        }
    }

    pub fn new_record(path: PathBuf) -> Result<Self, String> {
        // 确保父目录存在
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent).map_err(|e| {
                format!("recorder: failed to create dir {}: {}", parent.display(), e)
            })?;
        }
        Ok(Self {
            mode: Mode::Record(path),
            events: Vec::new(),
            next_id: 1,
            index: HashMap::new(),
        })
    }

    pub fn new_replay(path: PathBuf) -> Result<Self, String> {
        let events = load_jsonl(&path)?;
        let mut index = HashMap::new();
        for ev in &events {
            if let Some((kind, key, resp)) = event_to_replay_entry(ev) {
                index.entry((kind, key)).or_insert(resp);
            }
        }
        Ok(Self {
            mode: Mode::Replay(path),
            events,
            next_id: 0,
            index,
        })
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// 计算当前时间戳 (ms since epoch)
    pub fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// 录制一个事件 (Record 模式)
    pub fn record(&mut self, event: Event) {
        if !self.mode.is_record() {
            return;
        }
        self.events.push(event);
    }

    /// 重放: 查找 ai.chat 的录制响应
    pub fn lookup_ai_chat(&self, model: &str, prompt: &str) -> Option<RecordedResponse> {
        if !self.mode.is_replay() {
            return None;
        }
        let key = format!("{}|{}", model, hash_prompt(prompt));
        self.index.get(&("ai.chat".to_string(), key)).cloned()
    }

    /// 重放: 查找 web.fetch 的录制响应
    pub fn lookup_web_fetch(&self, url: &str) -> Option<RecordedResponse> {
        if !self.mode.is_replay() {
            return None;
        }
        self.index
            .get(&("web.fetch".to_string(), url.to_string()))
            .cloned()
    }

    /// 录制模式: 把累积事件 flush 到 JSONL 文件
    /// v0.22: 支持压缩存储（.jsonl.gz）
    pub fn save(&self) -> Result<(), String> {
        let path = match &self.mode {
            Mode::Record(p) => p,
            _ => return Ok(()),
        };
        let mut out = String::new();
        for ev in &self.events {
            out.push_str(&event_to_jsonl(ev));
            out.push('\n');
        }

        // v0.22: 压缩存储 - 如果文件名以 .gz 结尾，使用 gzip 压缩
        if path.extension().map(|e| e == "gz").unwrap_or(false) {
            use std::io::Write;
            let file = fs::File::create(path)
                .map_err(|e| format!("recorder: failed to create {}: {}", path.display(), e))?;
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            encoder.write_all(out.as_bytes())
                .map_err(|e| format!("recorder: failed to compress: {}", e))?;
            encoder.finish()
                .map_err(|e| format!("recorder: failed to finish compression: {}", e))?;
        } else {
            fs::write(path, out)
                .map_err(|e| format!("recorder: failed to write {}: {}", path.display(), e))?
        }
        Ok(())
    }

    pub fn next_event_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 便利: 录制 ai.chat 事件
    #[allow(clippy::too_many_arguments)]
    pub fn record_ai_chat(
        &mut self,
        model: String,
        prompt: String,
        response: String,
        tokens_in: usize,
        tokens_out: usize,
        latency_ms: u128,
        error: Option<String>,
    ) {
        if !self.mode.is_record() {
            return;
        }
        let id = self.next_event_id();
        let prompt_hash = hash_prompt(&prompt);
        let prompt_preview: String = prompt.chars().take(120).collect();
        self.events.push(Event::AiChat {
            id,
            ts_ms: Self::now_ms(),
            model,
            prompt_hash,
            prompt_preview,
            response,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
        });
    }

    /// 便利: 录制 web.fetch 事件
    pub fn record_web_fetch(
        &mut self,
        url: String,
        method: String,
        status: u16,
        body_len: usize,
        latency_ms: u128,
        error: Option<String>,
    ) {
        if !self.mode.is_record() {
            return;
        }
        let id = self.next_event_id();
        self.events.push(Event::WebFetch {
            id,
            ts_ms: Self::now_ms(),
            url,
            method,
            status,
            body_len,
            latency_ms,
            error,
        });
    }

    /// 便利: 录制 note
    pub fn record_note(&mut self, message: String) {
        if !self.mode.is_record() {
            return;
        }
        let id = self.next_event_id();
        self.events.push(Event::Note {
            id,
            ts_ms: Self::now_ms(),
            message,
        });
    }
}

/// 简单 prompt hash (FNV-1a 64-bit, hex)
pub fn hash_prompt(prompt: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in prompt.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Event → JSONL 字符串 (单行)
fn event_to_jsonl(ev: &Event) -> String {
    match ev {
        Event::AiChat {
            id,
            ts_ms,
            model,
            prompt_hash,
            prompt_preview,
            response,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
        } => {
            let mut s = format!(
                r#"{{"kind":"ai.chat","id":{},"ts_ms":{},"model":"{}","prompt_hash":"{}","prompt_preview":"{}","response":"{}","tokens_in":{},"tokens_out":{},"latency_ms":{}"#,
                id,
                ts_ms,
                esc(model),
                prompt_hash,
                esc(prompt_preview),
                esc(response),
                tokens_in,
                tokens_out,
                latency_ms
            );
            if let Some(e) = error {
                s.push_str(&format!(r#","error":"{}""#, esc(e)));
            }
            s.push('}');
            s
        }
        Event::WebFetch {
            id,
            ts_ms,
            url,
            method,
            status,
            body_len,
            latency_ms,
            error,
        } => {
            let mut s = format!(
                r#"{{"kind":"web.fetch","id":{},"ts_ms":{},"url":"{}","method":"{}","status":{},"body_len":{},"latency_ms":{}"#,
                id,
                ts_ms,
                esc(url),
                method,
                status,
                body_len,
                latency_ms
            );
            if let Some(e) = error {
                s.push_str(&format!(r#","error":"{}""#, esc(e)));
            }
            s.push('}');
            s
        }
        Event::Note { id, ts_ms, message } => {
            format!(
                r#"{{"kind":"note","id":{},"ts_ms":{},"message":"{}"}}"#,
                id,
                ts_ms,
                esc(message)
            )
        }
    }
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Event → (kind, key, RecordedResponse) 用于 replay 索引
fn event_to_replay_entry(ev: &Event) -> Option<(String, String, RecordedResponse)> {
    match ev {
        Event::AiChat {
            model,
            prompt_hash,
            response,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
            ..
        } => {
            if error.is_some() {
                return None;
            } // 重放不重放错误
            Some((
                "ai.chat".to_string(),
                format!("{}|{}", model, prompt_hash),
                RecordedResponse {
                    response: response.clone(),
                    tokens_in: *tokens_in,
                    tokens_out: *tokens_out,
                    latency_ms: *latency_ms,
                    status: None,
                    body_len: None,
                },
            ))
        }
        Event::WebFetch {
            url,
            status,
            body_len,
            latency_ms,
            error,
            ..
        } => {
            if error.is_some() {
                return None;
            }
            Some((
                "web.fetch".to_string(),
                url.clone(),
                RecordedResponse {
                    response: String::new(),
                    tokens_in: 0,
                    tokens_out: 0,
                    latency_ms: *latency_ms,
                    status: Some(*status),
                    body_len: Some(*body_len),
                },
            ))
        }
        Event::Note { .. } => None,
    }
}

/// 从 JSONL 文件加载事件 (简化解析: 因为我们写的格式固定, 用字符串匹配)
fn load_jsonl(path: &Path) -> Result<Vec<Event>, String> {
    if !path.exists() {
        return Err(format!(
            "recorder: recording not found at {} (run `mora record <file> <name>` first)",
            path.display()
        ));
    }
    let file = fs::File::open(path)
        .map_err(|e| format!("recorder: failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("recorder: read error at line {}: {}", idx + 1, e))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // v0.14: 暂用简化 JSON 解析, 提取关键字段
        if let Some(ev) = parse_event_line(trimmed) {
            events.push(ev);
        }
        // 解析失败的行跳过 (前向兼容)
    }
    Ok(events)
}

/// 极简 JSON 行解析 —— 因为我们的输出格式固定
/// 支持 kind / id / ts_ms / model / prompt_hash / response / tokens_in/out / latency_ms / error / url / method / status / body_len / message
fn parse_event_line(line: &str) -> Option<Event> {
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }
    let inner = &line[1..line.len() - 1];
    let mut fields: HashMap<String, String> = HashMap::new();
    // 简易解析: 按 "," 分割但尊重引号
    let chars: Vec<char> = inner.chars().collect();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    let mut parts = Vec::new();
    let mut idx = 0;
    while idx < chars.len() {
        let c = chars[idx];
        idx += 1;
        if escape {
            current.push(c);
            escape = false;
            continue;
        }
        if c == '\\' && in_string {
            escape = true;
            current.push(c);
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            current.push(c);
            continue;
        }
        if c == ',' && !in_string {
            parts.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    for part in parts {
        if let Some(idx) = part.find(':') {
            let key = part[..idx].trim().trim_matches('"').to_string();
            let val = part[idx + 1..].trim().to_string();
            fields.insert(key, unquote(&val));
        }
    }
    let kind = fields.get("kind")?.as_str();
    match kind {
        "ai.chat" => Some(Event::AiChat {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            model: fields.get("model").cloned().unwrap_or_default(),
            prompt_hash: fields.get("prompt_hash").cloned().unwrap_or_default(),
            prompt_preview: fields.get("prompt_preview").cloned().unwrap_or_default(),
            response: fields.get("response").cloned().unwrap_or_default(),
            tokens_in: fields
                .get("tokens_in")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            tokens_out: fields
                .get("tokens_out")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            latency_ms: fields
                .get("latency_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            error: fields.get("error").cloned(),
        }),
        "web.fetch" => Some(Event::WebFetch {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            url: fields.get("url").cloned().unwrap_or_default(),
            method: fields.get("method").cloned().unwrap_or_default(),
            status: fields
                .get("status")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            body_len: fields
                .get("body_len")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            latency_ms: fields
                .get("latency_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            error: fields.get("error").cloned(),
        }),
        "note" => Some(Event::Note {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            message: fields.get("message").cloned().unwrap_or_default(),
        }),
        _ => None,
    }
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => {
                        out.push('\\');
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}

/// 对比两个 recording: 输出事件级别的 diff
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

/// 录制目录下的所有 .jsonl 文件列表
pub fn list_recordings(dir: &Path) -> Result<Vec<RecordingInfo>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("list: failed to read {}: {}", dir.display(), e))?;
    let mut infos = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("list: read_dir error: {}", e))?;
        let path = entry.path();
        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("list: metadata error: {}", e))?;
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
    Ok(reader.lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).count())
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
                latency_ms,
                error,
                ..
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
    md.push_str(&format!("- Tokens: {} in + {} out\n", stats.total_tokens_in, stats.total_tokens_out));
    md.push_str(&format!("- Duration: {}ms\n\n", stats.duration_ms));

    md.push_str("## Timeline\n\n");
    md.push_str("| # | Kind | Detail | Tokens | Latency | Status |\n");
    md.push_str("|---|------|--------|--------|---------|--------|\n");
    let rows = build_timeline(events);
    for row in &rows {
        let detail = if row.detail.len() > 40 { format!("{}…", &row.detail[..39]) } else { row.detail.clone() };
        md.push_str(&format!("| {} | {} | {} | {} | {}ms | {} |\n",
            row.seq, row.kind, detail, row.tokens, row.latency_ms, row.status));
    }
    md
}

/// 脱敏: 替换常见 secret 模式 (无 regex 依赖)
pub fn redact_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        // 检查 sk-xxx, key-xxx 模式
        if i + 3 <= len {
            let prefix: String = chars[i..i+3.min(len - i)].iter().collect();
            if (prefix == "sk-" || prefix == "ke") && i + 4 <= len {
                // key-
                let longer: String = chars[i..i+4.min(len - i)].iter().collect();
                if prefix == "sk-" || longer == "key-" {
                    // 找到前缀，收集后续字母数字
                    let start = i;
                    let p = if prefix == "sk-" { "sk-" } else { "key-" };
                    i += p.len();
                    while i < len && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-') {
                        i += 1;
                    }
                    if i - start > 20 {
                        // 长度足够，视为 secret
                        out.push_str("<REDACTED>");
                        continue;
                    } else {
                        // 不够长，原样输出
                        for c in &chars[start..i] {
                            out.push(*c);
                        }
                        continue;
                    }
                }
            }
        }
        // 检查 Bearer xxx
        if i + 7 <= len {
            let bearer: String = chars[i..i+7].iter().collect();
            if bearer == "Bearer " {
                let start = i;
                i += 7;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-' || chars[i] == '.') {
                    i += 1;
                }
                if i - start > 27 { // "Bearer " (7) + 20+ chars
                    out.push_str("Bearer <REDACTED>");
                    continue;
                } else {
                    for c in &chars[start..i] {
                        out.push(*c);
                    }
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Audit 发现项
#[derive(Clone, Debug)]
pub struct AuditFinding {
    pub event_id: u64,
    pub field: String,
    pub pattern: String,
    pub preview: String, // 脱敏后的预览
}

/// .moraignore 策略条目
#[derive(Clone, Debug)]
pub enum IgnoreRule {
    /// 忽略整个顶层字段: field:token_usage
    Field(String),
    /// 忽略 JSON 路径: path:request.messages.*.content
    Path(String),
    /// 按名称禁用模式: pattern:github-token
    Pattern(String),
}

/// 解析 .moraignore 文件
pub fn parse_moraignore(content: &str) -> Vec<IgnoreRule> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            if let Some(field) = line.strip_prefix("field:") {
                Some(IgnoreRule::Field(field.trim().to_string()))
            } else if let Some(path) = line.strip_prefix("path:") {
                Some(IgnoreRule::Path(path.trim().to_string()))
            } else { line.strip_prefix("pattern:").map(|pat| IgnoreRule::Pattern(pat.trim().to_string())) }
        })
        .collect()
}

/// 审计单个事件的 JSON 字符串
fn audit_json_value(event_id: u64, field: &str, value: &str, findings: &mut Vec<AuditFinding>) {
    // 检查常见 secret 模式
    let patterns = [
        ("sk-[a-zA-Z0-9]{20,}", "openai-api-key"),
        ("key-[a-zA-Z0-9]{20,}", "generic-api-key"),
        ("Bearer [a-zA-Z0-9_\\-\\.]{20,}", "bearer-token"),
        ("ghp_[a-zA-Z0-9]{36}", "github-pat"),
        ("gho_[a-zA-Z0-9]{36}", "github-oauth"),
        ("xoxb-[a-zA-Z0-9\\-]+", "slack-bot-token"),
        ("xoxp-[a-zA-Z0-9\\-]+", "slack-user-token"),
    ];

    for (_pattern, name) in &patterns {
        // 简单匹配: 检查值中是否包含模式前缀
        let prefix = if name.contains("sk-") || name == &"openai-api-key" {
            "sk-"
        } else if name.contains("key-") || name == &"generic-api-key" {
            "key-"
        } else if name.contains("Bearer") {
            "Bearer "
        } else if name.contains("ghp_") {
            "ghp_"
        } else if name.contains("gho_") {
            "gho_"
        } else if name.contains("xoxb") {
            "xoxb-"
        } else if name.contains("xoxp") {
            "xoxp-"
        } else {
            continue;
        };

        if let Some(pos) = value.find(prefix) {
            // 计算 token 长度
            let token_start = pos + prefix.len();
            let token_len = value[token_start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .count();
            if token_len >= 20 {
                let preview = format!("{}{}", prefix, &value[token_start..token_start + 5.min(token_len)]);
                findings.push(AuditFinding {
                    event_id,
                    field: field.to_string(),
                    pattern: name.to_string(),
                    preview: format!("{}...", preview),
                });
            }
        }
    }
}

/// 审计整个录制
pub fn audit_recording(events: &[Event], ignore_rules: &[IgnoreRule]) -> Vec<AuditFinding> {
    let mut findings = Vec::new();
    for ev in events {
        let (id, json_str) = match ev {
            Event::AiChat { id, response, prompt_preview, .. } => {
                (*id, format!("{} {}", prompt_preview, response))
            }
            Event::WebFetch { id, url, .. } => (*id, url.clone()),
            Event::Note { id, message, .. } => (*id, message.clone()),
        };
        // 检查是否被忽略
        let should_ignore = ignore_rules.iter().any(|rule| match rule {
            IgnoreRule::Field(f) => f == "response" || f == "prompt_preview",
            IgnoreRule::Pattern(p) => json_str.contains(p.as_str()),
            _ => false,
        });
        if !should_ignore {
            audit_json_value(id, "content", &json_str, &mut findings);
        }
    }
    findings
}

/// 生成红线报告 (Markdown)
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
    md.push_str(&format!("| Tokens (in+out) | {}+{}={} |\n",
        stats.total_tokens_in, stats.total_tokens_out,
        stats.total_tokens_in + stats.total_tokens_out));
    md.push_str(&format!("| Duration | {}ms |\n", stats.duration_ms));
    md.push('\n');

    // Audit
    md.push_str("## Audit\n\n");
    if findings.is_empty() {
        md.push_str("✓ No secrets detected.\n\n");
    } else {
        md.push_str(&format!("⚠ {} potential secret(s) found:\n\n", findings.len()));
        md.push_str("| Event | Field | Pattern | Preview |\n|-------|-------|---------|----------|\n");
        for f in &findings {
            md.push_str(&format!("| {} | {} | {} | {} |\n", f.event_id, f.field, f.pattern, f.preview));
        }
        md.push('\n');
    }

    // Timeline
    md.push_str("## Timeline\n\n");
    md.push_str("| # | Kind | Detail | Tokens | Lat(ms) | Status |\n");
    md.push_str("|---|------|--------|--------|---------|--------|\n");
    for row in &build_timeline(events) {
        let detail = if row.detail.len() > 50 { format!("{}…", &row.detail[..49]) } else { row.detail.clone() };
        md.push_str(&format!("| {} | {} | {} | {} | {} | {} |\n",
            row.seq, row.kind, detail, row.tokens, row.latency_ms, row.status));
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
    pub key: String,      // model+prompt_hash 或 url
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub has_error: bool,
}

fn event_to_summary(ev: &Event) -> EventSummary {
    match ev {
        Event::AiChat { model, prompt_hash, tokens_in, tokens_out, error, .. } => EventSummary {
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
    let mut out = format!("{{\"name\":\"{}\",\"created_ms\":{}}}\n", esc(&snap.name), snap.created_ms);
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
    if lines.is_empty() { return None; }
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
        summaries.push(EventSummary { kind, key, tokens_in, tokens_out, has_error });
    }
    Some(SnapshotBaseline { name, event_summaries: summaries, created_ms })
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
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
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
    EventChanged { index: usize, expected: EventSummary, actual: EventSummary },
    /// 新增事件
    EventAdded { index: usize, actual: EventSummary },
    /// 缺失事件
    EventMissing { index: usize, expected: EventSummary },
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
                diffs.push(SnapshotDiff::EventMissing { index: i, expected: expected.clone() });
            }
            (None, Some(actual)) => {
                diffs.push(SnapshotDiff::EventAdded { index: i, actual: actual.clone() });
            }
            (None, None) => {}
        }
    }
    diffs
}

// ===================================================================
// Tests
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!(
            "mora_record_test_{}_{}.jsonl",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    fn recorder_off_is_noop() {
        let mut r = Recorder::new_off();
        assert!(r.mode().is_off());
        r.record_ai_chat(
            "gpt-4o".to_string(),
            "hi".to_string(),
            "hello".to_string(),
            1,
            1,
            100,
            None,
        );
        assert_eq!(r.events().len(), 0); // off 模式不录制
    }

    #[test]
    fn record_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = fs::remove_file(&path);

        let mut r = Recorder::new_record(path.clone()).unwrap();
        assert!(r.mode().is_record());
        r.record_ai_chat(
            "gpt-4o".to_string(),
            "hello".to_string(),
            "world".to_string(),
            5,
            7,
            123,
            None,
        );
        r.record_web_fetch(
            "https://example.com/api".to_string(),
            "GET".to_string(),
            200,
            1024,
            45,
            None,
        );
        r.record_note("test note".to_string());
        r.save().unwrap();

        // load + replay
        let r2 = Recorder::new_replay(path.clone()).unwrap();
        assert!(r2.mode().is_replay());
        assert_eq!(r2.events().len(), 3);
        // lookup ai.chat
        let resp = r2.lookup_ai_chat("gpt-4o", "hello");
        assert!(resp.is_some());
        let resp = resp.unwrap();
        assert_eq!(resp.response, "world");
        assert_eq!(resp.tokens_in, 5);
        // lookup web.fetch
        let wresp = r2.lookup_web_fetch("https://example.com/api");
        assert!(wresp.is_some());
        let wresp = wresp.unwrap();
        assert_eq!(wresp.status, Some(200));
        assert_eq!(wresp.body_len, Some(1024));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn replay_missing_returns_none() {
        let path = tmp_path("missing");
        let _ = fs::remove_file(&path);
        let mut r = Recorder::new_record(path.clone()).unwrap();
        r.record_ai_chat(
            "gpt-4o".to_string(),
            "first".to_string(),
            "one".to_string(),
            1,
            1,
            50,
            None,
        );
        r.save().unwrap();

        let r2 = Recorder::new_replay(path.clone()).unwrap();
        // 询问不同 prompt → 找不到
        let resp = r2.lookup_ai_chat("gpt-4o", "second");
        assert!(resp.is_none());
        // 询问不同 model → 找不到
        let resp = r2.lookup_ai_chat("gpt-4o-mini", "first");
        assert!(resp.is_none());
        // 询问 web.fetch 不存在 url
        let resp = r2.lookup_web_fetch("https://nope.com");
        assert!(resp.is_none());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn hash_prompt_deterministic() {
        assert_eq!(hash_prompt("hello"), hash_prompt("hello"));
        assert_ne!(hash_prompt("hello"), hash_prompt("world"));
        assert_eq!(hash_prompt("hello").len(), 16); // 64-bit hex = 16 chars
    }

    #[test]
    fn diff_identical_recordings() {
        let path_a = tmp_path("diff_a");
        let path_b = tmp_path("diff_b");
        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);

        let mut a = Recorder::new_record(path_a.clone()).unwrap();
        a.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
        a.save().unwrap();

        let mut b = Recorder::new_record(path_b.clone()).unwrap();
        b.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
        b.save().unwrap();

        let ra = Recorder::new_replay(path_a.clone()).unwrap();
        let rb = Recorder::new_replay(path_b.clone()).unwrap();
        let diff = diff_recordings(ra.events(), rb.events());
        assert_eq!(diff.len(), 1);
        assert!(matches!(diff[0], DiffLine::Identical(1, _)));

        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);
    }

    #[test]
    fn diff_changed_response() {
        let path_a = tmp_path("diff_chg_a");
        let path_b = tmp_path("diff_chg_b");
        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);

        let mut a = Recorder::new_record(path_a.clone()).unwrap();
        a.record_ai_chat(
            "m".into(),
            "p".into(),
            "old response".into(),
            1,
            1,
            10,
            None,
        );
        a.save().unwrap();

        let mut b = Recorder::new_record(path_b.clone()).unwrap();
        b.record_ai_chat(
            "m".into(),
            "p".into(),
            "new response longer".into(),
            2,
            2,
            20,
            None,
        );
        b.save().unwrap();

        let ra = Recorder::new_replay(path_a.clone()).unwrap();
        let rb = Recorder::new_replay(path_b.clone()).unwrap();
        let diff = diff_recordings(ra.events(), rb.events());
        assert_eq!(diff.len(), 1);
        assert!(matches!(diff[0], DiffLine::Changed(1, _, _)));

        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);
    }

    #[test]
    fn diff_only_in_b() {
        let path_a = tmp_path("only_a");
        let path_b = tmp_path("only_b");
        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);

        let a = Recorder::new_record(path_a.clone()).unwrap();
        a.save().unwrap(); // empty

        let mut b = Recorder::new_record(path_b.clone()).unwrap();
        b.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
        b.save().unwrap();

        let ra = Recorder::new_replay(path_a.clone()).unwrap();
        let rb = Recorder::new_replay(path_b.clone()).unwrap();
        let diff = diff_recordings(ra.events(), rb.events());
        assert_eq!(diff.len(), 1);
        assert!(matches!(diff[0], DiffLine::OnlyInB(1, _)));

        let _ = fs::remove_file(&path_a);
        let _ = fs::remove_file(&path_b);
    }

    #[test]
    fn list_recordings_empty_dir() {
        let mut dir = env::temp_dir();
        dir.push(format!("mora_list_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let result = list_recordings(&dir);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_recordings_finds_files() {
        let mut dir = env::temp_dir();
        dir.push(format!("mora_list_test2_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // 创建一个录制文件
        let mut path = dir.clone();
        path.push("test-rec.jsonl");
        let mut r = Recorder::new_record(path).unwrap();
        r.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
        r.save().unwrap();

        let result = list_recordings(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "test-rec");
        assert_eq!(result[0].event_count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_stats_basic() {
        let path = tmp_path("stats");
        let _ = fs::remove_file(&path);
        let mut r = Recorder::new_record(path.clone()).unwrap();
        r.record_ai_chat("gpt-4o".into(), "p".into(), "r".into(), 100, 50, 200, None);
        r.record_ai_chat("gpt-4o".into(), "p2".into(), "r2".into(), 200, 100, 300, None);
        r.record_web_fetch("https://x.com".into(), "GET".into(), 200, 1024, 50, None);
        r.record_note("test".into());
        r.save().unwrap();

        let r2 = Recorder::new_replay(path.clone()).unwrap();
        let stats = compute_stats(r2.events());
        assert_eq!(stats.total_events, 4);
        assert_eq!(stats.ai_chat_count, 2);
        assert_eq!(stats.web_fetch_count, 1);
        assert_eq!(stats.note_count, 1);
        assert_eq!(stats.total_tokens_in, 300);
        assert_eq!(stats.total_tokens_out, 150);
        assert_eq!(stats.min_latency_ms, 50);
        assert_eq!(stats.max_latency_ms, 300);
        assert_eq!(stats.models, vec!["gpt-4o"]);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn compute_stats_empty() {
        let stats = compute_stats(&[]);
        assert_eq!(stats.total_events, 0);
        assert_eq!(stats.total_tokens_in, 0);
    }

    #[test]
    fn export_jsonl_roundtrip() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let jsonl = export_recording(&events, &ExportFormat::Jsonl, "test");
        assert!(jsonl.contains("\"kind\":\"ai.chat\""));
        assert!(jsonl.contains("\"model\":\"m\""));
    }

    #[test]
    fn export_markdown_has_table() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let md = export_recording(&events, &ExportFormat::Markdown, "test");
        assert!(md.contains("# Recording: test"));
        assert!(md.contains("| # | Kind |"));
        assert!(md.contains("ai.chat"));
    }

    #[test]
    fn redact_secrets_masks_sk_key() {
        let input = "api_key=sk-abc123def456ghi789jkl012mno";
        let redacted = redact_secrets(input);
        assert!(redacted.contains("<REDACTED>"));
        assert!(!redacted.contains("sk-abc123"));
    }

    #[test]
    fn redact_secrets_masks_bearer() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0";
        let redacted = redact_secrets(input);
        assert!(redacted.contains("Bearer <REDACTED>"));
        assert!(!redacted.contains("eyJhbGci"));
    }

    #[test]
    fn snapshot_roundtrip() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let snap = create_snapshot("test", &events);
        let jsonl = snapshot_to_jsonl(&snap);
        let restored = snapshot_from_jsonl(&jsonl).unwrap();
        assert_eq!(restored.name, "test");
        assert_eq!(restored.event_summaries.len(), 1);
        assert_eq!(restored.event_summaries[0].kind, "ai.chat");
    }

    #[test]
    fn snapshot_diff_match() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let snap = create_snapshot("test", &events);
        let diffs = diff_snapshot(&snap, &events);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0], SnapshotDiff::Match(0)));
    }

    #[test]
    fn snapshot_diff_changed() {
        let events_a = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let events_b = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m2".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 20, tokens_out: 10, latency_ms: 200, error: None },
        ];
        let snap = create_snapshot("test", &events_a);
        let diffs = diff_snapshot(&snap, &events_b);
        assert!(diffs.iter().any(|d| matches!(d, SnapshotDiff::EventChanged { .. })));
    }

    #[test]
    fn snapshot_diff_missing_event() {
        let events_a = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
            Event::Note { id: 2, ts_ms: 1100, message: "note".into() },
        ];
        let events_b = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let snap = create_snapshot("test", &events_a);
        let diffs = diff_snapshot(&snap, &events_b);
        assert!(diffs.iter().any(|d| matches!(d, SnapshotDiff::EventMissing { .. })));
    }

    #[test]
    fn generate_report_basic() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "r".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let report = generate_report(&events, "test", Some("fix retry"), Some("pytest -q"), &[("os", "windows")]);
        assert!(report.contains("# Evidence Report: test"));
        assert!(report.contains("fix retry"));
        assert!(report.contains("pytest -q"));
        assert!(report.contains("os=windows"));
        assert!(report.contains("## Audit"));
        assert!(report.contains("## Timeline"));
        assert!(report.contains("## Event Log"));
    }

    #[test]
    fn parse_moraignore_basic() {
        let content = r#"
# comment
field:token_usage
path:request.messages.*.content
pattern:github-token
"#;
        let rules = parse_moraignore(content);
        assert_eq!(rules.len(), 3);
        assert!(matches!(&rules[0], IgnoreRule::Field(f) if f == "token_usage"));
        assert!(matches!(&rules[1], IgnoreRule::Path(p) if p == "request.messages.*.content"));
        assert!(matches!(&rules[2], IgnoreRule::Pattern(p) if p == "github-token"));
    }

    #[test]
    fn audit_recording_clean() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "hello".into(), response: "world".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let findings = audit_recording(&events, &[]);
        assert_eq!(findings.len(), 0);
    }

    #[test]
    fn audit_recording_finds_sk_key() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "test".into(), response: "api_key=sk-abc123def456ghi789jkl012mno".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let findings = audit_recording(&events, &[]);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].pattern, "openai-api-key");
    }

    #[test]
    fn audit_recording_respects_ignore_rules() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "test".into(), response: "api_key=sk-abc123def456ghi789jkl012mno".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
        ];
        let rules = vec![IgnoreRule::Pattern("sk-".to_string())];
        let findings = audit_recording(&events, &rules);
        assert_eq!(findings.len(), 0);
    }

    #[test]
    fn redact_secrets_preserves_normal_text() {
        let input = "Hello world, this is a normal message";
        let redacted = redact_secrets(input);
        assert_eq!(redacted, input);
    }

    #[test]
    fn build_timeline_basic() {
        let events = vec![
            Event::AiChat { id: 1, ts_ms: 1000, model: "m".into(), prompt_hash: "h".into(), prompt_preview: "p".into(), response: "hi".into(), tokens_in: 10, tokens_out: 5, latency_ms: 100, error: None },
            Event::Note { id: 2, ts_ms: 1100, message: "note".into() },
        ];
        let rows = build_timeline(&events);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, "ai.chat");
        assert_eq!(rows[0].tokens, "10+5");
        assert_eq!(rows[1].kind, "note");
    }

    #[test]
    fn new_record_creates_parent_dir() {
        let mut p = env::temp_dir();
        p.push(format!("mora_record_test_subdir_{}", std::process::id()));
        p.push("nested");
        p.push("test.jsonl");
        let _ = fs::remove_dir_all(p.parent().unwrap());

        let r = Recorder::new_record(p.clone());
        assert!(r.is_ok());
        assert!(p.parent().unwrap().exists());

        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
