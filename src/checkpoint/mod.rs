//! v0.50: Checkpoint persistence layer for Pregel BSP execution engine.
//!
//! v0.76.10: `Result<_, String>` → `Result<_, MoraError>`（MoraError 统一计划推进）
//! - `MemorySaver`: in-memory storage for testing and ephemeral workflows
//! - `SqliteSaver` (feature `checkpoint-sqlite`): file-backed durable storage
//!
//! All operations return `Result` (zero-panic policy). JSON serialization is
//! hand-written via `flow::value_to_json` / `flow::json_to_value` to avoid
//! introducing a serde dependency (consistent with v0.11+ design).

use crate::error::MoraError;
use crate::flow::{json_to_value, value_to_json};
use crate::value::Value;
use std::collections::HashMap;

// ============================================================
// 往返可逆性检查（v0.104.6 D233）
// ============================================================

/// 判定一个 `Value` 经 `value_to_json` → `json_to_value` 后是否**逐类型恒等**。
///
/// v0.104.6 D233：`Checkpoint` 的文档说「Captures the **complete** state」，
/// 而 `to_json` 把不可 JSON 化的变体交给 `value_to_json` 处理 ——
/// 后者对它们输出**占位字符串**（`"<agent X>"` / `"<conversation X>"` / …）。
/// 往返后：
///
/// | 存入 | 读回 |
/// |---|---|
/// | `Char('中')` | `String("中")` |
/// | `Code("fn main() {}")` | `String("fn main() {}")` |
/// | `Agent { .. }` | `String("<agent worker>")` |
/// | `Conversation { .. }` | `String("<conversation gpt>")` |
/// | `HttpRequest { .. }` | `String("<http_request GET /x>")` |
///
/// 即：**类型降级 + 信息全丢**，而 `to_json` 返回 `Ok`、零诊断。
/// `restore_checkpoint` 再把这个字符串写回 `channels`，Pregel 引擎
/// 拿着**损坏的状态**继续跑。
///
/// 这个函数让 `to_json` 能在**序列化前**发现不可逆的值并报错，
/// 把「静默损坏」变成「明确失败」——`pregel::run` 的
/// `saver.save(&thread_id, &cp)?` 会把错误向上传播。
///
/// ⚠ 对 `json.stringify` builtin 而言，占位串是**合理取舍**（用户的
/// 函数/Agent 本来就无法 JSON 化）；对 checkpoint 而言是**缺陷**
/// （目标是恢复状态，不是展示）。两处语义不同，故检查放在 checkpoint 层。
pub fn is_roundtrip_faithful(v: &Value) -> bool {
    match v {
        // JSON 原生可表示且逐类型恒等（D99/D223 已把 Float/String 转义做穷举）
        Value::Nil
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::BigInt(_)
        | Value::String(_)
        | Value::List(_)
        | Value::Dict(_) => true,
        // 其余：要么被降级成 String（Char / Code），要么被换成占位串
        // （Agent / Conversation / HttpRequest / Router / McpServer / …），
        // 要么直接变 `null`（Compose / Curry / Cons / Macro / …）。
        // 一律判为**不可逆**。
        _ => false,
    }
}

/// 递归检查 `channel_values` 与 `pending_sends[].input` 里的所有值。
fn find_unfaithful(v: &Value, path: &str) -> Option<String> {
    if !is_roundtrip_faithful(v) {
        return Some(format!("{path}: {}", value_kind_name(v)));
    }
    match v {
        Value::List(items) => {
            for (i, item) in items.iter().enumerate() {
                if let Some(p) = find_unfaithful(item, &format!("{path}[{i}]")) {
                    return Some(p);
                }
            }
            None
        }
        Value::Dict(map) => {
            // 排序保证报错信息稳定（HashMap 迭代序每进程不同）
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(p) = find_unfaithful(&map[k], &format!("{path}.{k}")) {
                    return Some(p);
                }
            }
            None
        }
        _ => None,
    }
}

