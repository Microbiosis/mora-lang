//! `exec.*` builtin implementation — zero-dependency native async process runner
//! built on `std::process::Command`, `TaskScheduler`, `TaskGroup`, and a native
//! `AsyncSemaphore`, honoring `FsContext::cwd()`, `TaskContext` cooperative cancellation,
//! per-command timeout, and process-group teardown.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::common::sync::Mutex;
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::scheduler::{Select2, TaskContext, TaskScheduler, race2, sleep};
use crate::value::Value;

// ─── Native Async Semaphore ──────────────────────────────────────────────────

struct SemaphoreState {
    permits: usize,
    waiters: VecDeque<(u64, Waker)>,
    next_id: u64,
}

struct AsyncSemaphore {
    state: Mutex<SemaphoreState>,
}

struct OwnedSemaphorePermit {
    sem: Arc<AsyncSemaphore>,
}

impl Drop for OwnedSemaphorePermit {
    fn drop(&mut self) {
        let waker = {
            let mut st = self.sem.state.lock();
            st.permits += 1;
            st.waiters.pop_front().map(|(_, w)| w)
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl AsyncSemaphore {
    fn new(permits: usize) -> Self {
        Self {
            state: Mutex::new(SemaphoreState {
                permits,
                waiters: VecDeque::new(),
                next_id: 0,
            }),
        }
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.state.lock().permits
    }

    async fn acquire_owned(self: Arc<Self>) -> OwnedSemaphorePermit {
        let mut waiter_id = 0u64;
        std::future::poll_fn(|cx| {
            let mut st = self.state.lock();
            if st.permits > 0 {
                st.permits -= 1;
                if waiter_id != 0 {
                    st.waiters.retain(|(id, _)| *id != waiter_id);
                }
                return Poll::Ready(());
            }
            if waiter_id == 0 {
                st.next_id += 1;
                waiter_id = st.next_id;
                st.waiters.push_back((waiter_id, cx.waker().clone()));
            } else if let Some(entry) = st.waiters.iter_mut().find(|(id, _)| *id == waiter_id) {
                entry.1 = cx.waker().clone();
            } else {
                st.waiters.push_back((waiter_id, cx.waker().clone()));
            }
            Poll::Pending
        })
        .await;
        OwnedSemaphorePermit { sem: self }
    }
}

/// Parallel command execution outcome for a single command entry.
#[derive(Debug, Clone)]
struct ParallelResult {
    /// Original input index so results preserve caller ordering regardless of completion order.
    idx: usize,
    cmd: String,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    elapsed_ms: u64,
    /// Process ID (`0` = unknown / spawn failed).
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

/// Grace period after process-group kill to let child stdout/stderr pipes close cleanly.
const KILL_GRACE_MS: u64 = 1_500;

struct ActivePidsGuard {
    active_pids: Arc<Mutex<HashSet<u32>>>,
}

impl ActivePidsGuard {
    fn abort_all(&self) {
        let pids: Vec<u32> = self.active_pids.lock().iter().copied().collect();
        for pid in pids {
            kill_process_group_sync(pid);
        }
    }
}

impl Drop for ActivePidsGuard {
    fn drop(&mut self) {
        let has_active = !self.active_pids.lock().is_empty();
        if has_active {
            self.abort_all();
        }
    }
}

/// `exec.parallel(cmds, max_concurrent?, timeout_ms?)` driven by `TaskScheduler` and `AsyncSemaphore`.
fn exec_parallel(core: &mut CoreRuntime, args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "exec.parallel: requires at least 1 arg (cmds list)".to_string(),
        ));
    }

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

    // Parameter validation MUST execute before the empty-list fast return (D285 / D337).
    let max_concurrent: usize = if args.len() >= 2 {
        match &args[1] {
            Value::Nil => cmds.len().max(1),
            other => crate::flow::value_as_usize(other)
                .map(|n| n.max(1))
                .ok_or_else(|| {
                    MoraError::Runtime(
                        "exec.parallel: max_concurrent must be a non-negative number".to_string(),
                    )
                })?,
        }
    } else {
        cmds.len().max(1)
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
        return Ok(Value::List(Vec::new().into()));
    }

    let cwd = core.fs_context().cwd().to_path_buf();
    let sched = Arc::clone(core.reactor().scheduler());
    sched.block_on(exec_parallel_async(
        sched.clone(),
        cmds,
        max_concurrent,
        timeout,
        cwd,
        task_ctx,
    ))
}

async fn exec_parallel_async(
    sched: Arc<TaskScheduler>,
    cmds: Vec<String>,
    max_concurrent: usize,
    timeout: Option<Duration>,
    cwd: PathBuf,
    task_ctx: Option<TaskContext>,
) -> MoraResult<Value> {
    let sem = Arc::new(AsyncSemaphore::new(max_concurrent.max(1)));
    let active_pids = Arc::new(Mutex::new(HashSet::new()));
    let guard = ActivePidsGuard {
        active_pids: Arc::clone(&active_pids),
    };
    let cwd_arc = Arc::new(cwd);

    let mut group = sched.group::<ParallelResult>(task_ctx.clone());
    for (idx, cmd_str) in cmds.into_iter().enumerate() {
        let sem = Arc::clone(&sem);
        let active_pids = Arc::clone(&active_pids);
        let cwd = Arc::clone(&cwd_arc);
        let ctx = task_ctx.clone();
        group.spawn(move |_child_ctx| async move {
            let permit = match ctx.as_ref() {
                Some(c) => match race2(c.cancelled(), Arc::clone(&sem).acquire_owned()).await {
                    Select2::First(_) => {
                        return Ok(ParallelResult {
                            idx,
                            cmd: cmd_str,
                            stdout: String::new(),
                            stderr: String::new(),
                            exit_code: None,
                            elapsed_ms: 0,
                            pid: 0,
                            error: Some("cancelled".to_string()),
                        });
                    }
                    Select2::Second(p) => p,
                },
                None => Arc::clone(&sem).acquire_owned().await,
            };
            let result = run_single_cmd_async(idx, cmd_str, cwd, timeout, ctx, active_pids).await;
            drop(permit);
            Ok(result)
        });
    }

    let mut results = Vec::new();
    while let Some(joined) = group.join_next().await {
        match joined {
            Ok(res) => results.push(res),
            Err(err) => {
                guard.abort_all();
                group.abort_all();
                return Err(err);
            }
        }
    }

    results.sort_by_key(|r| r.idx);
    drop(guard);
    Ok(Value::List(results.iter().map(|r| r.to_value()).collect()))
}

async fn run_single_cmd_async(
    idx: usize,
    cmd_str: String,
    cwd: Arc<PathBuf>,
    timeout: Option<Duration>,
    task_ctx: Option<TaskContext>,
    active_pids: Arc<Mutex<HashSet<u32>>>,
) -> ParallelResult {
    let start = Instant::now();
    if task_ctx.as_ref().is_some_and(|c| c.status().is_err()) {
        return ParallelResult {
            idx,
            cmd: cmd_str,
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
        .arg(&cmd_str)
        .current_dir(cwd.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        // SAFETY: pre_exec executes between fork and exec; only calls async-signal-safe setpgid.
        unsafe {
            command.pre_exec(|| {
                crate::common::sys::unix::setpgid(0, 0);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        command.creation_flags(0x0000_0200);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ParallelResult {
                idx,
                cmd: cmd_str,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid: 0,
                error: Some(format!("spawn failed: {e}")),
            };
        }
    };
    let pid = child.id();
    if pid != 0 {
        active_pids.lock().insert(pid);
    }

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut out) = stdout_pipe {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut err) = stderr_pipe {
            let _ = err.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = timeout.and_then(|dur| start.checked_add(dur));
    let status_res: MoraResult<std::process::ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(MoraError::Io(format!("wait failed: {e}"))),
        }
        if let Some(ref ctx) = task_ctx
            && let Err(err) = ctx.status()
        {
            kill_process_group_sync(pid);
            let _ = child.kill();
            let _ = child.wait();
            break Err(err);
        }
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            kill_process_group_sync(pid);
            let _ = child.kill();
            let _ = child.wait();
            let ms = timeout.map(|d| d.as_millis()).unwrap_or(0);
            break Err(MoraError::Timeout(format!("timeout after {ms}ms")));
        }
        sleep(Duration::from_millis(2)).await;
    };

    if pid != 0 {
        active_pids.lock().remove(&pid);
    }

    match status_res {
        Ok(status) => {
            let out_bytes = stdout_thread.join().unwrap_or_default();
            let err_bytes = stderr_thread.join().unwrap_or_default();
            ParallelResult {
                idx,
                cmd: cmd_str,
                stdout: String::from_utf8_lossy(&out_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
                exit_code: status.code(),
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid,
                error: None,
            }
        }
        Err(err) => {
            // Bound pipe drain by KILL_GRACE_MS on killed process groups
            let _ = sleep(Duration::from_millis(KILL_GRACE_MS.min(20))).await;
            ParallelResult {
                idx,
                cmd: cmd_str,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid,
                error: Some(err.message().to_string()),
            }
        }
    }
}

#[cfg(unix)]
fn kill_process_group_sync(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: POSIX `killpg` direct syscall to tear down the isolated process group.
    unsafe {
        crate::common::sys::unix::killpg(pid as i32, crate::common::sys::unix::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_process_group_sync(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

    #[test]
    fn exec_parallel_runs_all_commands() {
        let cmds = vec![cmd("echo a"), cmd("echo b"), cmd("echo c")];
        let result = call_exec("parallel", &[Value::List(cmds.into())]).unwrap();
        let list = match result {
            Value::List(l) => l,
            other => panic!("expected List, got {:?}", other),
        };
        assert_eq!(list.len(), 3);
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
    fn exec_parallel_collects_stdout_per_command() {
        let cmds = vec![cmd("echo line1"), cmd("printf line2"), cmd("echo line3")];
        let result = call_exec("parallel", &[Value::List(cmds.into())]).unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 3);
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
    fn exec_parallel_kills_process_group_on_timeout() {
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
        match d.get("exit_code") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil exit_code on timeout, got: {:?}", other),
        }
        match d.get("error") {
            Some(Value::String(s)) => assert!(s.contains("timeout"), "got: {}", s),
            other => panic!("expected timeout error, got: {:?}", other),
        }
    }

    #[test]
    fn exec_parallel_timeout_returns_within_bounded_time() {
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
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "exec.parallel did not return within bounded time after timeout (elapsed {:?})",
            elapsed
        );
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn exec_parallel_validates_arg_types() {
        let err = call_exec("parallel", &[Value::Float(42.0)])
            .expect_err("non-list first arg should fail");
        assert!(err.to_string().contains("list of strings"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_validates_cmd_elements() {
        let cmds = vec![cmd("echo ok"), Value::Float(42.0)];
        let err = call_exec("parallel", &[Value::List(cmds.into())])
            .expect_err("non-string cmd should fail");
        assert!(err.to_string().contains("must be a string"), "got: {}", err);
    }

    #[test]
    #[cfg(unix)]
    fn exec_parallel_returns_error_for_missing_command() {
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
        match d.get("exit_code") {
            Some(Value::Int(127)) => {}
            Some(Value::Int(other)) => panic!("expected 127, got {}", other),
            other => panic!("expected Int exit_code, got: {:?}", other),
        }
        match d.get("error") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil error, got: {:?}", other),
        }
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

    #[test]
    fn native_semaphore_high_contention_completes_and_conserves_permits() {
        const TASKS: usize = 32;
        const ITERS: usize = 200;
        const PERMITS: usize = 4;

        let core = CoreRuntime::default();
        let sched = Arc::clone(core.reactor().scheduler());
        let sem = Arc::new(AsyncSemaphore::new(PERMITS));
        let remaining = sched.block_on(async {
            let mut group = sched.group::<()>(None);
            for _ in 0..TASKS {
                let s = Arc::clone(&sem);
                group.spawn(move |_c| async move {
                    for _ in 0..ITERS {
                        let permit = Arc::clone(&s).acquire_owned().await;
                        drop(permit);
                    }
                    Ok(())
                });
            }
            group.join_all().await.expect("group succeeded");
            sem.available_permits()
        });
        assert_eq!(remaining, PERMITS);
    }
}
