//! v0.50: MemorySaver — in-memory checkpoint storage for testing and ephemeral workflows.

use super::{Checkpoint, CheckpointSaver};
use crate::error::MoraError;
use parking_lot::Mutex;
use std::collections::HashMap;

/// In-memory checkpoint storage.
///
/// Checkpoints are organized by `thread_id` and kept in insertion order.
/// All operations are `Mutex`-guarded for thread-safe access.
pub struct MemorySaver {
    checkpoints: Mutex<HashMap<String, Vec<Checkpoint>>>,
}

impl Default for MemorySaver {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySaver {
    pub fn new() -> Self {
        Self {
            checkpoints: Mutex::new(HashMap::new()),
        }
    }
}

impl CheckpointSaver for MemorySaver {
    /// v0.104.6 D234：**先经 `to_json` 校验**，再存原始对象。
    ///
    /// 修前直接 `push(checkpoint.clone())`，**绕过**了 `to_json` ——
    /// 于是 D233 新增的「不可逆值必须报错」检查对 `MemorySaver` **完全无效**：
    ///
    /// | 输入 | 修前 MemorySaver | 修前 SqliteSaver |
    /// |---|---|---|
    /// | `Char('中')` | `Ok`（存下原始 `Char`） | **报错** |
    ///
    /// 同一 trait、同一输入，两个实现给出**相反**的结果 —— 用 MemorySaver
    /// 跑通的流程换 SQLite 就会挂（且反之亦然）。
    ///
    /// 修法：**校验**走 `to_json`（拿到 D233 的检查），**存储**仍存原始对象
    /// （内存 saver 不需要序列化，读回零成本）。这样两个实现在
    /// 「什么能被接受」上**完全一致**。
    fn save(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<(), MoraError> {
        // v0.104.6 D234：与 SqliteSaver 走同一道校验
        checkpoint.to_json()?;

        let mut guard = self.checkpoints.lock();
        let entry = guard.entry(thread_id.to_string()).or_default();

        // v0.104.6 D234：同 id 重复保存应**替换**而非追加。
        //
        // 修前无条件 `push` ⇒ `list()` 返回 `["dup","dup"]`（**同一 id 出现两次**），
        // 而 `load(Some("dup"))` 取到的是**第一次**保存的那份（step=1）——
        // `SqliteSaver` 用 `INSERT OR REPLACE`，同一 id 存两次后
        // `list()` 是 `["dup"]` 且 load 得 step=2（后写的）。
        //
        // 两处分叉的根因都是「同 id 该不该替换」在 trait 文档里没写。
        // 现取 **SqliteSaver 的语义**（替换）：`insert_or_replace` 之后
        // `list()` 无重复，`load` 取到最新。
        if let Some(pos) = entry.iter().position(|cp| cp.id == checkpoint.id) {
            entry[pos] = checkpoint.clone();
        } else {
            entry.push(checkpoint.clone());
        }
        Ok(())
    }

    fn load(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Checkpoint>, MoraError> {
        let guard = self.checkpoints.lock();
        let entry = match guard.get(thread_id) {
            None => return Ok(None),
            Some(v) => v,
        };

        match checkpoint_id {
            None => {
                // v0.104.6 D234：「最新」必须有**确定性**的 tie-break。
                //
                // 修前是 `max_by_key(|cp| cp.step)` —— Rust 的 `max_by_key`
                // 在**最大值并列**时返回**最后一个**匹配的；
                // 而 `SqliteSaver` 的 `ORDER BY step DESC LIMIT 1`
                // 在 step 相同时返回 SQLite 碰巧先吐出的那行（实测是先写入的）。
                //
                // ⇒ 两个 checkpoint 同 step 时，恢复到的**状态不同**
                // （实测 memory 得 `b`、sqlite 得 `a`）。
                // 而同 step 完全正常：`pregel` 的 fault-retry 会重跑同一步，
                // 两条路径都往同一步写检查点。
                //
                // 现统一用 `(step, timestamp_ms, id)` 三级比较 ——
                // 全序、两边一致、且与「后写入的更新」直觉相符。
                let latest = entry.iter().max_by(|a, b| {
                    a.step
                        .cmp(&b.step)
                        .then_with(|| a.timestamp_ms.cmp(&b.timestamp_ms))
                        .then_with(|| a.id.cmp(&b.id))
                });
                Ok(latest.cloned())
            }
            Some(id) => Ok(entry.iter().find(|cp| cp.id == id).cloned()),
        }
    }

    fn list(&self, thread_id: &str) -> Result<Vec<String>, MoraError> {
        let guard = self.checkpoints.lock();
        let entry = match guard.get(thread_id) {
            None => return Ok(vec![]),
            Some(v) => v,
        };

        // v0.104.6 D234：按 `(step, timestamp_ms, id)` 排序，与
        // `SqliteSaver::list` 的 `ORDER BY step ASC, timestamp_ms ASC, id ASC`
        // 逐项对应。
        //
        // 修前只用 `sort_by_key(step)`：Rust 的 `sort_by_key` 是**稳定**排序
        // （同 step 保持插入序），而 SQLite 的 `ORDER BY step ASC` 在同 step
        // 时**不保证**顺序 ⇒ 同 step 的多个检查点会列出不同次序。
        let mut ordered: Vec<&Checkpoint> = entry.iter().collect();
        ordered.sort_by(|a, b| {
            a.step
                .cmp(&b.step)
                .then_with(|| a.timestamp_ms.cmp(&b.timestamp_ms))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(ordered.into_iter().map(|cp| cp.id.clone()).collect())
    }

    fn delete(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), MoraError> {
        let mut guard = self.checkpoints.lock();
        if let Some(entry) = guard.get_mut(thread_id) {
            entry.retain(|cp| cp.id != checkpoint_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_saver_smoke() {
        let saver = MemorySaver::new();
        let cp = Checkpoint {
            id: "smoke".to_string(),
            v: 1,
            thread_id: "t".to_string(),
            step: 0,
            channel_values: HashMap::new(),
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: vec![],
            timestamp_ms: 0,
        };
        saver.save("t", &cp).unwrap();
        let loaded = saver.load("t", None).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, "smoke");
    }

    #[test]
    fn memory_saver_latest_by_step() {
        let saver = MemorySaver::new();
        saver
            .save(
                "t",
                &Checkpoint {
                    id: "a".to_string(),
                    v: 1,
                    thread_id: "t".to_string(),
                    step: 1,
                    channel_values: HashMap::new(),
                    channel_versions: HashMap::new(),
                    versions_seen: HashMap::new(),
                    pending_sends: vec![],
                    timestamp_ms: 0,
                },
            )
            .unwrap();
        saver
            .save(
                "t",
                &Checkpoint {
                    id: "b".to_string(),
                    v: 1,
                    thread_id: "t".to_string(),
                    step: 3,
                    channel_values: HashMap::new(),
                    channel_versions: HashMap::new(),
                    versions_seen: HashMap::new(),
                    pending_sends: vec![],
                    timestamp_ms: 0,
                },
            )
            .unwrap();

        let latest = saver.load("t", None).unwrap();
        assert_eq!(latest.unwrap().id, "b");
    }
}
