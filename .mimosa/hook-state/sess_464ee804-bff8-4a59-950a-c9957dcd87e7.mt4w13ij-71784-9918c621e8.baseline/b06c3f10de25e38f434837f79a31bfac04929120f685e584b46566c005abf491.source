//! v0.42.1: Audit Sink — SHA-256 hash-chained JSONL (loongclaw-inspired)
//!
//! 灵感: loongclaw `crates/kernel/src/audit.rs:34-204`
//! - `AuditSink` trait (write / flush / verify_chain)
//! - `JsonlAuditSink`: 追加 JSONL, 每行含 prev_hash + self.hash
//! - `hash = SHA-256(canonical_json(event) + prev_hash)` 形成链
//! - 验证: 重读文件, 重算 hash 链, 任一不匹配则报 ChainBroken / HashMismatch
//!
//! 设计:
//! - 单线程同步, 用 Mutex 共享 (与 EventBus / CapabilityStore 一致)
//! - `payload` 字段手写 JSON 序列化 (不引入 serde 依赖)
//! - `last_hash` 持久化在内存中 (sink 进程重启时初始化为 genesis)
//! - 重启后 verify_chain 仍能从文件重建 hash 链
//!
//! 命名说明: 与 `crate::record::audit` (secret redaction) 不同,
//! 本模块是审计 *日志* 系统 (audit trail).

use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// v0.42.1: 审计事件 (不可变记录, 一旦写入不可修改)
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Unix timestamp (millis since epoch)
    pub timestamp_ms: u128,
    /// 触发主体 (e.g. "user_script", "agent.researcher", "tool.web_fetch")
    pub actor: String,
    /// 动作 (e.g. "tool.invoke", "sandbox.issue", "file.write")
    pub action: String,
    /// 资源路径 / 工具名 (e.g. "/workspace/foo.txt", "ai.chat")
    pub target: Option<String>,
    /// payload (任意 JSON-serializable 字符串 — caller 提供)
    pub payload_json: Option<String>,
    /// 关联 CapabilityToken (来自 v0.42.0 sandbox.key)
    pub token_id: Option<u64>,
    /// 前一个事件的 hash (由 sink 在 write 时填充)
    pub prev_hash: String,
    /// 本事件 hash (由 sink 在 write 时计算)
    pub hash: String,
}

impl AuditEvent {
    /// 创建新事件 (hash / prev_hash 由 sink 设置)
    pub fn new(
        actor: impl Into<String>,
        action: impl Into<String>,
        target: Option<String>,
        payload_json: Option<String>,
        token_id: Option<u64>,
    ) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Self {
            timestamp_ms,
            actor: actor.into(),
            action: action.into(),
            target,
            payload_json,
            token_id,
            prev_hash: String::new(),
            hash: String::new(),
        }
    }

    /// 计算 canonical JSON (字段顺序固定: ts, actor, action, target, payload, token, prev)
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push('{');
        out.push_str(&format!("\"ts\":{},", self.timestamp_ms));
        out.push_str(&format!("\"actor\":{},", json_string(&self.actor)));
        out.push_str(&format!("\"action\":{},", json_string(&self.action)));
        out.push_str(&format!(
            "\"target\":{},",
            self.target
                .as_ref()
                .map(|s| json_string(s))
                .unwrap_or_else(|| "null".to_string())
        ));
        out.push_str(&format!(
            "\"payload\":{},",
            self.payload_json
                .as_ref()
                .map(|s| json_string(s))
                .unwrap_or_else(|| "null".to_string())
        ));
        out.push_str(&format!(
            "\"token\":{},",
            self.token_id
                .map(|t| t.to_string())
                .unwrap_or_else(|| "null".to_string())
        ));
        out.push_str(&format!("\"prev\":{}", json_string(&self.prev_hash)));
        out.push('}');
        out.into_bytes()
    }

    /// seal: 计算 self.hash = SHA-256(canonical_bytes + prev_hash)
    pub fn seal(&mut self) {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_bytes());
        let result = hasher.finalize();
        self.hash = hex_encode(&result);
    }
}

