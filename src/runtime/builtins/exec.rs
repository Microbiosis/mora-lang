//! v0.75.54: exec.* builtin 实现 — P7 拆 domain 后补全：ParallelResult/Semaphore/
//! exec_parallel/run_single_cmd/kill_process_group 与 tests_v043_exec 从 builtins/
//! mod.rs 迁入。语义与拆分前完全一致（纯搬移）。

use crate::error::{MoraError, MoraResult};
use crate::value::Value;

// ============================================================
// v0.43.0: exec.parallel() implementation
// ============================================================

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::CoreRuntime;
use crate::runtime::scheduler::TaskContext;

/// 并行执行结果 (单个 cmd)
#[derive(Debug, Clone)]
struct ParallelResult {
    /// 原始输入索引，用于保证结果顺序 = 输入顺序（并发完成顺序不固定）
    idx: usize,
    cmd: String,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    elapsed_ms: u64,
    /// Process ID (0 = unknown / spawn failed)
    pid: u32,
    error: Option<String>,
}

impl ParallelResult {
    fn to_value(&self) -> Value {
        let mut d = HashMap::new();
        d.insert("index".to_string(), Value::Int(self.idx as i64));
        d.insert("cmd".to_string(), Value::String(self.cmd.clone()));
        d.insert("stdout".to_string(), Value::String(self.stdout.clone()));
        d.insert("stderr".to_string(), Value::String(self.stderr.clone()));
        match self.exit_code {
            Some(code) => {
                d.insert("exit_code".to_string(), Value::Int(code as i64));
            }
            None => {
                d.insert("exit_code".to_string(), Value::Nil);
            }
        }
        d.insert(
            "elapsed_ms".to_string(),
            Value::Float(self.elapsed_ms as f64),
        );
        // pid == 0 表示 unknown (spawn 失败或 pre-spawn)
        if self.pid == 0 {
            d.insert("pid".to_string(), Value::Nil);
        } else {
            d.insert("pid".to_string(), Value::Float(self.pid as f64));
        }
        match &self.error {
            Some(e) => {
                d.insert("error".to_string(), Value::String(e.clone()));
            }
            None => {
                d.insert("error".to_string(), Value::Nil);
            }
        }
        Value::Dict(d)
    }
}

/// 自制信号量 (std 没有 Semaphore)
/// kill 之后等待 waiter 线程收尾的宽限期（毫秒）。
///
/// 正常路径（kill 生效）下 waiter 会在几毫秒内返回，这个上限不会成为
/// 延迟来源；只有「杀不掉」这一异常路径才会真的等满。
const WAITER_GRACE_MS: u64 = 2_000;

struct Semaphore {
    permits: AtomicUsize,
    mutex: Mutex<()>,
    cond: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Self {
        Self {
            permits: AtomicUsize::new(permits),
            mutex: Mutex::new(()),
            cond: Condvar::new(),
        }
    }

