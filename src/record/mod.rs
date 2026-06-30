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
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Recorder 运行模式
mod analysis;
mod audit;
mod diff;
mod serialization;
mod snapshot;
#[cfg(test)]
mod tests;

pub use analysis::*;
pub use audit::*;
pub use diff::*;
pub use serialization::hash_prompt;
use serialization::{event_to_jsonl, event_to_replay_entry, load_jsonl};
pub use snapshot::*;

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
            encoder
                .write_all(out.as_bytes())
                .map_err(|e| format!("recorder: failed to compress: {}", e))?;
            encoder
                .finish()
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