/// 给人看的类型名（`Value` 无 `Display` 契约，全量 match 太重）。
fn value_kind_name(v: &Value) -> &'static str {
    match v {
        Value::Char(_) => "char",
        Value::Code(_) => "code",
        Value::Task { .. } => "task",
        Value::Tool { .. } => "tool",
        Value::Closure { .. } => "closure",
        Value::Conversation { .. } => "conversation",
        Value::Stream { .. } => "stream",
        Value::Agent { .. } => "agent",
        Value::AiConfig { .. } => "ai_config",
        Value::Router { .. } => "router",
        Value::HttpRequest { .. } => "http_request",
        Value::McpServer { .. } => "mcp_server",
        Value::TraitObject { .. } => "trait_object",
        Value::Document { .. } => "document",
        _ => "non-JSON value",
    }
}

// ============================================================
// SendTask
// ============================================================

/// A dynamic dispatch task queued by a Pregel node (`send` expression).
///
/// When a node returns `Send { target, input }`, the Pregel engine queues it
/// into `pending_sends`. After the UPDATE phase, these tasks are expanded into
/// new active nodes for the next super-step.
#[derive(Debug, Clone, PartialEq)]
pub struct SendTask {
    pub target_node: String,
    pub input: Value,
}

// ============================================================
// Checkpoint
// ============================================================

/// A single checkpoint snapshot of Pregel execution state.
///
/// Captures the complete state at the end of a super-step, enabling:
/// - Fault recovery (resume from latest checkpoint)
/// - Time-travel debugging (rewind to any prior step)
/// - Human-in-the-loop (interrupt + resume with `Command`)
#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    /// Unique checkpoint identifier (UUID v4).
    pub id: String,
    /// Schema version for forward-compatibility.
    pub v: u32,
    /// Thread identifier (isolates concurrent orchestrate instances).
    pub thread_id: String,
    /// Super-step index (0 = initial state before first step).
    pub step: usize,
    /// Current channel values (state).
    pub channel_values: HashMap<String, Value>,
    /// Monotonically increasing version per channel.
    pub channel_versions: HashMap<String, u64>,
    /// Last observed version per (node, channel).
    pub versions_seen: HashMap<String, HashMap<String, u64>>,
    /// Dynamic sends queued during this step, processed after UPDATE.
    pub pending_sends: Vec<SendTask>,
    /// Wall-clock timestamp (millis since Unix epoch).
    pub timestamp_ms: u128,
}

/// v0.104.6 D244：所有「外部数字 → 非负整数」转换的**唯一收口**。
///
/// 此前 D148 只给 `v` / `step` 加了负数守卫，而**同一个函数**里的
/// `channel_versions` / `versions_seen` / `timestamp_ms` 三处同样是从
/// `Value`（外部 JSON，或 `SqliteSaver::load` 读出的 `data_json`）转换，
/// 却没有守卫 —— **守卫漏了 5 处中的 3 处**，且无任何统一入口可查。
///
/// 缺陷后果实测（`Checkpoint::from_json`，喂 `-1`）：
///
/// | 字段 | 修前 | 修后 |
/// |---|---|---|
/// | `v` | 报错 | 报错（D148 已有） |
/// | `step` | 报错 | 报错（D148 已有） |
/// | `channel_versions` | **18446744073709551615** | 报错 |
/// | `versions_seen` | **18446744073709551615** | 报错 |
/// | `timestamp_ms` | **340282366920938463463374607431768211455** | 报错 |
///
/// 两种失败模式都**零诊断**，且方向相反：
/// 整数 `as` 是**回绕**（`-1i64 as u64 == u64::MAX`），
/// 浮点 `as` 是**饱和**（`-1.0f64 as u64 == 0`）。
///
/// `channel_versions` / `versions_seen` 变 `u64::MAX` 尤其隐蔽：版本号的
/// 语义是「已观测到的最大版本」，`u64::MAX` 等于宣告「这个 channel 的一切
/// 都已见过」⇒ **增量计算永久停滞**。`timestamp_ms` 变 `u128::MAX` 则让
/// D234 的三级排序键 `(step, timestamp_ms, id)` 永远把它排到 `load` 的
/// 第一位 / `list` 的最后一位。
///
/// `TryFrom` 一并把**大值回绕**（如 `2^32+1 as u32 == 1`）也变成报错 ——
/// 那是同一类「静默得到看似合法的错值」。
fn nonneg_num<T>(field: &str, v: Option<&Value>) -> Result<T, String>
where
    T: TryFrom<u64>,
{
    let raw = match v {
        Some(Value::Int(i)) if *i < 0 => {
            return Err(format!("Checkpoint {field} 不能为负数（得到 {i}）"));
        }
        Some(Value::Int(i)) => *i as u64,
        Some(Value::Float(n)) if *n < 0.0 => {
            return Err(format!("Checkpoint {field} 不能为负数（得到 {n}）"));
        }
        Some(Value::Float(n)) => *n as u64,
        _ => return Err(format!("Checkpoint {field} must be a number")),
    };
    T::try_from(raw).map_err(|_| format!("Checkpoint {field} 超出可表示范围（得到 {raw}）"))
}

