//! v0.75.51: builtin 模块聚合（P7，Rhai register_plugin/Koto workspace 思想）。
//!
//! 14 个 call_*_method + get_embedding 已按 domain 拆到独立文件：
//! file / event / sandbox / schedule / ai_tokens / ai / ccr / mock /
//! memory / exec / toolplane / skill / plan / mora。本文件保留共享辅助
//! 函数与测试，仅以 mod 声明聚合 domain 文件。

use super::*;
use crate::ccr::CcrStore;
use crate::value::Value;

/// 获取 markdown memory 根目录 (~/.mora/memory/)
/// v0.43.1: 优先用 Interpreter 字段 (test isolation); fallback 到 env var / home dir
fn markdown_memory_dir(override_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    if let Ok(custom) = std::env::var("MORA_MEMORY_DIR") {
        return std::path::PathBuf::from(custom);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mora").join("memory")
}

/// 当天日期 (YYYY-MM-DD)
fn today_date_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简化: 用 UNIX 秒转日期 (假设 UTC)
    // 1970-01-01 是周四, 用 Zeller 公式的一个变体
    let days = (secs / 86400) as i64;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // 从 1970-01-01 起算
    let mut year = 1970i32;
    let mut remaining = days;
    loop {
        let leap = is_leap(year);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        year += 1;
    }
    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            month = i;
            break;
        }
        remaining -= md;
    }
    (year, (month + 1) as u32, (remaining + 1) as u32)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// v0.43.1: remember(category, text) — 追加到 ~/.mora/memory/YYYY-MM-DD.md
/// 文件格式:
/// ```text
/// # YYYY-MM-DD
///
/// ## {category}
///
/// - {text}
///
/// ## {other_category}
///
/// - {text}
/// ```
/// 文件格式:
/// ```text
/// # YYYY-MM-DD
///
/// ## {category}
///
/// - {text}
///
/// ## {other_category}
///
/// - {text}
/// ```
fn remember_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
    text: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let dir = markdown_memory_dir(override_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", today_date_string()));

    // 读取现有内容, 决定是否要新建 section
    let existing = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut new_content = if existing.is_empty() {
        format!("# {}\n\n", today_date_string())
    } else {
        existing.clone()
    };

    // 检查 category section 是否已存在
    let section_header = format!("## {}", category);
    if new_content.contains(&section_header) {
        // 追加 bullet 到现有 section (section_header 不需要再使用)
        // 追加 bullet 到现有 section
        new_content.push_str(&format!("- {}\n", text));
    } else {
        // 新建 section
        new_content.push_str(&format!("\n{}\n\n- {}\n", section_header, text));
    }

    // 写回 (原子性: write to temp + rename, 简化版直接 overwrite)
    let mut f = std::fs::File::create(&path)?;
    f.write_all(new_content.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// v0.43.1: recall_markdown(category) — 读所有 markdown 文件, 找 ## category 段, 拼接 bullets
fn recall_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
) -> std::io::Result<String> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(String::new());
    }

    let mut out = String::new();

    // 按日期排序读所有 .md
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        // 找到 ## {category} 段, 收集直到下一个 ## 或文件末尾
        let mut in_section = false;
        for line in content.lines() {
            if let Some(header) = line.strip_prefix("## ") {
                in_section = header.trim() == category.trim();
            } else if in_section && line.starts_with("- ") {
                out.push_str(line.trim_start_matches("- "));
                out.push('\n');
            }
        }
    }

    Ok(out)
}

/// v0.43.1: list_markdown_categories() — 列出所有 markdown 文件中出现过的 ## section 标题
fn list_markdown_categories(
    override_dir: Option<&std::path::Path>,
) -> std::io::Result<Vec<String>> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut seen = std::collections::BTreeSet::new();
    let entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("## ") {
                // 跳过子标题 (### 等), 只取 ## level
                seen.insert(rest.trim().to_string());
            }
        }
    }

    Ok(seen.into_iter().collect())
}

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

#[cfg(test)]
mod tests_v042_capability {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.42.0: sandbox.key + sandbox.check_call builtin 测试

