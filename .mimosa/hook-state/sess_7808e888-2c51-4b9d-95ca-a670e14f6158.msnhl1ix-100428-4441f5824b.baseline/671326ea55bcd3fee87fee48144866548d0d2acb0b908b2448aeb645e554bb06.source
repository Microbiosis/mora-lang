//! v0.33: Schedule (cron) builtin
//!
//! 灵感: MimiClaw cron_service.c
//! (<https://github.com/memovai/mimiclaw/blob/main/cron/cron_service.c>)
//!
//! cron_job_t 9 字段 (MimiClaw):
//!   id (8-char hex) / name (32 char) / kind (EVERY / AT) /
//!   interval_s / at_epoch / message (256 char) /
//!   channel (16 char) / chat_id (96 char) / delete_after_run
//!
//! v0.33 简化版: 只实现核心 4 字段 (id / kind / interval_s / at_epoch / message),
//! 持久化到 `<cwd>`/`.mora_schedule.json (MimiClaw 用 SPIFFS; Mora 用 std::fs).
//!
//! 提供 builtin:
//!   schedule.add(name, kind, message, [interval_s | at_epoch]) -> id
//!   schedule.list() -> [{id, name, kind, message, ...}]
//!   schedule.remove(id) -> bool
//!   schedule.tick(now) -> [triggered messages]  (内部: 由 event loop 调用)

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Job kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Every,
    At,
}

/// v0.33: 调度 job
#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub kind: JobKind,
    /// Seconds between runs (for Every) or 0 (for At)
    pub interval_s: u64,
    /// Unix epoch seconds for execution (for At) or 0 (for Every)
    pub at_epoch: u64,
    pub message: String,
    /// Last run time (for Every) or 0
    pub last_run_epoch: u64,
    /// Delete after next run (default true for At)
    pub delete_after_run: bool,
}

/// v0.33: Scheduler
///
/// v0.75.2: tick 从 O(全部 job) 优化为 O(到期项) — BTreeMap 时间索引
/// （timer-wheel 家族，与 tokio 的哈希桶时间轮同思路）。
/// `buckets` 是稀疏的「next_fire_epoch → job ids」索引：`tick(now)` 用
/// `range(..=now)` 直接跳到有到期桶的时刻，零空推进、对 now 大跳跃免疫。
/// `jobs` 仍是事实来源（list/save/count 依赖它）；`remove` 只删 jobs，
/// 桶内 id 由 tick 触发时惰性清理。
#[derive(Clone, Default)]
pub struct Scheduler {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    /// v0.75.2: 到期时间索引（next_fire_epoch → job ids）。所有到期桶
    /// 恒在未来（add 已校验 At 需 at_epoch > now、Every 需 interval > 0）。
    buckets: Arc<Mutex<BTreeMap<u64, Vec<String>>>>,
    /// v0.36 (P1-1.8): AtomicU64 — was Mutex<u32> which overflowed at 4B adds.
    next_id: Arc<std::sync::atomic::AtomicU64>,
    /// Persistence file path (None = in-memory only)
    persist_path: Arc<Mutex<Option<PathBuf>>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.jobs.lock().map(|j| j.len()).unwrap_or(0);
        f.debug_struct("Scheduler").field("jobs", &count).finish()
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置持久化路径 (默认 None, 纯内存)
    pub fn set_persist_path(&self, path: PathBuf) {
        let mut p = self.persist_path.lock().expect("scheduler mutex poisoned");
        *p = Some(path);
    }

    /// 生成下一个 id (8-char hex from counter)
    /// v0.36 (P1-1.8): AtomicU64 fetch_add — no lock, no u32 overflow.
    fn next_job_id(&self) -> String {
        let n = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        format!("{:08x}", n)
    }

