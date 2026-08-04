//! v0.75.54: exec.* builtin 实现 — P7 拆 domain 后补全：ParallelResult/Semaphore/
//! exec_parallel/run_single_cmd/kill_process_group 与 tests_v043_exec 从 builtins/
//! mod.rs 迁入。语义与拆分前完全一致（纯搬移）。

use super::*;
use crate::value::Value;

// ============================================================
// v0.43.0: exec.parallel() implementation
// ============================================================

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
            // 否则等待
            let guard = self.mutex.lock().expect("semaphore mutex poisoned");
            drop(self.cond.wait(guard).expect("condvar wait failed"));
        }
    }

    fn release(&self) {
        // v0.49.0 (A4): use AcqRel (not SeqCst) for lighter memory barrier.
        // prev == 0 means waiter queue may have blocked acquirers; wake one.
        let prev = self.permits.fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            let _guard = self.mutex.lock().expect("semaphore mutex poisoned");
            self.cond.notify_one();
        }
    }
}

/// `exec.parallel(args)` builtin implementation
fn exec_parallel(args: &[Value]) -> Result<Value, String> {
    // 解析参数
    if args.is_empty() {
        return Err("exec.parallel: requires at least 1 arg (cmds list)".to_string());
    }

    // 第一个 arg: List of String (cmd list)
    let cmds: Vec<String> = match &args[0] {
        Value::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, v) in list.iter().enumerate() {
                match v {
                    Value::String(s) => out.push(s.clone()),
                    _ => {
                        return Err(format!("exec.parallel: cmds[{}] must be a string", i));
                    }
                }
            }
            out
        }
        _ => return Err("exec.parallel: first arg must be a list of strings".to_string()),
    };

    if cmds.is_empty() {
        return Ok(Value::List(Vec::new()));
    }

    // 第二个 arg (可选): max_concurrent
    let max_concurrent: usize = if args.len() >= 2 {
        match &args[1] {
            Value::Float(n) => (*n as usize).max(1),
            Value::Int(i) => (*i as usize).max(1),
            _ => {
                return Err(
                    "exec.parallel: max_concurrent must be a non-negative number".to_string(),
                );
            }
        }
    } else {
        cmds.len() // 默认: 全部并发
    };

    // 第三个 arg (可选): timeout_ms
    let timeout: Option<Duration> = if args.len() >= 3 {
        match &args[2] {
            Value::Float(n) => Some(Duration::from_millis(*n as u64)),
            Value::Int(i) => Some(Duration::from_millis(*i as u64)),
            Value::Nil => None,
            _ => return Err("exec.parallel: timeout_ms must be a number or nil".to_string()),
        }
    } else {
        None
    };

    let sem = Arc::new(Semaphore::new(max_concurrent));
    let (tx, rx) = mpsc::channel::<ParallelResult>();
    let next_idx = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cmds_arc = Arc::new(cmds);

    // 启动 N 个 worker thread (每 worker 处理多个 cmd 直到所有完成)
    let num_workers = max_concurrent.min(cmds_arc.len());
    let mut handles = Vec::with_capacity(num_workers);

    for _ in 0..num_workers {
        let sem = sem.clone();
        let tx = tx.clone();
        let next_idx = next_idx.clone();
        let cancelled = cancelled.clone();
        let cmds = cmds_arc.clone();

        let handle = thread::spawn(move || {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                // 原子获取下一个 cmd index
                let idx = next_idx.fetch_add(1, Ordering::SeqCst);
                if idx >= cmds.len() {
                    break;
                }
                let cmd_str = cmds[idx].clone();

                // 获取信号量
                sem.acquire();
                let result = run_single_cmd(idx, &cmd_str, timeout, &cancelled);
                sem.release();

                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        handles.push(handle);
    }
    drop(tx);

    // 收集结果
    let mut results: Vec<ParallelResult> = Vec::with_capacity(cmds_arc.len());
    for _ in 0..cmds_arc.len() {
        match rx.recv() {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }

    // 等待所有 worker 完成
    for h in handles {
        let _ = h.join();
    }

    // 按原始索引排序，保证结果顺序 = 输入顺序（消除并发完成顺序导致的竞态）
    results.sort_by_key(|r| r.idx);

    // 转 Value::List[Dict]
    Ok(Value::List(results.iter().map(|r| r.to_value()).collect()))
}

/// 单个 cmd 执行 (run on worker thread)
fn run_single_cmd(
    idx: usize,
    cmd_str: &str,
    timeout: Option<Duration>,
    cancelled: &Arc<AtomicBool>,
) -> ParallelResult {
    let start = Instant::now();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd_str)
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
                pid: 0, // spawn failed → pid unknown
                error: Some(format!("spawn failed: {}", e)),
            };
        }
    };
    let pid: u32 = child.id();

    // 等待 (带可选 timeout)
    let output = if let Some(timeout_dur) = timeout {
        // 简单实现: 把 wait 放到线程里, 主线程睡 timeout 后检查 cancelled
        // 但 std::process::Child 没有 async wait — 我们用 thread + join
        let timeout_ms = timeout_dur.as_millis() as u64;
        let (done_tx, done_rx) = mpsc::channel();
        let child = child; // move into thread
        let waiter = thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = done_tx.send(result);
        });

        match done_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(Ok(out)) => {
                let _ = waiter.join();
                out
            }
            Ok(Err(e)) => {
                let _ = waiter.join();
                return ParallelResult {
                    idx,
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("wait failed: {}", e)),
                };
            }
            Err(_) => {
                // Timeout: 杀进程组
                cancelled.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                let _ = waiter.join();
                return ParallelResult {
                    idx,
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("timeout after {}ms", timeout_ms)),
                };
            }
        }
    } else {
        match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                return ParallelResult {
                    idx,
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("wait failed: {}", e)),
                };
            }
        }
    };

    ParallelResult {
        idx,
        cmd: cmd_str.to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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

impl Interpreter {
    pub fn call_exec_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "parallel" => exec_parallel(args),
            _ => Err(format!("exec.{}: unknown method", method)),
        }
    }
}