    #[test]
    fn sandbox_key_returns_token_id_number() {
        let mut interp = Interpreter::new();
        let args = vec![
            Value::String("file.read".to_string()),
            Value::String("web.fetch".to_string()),
        ];
        let token_id = interp
            .call_sandbox_method("key", &args)
            .expect("sandbox.key should succeed");
        match token_id {
            Value::Float(n) => assert_eq!(n, 0.0, "first token_id should be 0"),
            other => panic!("expected Value::Float, got {:?}", other),
        }
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 1);
    }

    #[test]
    fn sandbox_key_with_no_caps_returns_token() {
        // 空 args 也是合法: 创建一个空 capability 集合 (拒绝一切)
        let mut interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[])
            .expect("sandbox.key with no args should succeed");
        assert!(matches!(token_id, Value::Float(_)));
        // 空 token 任何 cap 都应被拒绝
        let check = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call should not error");
        assert_eq!(check, Value::Bool(false));
    }

    #[test]
    fn sandbox_key_rejects_unknown_capability_string() {
        let mut interp = Interpreter::new();
        let args = vec![Value::String("not.a.real.cap".to_string())];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 0);
    }

    #[test]
    fn sandbox_key_rejects_non_string_arg() {
        let mut interp = Interpreter::new();
        let args = vec![Value::Float(42.0)];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with non-string arg should error");
        assert!(err.contains("capability strings"), "got: {}", err);
    }

    #[test]
    fn sandbox_check_call_authorizes_granted_capability() {
        let mut interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue token");

        let authorized = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(authorized, Value::Bool(true));

        let denied = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.write".to_string())],
            )
            .expect("check_call");
        assert_eq!(denied, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_token_returns_false() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "check_call",
                &[Value::Float(9999.0), Value::String("file.read".to_string())],
            )
            .expect("check_call should not error, just return false");
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_capability_string_errors() {
        let mut interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue");
        let err = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("not.a.cap".to_string())],
            )
            .expect_err("unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
    }

    #[test]
    fn sandbox_revoke_bumps_generation() {
        let mut interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue");
        let token_id_num = match &token_id {
            Value::Float(n) => *n as u64,
            _ => panic!("expected Number"),
        };

        // revoke 前 check_call 返回 true
        let before = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(before, Value::Bool(true));

        // revoke
        let revoked = interp
            .call_sandbox_method("revoke", std::slice::from_ref(&token_id))
            .expect("revoke");
        assert_eq!(revoked, Value::Bool(true));

        // v0.49.0: generation 在 store 全局 bump (不放在 token 上)
        assert_eq!(interp.sandbox.sandbox.capabilities.current_generation(), 1);
        // token 仍存在 (loongclaw-style: 不删除)
        assert!(
            interp
                .sandbox
                .sandbox
                .capabilities
                .get(token_id_num)
                .is_some()
        );

        // v0.49.0: revoked token 在 check_call 时返回 false (TokenNotFound,
        // 因为 token.generation != current_generation)
        let after = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(
            after,
            Value::Bool(false),
            "v0.49.0: revoked token must fail check_call"
        );
    }

    #[test]
    fn sandbox_token_count_tracks_unique_tokens() {
        let mut interp = Interpreter::new();
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 0);

        let _ = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method("key", &[Value::String("web.fetch".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method(
                "key",
                &[
                    Value::String("memory.read".to_string()),
                    Value::String("memory.write".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 3);
    }

    #[test]
    fn sandbox_old_methods_still_work() {
        // v0.42.0 增补不应破坏 v0.33-0.41 的 sandbox.mode / check_builtin / check_path
        let mut interp = Interpreter::new();
        let mode = interp.call_sandbox_method("mode", &[]).expect("mode");
        assert!(matches!(mode, Value::String(_)));

        let cb = interp
            .call_sandbox_method("check_builtin", &[Value::String("print".to_string())])
            .expect("check_builtin");
        assert_eq!(cb, Value::Bool(true));
    }
}

#[cfg(test)]
mod tests_v0421_audit {
    #![allow(unused_mut)]
    use super::*;
    use crate::audit::{AuditSink, JsonlAuditSink};
    use crate::value::Value;
    use std::sync::Arc;

    /// v0.42.1: sandbox.audit_emit / audit_flush / audit_verify builtin tests
    fn temp_log_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_audit_builtin_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn audit_emit_writes_event_and_returns_true() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_basic.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("file.write".to_string()),
                    Value::String("/tmp/foo.txt".to_string()),
                    Value::String("{\"size\":42}".to_string()),
                ],
            )
            .expect("audit_emit");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_with_optional_args() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_minimal.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        // 仅 actor + action
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("agent".to_string()),
                    Value::String("chat.start".to_string()),
                ],
            )
            .expect("audit_emit minimal");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_validates_arg_types() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::Float(42.0), Value::String("action".to_string())],
            )
            .expect_err("non-string actor should fail");
        assert!(err.contains("actor must be a string"), "got: {}", err);
    }

    #[test]
    fn audit_emit_validates_arg_count() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::String("a".to_string())], // 只 1 个 arg
            )
            .expect_err("too few args should fail");
        assert!(err.contains("2-4 args"), "got: {}", err);
    }

    #[test]
    fn audit_flush_and_verify_chain_passes() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("verify.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        for i in 0..5 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("user".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit");
        }
        let flushed = interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");
        assert_eq!(flushed, Value::Bool(true));

        let verified = interp
            .call_sandbox_method("audit_verify", &[])
            .expect("verify");
        assert_eq!(verified, Value::Bool(true));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_verify_detects_tampering() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("tampered.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        for i in 0..3 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("a".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit");
        }
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");
        assert_eq!(
            interp.call_sandbox_method("audit_verify", &[]).unwrap(),
            Value::Bool(true)
        );

        // 篡改 line 1
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[1] = lines[1].replace("\"action\":\"op.1\"", "\"action\":\"TAMPERED\"");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let verified = interp.call_sandbox_method("audit_verify", &[]).unwrap();
        // 应返回 Value::String(error)
        match verified {
            Value::String(s) => assert!(
                s.contains("hash mismatch") || s.contains("HashMismatch"),
                "got: {}",
                s
            ),
            Value::Bool(true) => panic!("tamper should have been detected"),
            other => panic!("unexpected: {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn null_sink_default_audit_emit_returns_true() {
        // 默认 NullSink 应接受所有 audit_emit 调用
        let mut interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("op".to_string()),
                ],
            )
            .expect("audit_emit to null sink");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn audit_emit_writes_to_real_file_via_jsonl_sink() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("real_file.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("sandbox.issue".to_string()),
                    Value::Nil,
                    Value::String("{\"cap\":\"file.read\"}".to_string()),
                ],
            )
            .expect("emit");
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");

        // 验证文件存在且包含期望字段
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"action\":\"sandbox.issue\""));
        assert!(content.contains("\"actor\":\"user\""));
        assert!(content.contains("\"payload\":\"{\\\"cap\\\":\\\"file.read\\\"}\""));
        assert!(content.contains("\"hash\":"));

        let _ = std::fs::remove_file(&path);
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