    /// 当前 unix epoch seconds
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 添加一个 cron job. 返回生成的 id.
    pub fn add(
        &self,
        name: &str,
        kind: JobKind,
        message: &str,
        interval_s: u64,
        at_epoch: u64,
    ) -> Result<String, String> {
        if name.is_empty() {
            return Err("schedule.add: name cannot be empty".to_string());
        }
        if message.is_empty() {
            return Err("schedule.add: message cannot be empty".to_string());
        }
        match kind {
            JobKind::Every => {
                if interval_s == 0 {
                    return Err("schedule.add: Every kind needs interval_s > 0".to_string());
                }
            }
            JobKind::At => {
                if at_epoch == 0 {
                    return Err("schedule.add: At kind needs at_epoch > 0".to_string());
                }
                if at_epoch <= Self::now() {
                    return Err(format!(
                        "schedule.add: at_epoch {} is in the past (now={})",
                        at_epoch,
                        Self::now()
                    ));
                }
            }
        }
        let id = self.next_job_id();
        let now = Self::now();
        let job = Job {
            id: id.clone(),
            name: name.to_string(),
            kind,
            interval_s,
            at_epoch,
            message: message.to_string(),
            // 让 Every job 第一次 tick 在 interval_s 后才 fire
            last_run_epoch: if kind == JobKind::Every { now } else { 0 },
            delete_after_run: kind == JobKind::At, // default true for At
        };
        // v0.75.2: next_fire → buckets 索引。Every 首次到期 = now + interval；
        // At 到期 = at_epoch（add 已校验恒在未来）。
        let next_fire = match kind {
            JobKind::Every => now + interval_s,
            JobKind::At => at_epoch,
        };
        self.jobs
            .lock()
            .expect("scheduler mutex poisoned")
            .insert(id.clone(), job);
        self.buckets
            .lock()
            .expect("scheduler mutex poisoned")
            .entry(next_fire)
            .or_default()
            .push(id.clone());
        self.save();
        Ok(id)
    }