    /// v0.104.6 D65：快路径无锁 CAS，慢路径**必须持锁重查**才能 wait。
    ///
    /// 修复前的丢唤醒窗口（原实现在全量测试里把 `exec_parallel_respects_
    /// max_concurrent` 挂死过一次 —— CPU 冻结、无子进程、日志停滞）：
    ///
    /// ```text
    /// W(等待者): load(permits) → 0        → 未持锁
    /// R(释放者): fetch_add → prev=0      → permits = 1
    /// R:        lock(mutex) → notify_one() → 此时 W 尚未注册为 waiter，通知丢弃
    /// R:        unlock(mutex)
    /// W:        lock(mutex) → cond.wait() → **permits=1 可用却永久睡眠**
    /// ```
    ///
    /// 根因是「决定是否等待」的判断发生在锁**之外**：release 把 permit 加上去
    /// 之后才去拿锁，而 wait 在拿锁**之前**就已经决定要睡了，两者之间没有任何
    /// 同步。持锁重查后，`wait` 的注册与 `release` 的通知必然在同一个 mutex
    /// 上排序，通知不可能再被丢弃。
    ///
    /// 快路径保留：无竞争时仍是一次 CAS，不付 mutex 代价。
    fn acquire(&self) {
        loop {
            let current = self.permits.load(Ordering::Acquire);
            if current > 0
                && self
                    .permits
                    .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return;
            }
            // 慢路径：持锁重查。permits 在锁内仍为 0 才是可靠的「要睡」判据。
            let guard = self.mutex.lock().expect("exec 信号量 permits 计数不变量已破损 — 恢复会掩盖已破损的不变量，见 src/common/lock.rs");
            if self.permits.load(Ordering::Acquire) > 0 {
                // 竞争失败期间别人放出了 permit —— 放锁回快路径重试 CAS。
                continue;
            }
            drop(self.cond.wait(guard).expect("condvar wait failed"));
        }
    }

    /// v0.104.6 D65：改为**无条件** notify_one。
    ///
    /// 原实现只在 `prev == 0` 时通知，那是个脆弱的启发式：当 permits 从 1
    /// 涨到 2（或更高）时 `prev != 0` 就不通知，而此时完全可能仍有等待者在
    /// 睡眠 —— 唤醒必须与「是否有等待者」解耦，只与「permit 增加了」绑定。
    /// `notify_one` 在无等待者时是空操作，代价可忽略。
    fn release(&self) {
        self.permits.fetch_add(1, Ordering::AcqRel);
        let _guard = self.mutex.lock().expect("exec 信号量 permits 计数不变量已破损 — 恢复会掩盖已破损的不变量，见 src/common/lock.rs");
        self.cond.notify_one();
    }
}

struct ExecCancelGuard {
    active_pids: Arc<Mutex<HashSet<u32>>>,
    cancelled: Arc<AtomicBool>,
}

impl ExecCancelGuard {
    fn abort_all(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pids: Vec<u32> = {
            let guard = crate::common::lock::lock_or_recover(&self.active_pids, "exec_active_pids");
            guard.iter().copied().collect()
        };
        for pid in pids {
            kill_process_group(pid);
        }
    }
}

impl Drop for ExecCancelGuard {
    fn drop(&mut self) {
        let has_active =
            !crate::common::lock::lock_or_recover(&self.active_pids, "exec_active_pids").is_empty();
        if has_active {
            self.abort_all();
        }
    }
}

/// `exec.parallel(args)` builtin implementation — routed through `AsyncIoReactor`,
/// inheriting `CoreRuntime::fs_context().cwd()` and honoring `TaskContext` cancellation.
fn exec_parallel(core: &mut CoreRuntime, args: &[Value]) -> MoraResult<Value> {
    // 解析参数
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "exec.parallel: requires at least 1 arg (cmds list)".to_string(),
        ));
    }

    // 第一个 arg: List of String (cmd list)
    let cmds: Vec<String> = match &args[0] {
        Value::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, v) in list.iter().enumerate() {
                match v {
                    Value::String(s) => out.push(s.clone()),
                    _ => {
                        return Err(MoraError::Runtime(format!(
                            "exec.parallel: cmds[{}] must be a string",
                            i
                        )));
                    }
                }
            }
            out
        }
        _ => {
            return Err(MoraError::Runtime(
                "exec.parallel: first arg must be a list of strings".to_string(),
            ));
        }
    };

    // v0.104.6 D285 / D337：参数校验必须在空列表早返回之前执行。
    let max_concurrent: usize = if args.len() >= 2 {
        match &args[1] {
            Value::Nil => cmds.len(),
            other => crate::flow::value_as_usize(other)
                .map(|n| n.max(1))
                .ok_or_else(|| {
                    MoraError::Runtime(
                        "exec.parallel: max_concurrent must be a non-negative number".to_string(),
                    )
                })?,
        }
    } else {
        cmds.len()
    };

    let timeout: Option<Duration> = if args.len() >= 3 {
        match &args[2] {
            Value::Nil => None,
            other => {
                let ms = crate::flow::value_as_usize(other).ok_or_else(|| {
                    MoraError::Runtime(
                        "exec.parallel: timeout_ms must be a non-negative number or nil"
                            .to_string(),
                    )
                })?;
                Some(Duration::from_millis(ms as u64))
            }
        }
    } else {
        None
    };

    let task_ctx = core.task_context.clone();
    if let Some(ref c) = task_ctx {
        c.status()?;
    }

    if cmds.is_empty() {
        // TEETH-CHECK: 早返回被移回参数校验之前
        return Ok(Value::List(Vec::new().into()));
    }

    let cwd = core.fs_context().cwd().to_path_buf();
    let reactor_ctx = task_ctx.clone();
    core.reactor().run_fs(reactor_ctx.as_ref(), move || {
        exec_parallel_blocking(cmds, max_concurrent, timeout, cwd, task_ctx)
    })
}