impl Checkpoint {
    /// Generate a new random checkpoint ID.
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Convenience constructor.
    pub fn new(
        thread_id: String,
        step: usize,
        channel_values: HashMap<String, Value>,
        channel_versions: HashMap<String, u64>,
        versions_seen: HashMap<String, HashMap<String, u64>>,
        pending_sends: Vec<SendTask>,
    ) -> Self {
        Self {
            id: Self::new_id(),
            v: 1,
            thread_id,
            step,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        }
    }

    /// Serialize to JSON string (hand-written, no serde).
    ///
    /// v0.104.6 D233：序列化**前**先验证所有值可无损往返；不可逆的
    /// 值（`Char` / `Code` / `Agent` / `Conversation` / `HttpRequest` …）
    /// **报错**而不是交给 `value_to_json` 静默写成占位字符串。
    /// 理由见 [`is_roundtrip_faithful`] 的文档：checkpoint 的目的是
    /// **恢复状态**，写一个恢复不出来的检查点比停下更糟。
    pub fn to_json(&self) -> Result<String, String> {
        // 排序保证报错信息稳定（HashMap 迭代序每进程不同）
        let mut ch_keys: Vec<&String> = self.channel_values.keys().collect();
        ch_keys.sort();
        for k in &ch_keys {
            if let Some(p) =
                find_unfaithful(&self.channel_values[*k], &format!("channel_values.{k}"))
            {
                return Err(format!(
                    "Checkpoint 不能序列化不可逆的值（{p}）—— \
                     恢复后状态会与保存时不同。请改用 JSON 可表示的类型 \
                     （nil / bool / int / float / bigint / string / list / dict）。"
                ));
            }
        }
        for (i, send) in self.pending_sends.iter().enumerate() {
            if let Some(p) = find_unfaithful(&send.input, &format!("pending_sends[{i}].input")) {
                return Err(format!(
                    "Checkpoint 不能序列化不可逆的值（{p}）—— \
                     恢复后状态会与保存时不同。请改用 JSON 可表示的类型 \
                     （nil / bool / int / float / bigint / string / list / dict）。"
                ));
            }
        }

        let mut map = HashMap::new();

        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("v".to_string(), Value::Int(self.v as i64));
        map.insert(
            "thread_id".to_string(),
            Value::String(self.thread_id.clone()),
        );
        map.insert("step".to_string(), Value::Int(self.step as i64));
        map.insert(
            "channel_values".to_string(),
            Value::Dict(self.channel_values.clone()),
        );

        let channel_versions: HashMap<String, Value> = self
            .channel_versions
            .iter()
            .map(|(k, v)| (k.clone(), Value::Int(*v as i64)))
            .collect();
        map.insert(
            "channel_versions".to_string(),
            Value::Dict(channel_versions),
        );

        let mut versions_seen = HashMap::new();
        for (node, versions) in &self.versions_seen {
            let inner: HashMap<String, Value> = versions
                .iter()
                .map(|(k, v)| (k.clone(), Value::Int(*v as i64)))
                .collect();
            versions_seen.insert(node.clone(), Value::Dict(inner));
        }
        map.insert("versions_seen".to_string(), Value::Dict(versions_seen));

        let pending_sends: Vec<Value> = self
            .pending_sends
            .iter()
            .map(|send| {
                let mut send_map = HashMap::new();
                send_map.insert(
                    "target_node".to_string(),
                    Value::String(send.target_node.clone()),
                );
                send_map.insert("input".to_string(), send.input.clone());
                Value::Dict(send_map)
            })
            .collect();
        map.insert(
            "pending_sends".to_string(),
            Value::List(pending_sends.into()),
        );

        // u128 does not fit safely in f64 (JSON number), so store as string.
        map.insert(
            "timestamp_ms".to_string(),
            Value::String(self.timestamp_ms.to_string()),
        );

        Ok(value_to_json(&Value::Dict(map)))
    }