/// v0.42.1: 简单 JSON 字符串转义 (用于 actor / action / target / payload_json / prev_hash)
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// v0.42.1: Audit Sink trait (抽象 sink 接口)
pub trait AuditSink: Send + Sync {
    /// 写入一个事件 (sink 负责计算 prev_hash + self.hash)
    fn write(&self, event: AuditEvent) -> Result<(), AuditError>;
    /// 刷新缓冲区
    fn flush(&self) -> Result<(), AuditError>;
    /// 验证整个 hash 链 (返回 Ok(()) 或 Err)
    fn verify_chain(&self) -> Result<(), AuditError>;
    /// 已写入事件数 (test helper)
    fn event_count(&self) -> u64;
}

/// v0.42.1: Audit 错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditError {
    /// 文件 I/O 错误
    Io(String),
    /// hash 链在某一行断裂 (prev_hash 不匹配)
    ChainBroken {
        line: usize,
        expected_prev: String,
        actual_prev: String,
    },
    /// 某一行 hash 与存储不符 (内容被篡改)
    HashMismatch {
        line: usize,
        stored: String,
        computed: String,
    },
    /// JSON 解析错误
    ParseError { line: usize, msg: String },
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "audit I/O error: {}", msg),
            Self::ChainBroken {
                line,
                expected_prev,
                actual_prev,
            } => write!(
                f,
                "audit chain broken at line {}: expected prev_hash={}, got {}",
                line, expected_prev, actual_prev
            ),
            Self::HashMismatch {
                line,
                stored,
                computed,
            } => write!(
                f,
                "audit hash mismatch at line {}: stored={}, computed={}",
                line, stored, computed
            ),
            Self::ParseError { line, msg } => {
                write!(f, "audit JSON parse error at line {}: {}", line, msg)
            }
        }
    }
}

impl std::error::Error for AuditError {}

/// v0.42.1: 默认 JSONL + SHA-256 链式 sink
///
/// 文件格式: 每行一条 JSON, 字段:
/// `{ts, actor, action, target, payload, token, prev, hash}`
/// `prev` 是上一行的 `hash`; 第一行 prev = "0" * 64 (genesis)
pub struct JsonlAuditSink {
    path: String,
    state: Mutex<JsonlAuditSinkState>,
}

#[derive(Debug)]
struct JsonlAuditSinkState {
    last_hash: String,
    events_count: u64,
    writer: std::io::BufWriter<File>,
}

impl JsonlAuditSink {
    pub const GENESIS_HASH: &'static str =
        "0000000000000000000000000000000000000000000000000000000000000000";

    /// 创建或追加打开一个 JSONL audit log
    /// 若文件已存在, last_hash 从末尾读取 (用于进程重启)
    pub fn new(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| AuditError::Io(e.to_string()))?;

        // 读取末尾行, 提取 last_hash (如果文件非空)
        let (last_hash, events_count) = read_tail_hash(path).unwrap_or_else(|_| {
            // 文件不存在或为空 → genesis
            (Self::GENESIS_HASH.to_string(), 0)
        });

        let writer = std::io::BufWriter::new(file);

        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            state: Mutex::new(JsonlAuditSinkState {
                last_hash,
                events_count,
                writer,
            }),
        })
    }

    /// 进程内 builder: 强制以 genesis 启动 (丢弃文件中已有 hash 链)
    /// 用于测试隔离
    pub fn new_fresh(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| AuditError::Io(e.to_string()))?;
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            state: Mutex::new(JsonlAuditSinkState {
                last_hash: Self::GENESIS_HASH.to_string(),
                events_count: 0,
                writer: std::io::BufWriter::new(file),
            }),
        })
    }
}