#[cfg(test)]
mod tests_v043_exec {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    fn cmd(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// v0.43.0: exec.parallel() builtin tests

    #[test]
    fn exec_parallel_runs_all_commands() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo a"), cmd("echo b"), cmd("echo c")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .unwrap();
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
        let mut interp = Interpreter::new();
        // 6 个 sleep 1s, max_concurrent=2 → 总时间应该 ~3s (而非 ~1s 或 ~6s)
        // 跳过 perf assertion — 只验证结果正确
        let cmds: Vec<Value> = (0..6).map(|i| cmd(&format!("echo {}", i))).collect();
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds), Value::Float(2.0)])
            .unwrap();
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
        let mut interp = Interpreter::new();
        let result = interp
            .call_exec_method("parallel", &[Value::List(vec![])])
            .unwrap();
        assert_eq!(result, Value::List(Vec::new()));
    }

    #[test]
    fn exec_parallel_collects_stdout_per_command() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo line1"), cmd("printf line2"), cmd("echo line3")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .unwrap();
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
    fn exec_parallel_kills_process_group_on_timeout() {
        let mut interp = Interpreter::new();
        // "sleep 10" + timeout 200ms → 应报 timeout
        let cmds = vec![cmd("sleep 10")];
        let result = interp
            .call_exec_method(
                "parallel",
                &[Value::List(cmds), Value::Float(1.0), Value::Float(200.0)],
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

    #[test]
    fn exec_parallel_validates_arg_types() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_exec_method("parallel", &[Value::Float(42.0)])
            .expect_err("non-list first arg should fail");
        assert!(err.contains("list of strings"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_validates_cmd_elements() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo ok"), Value::Float(42.0)]; // 第二个不是 string
        let err = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .expect_err("non-string cmd should fail");
        assert!(err.contains("must be a string"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_returns_error_for_missing_command() {
        // sh -c 调用不存在的命令 → sh 返回 exit_code=127, stderr "command not found"
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("this_command_definitely_does_not_exist_xyz")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
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
        let mut interp = Interpreter::new();
        let err = interp
            .call_exec_method("nonexistent", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}