    /// Deserialize from JSON string.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let value = json_to_value(s)?;
        let map = match value {
            Value::Dict(m) => m,
            _ => return Err("Checkpoint JSON must be a dict".to_string()),
        };

        let id = match map.get("id") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("Checkpoint id must be a string".to_string()),
        };

        // v0.104.6 D148 定位、D244 收敛：`v` / `step` 的负数守卫与
        // `channel_versions` / `versions_seen` / `timestamp_ms` 是**同一件事**
        // （外部数字 → 非负整数），此前被写成 5 段独立内联逻辑，于是守卫
        // 只覆盖了 2/5 处（详见 `nonneg_num` 的文档）。
        let v: u32 = nonneg_num("v", map.get("v"))?;

        let thread_id = match map.get("thread_id") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("Checkpoint thread_id must be a string".to_string()),
        };

        let step: usize = nonneg_num("step", map.get("step"))?;

        let channel_values = match map.get("channel_values") {
            Some(Value::Dict(m)) => m.clone(),
            _ => return Err("Checkpoint channel_values must be a dict".to_string()),
        };

        // v0.104.6 D244：版本号同样是「非负整数」语义，走同一收口。
        // 修前 `Value::Int(-1) as u64` **回绕**成 `u64::MAX`，
        // 等于宣告「这个 channel 的一切都已见过」⇒ 增量计算永久停滞。
        let channel_versions = match map.get("channel_versions") {
            Some(Value::Dict(m)) => m
                .iter()
                .map(|(k, v)| {
                    let num = nonneg_num::<u64>(&format!("channel_versions[{k}]"), Some(v))?;
                    Ok((k.clone(), num))
                })
                .collect::<Result<HashMap<String, u64>, String>>()?,
            _ => return Err("Checkpoint channel_versions must be a dict".to_string()),
        };

        let versions_seen = match map.get("versions_seen") {
            Some(Value::Dict(m)) => m
                .iter()
                .map(|(node, v)| {
                    let inner = match v {
                        Value::Dict(inner_map) => inner_map
                            .iter()
                            .map(|(k, v)| {
                                let num = nonneg_num::<u64>(
                                    &format!("versions_seen[{node}][{k}]"),
                                    Some(v),
                                )?;
                                Ok((k.clone(), num))
                            })
                            .collect::<Result<HashMap<String, u64>, String>>()?,
                        _ => return Err(format!("versions_seen inner must be a dict: {:?}", v)),
                    };
                    Ok((node.clone(), inner))
                })
                .collect::<Result<HashMap<String, HashMap<String, u64>>, String>>()?,
            _ => return Err("Checkpoint versions_seen must be a dict".to_string()),
        };

        let pending_sends = match map.get("pending_sends") {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| {
                    let send_map = match item {
                        Value::Dict(m) => m,
                        _ => return Err("pending_sends item must be a dict".to_string()),
                    };
                    let target_node = match send_map.get("target_node") {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("pending_sends target_node must be a string".to_string()),
                    };
                    let input = match send_map.get("input") {
                        Some(v) => v.clone(),
                        None => return Err("pending_sends input missing".to_string()),
                    };
                    Ok(SendTask { target_node, input })
                })
                .collect::<Result<Vec<SendTask>, String>>()?,
            _ => return Err("Checkpoint pending_sends must be a list".to_string()),
        };

        // v0.104.6 D244：同族第三处。修前 `Value::Int(-1) as u128` 回绕成
        // `u128::MAX`，而 `timestamp_ms` 是 D234 三级排序键
        // `(step, timestamp_ms, id)` 的第二项 ⇒ 永远被 `load` 排到第一位。
        let timestamp_ms: u128 = match map.get("timestamp_ms") {
            Some(Value::String(s)) => s
                .parse::<u128>()
                .map_err(|e| format!("Invalid timestamp_ms: {}", e))?,
            other => nonneg_num("timestamp_ms", other)?,
        };

        Ok(Checkpoint {
            id,
            v,
            thread_id,
            step,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms,
        })
    }
}