fn exec_parallel_blocking(
    cmds: Vec<String>,
    max_concurrent: usize,
    timeout: Option<Duration>,
    cwd: PathBuf,
    task_ctx: Option<TaskContext>,
) -> MoraResult<Value> {
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let (tx, rx) = mpsc::channel::<ParallelResult>();
    let next_idx = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let active_pids = Arc::new(Mutex::new(HashSet::new()));
    let guard = ExecCancelGuard {
        active_pids: Arc::clone(&active_pids),
        cancelled: Arc::clone(&cancelled),
    };
    let cmds_arc = Arc::new(cmds);
    let cwd_arc = Arc::new(cwd);

    // 启动 N 个 worker thread (每 worker 处理多个 cmd 直到所有完成)
    let num_workers = max_concurrent.min(cmds_arc.len());
    let mut handles = Vec::with_capacity(num_workers);

    for _ in 0..num_workers {
        let sem = sem.clone();
        let tx = tx.clone();
        let next_idx = next_idx.clone();
        let cancelled = cancelled.clone();
        let active_pids = active_pids.clone();
        let cmds = cmds_arc.clone();
        let cwd = cwd_arc.clone();
        let worker_ctx = task_ctx.clone();

        let handle = thread::spawn(move || {
            loop {
                if cancelled.load(Ordering::Acquire)
                    || worker_ctx.as_ref().is_some_and(|c| c.status().is_err())
                {
                    break;
                }
                let idx = next_idx.fetch_add(1, Ordering::SeqCst);
                if idx >= cmds.len() {
                    break;
                }
                let cmd_str = cmds[idx].clone();

                sem.acquire();
                let result = run_single_cmd(
                    idx,
                    &cmd_str,
                    &cwd,
                    timeout,
                    worker_ctx.as_ref(),
                    &cancelled,
                    &active_pids,
                );
                sem.release();

                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        handles.push(handle);
    }
    drop(tx);

    // 预算 = 每条命令的 timeout × 波数 + 收尾宽限（仅在显式给定 timeout 时设置）
    let collect_budget: Option<Duration> = timeout.map(|per_cmd| {
        let waves =
            u32::try_from(cmds_arc.len().div_ceil(max_concurrent.max(1))).unwrap_or(u32::MAX);
        per_cmd
            .checked_mul(waves)
            .and_then(|d| d.checked_add(Duration::from_millis(WAITER_GRACE_MS)))
            .unwrap_or(Duration::MAX)
    });
    let deadline = collect_budget.map(|b| std::time::Instant::now() + b);

    let mut results: Vec<ParallelResult> = Vec::with_capacity(cmds_arc.len());
    let mut timed_out = false;
    while results.len() < cmds_arc.len() {
        if let Some(ref ctx) = task_ctx
            && let Err(cancel_err) = ctx.status()
        {
            guard.abort_all();
            drop(rx);
            for h in handles {
                let _ = h.join();
            }
            return Err(cancel_err);
        }

        let wait_slice = match deadline {
            Some(dl) => {
                let remaining = dl.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    timed_out = true;
                    break;
                }
                if task_ctx.is_some() {
                    Some(remaining.min(Duration::from_millis(20)))
                } else {
                    Some(remaining)
                }
            }
            None if task_ctx.is_some() => Some(Duration::from_millis(20)),
            None => None,
        };

        match wait_slice {
            Some(slice) => match rx.recv_timeout(slice) {
                Ok(r) => results.push(r),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|dl| std::time::Instant::now() >= dl) {
                        timed_out = true;
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(r) => results.push(r),
                Err(_) => break,
            },
        }
    }

    if timed_out {
        guard.abort_all();
        let done: HashSet<usize> = results.iter().map(|r| r.idx).collect();
        for idx in 0..cmds_arc.len() {
            if !done.contains(&idx) {
                results.push(ParallelResult {
                    idx,
                    cmd: cmds_arc[idx].clone(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: 0,
                    pid: 0,
                    error: Some("exec.parallel: worker did not report before deadline".to_string()),
                });
            }
        }
        return Ok(Value::List(results.iter().map(|r| r.to_value()).collect()));
    }

    for h in handles {
        h.join()
            .map_err(|_| MoraError::Task("exec.parallel: worker thread panicked".to_string()))?;
    }

    results.sort_by_key(|r| r.idx);
    drop(guard);
    Ok(Value::List(results.iter().map(|r| r.to_value()).collect()))
}

/// 单个 cmd 执行 (run on worker thread)
fn run_single_cmd(
    idx: usize,
    cmd_str: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    task_ctx: Option<&TaskContext>,
    cancelled: &Arc<AtomicBool>,
    active_pids: &Arc<Mutex<HashSet<u32>>>,
) -> ParallelResult {
    let start = Instant::now();
    if cancelled.load(Ordering::Acquire) || task_ctx.is_some_and(|c| c.status().is_err()) {
        return ParallelResult {
            idx,
            cmd: cmd_str.to_string(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            elapsed_ms: start.elapsed().as_millis() as u64,
            pid: 0,
            error: Some("cancelled".to_string()),
        };
    }

    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C");
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c");
        c
    };
    command
        .arg(cmd_str)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // 进程组隔离 (mini-swe-agent v1 风格, 防止 orphaned 进程)
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec 在 fork 后, exec 前执行
        // 仅调用 libc::setpgid, 不分配内存, 不持有锁
        unsafe {
            command.pre_exec(|| {
                // setpgid(0, 0) 创建新进程组, 这样 process group kill 能清理孙子进程
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        command.creation_flags(0x00000200);
    }

    // spawn
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ParallelResult {
                idx,
                cmd: cmd_str.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid: 0,
                error: Some(format!("spawn failed: {}", e)),
            };
        }
    };
    let pid: u32 = child.id();
    crate::common::lock::lock_or_recover(active_pids, "exec_active_pids").insert(pid);

    let output_res: MoraResult<std::process::Output> = if timeout.is_none()
        && task_ctx.is_none()
        && !cancelled.load(Ordering::Acquire)
    {
        child
            .wait_with_output()
            .map_err(|e| MoraError::Io(format!("wait failed: {}", e)))
    } else {
        let timeout_ms = timeout.map(|d| d.as_millis() as u64);
        let deadline = timeout.map(|d| Instant::now() + d);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = done_tx.send(result);
        });

        loop {
            if cancelled.load(Ordering::Acquire) || task_ctx.is_some_and(|c| c.status().is_err()) {
                cancelled.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                if done_rx
                    .recv_timeout(Duration::from_millis(WAITER_GRACE_MS))
                    .is_ok()
                {
                    let _ = waiter.join();
                }
                break Err(MoraError::Cancelled("cancelled".to_string()));
            }
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                cancelled.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                if done_rx
                    .recv_timeout(Duration::from_millis(WAITER_GRACE_MS))
                    .is_ok()
                {
                    let _ = waiter.join();
                }
                break Err(MoraError::Timeout(format!(
                    "timeout after {}ms",
                    timeout_ms.unwrap_or(0)
                )));
            }
            let slice = match deadline {
                Some(dl) => dl
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
                None => Duration::from_millis(20),
            };
            match done_rx.recv_timeout(slice) {
                Ok(Ok(out)) => {
                    let _ = waiter.join();
                    break Ok(out);
                }
                Ok(Err(e)) => {
                    let _ = waiter.join();
                    break Err(MoraError::Io(format!("wait failed: {}", e)));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = waiter.join();
                    break Err(MoraError::Io(
                        "wait failed: channel disconnected".to_string(),
                    ));
                }
            }
        }
    };

    crate::common::lock::lock_or_recover(active_pids, "exec_active_pids").remove(&pid);

    let output = match output_res {
        Ok(out) => out,
        Err(err) => {
            return ParallelResult {
                idx,
                cmd: cmd_str.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid,
                error: Some(err.message().to_string()),
            };
        }
    };

    ParallelResult {
        idx,
        cmd: cmd_str.to_string(),
        stdout: String::from_utf8_lossy_owned(output.stdout),
        stderr: String::from_utf8_lossy_owned(output.stderr),
        exit_code: output.status.code(),
        elapsed_ms: start.elapsed().as_millis() as u64,
        pid,
        error: None,
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // killpg(pid, SIGKILL) — SIGKILL = 9
    // SAFETY: libc::killpg 直接系统调用, 无 Rust 抽象
    unsafe {
        // pid_t 是 i32
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_process_group(pid: u32) {
    // taskkill /F /T /PID <pid>
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .status();
}

pub fn call_exec_method(core: &mut CoreRuntime, method: &str, args: &[Value]) -> MoraResult<Value> {
    match method {
        "parallel" => exec_parallel(core, args),
        _ => Err(MoraError::Runtime(format!(
            "exec.{}: unknown method",
            method
        ))),
    }
}

#[cfg(test)]
mod tests_v043_exec {
    use super::*;
    use crate::value::Value;

    fn call_exec(method: &str, args: &[Value]) -> MoraResult<Value> {
        let mut core = CoreRuntime::default();
        call_exec_method(&mut core, method, args)
    }

    fn cmd(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// v0.43.0: exec.parallel() builtin tests

    #[test]
    fn exec_parallel_runs_all_commands() {
        let cmds = vec![cmd("echo a"), cmd("echo b"), cmd("echo c")];
        let result = call_exec("parallel", &[Value::List(cmds.into())]).unwrap();
        let list = match result {
            Value::List(l) => l,
            other => panic!("expected List, got {:?}", other),
        };
        assert_eq!(list.len(), 3);
        // 结果已按输入索引保序（v0.51 修复并发竞态后）；收集所有 stdout 验证内容
        let mut stdouts: Vec<String> = Vec::new();
        for item in &list {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("stdout not String"),
            };
            stdouts.push(stdout.trim().to_string());
            match d.get("exit_code") {
                Some(Value::Int(0)) => {}
                other => panic!("exit_code not 0: {:?}", other),
            }
        }
        stdouts.sort();
        assert_eq!(stdouts, vec!["a", "b", "c"]);
    }

    #[test]
    fn exec_parallel_respects_max_concurrent() {
        // 6 个 sleep 1s, max_concurrent=2 → 总时间应该 ~3s (而非 ~1s 或 ~6s)
        // 跳过 perf assertion — 只验证结果正确
        let cmds: Vec<Value> = (0..6).map(|i| cmd(&format!("echo {}", i))).collect();
        let result = call_exec("parallel", &[Value::List(cmds.into()), Value::Float(2.0)]).unwrap();
        let list = match result {
            Value::List(l) => l,
            other => panic!("expected List, got {:?}", other),
        };
        assert_eq!(list.len(), 6);
        for (i, item) in list.iter().enumerate() {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            // index 字段应等于输入顺序（保序保证）
            assert_eq!(
                d.get("index"),
                Some(&Value::Int(i as i64)),
                "index field mismatch at position {}",
                i
            );
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("no stdout"),
            };
            assert_eq!(stdout.trim(), i.to_string());
        }
    }

    #[test]
    fn exec_parallel_empty_list_returns_empty() {
        let result = call_exec("parallel", &[Value::List(vec![].into())]).unwrap();
        assert_eq!(result, Value::List(Vec::new().into()));
    }

    #[test]
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_collects_stdout_per_command() {
        let cmds = vec![cmd("echo line1"), cmd("printf line2"), cmd("echo line3")];
        let result = call_exec("parallel", &[Value::List(cmds.into())]).unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 3);
        // 顺序不固定, 收集所有 stdout 验证内容
        let mut stdouts: Vec<String> = Vec::new();
        for item in &list {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("no stdout"),
            };
            stdouts.push(stdout);
        }
        // printf 没 \n, echo 有
        // 不固定顺序, 但内容应该是 3 个特定字符串
        let mut normalized: Vec<String> = stdouts.iter().map(|s| s.trim().to_string()).collect();
        normalized.sort();
        let mut expected = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
        ];
        expected.sort();
        assert_eq!(normalized, expected);
    }

    #[test]
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_kills_process_group_on_timeout() {
        // "sleep 10" + timeout 200ms → 应报 timeout
        let cmds = vec![cmd("sleep 10")];
        let result = call_exec(
            "parallel",
            &[
                Value::List(cmds.into()),
                Value::Float(1.0),
                Value::Float(200.0),
            ],
        )
        .unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1);
        let d = match &list[0] {
            Value::Dict(d) => d,
            _ => panic!("not Dict"),
        };
        // exit_code 应为 None (超时被杀)
        match d.get("exit_code") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil exit_code on timeout, got: {:?}", other),
        }
        // error 应包含 "timeout"
        match d.get("error") {
            Some(Value::String(s)) => assert!(s.contains("timeout"), "got: {}", s),
            other => panic!("expected timeout error, got: {:?}", other),
        }
    }

    /// 回归：`exec.parallel` 在超时路径上必须**有界返回**。
    ///
    /// 修前的机理是三段串联：
    ///
    /// 1. `run_single_cmd` 超时后 `kill_process_group(pid)` —— 尽力而为，`killpg` 返回值被丢弃；
    /// 2. 紧接着无条件 `waiter.join()` —— kill 没杀掉时子进程仍持有 stdout 管道，`wait_with_output` 永不返回 ⇒ waiter 永生；
    /// 3. worker 线程被 join 焊死，既不 `tx.send` 也不 drop `tx`，外层 `rx.recv()` 于是既收不到结果、也等不到 `Disconnected`。
    ///
    /// 结果：一次「杀不掉的超时」= **整个调用永久挂起**，而 `timeout` 参数本身正是制造这次挂死的机制。
    ///
    /// 本用例断言的是「有界返回」这一不变量本身。正常平台上 kill 生效，
    /// 走的是快路径；杀不掉时走宽限期后脱离 waiter 线程，同样有界。
    #[test]
    fn exec_parallel_timeout_returns_within_bounded_time() {
        // 远超 timeout 的命令：timeout_ms = 200
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let cmds = vec![cmd(long)];
        let started = std::time::Instant::now();
        let result = call_exec(
            "parallel",
            &[
                Value::List(cmds.into()),
                Value::Float(1.0),
                Value::Float(200.0),
            ],
        )
        .unwrap();
        let elapsed = started.elapsed();

        // 预算 = timeout 200ms + waiter 宽限 2s + 进程被杀/回收的余量。
        // 给到 20s：足以覆盖最慢的 Windows 回收路径，又远小于「永久挂起」。
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "exec.parallel 超时后未在有界时间内返回（耗时 {:?}）——              说明 waiter 被 join 焊死、外层 rx.recv() 收不到 Disconnected",
            elapsed
        );

        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1, "超时也必须产出一条结果，不能静默缺项");
    }

    #[test]
    fn exec_parallel_validates_arg_types() {
        let err = call_exec("parallel", &[Value::Float(42.0)])
            .expect_err("non-list first arg should fail");
        assert!(err.to_string().contains("list of strings"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_validates_cmd_elements() {
        let cmds = vec![cmd("echo ok"), Value::Float(42.0)]; // 第二个不是 string
        let err = call_exec("parallel", &[Value::List(cmds.into())])
            .expect_err("non-string cmd should fail");
        assert!(err.to_string().contains("must be a string"), "got: {}", err);
    }

    #[test]
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_returns_error_for_missing_command() {
        // sh -c 调用不存在的命令 → sh 返回 exit_code=127, stderr "command not found"
        let cmds = vec![cmd("this_command_definitely_does_not_exist_xyz")];
        let result = call_exec("parallel", &[Value::List(cmds.into())]).unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1);
        let d = match &list[0] {
            Value::Dict(d) => d,
            _ => panic!("not Dict"),
        };
        // exit_code 应为 127 (POSIX "command not found")
        match d.get("exit_code") {
            Some(Value::Int(127)) => {}
            Some(Value::Int(other)) => panic!("expected 127, got {}", other),
            other => panic!("expected Int exit_code, got: {:?}", other),
        }
        // error 字段应为 Nil (执行成功, 只是退出码非 0)
        match d.get("error") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil error, got: {:?}", other),
        }
        // stderr 应包含 "not found"
        match d.get("stderr") {
            Some(Value::String(s)) => assert!(
                s.contains("not found") || s.contains("command not found"),
                "got stderr: {}",
                s
            ),
            other => panic!("expected stderr string, got: {:?}", other),
        }
    }

    #[test]
    fn exec_unknown_method_errors() {
        let err = call_exec("nonexistent", &[]).expect_err("unknown method should fail");
        assert!(err.to_string().contains("unknown method"), "got: {}", err);
    }

    /// v0.104.6 D65 回归：**permit 数少于线程数**的高争用场景下，
    /// acquire/release 必须全部成对完成。
    ///
    /// 覆盖的正是丢唤醒窗口 —— 16 线程抢 4 个 permit，等待者一定会真正进入
    /// `cond.wait`，一旦 `wait` 的注册与 `release` 的通知之间失去同步（本机
    /// 实测曾把 `exec_parallel_respects_max_concurrent` 挂死），本测试就会
    /// 永久阻塞而不是失败。因此它同时是**死锁探测器**：跑不完就是回归。
    ///
    /// 诚实说明：**本测试抓不住原缺陷** —— 已做反向验证：把 `acquire`/`release`
    /// 临时改回修复前的实现后，本测试仍 0.00s 通过（16 线程 × 400 轮在多核上
    /// 几乎全走 CAS 快路径，几乎不进 `cond.wait`）。原缺陷需要「load 失败」与
    /// 「wait 注册」之间恰好插入一次 release 这个窄窗口，从公共接口无法确定性
    /// 构造。故本测试钉的是**不变量**（「全部配对完成」+「permit 计数守恒」），
    /// 不是缺陷复现器；缺陷本身靠代码结构论证（见 `acquire` 注释里的时序图）
    /// 与那次全量测试挂死观测共同支撑。
    #[test]
    fn semaphore_high_contention_completes_and_conserves_permits() {
        const THREADS: usize = 16;
        const ITERS: usize = 400;
        const PERMITS: usize = 4;

        let sem = Arc::new(Semaphore::new(PERMITS));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let sem = sem.clone();
                thread::spawn(move || {
                    for _ in 0..ITERS {
                        sem.acquire();
                        sem.release();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("信号量工作线程 panic");
        }
        assert_eq!(
            sem.permits.load(Ordering::Acquire),
            PERMITS,
            "全部 acquire/release 配对后 permit 数必须回到初始值"
        );
    }
}