    /// 列出所有 jobs
    pub fn list(&self) -> Vec<Job> {
        self.jobs
            .lock()
            .expect("scheduler mutex poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// 删除一个 job
    ///
    /// v0.75.2: 只删 jobs map（事实来源）。buckets 里的 id 不主动清理 —
    /// tick 触发时发现 job 缺失即跳过（惰性清理），避免 remove 需同时锁
    /// 两个 map / 维护 id→epoch 反查表。
    pub fn remove(&self, id: &str) -> bool {
        let removed = self
            .jobs
            .lock()
            .expect("scheduler mutex poisoned")
            .remove(id)
            .is_some();
        if removed {
            self.save();
        }
        removed
    }

    /// tick: 返回应该触发的 messages + 移除 delete_after_run 的 jobs.
    /// caller (event loop) 负责把 messages 注入 agent.
    ///
    /// v0.75.2: O(到期项) — 经 buckets 索引直接取走所有 ≤ now 的到期桶，
    /// 不再线性扫描全部 jobs。Every 触发后以 `now + interval_s` 重排入桶；
    /// At 触发即删（delete_after_run）。
    pub fn tick(&self, now: u64) -> Vec<String> {
        let mut jobs = self.jobs.lock().expect("scheduler mutex poisoned");
        let mut buckets = self.buckets.lock().expect("scheduler mutex poisoned");

        // 先 collect 再 remove，避免 range 迭代期间可变借用 buckets。
        let due: Vec<(u64, Vec<String>)> = buckets
            .range(..=now)
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (k, _) in &due {
            buckets.remove(k);
        }

        let mut triggered = Vec::new();
        for (_, ids) in due {
            for id in ids {
                // 惰性清理：remove 过的 job，桶内 id 在此被跳过。
                let Some(job) = jobs.get_mut(&id) else {
                    continue;
                };
                match job.kind {
                    JobKind::Every => {
                        if job.interval_s > 0 {
                            triggered.push(job.message.clone());
                            job.last_run_epoch = now;
                            // 非对齐：next 从当前时刻重算（与旧实现一致）
                            buckets.entry(now + job.interval_s).or_default().push(id);
                        }
                    }
                    JobKind::At => {
                        triggered.push(job.message.clone());
                        jobs.remove(&id);
                    }
                }
            }
        }
        drop(jobs);
        drop(buckets);
        if !triggered.is_empty() {
            self.save();
        }
        triggered
    }

    /// 当前 jobs 数 (test helper)
    pub fn count(&self) -> usize {
        self.jobs.lock().expect("scheduler mutex poisoned").len()
    }

    /// 持久化到 JSON (简单 dump, 不用 serde)
    fn save(&self) {
        let path_opt = self
            .persist_path
            .lock()
            .expect("scheduler mutex poisoned")
            .clone();
        if let Some(path) = path_opt {
            let jobs = self.list();
            // 简单 JSON 序列化 (不用 serde)
            let mut json = String::from("[\n");
            for (i, job) in jobs.iter().enumerate() {
                if i > 0 {
                    json.push_str(",\n");
                }
                json.push_str(&format!(
                    "  {{\"id\":\"{}\",\"name\":\"{}\",\"kind\":\"{}\",\"message\":\"{}\",\"interval_s\":{},\"at_epoch\":{},\"last_run_epoch\":{}}}",
                    job.id,
                    job.name,
                    match job.kind {
                        JobKind::Every => "every",
                        JobKind::At => "at",
                    },
                    job.message.replace('"', "\\\""),
                    job.interval_s,
                    job.at_epoch,
                    job.last_run_epoch
                ));
            }
            json.push_str("\n]\n");
            // 忽略写入错误 (best-effort persistence)
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_every_job() {
        let s = Scheduler::new();
        let id = s.add("test", JobKind::Every, "tick", 60, 0).unwrap();
        assert_eq!(id.len(), 8);
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn add_at_job() {
        let s = Scheduler::new();
        let future = Scheduler::now() + 3600;
        let id = s.add("future", JobKind::At, "wake", 0, future).unwrap();
        assert!(s.list().iter().any(|j| j.id == id));
    }

    #[test]
    fn add_at_past_fails() {
        let s = Scheduler::new();
        let past = Scheduler::now() - 100;
        let r = s.add("past", JobKind::At, "msg", 0, past);
        assert!(r.is_err());
    }

    #[test]
    fn add_every_zero_interval_fails() {
        let s = Scheduler::new();
        let r = s.add("bad", JobKind::Every, "msg", 0, 0);
        assert!(r.is_err());
    }

    #[test]
    fn add_empty_name_fails() {
        let s = Scheduler::new();
        let r = s.add("", JobKind::Every, "msg", 60, 0);
        assert!(r.is_err());
    }

    #[test]
    fn add_empty_message_fails() {
        let s = Scheduler::new();
        let r = s.add("name", JobKind::Every, "", 60, 0);
        assert!(r.is_err());
    }

    #[test]
    fn remove_job() {
        let s = Scheduler::new();
        let id = s.add("test", JobKind::Every, "msg", 60, 0).unwrap();
        assert_eq!(s.count(), 1);
        assert!(s.remove(&id));
        assert_eq!(s.count(), 0);
        assert!(!s.remove(&id)); // double-remove
    }

    #[test]
    fn tick_triggers_every_after_interval() {
        let s = Scheduler::new();
        s.add("tick", JobKind::Every, "msg", 60, 0).unwrap();
        // tick at now+0: last_run=0, next=60, 0 < 60 -> not fire
        let t0 = Scheduler::now();
        assert!(s.tick(t0).is_empty());
        // tick at now+60: next=60, 60 >= 60 -> fire
        let t1 = t0 + 60;
        let triggered = s.tick(t1);
        assert_eq!(triggered, vec!["msg".to_string()]);
        // 第二次 tick 60s 后再次触发
        let t2 = t1 + 60;
        assert_eq!(s.tick(t2), vec!["msg".to_string()]);
    }

    #[test]
    fn tick_triggers_at_then_removes() {
        let s = Scheduler::new();
        let target = Scheduler::now() + 100;
        s.add("once", JobKind::At, "boom", 0, target).unwrap();
        assert_eq!(s.count(), 1);
        // tick before target: not fire
        assert!(s.tick(target - 1).is_empty());
        assert_eq!(s.count(), 1);
        // tick at/after target: fire + remove
        let triggered = s.tick(target);
        assert_eq!(triggered, vec!["boom".to_string()]);
        assert_eq!(s.count(), 0); // delete_after_run
    }

    #[test]
    fn list_returns_all_jobs() {
        let s = Scheduler::new();
        s.add("a", JobKind::Every, "m1", 60, 0).unwrap();
        s.add("b", JobKind::Every, "m2", 120, 0).unwrap();
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join("mora_schedule_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("schedule.json");
        let _ = std::fs::remove_file(&path);

        // 1. add with persistence
        {
            let s = Scheduler::new();
            s.set_persist_path(path.clone());
            s.add("persisted", JobKind::Every, "saved", 60, 0).unwrap();
        }
        // file should exist now
        assert!(path.exists(), "schedule.json not written");

        // 2. read back
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("persisted"));
        assert!(content.contains("saved"));

        // cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    // ─── v0.75.2: BTreeMap 时间索引（tick O(到期项)）───

    #[test]
    fn tick_large_schedule_only_processes_due() {
        // 1000 个 Every job，interval 1..=1000。tick 只应处理到期桶 —
        // 结构性证明：任一时刻触发数 << 1000（无全量扫描）。
        let s = Scheduler::new();
        for i in 1..=1000u64 {
            s.add(
                &format!("job{}", i),
                JobKind::Every,
                &format!("m{}", i),
                i,
                0,
            )
            .unwrap();
        }
        assert_eq!(s.count(), 1000);
        // t0 在 add 之后取（≥ 所有 add 内部 now），消除跨秒边界：
        // interval=i 的 next_fire = now_add_i + i ≤ t0 + i 恒成立。
        let t0 = Scheduler::now();

        // interval=1 的 job 必然到期
        let triggered = s.tick(t0 + 1);
        assert!(triggered.contains(&"m1".to_string()));

        // interval ≤ 10 的到期；触发数远小于 1000
        let triggered = s.tick(t0 + 10);
        assert!(triggered.contains(&"m10".to_string()));
        assert!(
            triggered.len() < 100,
            "tick 不应全量扫描: got {}",
            triggered.len()
        );

        // 未到期的不被触发：interval=1000 的 next 远在未来
        assert!(!s.tick(t0 + 999).contains(&"m1000".to_string()));
        assert!(s.tick(t0 + 1000).contains(&"m1000".to_string()));
    }

    #[test]
    fn lazy_removal_bucket_cleanup() {
        // remove 只删 jobs；到期桶内的 id 由 tick 惰性清理，不产生触发。
        let s = Scheduler::new();
        let t0 = Scheduler::now();
        let id = s.add("gone", JobKind::Every, "ghost", 60, 0).unwrap();
        assert!(s.remove(&id));
        // 到期时刻 tick：job 已删，桶内 id 被跳过
        assert!(s.tick(t0 + 60).is_empty());
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn at_job_not_rescheduled() {
        // At 触发即删（delete_after_run），且不再进入任何到期桶。
        let s = Scheduler::new();
        let target = Scheduler::now() + 100;
        s.add("once", JobKind::At, "boom", 0, target).unwrap();
        assert_eq!(s.tick(target), vec!["boom".to_string()]);
        assert_eq!(s.count(), 0);
        // 再次 tick 更晚时刻：无残留触发
        assert!(s.tick(target + 1000).is_empty());
    }
}
