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
pub use serialization::SkippedLine;
pub use serialization::hash_prompt;
use serialization::{event_to_jsonl, event_to_replay_entry, load_jsonl};
pub use snapshot::*;

#[derive(Clone, Debug)]
pub enum Mode {
    /// 不录制也不重放 (默认)
    Off,
    /// 录制所有事件到 `path` (JSONL)
    Record(PathBuf),
    /// v0.104.6: 只在内存里录制,事件**不落盘**。
    ///
    /// 供 `mora snapshot` 使用 —— 它只需要 `events()` 拿去和基线比对,
    /// 不需要一份 JSONL 录像。v0.104.5 及以前该命令根本不装录制器
    /// (`Interpreter::new()` 的默认 `new_off()`),于是 `current_events`
    /// 恒空,比对 0 vs 0 恒 Match → **对完全不同的输入也报 "passed"**。
    RecordMemory,
    /// 从 `path` 重放,匹配 (kind, key) 返回录制响应
    Replay(PathBuf),
}

impl Mode {
    pub fn is_off(&self) -> bool {
        matches!(self, Mode::Off)
    }
    pub fn is_record(&self) -> bool {
        matches!(self, Mode::Record(_) | Mode::RecordMemory)
    }
    pub fn is_replay(&self) -> bool {
        matches!(self, Mode::Replay(_))
    }
}

/// 单个录制事件 —— JSONL 一行
///
/// v0.76.04: 加 `arg_signature` 字段——录制时记录函数签名（typeck::Type
/// 可读化），replay 时校验当前函数签名是否与录制一致，避免签名漂移导致
/// 拿错响应。
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
        /// v0.76.04: 函数签名（typeck::Type 可读化）— replay 时校验
        arg_signature: String,
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
        /// v0.76.04: 函数签名（typeck::Type 可读化）— replay 时校验
        arg_signature: String,
    },
    /// 用户/系统 note
    Note {
        id: u64,
        ts_ms: u128,
        message: String,
    },
    /// v0.83: TEA-style Msg 事件 — 应用层消息（区别于 ai/web 副作用）。
    /// channel 路由到 env 的 key（route by name）。
    /// prior_state_hash 用于 replay 时校验状态一致性。
    Msg {
        id: u64,
        ts_ms: u128,
        channel: String,
        payload: crate::value::Value,
        prior_state_hash: u64,
    },
    /// v0.83: state mutation（env diff）— h_define/h_assign/h_send 触发。
    /// 记录 var 名前后值变化，用于 replay 重放 +time-travel debugging。
    StateMutation {
        id: u64,
        ts_ms: u128,
        var: String,
        old: crate::value::Value,
        new: crate::value::Value,
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
    /// v0.76.05: 录制时的函数签名（typeck::Type 可读化）——replay 校验用
    pub arg_signature: String,
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
    /// v0.76.06: 签名漂移 warning 收集（replay 时签名不匹配 push 警告）
    pub warnings: Vec<String>,
    /// v0.104.6 D178：加载录像时**没能解析出来**的行（截断/畸形/非 JSON）。
    ///
    /// 以前这些行被静默丢弃，于是 `replay` / `diff` / `stats` / `export` /
    /// **`audit`** 全都在**不完整数据**上照常报成功 —— 密钥扫描器尤其危险：
    /// 缺了的那行若含密钥，`audit` 仍会说「No secrets found」。
    /// 容忍畸形行（前向兼容）不变，但不再**沉默**。
    pub skipped_lines: Vec<SkippedLine>,
    /// v0.104.6 D182：重放时**实际从录像取到了响应**的次数。
    ///
    /// 此前 `mora replay` 报的是「加载了多少条事件」——
    /// 一次都没命中时照样打 `✓ replayed 3 events`（D174 同族的假绿）。
    /// 而其中 `state_mutation` 之类**根本不可重放**，那个数字天生虚高。
    pub replay_hits: u64,
    /// v0.104.6 D182：重放时**查了但没取到**的次数（prompt/model 对不上、
    /// url 不一致、签名漂移）。与 [`Self::replay_hits`] 一起才能回答
    /// 「这次重放到底复现了没有」。
    pub replay_misses: u64,
}