impl AuditSink for JsonlAuditSink {
    fn write(&self, mut event: AuditEvent) -> Result<(), AuditError> {
        let mut state = self.state.lock().expect("audit sink mutex poisoned");
        event.prev_hash = state.last_hash.clone();
        event.seal();

        // 序列化整行 JSON (含 prev + hash)
        let mut line = String::new();
        line.push('{');
        line.push_str(&format!("\"ts\":{},", event.timestamp_ms));
        line.push_str(&format!("\"actor\":{},", json_string(&event.actor)));
        line.push_str(&format!("\"action\":{},", json_string(&event.action)));
        line.push_str(&format!(
            "\"target\":{},",
            event
                .target
                .as_ref()
                .map(|s| json_string(s))
                .unwrap_or_else(|| "null".to_string())
        ));
        line.push_str(&format!(
            "\"payload\":{},",
            event
                .payload_json
                .as_ref()
                .map(|s| json_string(s))
                .unwrap_or_else(|| "null".to_string())
        ));
        line.push_str(&format!(
            "\"token\":{},",
            event
                .token_id
                .map(|t| t.to_string())
                .unwrap_or_else(|| "null".to_string())
        ));
        line.push_str(&format!("\"prev\":{},", json_string(&event.prev_hash)));
        line.push_str(&format!("\"hash\":{}", json_string(&event.hash)));
        line.push('}');
        line.push('\n');

        state
            .writer
            .write_all(line.as_bytes())
            .map_err(|e| AuditError::Io(e.to_string()))?;

        state.last_hash = event.hash.clone();
        state.events_count += 1;
        Ok(())
    }

    fn flush(&self) -> Result<(), AuditError> {
        let mut state = self.state.lock().expect("audit sink mutex poisoned");
        state
            .writer
            .flush()
            .map_err(|e| AuditError::Io(e.to_string()))?;
        Ok(())
    }

    fn verify_chain(&self) -> Result<(), AuditError> {
        let state = self.state.lock().expect("audit sink mutex poisoned");
        // 必须先 flush 才能读到所有写入
        let _ = state.writer.get_ref().sync_all();
        drop(state);

        let file = File::open(&self.path).map_err(|e| AuditError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        let mut prev = Self::GENESIS_HASH.to_string();
        for (i, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| AuditError::Io(e.to_string()))?;
            if line.is_empty() {
                continue;
            }
            let (stored_prev, stored_hash) =
                parse_prev_hash(&line).ok_or_else(|| AuditError::ParseError {
                    line: i,
                    msg: "missing prev/hash fields".to_string(),
                })?;
            if stored_prev != prev {
                return Err(AuditError::ChainBroken {
                    line: i,
                    expected_prev: prev,
                    actual_prev: stored_prev,
                });
            }
            // 重新计算 hash
            let mut computed_event =
                parse_event_for_seal(&line).ok_or_else(|| AuditError::ParseError {
                    line: i,
                    msg: "missing required fields".to_string(),
                })?;
            computed_event.prev_hash = stored_prev.clone();
            computed_event.seal();
            if computed_event.hash != stored_hash {
                return Err(AuditError::HashMismatch {
                    line: i,
                    stored: stored_hash,
                    computed: computed_event.hash,
                });
            }
            prev = stored_hash;
        }
        Ok(())
    }

    fn event_count(&self) -> u64 {
        self.state
            .lock()
            .expect("audit sink mutex poisoned")
            .events_count
    }
}

/// v0.42.1: 从文件末尾读取最后一行, 提取 hash (用于进程重启恢复 last_hash)
fn read_tail_hash(path: &Path) -> Result<(String, u64), AuditError> {
    let file = File::open(path).map_err(|e| AuditError::Io(e.to_string()))?;
    let reader = BufReader::new(file);
    let mut last_line = String::new();
    let mut count: u64 = 0;
    for line in reader.lines() {
        let line = line.map_err(|e| AuditError::Io(e.to_string()))?;
        if line.is_empty() {
            continue;
        }
        count += 1;
        last_line = line;
    }
    if last_line.is_empty() {
        return Ok((JsonlAuditSink::GENESIS_HASH.to_string(), 0));
    }
    let (_prev, hash) = parse_prev_hash(&last_line).ok_or_else(|| AuditError::ParseError {
        line: count as usize,
        msg: "missing prev/hash fields".to_string(),
    })?;
    Ok((hash, count))
}

/// 简化 JSON parser: 提取 `"prev":"..."` 和 `"hash":"..."` 字段
/// 假设字段值不含嵌套引号 (我们手写的 JSON 是这样, 除了 payload)
/// 对于 payload 等可能含 `"` 的字段, 用 `extract_field_skip_escaped`
fn parse_prev_hash(line: &str) -> Option<(String, String)> {
    let prev = extract_field_skip_escaped(line, "prev")?;
    let hash = extract_field_skip_escaped(line, "hash")?;
    Some((prev, hash))
}