// ============================================================
// CheckpointSaver trait
// ============================================================

/// Trait for checkpoint persistence backends.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// Pregel engine threads via `Arc<dyn CheckpointSaver>`.
pub trait CheckpointSaver: Send + Sync {
    /// Persist a checkpoint.
    fn save(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<(), MoraError>;

    /// Load a checkpoint by ID. If `checkpoint_id` is `None`, returns the
    /// latest checkpoint for the thread (highest `step`).
    fn load(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Checkpoint>, MoraError>;

    /// List all checkpoint IDs for a thread, ordered by step ascending.
    fn list(&self, thread_id: &str) -> Result<Vec<String>, MoraError>;

    /// Delete a checkpoint by ID.
    fn delete(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), MoraError>;
}

// ============================================================
// High-level helpers: rewind / resume
// ============================================================

/// Rewind: delete all checkpoints at or after `before_step`.
///
/// This is the "time travel" primitive: after rewinding, the next
/// `resume` will load the last checkpoint before `before_step`.
pub fn rewind(
    saver: &dyn CheckpointSaver,
    thread_id: &str,
    before_step: usize,
) -> Result<(), MoraError> {
    let ids = saver.list(thread_id)?;
    for id in ids {
        if let Some(cp) = saver.load(thread_id, Some(&id))?
            && cp.step >= before_step
        {
            saver.delete(thread_id, &id)?;
        }
    }
    Ok(())
}

/// Resume: load the latest checkpoint for a thread (highest `step`).
///
/// Returns `None` if no checkpoints exist for the thread.
pub fn resume(saver: &dyn CheckpointSaver, thread_id: &str) -> Result<Option<Checkpoint>, String> {
    let ids = saver.list(thread_id)?;
    let mut latest: Option<Checkpoint> = None;
    for id in ids {
        if let Some(cp) = saver.load(thread_id, Some(&id))?
            && latest.as_ref().is_none_or(|l| cp.step > l.step)
        {
            latest = Some(cp);
        }
    }
    Ok(latest)
}

// ============================================================
// Sub-modules
// ============================================================

mod memory;
pub use memory::MemorySaver;

#[cfg(feature = "checkpoint-sqlite")]
mod sqlite;
#[cfg(feature = "checkpoint-sqlite")]
pub use sqlite::SqliteSaver;

// ============================================================
// Unit tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal checkpoint for testing.
    fn make_checkpoint(
        id: &str,
        thread_id: &str,
        step: usize,
        channel_values: HashMap<String, Value>,
    ) -> Checkpoint {
        Checkpoint {
            id: id.to_string(),
            v: 1,
            thread_id: thread_id.to_string(),
            step,
            channel_values,
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: vec![],
            timestamp_ms: 0,
        }
    }

    #[test]
    fn checkpoint_roundtrip_json() {
        let mut channel_values = HashMap::new();
        channel_values.insert(
            "messages".to_string(),
            Value::List(
                vec![
                    Value::String("hello".to_string()),
                    Value::String("world".to_string()),
                ]
                .into(),
            ),
        );
        let mut channel_versions = HashMap::new();
        channel_versions.insert("messages".to_string(), 3);
        let mut versions_seen = HashMap::new();
        let mut inner = HashMap::new();
        inner.insert("messages".to_string(), 2);
        versions_seen.insert("node_a".to_string(), inner);
        let pending_sends = vec![SendTask {
            target_node: "process".to_string(),
            input: Value::Dict(
                [("task".to_string(), Value::String("split".to_string()))]
                    .into_iter()
                    .collect(),
            ),
        }];

        let cp = Checkpoint {
            id: "test-id".to_string(),
            v: 1,
            thread_id: "thread_1".to_string(),
            step: 5,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms: 1234567890123,
        };

        let json = cp.to_json().unwrap();
        let cp2 = Checkpoint::from_json(&json).unwrap();
        assert_eq!(cp, cp2);
    }

    #[test]
    fn resume_returns_latest() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("c", "t1", 2, HashMap::new()))
            .unwrap();