impl Recorder {
    pub fn new_off() -> Self {
        Self {
            mode: Mode::Off,
            events: Vec::new(),
            next_id: 1,
            index: HashMap::new(),
            warnings: Vec::new(),
            skipped_lines: Vec::new(),
            replay_hits: 0,
            replay_misses: 0,
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
            warnings: Vec::new(),
            skipped_lines: Vec::new(),
            replay_hits: 0,
            replay_misses: 0,
        })
    }

    /// v0.104.6: 内存录制 —— 累积事件但不落盘。
    ///
    /// 与 `new_record` 的唯一区别是没有目标文件,故 `save()` 是 no-op。
    /// 事件只从 `events()` 读,供 `mora snapshot` 与基线比对。
    pub fn new_record_memory() -> Self {
        Self {
            mode: Mode::RecordMemory,
            events: Vec::new(),
            next_id: 1,
            index: HashMap::new(),
            warnings: Vec::new(),
            skipped_lines: Vec::new(),
            replay_hits: 0,
            replay_misses: 0,
        }
    }

    pub fn new_replay(path: PathBuf) -> Result<Self, String> {
        let (events, skipped) = load_jsonl(&path)?;
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
            warnings: Vec::new(),
            skipped_lines: skipped,
            replay_hits: 0,
            replay_misses: 0,
        })
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// v0.83: 便捷方法——false 表示 Off（不录制不重放）。
    pub fn is_off(&self) -> bool {
        matches!(self.mode, Mode::Off)
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

    /// v0.83: 录制 TEA Msg 事件（便捷构造）。
    pub fn record_msg(
        &mut self,
        channel: String,
        payload: crate::value::Value,
        prior_state_hash: u64,
    ) {
        let id = self.next_event_id();
        let ts_ms = Self::now_ms();
        self.record(Event::Msg {
            id,
            ts_ms,
            channel,
            payload,
            prior_state_hash,
        });
    }

    /// v0.83: 录制 StateMutation 事件（便捷构造）。
    pub fn record_state_mutation(
        &mut self,
        var: String,
        old: crate::value::Value,
        new: crate::value::Value,
    ) {
        let id = self.next_event_id();
        let ts_ms = Self::now_ms();
        self.record(Event::StateMutation {
            id,
            ts_ms,
            var,
            old,
            new,
        });
    }

    /// 重放: 查找 ai.chat 的录制响应
    /// v0.76.05: `current_arg_signature` 为当前函数签名（typeck::Type 可读化）。
    /// 与录制时的签名不匹配时返 None（按"签名漂移 = 当作没匹配"原则）。
    /// v0.76.06: 签名漂移时 push warning 到 `self.warnings`（不报 error——
    /// 静默返 None 用户的体验与 v0.76.05 一致，但能看到 warning）。
    pub fn lookup_ai_chat(
        &mut self,
        model: &str,
        prompt: &str,
        current_arg_signature: &str,
    ) -> Option<RecordedResponse> {
        if !self.mode.is_replay() {
            return None;
        }
        let key = format!("{}|{}", model, hash_prompt(prompt));
        // v0.104.6 D182：记录命中/未命中 —— 让「重放有没有真的发生」可见。
        // 详见 `replay_hits` 字段说明。
        let Some(rec) = self.index.get(&("ai.chat".to_string(), key)) else {
            self.replay_misses += 1;
            return None;
        };
        // v0.76.05: 签名校验——录制与当前签名不一致 = 不匹配
        if rec.arg_signature != current_arg_signature {
            // v0.76.06: 签名漂移 warning
            self.warnings.push(format!(
                "ai.chat({}) 签名漂移: 录制 '{}' vs 当前 '{}'",
                model, rec.arg_signature, current_arg_signature
            ));
            self.replay_misses += 1;
            return None;
        }
        self.replay_hits += 1;
        Some(rec.clone())
    }

    /// 重放: 查找 web.fetch 的录制响应
    /// v0.76.05: 同上——`current_arg_signature` 签名校验
    /// v0.76.06: 同上——签名漂移时 push warning
    pub fn lookup_web_fetch(
        &mut self,
        url: &str,
        current_arg_signature: &str,
    ) -> Option<RecordedResponse> {
        if !self.mode.is_replay() {
            return None;
        }
        let Some(rec) = self.index.get(&("web.fetch".to_string(), url.to_string())) else {
            self.replay_misses += 1;
            return None;
        };
        // v0.76.05: 签名校验
        if rec.arg_signature != current_arg_signature {
            // v0.76.06: 签名漂移 warning
            self.warnings.push(format!(
                "web.fetch({}) 签名漂移: 录制 '{}' vs 当前 '{}'",
                url, rec.arg_signature, current_arg_signature
            ));
            self.replay_misses += 1;
            return None;
        }
        self.replay_hits += 1;
        Some(rec.clone())
    }

    /// 录制模式: 把累积事件 flush 到 JSONL 文件
    /// v0.22: 支持压缩存储（.jsonl.gz）
    pub fn save(&self) -> Result<(), String> {
        let path = match &self.mode {
            Mode::Record(p) => p,
            // v0.104.6: 内存录制无落盘目标 —— save 是 no-op (不是错误:
            // 事件仍可从 events() 读, snapshot 正是这样用的)。
            Mode::RecordMemory => return Ok(()),
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
        // v0.76.04: typeck::Type 可读化签名（参数 + 返回类型 Debug 串）
        arg_signature: String,
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
            arg_signature,
        });
    }

    /// 便利: 录制 web.fetch 事件
    #[allow(clippy::too_many_arguments)]
    pub fn record_web_fetch(
        &mut self,
        url: String,
        method: String,
        status: u16,
        body_len: usize,
        latency_ms: u128,
        error: Option<String>,
        // v0.76.04: typeck::Type 可读化签名
        arg_signature: String,
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
            arg_signature,
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