/// 提取字符串字段, 正确处理 JSON 转义 (反转义 \", \\, \n, \r, \t)
fn extract_field_skip_escaped(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", field);
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let bytes = rest.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // 反转义: \" → ", \\ → \, \n → newline, \r → cr, \t → tab
            match bytes[i + 1] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                _ => {
                    out.push('\\');
                    out.push(bytes[i + 1] as char);
                }
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return Some(out);
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    None
}

fn extract_field(line: &str, field: &str) -> Option<String> {
    extract_field_skip_escaped(line, field)
}

/// 简化 JSON parser: 提取 6 个字段重建 AuditEvent (用于重算 hash)
fn parse_event_for_seal(line: &str) -> Option<AuditEvent> {
    Some(AuditEvent {
        timestamp_ms: extract_number(line, "ts")?,
        actor: extract_field(line, "actor")?,
        action: extract_field(line, "action")?,
        target: extract_field(line, "target"),
        payload_json: extract_field(line, "payload"),
        token_id: extract_number(line, "token").map(|n| n as u64),
        prev_hash: String::new(), // 由 caller 设置
        hash: String::new(),
    })
}

fn extract_number(line: &str, field: &str) -> Option<u128> {
    let needle = format!("\"{}\":", field);
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ============================================================
// v0.42.1: NullSink — no-op default (test / disabled mode)
// ============================================================

/// v0.42.1: 空操作 sink, 默认用于 Interpreter (audit 关闭)
#[derive(Debug, Default, Clone)]
pub struct NullSink;

impl NullSink {
    pub fn new() -> Self {
        Self
    }
}

impl AuditSink for NullSink {
    fn write(&self, _event: AuditEvent) -> Result<(), AuditError> {
        Ok(())
    }
    fn flush(&self) -> Result<(), AuditError> {
        Ok(())
    }
    fn verify_chain(&self) -> Result<(), AuditError> {
        Ok(())
    }
    fn event_count(&self) -> u64 {
        0
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn temp_log_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_audit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn write_appends_jsonl_line() {
        let path = temp_log_path("write_basic.jsonl");
        let sink = JsonlAuditSink::new_fresh(&path).unwrap();

        let event = AuditEvent::new("user", "test.action", None, None, None);
        sink.write(event).unwrap();

        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"action\":\"test.action\""));
        assert!(lines[0].contains("\"hash\":"));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn each_event_has_chained_hash() {
        let path = temp_log_path("chained.jsonl");
        let sink = JsonlAuditSink::new_fresh(&path).unwrap();

        let mut prev_hashes = Vec::new();
        for i in 0..3 {
            let event = AuditEvent::new("user", format!("action_{}", i), None, None, None);
            sink.write(event).unwrap();
            // 在 state 里读 prev_hash 不可见 (已 move); 用 verify_chain 验证
            prev_hashes.push(format!("event_{}", i));
        }
        sink.flush().unwrap();
        assert_eq!(sink.event_count(), 3);

        // 验证链
        assert!(sink.verify_chain().is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_chain_passes_for_valid_log() {
        let path = temp_log_path("valid.jsonl");
        let sink = JsonlAuditSink::new_fresh(&path).unwrap();

        for i in 0..10 {
            sink.write(AuditEvent::new(
                "actor",
                format!("op.{}", i),
                Some(format!("/target/{}", i)),
                Some(format!("{{\"i\":{}}}", i)),
                if i % 2 == 0 { Some(i as u64) } else { None },
            ))
            .unwrap();
        }
        sink.flush().unwrap();
        assert!(sink.verify_chain().is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_chain_fails_on_tampered_event() {
        let path = temp_log_path("tampered.jsonl");
        let sink = JsonlAuditSink::new_fresh(&path).unwrap();

        for i in 0..3 {
            sink.write(AuditEvent::new("a", format!("op.{}", i), None, None, None))
                .unwrap();
        }
        sink.flush().unwrap();
        assert!(sink.verify_chain().is_ok());

        // 篡改第 2 行 (index 1) 的 actor 字段
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[1] = lines[1].replace("\"actor\":\"a\"", "\"actor\":\"TAMPERED\"");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let err = sink.verify_chain().unwrap_err();
        match err {
            AuditError::HashMismatch { line, .. } => {
                assert_eq!(line, 1, "tamper at line 1 should fail at line 1")
            }
            other => panic!("expected HashMismatch, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_log_verifies_as_genesis() {
        let path = temp_log_path("empty.jsonl");
        let _sink = JsonlAuditSink::new_fresh(&path).unwrap();
        // 没有写入任何 event
        let sink2 = JsonlAuditSink::new(&path).unwrap(); // 重新打开
        assert!(sink2.verify_chain().is_ok());
        assert_eq!(sink2.event_count(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopen_preserves_last_hash_for_chaining() {
        let path = temp_log_path("reopen.jsonl");
        {
            let sink = JsonlAuditSink::new_fresh(&path).unwrap();
            sink.write(AuditEvent::new("a", "op.1", None, None, None))
                .unwrap();
            sink.flush().unwrap();
            assert_eq!(sink.event_count(), 1);
        }
        // 重新打开 — 应该从 file 末尾恢复 last_hash
        {
            let sink = JsonlAuditSink::new(&path).unwrap();
            assert_eq!(sink.event_count(), 1);
            sink.write(AuditEvent::new("a", "op.2", None, None, None))
                .unwrap();
            sink.flush().unwrap();
            assert!(sink.verify_chain().is_ok());
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hash_is_deterministic_for_same_inputs() {
        let mut e1 = AuditEvent::new("a", "op", Some("t".into()), Some("p".into()), Some(42));
        e1.prev_hash = "0".repeat(64);
        e1.timestamp_ms = 1234567890000;
        e1.seal();
        let hash1 = e1.hash.clone();

        let mut e2 = AuditEvent::new("a", "op", Some("t".into()), Some("p".into()), Some(42));
        e2.prev_hash = "0".repeat(64);
        e2.timestamp_ms = 1234567890000;
        e2.seal();
        let hash2 = e2.hash.clone();

        assert_eq!(hash1, hash2, "same inputs should produce same hash");
        assert_eq!(hash1.len(), 64, "SHA-256 hex should be 64 chars");
    }

    #[test]
    fn different_prev_hash_yields_different_event_hash() {
        let mut e1 = AuditEvent::new("a", "op", None, None, None);
        e1.timestamp_ms = 1000;
        e1.prev_hash = "0".repeat(64);
        e1.seal();

        let mut e2 = AuditEvent::new("a", "op", None, None, None);
        e2.timestamp_ms = 1000;
        e2.prev_hash = "1".repeat(64); // 不同 prev
        e2.seal();

        assert_ne!(e1.hash, e2.hash);
    }

    #[test]
    fn json_string_escapes_special_chars() {
        let s = "hello \"world\"\n\t\\";
        let escaped = json_string(s);
        assert_eq!(escaped, r#""hello \"world\"\n\t\\""#);
    }

    #[test]
    fn audit_event_new_populates_timestamp() {
        let e1 = AuditEvent::new("a", "op", None, None, None);
        let e2 = AuditEvent::new("a", "op", None, None, None);
        // 极大概率 e1.timestamp_ms <= e2.timestamp_ms
        assert!(e2.timestamp_ms >= e1.timestamp_ms);
        assert!(e1.timestamp_ms > 0);
    }

    #[test]
    fn genesis_hash_is_64_zeros() {
        assert_eq!(JsonlAuditSink::GENESIS_HASH.len(), 64);
        let zeros: HashSet<char> = HashSet::from_iter(JsonlAuditSink::GENESIS_HASH.chars());
        assert_eq!(zeros.len(), 1);
        assert!(zeros.contains(&'0'));
    }

    #[test]
    fn null_sink_is_noop() {
        let sink = NullSink::new();
        let event = AuditEvent::new("a", "op", None, None, None);
        assert!(sink.write(event).is_ok());
        assert!(sink.flush().is_ok());
        assert!(sink.verify_chain().is_ok());
        assert_eq!(sink.event_count(), 0);
    }
}
