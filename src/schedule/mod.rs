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

use std::collections::HashMap;
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
#[derive(Clone, Default)]
pub struct Scheduler {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    /// Counter for id generation
    next_id: Arc<Mutex<u32>>,
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
    fn next_job_id(&self) -> String {
        let mut counter = self.next_id.lock().expect("scheduler mutex poisoned");
        *counter += 1;
        format!("{:08x}", *counter)
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
        self.jobs
            .lock()
            .expect("scheduler mutex poisoned")
            .insert(id.clone(), job);
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

    /// tick: 扫描所有 jobs, 返回应该触发的 messages + 移除 delete_after_run 的 jobs.
    /// caller (event loop) 负责把 messages 注入 agent.
    pub fn tick(&self, now: u64) -> Vec<String> {
        let mut jobs = self.jobs.lock().expect("scheduler mutex poisoned");
        let mut triggered = Vec::new();
        let mut to_remove = Vec::new();
        for (id, job) in jobs.iter_mut() {
            let should_fire = match job.kind {
                JobKind::Every => {
                    if job.interval_s > 0 {
                        let next = job.last_run_epoch + job.interval_s;
                        now >= next
                    } else {
                        false
                    }
                }
                JobKind::At => now >= job.at_epoch,
            };
            if should_fire {
                triggered.push(job.message.clone());
                job.last_run_epoch = now;
                if job.delete_after_run {
                    to_remove.push(id.clone());
                }
            }
        }
        for id in &to_remove {
            jobs.remove(id);
        }
        drop(jobs);
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
}