        let latest = resume(&saver, "t1").unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().step, 3);
    }

    #[test]
    fn rewind_deletes_later_steps() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("c", "t1", 5, HashMap::new()))
            .unwrap();

        rewind(&saver, "t1", 3).unwrap();

        let ids = saver.list("t1").unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "a");
    }

    #[test]
    fn memory_save_and_load_by_id() {
        let saver = MemorySaver::new();
        let cp = make_checkpoint(
            "id-1",
            "t1",
            0,
            [("x".to_string(), Value::Int(42))].into_iter().collect(),
        );
        saver.save("t1", &cp).unwrap();

        let loaded = saver.load("t1", Some("id-1")).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, "id-1");
    }

    #[test]
    fn memory_load_latest_without_id() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 0, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 2, HashMap::new()))
            .unwrap();

        let latest = saver.load("t1", None).unwrap();
        assert_eq!(latest.unwrap().id, "b");
    }

    #[test]
    fn memory_list_is_sorted_by_step() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("c", "t1", 5, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();

        let ids = saver.list("t1").unwrap();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn memory_delete_removes_checkpoint() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 2, HashMap::new()))
            .unwrap();

        saver.delete("t1", "a").unwrap();
        let ids = saver.list("t1").unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "b");
    }

    #[test]
    fn memory_isolation_between_threads() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t2", &make_checkpoint("b", "t2", 2, HashMap::new()))
            .unwrap();

        assert_eq!(saver.list("t1").unwrap().len(), 1);
        assert_eq!(saver.list("t2").unwrap().len(), 1);
        assert_eq!(saver.list("t1").unwrap()[0], "a");
        assert_eq!(saver.list("t2").unwrap()[0], "b");
    }

    #[test]
    fn memory_delete_nonexistent_is_noop() {
        let saver = MemorySaver::new();
        // Should not panic / error
        saver.delete("t1", "ghost").unwrap();
    }

    #[test]
    fn memory_load_nonexistent_returns_none() {
        let saver = MemorySaver::new();
        let result = saver.load("t1", Some("ghost")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resume_empty_thread_returns_none() {
        let saver = MemorySaver::new();
        let result = resume(&saver, "ghost").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn rewind_empty_thread_is_noop() {
        let saver = MemorySaver::new();
        rewind(&saver, "ghost", 0).unwrap();
    }

    #[test]
    fn checkpoint_new_generates_uuid() {
        let cp1 = Checkpoint::new(
            "t1".to_string(),
            0,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            vec![],
        );
        let cp2 = Checkpoint::new(
            "t1".to_string(),
            0,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            vec![],
        );
        assert_ne!(cp1.id, cp2.id);
        assert_eq!(cp1.v, 1);
        assert_eq!(cp1.thread_id, "t1");
    }

    #[test]
    fn checkpoint_from_json_rejects_missing_field() {
        let bad = r#"{"id":"x","v":1}"#;
        let result = Checkpoint::from_json(bad);
        assert!(result.is_err());
    }

    #[test]
    fn checkpoint_from_json_rejects_non_dict() {
        let bad = r#"[1,2,3]"#;
        let result = Checkpoint::from_json(bad);
        assert!(result.is_err());
    }

    /// v0.104.6 D244：负数守卫必须覆盖 `from_json` 里的**全部五个**数字字段。
    ///
    /// D148 只给 `v` / `step` 加了守卫，而同一函数里的 `channel_versions` /
    /// `versions_seen` / `timestamp_ms` 同样从外部 `Value` 转换却没有 ——
    /// 覆盖 5 处中的 2 处。实测修前喂 `-1` 会得到 `u64::MAX` / `u128::MAX`，
    /// 且整数 `as` **回绕**、浮点 `as` **饱和**，两个方向都零诊断。
    ///
    /// 放在模块内是因为它只需覆盖 `from_json` 本身（`SqliteSaver::load` 的
    /// 调用方也走这里）；字段名点名的完整判据在
    /// `tests/checkpoint_negative_number_guard.rs`。
    #[test]
    fn d244_from_json_rejects_negative_in_every_numeric_field() {
        let base = |v: &str, step: &str, cv: &str, vs: &str, ts: &str| -> String {
            format!(
                r#"{{"id":"cp1","v":{v},"thread_id":"t1","step":{step},
                    "channel_values":{{}},
                    "channel_versions":{cv},
                    "versions_seen":{vs},
                    "pending_sends":[],
                    "timestamp_ms":{ts}}}"#
            )
        };
        let ok_cv = r#"{"messages":2}"#;
        let ok_vs = r#"{"node_a":{"messages":1}}"#;

        // 基线：全部合法。
        assert!(
            Checkpoint::from_json(&base("1", "3", ok_cv, ok_vs, "1700000000000")).is_ok(),
            "基线 checkpoint 应当可解析"
        );

        // 五个字段 × 两种负数表示（整数回绕 / 浮点饱和）—— 全部必须报错。
        for (field, neg_i, neg_f) in [
            (
                "v",
                base("-1", "3", ok_cv, ok_vs, "1700000000000"),
                base("-1.0", "3", ok_cv, ok_vs, "1700000000000"),
            ),
            (
                "step",
                base("1", "-1", ok_cv, ok_vs, "1700000000000"),
                base("1", "-1.0", ok_cv, ok_vs, "1700000000000"),
            ),
            (
                "channel_versions",
                base("1", "3", r#"{"messages":-1}"#, ok_vs, "1700000000000"),
                base("1", "3", r#"{"messages":-1.0}"#, ok_vs, "1700000000000"),
            ),
            (
                "versions_seen",
                base(
                    "1",
                    "3",
                    ok_cv,
                    r#"{"node_a":{"messages":-1}}"#,
                    "1700000000000",
                ),
                base(
                    "1",
                    "3",
                    ok_cv,
                    r#"{"node_a":{"messages":-1.0}}"#,
                    "1700000000000",
                ),
            ),
            (
                "timestamp_ms",
                base("1", "3", ok_cv, ok_vs, "-1"),
                base("1", "3", ok_cv, ok_vs, "-1.0"),
            ),
        ] {
            for (label, json) in [("int", neg_i), ("float", neg_f)] {
                assert!(
                    Checkpoint::from_json(&json).is_err(),
                    "{field} 的负数（{label}）竟然被接受：{json}"
                );
            }
        }
    }
}