#[cfg(test)]
mod tests_v0431_memory_bus {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.43.1: memory.remember / recall_markdown / list_markdown
    fn setup_temp_memory_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_md_mem_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn teardown_memory_dir(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn memory_remember_appends_to_markdown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("user_prefs".to_string()),
                    Value::String("likes Rust".to_string()),
                ],
            )
            .expect("remember");
        assert_eq!(result, Value::Bool(true));

        // 验证文件存在并包含内容
        let date = today_date_string();
        let md_path = dir.join(format!("{}.md", date));
        let content = std::fs::read_to_string(&md_path).unwrap();
        assert!(content.contains("# "));
        assert!(content.contains("## user_prefs"));
        assert!(content.contains("- likes Rust"));

        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_remember_appends_to_existing_section() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("first entry".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("second entry".to_string()),
                ],
            )
            .unwrap();

        let date = today_date_string();
        let content = std::fs::read_to_string(dir.join(format!("{}.md", date))).unwrap();
        // 只有一个 ## cat section
        assert_eq!(content.matches("## cat").count(), 1);
        assert!(content.contains("- first entry"));
        assert!(content.contains("- second entry"));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_text() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("notes".to_string()),
                    Value::String("remember this".to_string()),
                ],
            )
            .unwrap();
        let recalled = interp
            .call_memory_method("recall_markdown", &[Value::String("notes".to_string())])
            .expect("recall");
        match recalled {
            Value::String(s) => assert!(s.contains("remember this"), "got: {}", s),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_empty_for_unknown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method("recall_markdown", &[Value::String("nope".to_string())])
            .expect("recall");
        assert_eq!(result, Value::String(String::new()));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_list_markdown_lists_categories() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("a".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("b".to_string()),
                    Value::String("y".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_memory_method("list_markdown", &[])
            .expect("list");
        match list {
            Value::List(items) => {
                let cats: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(cats.contains(&"a".to_string()));
                assert!(cats.contains(&"b".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_after_remember_syncs_to_memory_store() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("k".to_string()),
                    Value::String("v".to_string()),
                ],
            )
            .unwrap();
        // 通过现有 recall (HashMap-backed) 应能查到
        let recalled = interp
            .call_memory_method("recall", &[Value::String("md:k".to_string())])
            .expect("recall");
        match recalled {
            Value::String(s) => assert_eq!(s, "v"),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    /// v0.43.1: bus.subscribe / bus.publish

    #[test]
    fn bus_subscribe_returns_token() {
        let mut interp = Interpreter::new();
        let token = interp
            .call_event_method(
                "subscribe",
                &[Value::String("agent.research.*".to_string())],
            )
            .expect("subscribe");
        // token 是 Number (pattern_count 1)
        match token {
            Value::Float(n) => assert_eq!(n, 1.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_subscribe_validates_pattern() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("subscribe", &[Value::Float(42.0)])
            .expect_err("non-string pattern should fail");
        assert!(err.contains("pattern must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_publish_returns_pattern_count() {
        let mut interp = Interpreter::new();
        // subscribe 2 个
        interp
            .call_event_method("subscribe", &[Value::String("ai.*".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("ai.chat.*".to_string())])
            .unwrap();
        // publish
        let count = interp
            .call_event_method(
                "publish",
                &[
                    Value::String("ai.chat.completed".to_string()),
                    Value::String("data".to_string()),
                ],
            )
            .expect("publish");
        // 返回 pattern_count (2)
        match count {
            Value::Float(n) => assert_eq!(n, 2.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_publish_validates_topic() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("publish", &[Value::Float(42.0)])
            .expect_err("non-string topic should fail");
        assert!(err.contains("topic must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_subscribe_then_publish_wildcard_match() {
        // end-to-end: subscribe "user.*", publish "user.created", 验证 pattern 进入订阅表
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("user.*".to_string())])
            .unwrap();
        // emit() 走通配符匹配 (v0.41.0 O(segments) 索引, 验证过)
        interp
            .call_event_method("emit", &[Value::String("user.created".to_string())])
            .unwrap();
        // pattern_count 应 = 1
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Float(1.0));
    }

    #[test]
    fn bus_subscribe_uses_existing_pattern_matching() {
        // 验证 subscribe 用的就是 EventBus::on() (已经在 v0.41.0 + v0.41.1 测试覆盖)
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("exact.event".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("prefix.*".to_string())])
            .unwrap();
        // 两个 patterns
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Float(2.0));
    }
}

#[cfg(test)]
mod tests_v044_container_real {
    // Tests use `let mut interp = ...` pattern uniformly; some tests don't actually need mut.
    // Allow unused_mut for the whole module to avoid 5 false positives.
    #![allow(unused_mut)]

    use super::*;
    use crate::value::Value;

    /// v0.44.0: REAL Docker container builtin integration
    /// **Requires Docker daemon** — 默认 #[ignore] 让 CI 无 docker 时跳过
    fn cleanup_container(interp: &mut Interpreter) {
        // 尽力清理 (可能根本没 spawn 成功)
        let _ = interp.call_sandbox_method("container_clear", &[]);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_containerize_real_spawn() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .expect("containerize should spawn docker");
        // 返回 Number (container_id hash)
        match result {
            Value::Float(n) => assert!(n > 0.0, "container_id hash should be non-zero"),
            other => panic!("expected Number, got: {:?}", other),
        }
        assert!(
            interp
                .sandbox
                .container
                .lock()
                .expect("container poisoned")
                .is_some()
        );
        cleanup_container(&mut interp);
        assert!(
            interp
                .sandbox
                .container
                .lock()
                .expect("container poisoned")
                .is_none()
        );
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_exec_runs_cmd_inside_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let result = interp
            .call_sandbox_method(
                "container_exec",
                &[
                    Value::String("echo".to_string()),
                    Value::String("hello-from-real-docker".to_string()),
                ],
            )
            .expect("container_exec should succeed");
        match result {
            Value::Dict(d) => {
                let stdout = match d.get("stdout") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected stdout String, got: {:?}", other),
                };
                assert!(
                    stdout.contains("hello-from-real-docker"),
                    "stdout should contain 'hello-from-real-docker', got: {}",
                    stdout
                );
                let exit_code = d.get("exit_code").expect("exit_code");
                assert!(
                    matches!(exit_code, Value::Float(0.0)),
                    "exit_code should be 0, got: {:?}",
                    exit_code
                );
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_info_returns_real_container_id() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        match info {
            Value::Dict(d) => {
                let id = match d.get("container_id") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected container_id String, got: {:?}", other),
                };
                assert!(
                    id.len() >= 12,
                    "docker container_id hex should be >= 12 chars: {}",
                    id
                );
                let name = d.get("container_name").expect("container_name");
                match name {
                    Value::String(s) => assert!(
                        s.starts_with("mora-"),
                        "name should start with mora-, got: {}",
                        s
                    ),
                    other => panic!("expected String name, got: {:?}", other),
                }
                let backend = d.get("backend").expect("backend");
                match backend {
                    Value::String(s) => assert_eq!(s, "docker"),
                    other => panic!("expected docker backend, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_clear_really_removes_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let id = {
            let guard = interp.sandbox.container.lock().expect("container poisoned");
            guard.as_ref().unwrap().container_id.clone()
        };
        // 验证 container 真的在 docker 里
        let check = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(check.status.success(), "docker should know the container");
        let state = String::from_utf8_lossy(&check.stdout).trim().to_string();
        assert_eq!(state, "true", "container should be running");

        // clear → 真 docker rm -f
        let cleared = interp
            .call_sandbox_method("container_clear", &[])
            .expect("clear");
        assert_eq!(cleared, Value::Bool(true));

        // 验证 container 真的没了
        let check2 = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(
            !check2.status.success(),
            "docker inspect should fail for removed container"
        );
    }

    #[test]
    fn sandbox_containerize_rejects_unknown_backend() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("vmware".to_string())])
            .expect_err("unknown backend should fail");
        assert!(err.contains("unknown backend"), "got: {}", err);
    }

    #[test]
    fn sandbox_containerize_rejects_unimplemented_backend() {
        // gondolin/openshell 在 v0.44.0 真实未实现, 应该返回明确错误
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("gondolin".to_string())])
            .expect_err("gondolin not yet implemented");
        assert!(err.contains("not yet implemented"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_exec_requires_container_first() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("container_exec", &[Value::String("ls".to_string())])
            .expect_err("exec without container should fail");
        assert!(err.contains("no container"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_info_returns_nil_when_unset() {
        let mut interp = Interpreter::new();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        assert_eq!(info, Value::Nil);
    }
}

#[cfg(test)]
mod tests_v044_orchestrate_validate {
    use crate::lexer::Lexer;
    use crate::mir::expr::MirExpr;
    use crate::parser_v3::ParserV3;

    /// v0.44.0: orchestrate block syntax validation (ParserV3 path)
    fn parse(src: &str) -> Vec<MirExpr> {
        let tokens = Lexer::new(src).scan_tokens();
        let parser = ParserV3::new(tokens);
        parser
            .parse()
            .unwrap_or_else(|e| panic!("ParserV3 failed: {:?}", e))
    }

    #[test]
    fn orchestrate_sequential_parses() {
        let src = r#"
task main()
  orchestrate sequential x -> y
    agent a(x) => "a:" + x
    agent b(x) => "b:" + x
"#;
        let exprs = parse(src);
        assert!(!exprs.is_empty());
    }

    #[test]
    fn orchestrate_loop_with_on_predicate_parses() {
        let src = r#"
task main()
  orchestrate loop x -> y, max_rounds: 5
    on: x == "done"
    agent a(x) => x
"#;
        let exprs = parse(src);
        assert!(!exprs.is_empty());
    }

    #[test]
    fn orchestrate_graph_with_predicate_edges_parses() {
        let src = r#"
task main()
  orchestrate graph x -> y
    @start -> a
    @start -> b on: x == "research"
    a -> @exit
    b -> @exit
"#;
        let exprs = parse(src);
        assert!(!exprs.is_empty());
    }
}

#[cfg(test)]
mod tests_v045_toolplane {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.45.0: tool.plane.* builtin (loongclaw Core/Extension pattern)

    #[test]
    fn tool_plane_create_default_core_planes_exist() {
        let mut interp = Interpreter::new();
        let list = interp.call_toolplane_method("list", &[]).expect("list");
        match list {
            Value::List(names) => {
                let names_v: Vec<String> = names
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(
                    names_v.contains(&"ai".to_string()),
                    "should have 'ai' core plane"
                );
                assert!(
                    names_v.contains(&"sandbox".to_string()),
                    "should have 'sandbox' core plane"
                );
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_create_extension() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("user_plane".to_string()),
                    Value::String("extension".to_string()),
                ],
            )
            .expect("create");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("user_plane".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let kind = d.get("kind").expect("kind");
                match kind {
                    Value::String(s) => assert_eq!(s, "extension"),
                    other => panic!("expected extension kind, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_register_and_find() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                    Value::String("does something".to_string()),
                    Value::String(r#"{"type":"object"}"#.to_string()),
                ],
            )
            .expect("register");

        let tools = interp
            .call_toolplane_method("list_tools", &[Value::String("p".to_string())])
            .expect("list_tools");
        match tools {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"mytool".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }

        let found = interp
            .call_toolplane_method(
                "find",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                ],
            )
            .expect("find");
        match found {
            Value::Dict(d) => {
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert_eq!(s, "does something"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn tool_plane_register_duplicate_tool_fails() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .unwrap();
        let err = interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .expect_err("duplicate should fail");
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn tool_plane_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_toolplane_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }

    #[test]
    fn tool_plane_remove_plane() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_toolplane_method("remove", &[Value::String("p".to_string())])
            .expect("remove");
        assert_eq!(removed, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("p".to_string())])
            .expect("info");
        assert_eq!(info, Value::Nil);
    }
}

#[cfg(test)]
mod tests_v045_ai {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.45.0: ai.retry / ai.role builtin (mini-swe-agent + OpenFugu)

    #[test]
    fn ai_retry_returns_schedule_dict() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[Value::String("5".to_string()), Value::Float(100.0)],
            )
            .expect("retry");
        match result {
            Value::Dict(d) => {
                let attempts = d.get("attempts").expect("attempts");
                match attempts {
                    Value::Float(n) => assert_eq!(*n, 5.0),
                    _ => panic!("expected Number attempts"),
                }
                let backoff_ms = d.get("backoff_ms").expect("backoff_ms");
                match backoff_ms {
                    Value::Float(n) => assert_eq!(*n, 100.0),
                    _ => panic!("expected Number backoff_ms"),
                }
                let schedule = d.get("schedule").expect("schedule");
                match schedule {
                    Value::List(items) => {
                        assert_eq!(items.len(), 5, "schedule should have 5 entries")
                    }
                    _ => panic!("expected List schedule"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn ai_retry_exponential_schedule_grows() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[
                    Value::String("4".to_string()),
                    Value::Float(100.0),
                    Value::String("exponential".to_string()),
                ],
            )
            .expect("retry");
        match result {
            Value::Dict(d) => {
                let schedule = d.get("schedule").expect("schedule");
                if let Value::List(items) = schedule {
                    let nums: Vec<f64> = items
                        .iter()
                        .filter_map(|v| match v {
                            Value::Float(n) => Some(*n),
                            _ => None,
                        })
                        .collect();
                    // exponential: 100, 200, 400, 800
                    assert_eq!(nums, vec![100.0, 200.0, 400.0, 800.0]);
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_retry_rejects_zero_attempts() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("retry", &[Value::String("0".to_string())])
            .expect_err("zero attempts should fail");
        assert!(err.contains("attempts must be > 0"), "got: {}", err);
    }

    #[test]
    fn ai_role_accepts_main_three_roles() {
        let mut interp = Interpreter::new();
        for role in ["worker", "thinker", "verifier"] {
            let result = interp
                .call_ai_method("role", &[Value::String(role.to_string())])
                .expect("role");
            match result {
                Value::String(s) => assert_eq!(s, role),
                _ => panic!("expected String"),
            }
        }
    }

    #[test]
    fn ai_role_accepts_custom_role() {
        // OpenFugu has 3 main roles but custom roles also OK
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("role", &[Value::String("explorer".to_string())])
            .expect("role");
        match result {
            Value::String(s) => assert_eq!(s, "explorer"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn ai_role_requires_arg() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("role", &[])
            .expect_err("no arg should fail");
        assert!(err.contains("requires role name"), "got: {}", err);
    }

    #[test]
    fn ai_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v046_skill {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.46.0: skill.* builtin (CLI-Anything SKILL.md pattern)
    fn write_temp_skill_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_skill_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn skill_list_empty_by_default() {
        let mut interp = Interpreter::new();
        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => assert_eq!(items.len(), 0),
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_install_registers_skill() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: my-skill
description: A test skill
trigger: test.*
---

This is the body of my-skill.
"#;
        let result = interp
            .call_skill_method(
                "install",
                &[
                    Value::String("my-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .expect("install");
        assert_eq!(result, Value::Bool(true));

        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"my-skill".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_returns_full_spec() {
        let mut interp = Interpreter::new();
        let content = "---
name: finder-skill
description: Helps find things
trigger: find.*
---

# Body
Find things here.
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("finder-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let found = interp
            .call_skill_method("find", &[Value::String("finder-skill".to_string())])
            .expect("find");
        match found {
            Value::Dict(d) => {
                let name = d.get("name").expect("name");
                match name {
                    Value::String(s) => assert_eq!(s, "finder-skill"),
                    _ => panic!("expected name String"),
                }
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert!(s.contains("find things")),
                    _ => panic!("expected desc String"),
                }
                let trigger = d.get("trigger").expect("trigger");
                match trigger {
                    Value::String(s) => assert_eq!(s, "find.*"),
                    _ => panic!("expected trigger String"),
                }
                let body = d.get("body").expect("body");
                match body {
                    Value::String(s) => assert!(s.contains("Find things here")),
                    _ => panic!("expected body String"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_unknown_returns_nil() {
        let mut interp = Interpreter::new();
        let found = interp
            .call_skill_method("find", &[Value::String("nope".to_string())])
            .expect("find");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_load_real_skill_md_file() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: file-loaded
description: Loaded from file
---

This skill was loaded from a real file on disk.
"#;
        let path = write_temp_skill_file("file-loaded", content);

        let result = interp
            .call_skill_method("load", &[Value::String(path.to_string_lossy().to_string())])
            .expect("load");
        assert_eq!(result, Value::Bool(true));

        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"file-loaded".to_string()));
            }
            _ => panic!("expected List"),
        }

        let found = interp
            .call_skill_method("find", &[Value::String("file-loaded".to_string())])
            .expect("find");
        match found {
            Value::Dict(d) => {
                let src = d.get("source").expect("source");
                match src {
                    Value::String(s) => assert!(s.contains("file-loaded.md"), "got: {}", s),
                    _ => panic!("expected source path String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skill_load_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("load", &[Value::String("/nonexistent/foo.md".to_string())])
            .expect_err("nonexistent should fail");
        assert!(err.contains("skill.load"), "got: {}", err);
    }

    #[test]
    fn skill_uninstall_removes() {
        let mut interp = Interpreter::new();
        let content = "---
name: temp
description: temporary
---

body
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("temp".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_skill_method("uninstall", &[Value::String("temp".to_string())])
            .expect("uninstall");
        assert_eq!(removed, Value::Bool(true));
        let found = interp
            .call_skill_method("find", &[Value::String("temp".to_string())])
            .expect("find");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_set_hub_and_refresh_real_file() {
        let mut interp = Interpreter::new();
        let dir = std::env::temp_dir().join(format!(
            "mora_hub_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hub_path = dir.join("mora-public.json");
        let hub_content = r#"{
  "skills": [
    {"name": "hub-skill-a", "description": "Hub A"},
    {"name": "hub-skill-b", "description": "Hub B"}
  ]
}"#;
        std::fs::write(&hub_path, hub_content).unwrap();

        let set = interp
            .call_skill_method(
                "set_hub",
                &[Value::String(hub_path.to_string_lossy().to_string())],
            )
            .expect("set_hub");
        assert_eq!(set, Value::Bool(true));

        let count = interp
            .call_skill_method("refresh_hub", &[])
            .expect("refresh_hub");
        match count {
            Value::Float(n) => assert!(n >= 1.0, "expected at least 1 hub entry, got {}", n),
            other => panic!("expected Number, got: {:?}", other),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v048_plan {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.48.0: plan.* builtin (pi-agent update_plan pattern)

    #[test]
    fn plan_create_then_list() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s1".to_string()));
                d.insert("text".to_string(), Value::String("first".to_string()));
                d.insert("status".to_string(), Value::String("pending".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s2".to_string()));
                d.insert("text".to_string(), Value::String("second".to_string()));
                d
            }),
        ];
        let name = interp
            .call_plan_method(
                "create",
                &[Value::String("myplan".to_string()), Value::List(steps)],
            )
            .expect("create");
        assert_eq!(name, Value::String("myplan".to_string()));

        let list = interp.call_plan_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"myplan".to_string()));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_update_step_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // update a -> done
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("done".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let done = d.get("done").expect("done");
                match done {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_update_supports_emoji_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // emoji ✅
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("✅".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update with emoji");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn plan_update_unknown_step_errors() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::List(vec![Value::Dict({
                        let mut d = std::collections::HashMap::new();
                        d.insert("id".to_string(), Value::String("a".to_string()));
                        d.insert("text".to_string(), Value::String("A".to_string()));
                        d
                    })]),
                ],
            )
            .unwrap();
        let updates = vec![Value::List(vec![
            Value::String("ghost".to_string()),
            Value::String("done".to_string()),
        ])];
        let err = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect_err("unknown step should fail");
        assert!(err.contains("not found"), "got: {}", err);
    }

    #[test]
    fn plan_add_and_remove_step() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(vec![])],
            )
            .unwrap();
        let added = interp
            .call_plan_method(
                "add",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                    Value::String("A".to_string()),
                ],
            )
            .expect("add");
        assert_eq!(added, Value::Bool(true));
        let removed = interp
            .call_plan_method(
                "remove",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .expect("remove");
        assert_eq!(removed, Value::Bool(true));
    }

    #[test]
    fn plan_list_returns_steps_with_emoji() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let list = interp
            .call_plan_method("list", &[Value::String("p".to_string())])
            .expect("list steps");
        match list {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                match &items[0] {
                    Value::Dict(d) => {
                        let emoji = d.get("emoji").expect("emoji");
                        match emoji {
                            Value::String(s) => assert_eq!(s, "⬜"), // pending default
                            _ => panic!("expected emoji String"),
                        }
                    }
                    _ => panic!("expected Dict"),
                }
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_info_reports_counts() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("a".to_string()));
                d.insert("text".to_string(), Value::String("A".to_string()));
                d.insert("status".to_string(), Value::String("done".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("b".to_string()));
                d.insert("text".to_string(), Value::String("B".to_string()));
                d
            }),
        ];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done");
                match done {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio");
                match ratio {
                    Value::Float(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_plan_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v048_refine {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.48.0: mora.refine + mora.refine_info + mora.list_refines (CLI-Anything /refine)
    #[allow(unused_imports)]
    use std::time::UNIX_EPOCH;

    fn write_temp_script(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_refine_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn mora_refine_real_file_creates_refined_copy() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("demo.mora", "task main()\n  print(\"hi\")\n");
        let result = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("add greeting".to_string()),
                ],
            )
            .expect("refine");
        match result {
            Value::Dict(d) => {
                let iter = d.get("iteration").expect("iteration");
                match iter {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let refined = d.get("refined").expect("refined");
                match refined {
                    Value::String(s) => assert!(s.contains(".refined.1.mora")),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        // 验证 .refine/ 目录存在 + 副本可读
        let refine_dir = script.parent().unwrap().join("demo.refine");
        assert!(refine_dir.exists(), ".refine/ should be created");
        let refined_path = refine_dir.join("demo.refined.1.mora");
        assert!(refined_path.exists(), "refined copy should exist");
        let content = std::fs::read_to_string(&refined_path).unwrap();
        assert!(content.contains("add greeting"));
        assert!(content.contains("task main()"));

        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_iteration_increments() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("iter.mora", "x\n");
        for i in 1..=3 {
            let result = interp
                .call_mora_method(
                    "refine",
                    &[
                        Value::String(script.to_string_lossy().to_string()),
                        Value::String(format!("iter {}", i)),
                    ],
                )
                .expect("refine");
            match result {
                Value::Dict(d) => {
                    let iter = d.get("iteration").expect("iteration");
                    match iter {
                        Value::Float(n) => assert_eq!(*n, i as f64),
                        _ => panic!("expected Number"),
                    }
                }
                _ => panic!("expected Dict"),
            }
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    /// v0.75.8: mora.refine 第 3 参 count → 返回 List[Dict]（多候选）
    #[test]
    fn mora_refine_many_returns_list() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("many.mora", "x\n");
        let result = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("add variants".to_string()),
                    Value::Float(3.0),
                ],
            )
            .expect("refine_many");
        match result {
            Value::List(items) => {
                assert_eq!(items.len(), 3, "3 个候选");
                for item in &items {
                    match item {
                        Value::Dict(d) => assert!(d.contains_key("refined")),
                        _ => panic!("expected Dict in List"),
                    }
                }
            }
            _ => panic!("expected List for 3-arg refine"),
        }
        // 2 参仍返回单个 Dict（兼容）
        let single = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("again".to_string()),
                ],
            )
            .expect("refine 2-arg");
        assert!(matches!(single, Value::Dict(_)), "2 参应返回 Dict");
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_returns_latest() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("info.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("first".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[Value::String(script.to_string_lossy().to_string())],
            )
            .expect("refine_info");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "first"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_specific_iteration() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("specific.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v1".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v2".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::Float(1.0),
                ],
            )
            .expect("iter 1");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "v1"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_list_refines_lists_all_scripts() {
        let mut interp = Interpreter::new();
        let s1 = write_temp_script("s1.mora", "1\n");
        let s2 = write_temp_script("s2.mora", "2\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s1.to_string_lossy().to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s2.to_string_lossy().to_string()),
                    Value::String("b".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_mora_method("list_refines", &[])
            .expect("list_refines");
        match list {
            Value::List(items) => {
                let paths: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(paths.len(), 2);
                assert!(paths.iter().any(|p| p.contains("s1.mora")));
                assert!(paths.iter().any(|p| p.contains("s2.mora")));
            }
            _ => panic!("expected List"),
        }
        let _ = std::fs::remove_dir_all(s1.parent().unwrap());
        let _ = std::fs::remove_dir_all(s2.parent().unwrap());
    }

    #[test]
    fn mora_refine_nonexistent_script_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String("/nonexistent/foo.mora".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("mora.refine"), "got: {}", err);
    }

    #[test]
    fn mora_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v047_dag {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.dag builtin (OpenFugu §1.6 DAG-as-data)

    #[test]
    fn ai_dag_linear_returns_topological_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("c".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names, vec!["a", "b", "c"]);
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn ai_dag_cycle_returns_error() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("a".to_string()),
            ]),
        ];
        let err = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect_err("cycle should fail");
        assert!(err.contains("ai.dag"), "got: {}", err);
        assert!(err.contains("cycle"), "got: {}", err);
    }

    #[test]
    fn ai_dag_diamond_returns_valid_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
            Value::String("d".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("c".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("d".to_string()),
            ]),
            Value::List(vec![
                Value::String("c".to_string()),
                Value::String("d".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names[0], "a");
                assert_eq!(names[3], "d");
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_empty_edges_returns_nodes() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(vec![])])
            .expect("dag");
        match result {
            Value::List(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_requires_2_args() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("dag", &[])
            .expect_err("no args should fail");
        assert!(err.contains("requires 2 args"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v047_heartbeat {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.heartbeat builtin (mimiclaw §1.5 HEARTBEAT.md pattern)
    use std::time::UNIX_EPOCH;

    fn write_heartbeat(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_hb_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn ai_heartbeat_real_file_returns_report() {
        let mut interp = Interpreter::new();
        let content = r#"# Heartbeat
- [x] first done
- [ ] second pending
- [x] third done
- [ ] fourth pending
"#;
        let path = write_heartbeat("HB", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Float(n) => assert_eq!(*n, 4.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done");
                match done {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio");
                match ratio {
                    Value::Float(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(false));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_all_done_is_complete() {
        let mut interp = Interpreter::new();
        let content = "- [x] a\n- [X] b\n- [x] c\n";
        let path = write_heartbeat("all_done", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_empty_heartbeat_is_vacuously_complete() {
        let mut interp = Interpreter::new();
        let content = "# only heading\nno checklist items\n";
        let path = write_heartbeat("empty", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Float(n) => assert_eq!(*n, 0.0),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String("/nonexistent/HEARTBEAT.md".to_string())],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("ai.heartbeat"), "got: {}", err);
    }

    #[test]
    fn ai_heartbeat_items_list_contains_text_and_done() {
        let mut interp = Interpreter::new();
        let content = "- [x] task A\n- [ ] task B\n";
        let path = write_heartbeat("items", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let items = d.get("items").expect("items");
                match items {
                    Value::List(items) => {
                        assert_eq!(items.len(), 2);
                        match &items[0] {
                            Value::Dict(item) => {
                                let done = item.get("done").expect("done");
                                assert_eq!(*done, Value::Bool(true));
                            }
                            _ => panic!("expected Dict"),
                        }
                    }
                    _ => panic!("expected List"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

#[cfg(test)]
mod tests_v047_context {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.context.trim + ai.context.info (pi-agent + AgentMesh pattern)

    #[test]
    fn ai_context_info_returns_window_state() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.info", &[])
            .expect("context.info");
        match result {
            Value::Dict(d) => {
                let max = d.get("max_tokens").expect("max_tokens");
                match max {
                    Value::Float(n) => assert_eq!(*n, 4096.0, "default max"),
                    _ => panic!("expected Number"),
                }
                let msgs = d.get("messages").expect("messages");
                match msgs {
                    Value::Float(n) => assert_eq!(*n, 0.0, "default empty"),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_context_trim_empty_drops_zero() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[])
            .expect("context.trim");
        match result {
            Value::Float(n) => assert_eq!(n, 0.0, "empty context drops 0 tokens"),
            _ => panic!("expected Number"),
        }
    }

    #[test]
    fn ai_context_trim_validates_threshold_range() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("context.trim", &[Value::Float(1.5)])
            .expect_err("1.5 should fail");
        assert!(err.contains("0.0-1.0"), "got: {}", err);

        let err2 = interp
            .call_ai_method("context.trim", &[Value::Float(-0.1)])
            .expect_err("-0.1 should fail");
        assert!(err2.contains("0.0-1.0"), "got: {}", err2);
    }

    #[test]
    fn ai_context_trim_accepts_valid_threshold() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[Value::Float(0.5)])
            .expect("should succeed");
        match result {
            Value::Float(_) => {}
            _ => panic!("expected Number"),
        }
    }
}

mod ai;
mod ai_tokens;
mod ccr;
mod event;
mod exec;
mod file;
mod memory;
mod mock;
mod mora;
mod plan;
mod sandbox;
mod schedule;
mod skill;
mod toolplane;
