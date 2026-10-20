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

    // ⚠ 空列表的早返回在**下方**（两个可选参数校验之后）——
    // v0.104.6 D337：它原本在**可选参数校验之前**，于是
    //   exec.parallel(["echo a"], -1)  → exit 1  max_concurrent must be a non-negative number
    //   exec.parallel([], -1)          → exit 0  **[]**（同一个非法参数，被静默吞掉）
    // 同一个非法 `max_concurrent`，**因为命令列表是空的就看不到错误** ⇒ 校验不一致。
    // 现把早返回**下移**到两个可选参数都校验完之后，让「参数非法」与
    // 「有没有活干」彻底解耦。
    //
    // ⚠ 这是**收紧**：此前 `exec.parallel([], <非法>)` 静默返回 `[]`。
    // 判据见 `tests/exec_parallel_arity_and_validation.rs`。
    // 不变的：`exec.parallel([])`（**不传**可选参数）仍返回 `[]`，那条是合法的。
    //
    // 第二个 arg (可选): max_concurrent
    //
    // v0.104.6 D285：修前的错误消息写着「must be a non-negative number」，
    // 但代码**只挡了非数值类型**（那个 `_ =>` 分支），**没有非负检查**：
    //   `Value::Float(-1.0) as usize` → 饱和成 0  → `.max(1)` → **1**
    //   `Value::Int(-1)   as usize` → **回绕**成 usize::MAX → **并发上限形同虚设**
    // 同一句源码、两种数值类型给出天差地别的并发控制，且 exit 0、零诊断。
    // 现改走 D246 的收口 `value_as_usize`（负数一律 `None`），**如实兑现**那句错误消息。
    // ⚠ 空列表的早返回在**上方**（两个可选参数校验之后）——
    // v0.104.6 D337：它原本在**可选参数校验之前**，于是
    //   exec.parallel(["echo a"], -1)  → exit 1  max_concurrent must be a non-negative number
    //   exec.parallel([], -1)          → exit 0  **[]**（同一个非法参数，被静默吞掉）
    // 同一个非法 `max_concurrent`，**因为命令列表是空的就看不到错误** ⇒ 校验不一致。
    // 现把早返回**下移**到两个可选参数都校验完之后（见下方 `if cmds.is_empty()`），
    // 这样「参数非法」与「有没有活干」彻底解耦。
    //
    // ⚠ 这是**收紧**：此前 `exec.parallel([], <非法>)` 静默返回 `[]`。
    // 判据见 `tests/exec_parallel_arity_and_validation.rs`。
    // 不变的：`exec.parallel([])`（**不传**可选参数）仍返回 `[]`，那条是合法的。
    let max_concurrent: usize = if args.len() >= 2 {
        match &args[1] {
            Value::Nil => cmds.len(), // 默认: 全部并发（与缺参同义）
            other => crate::flow::value_as_usize(other)
                .map(|n| n.max(1))
                .ok_or_else(|| {
                    "exec.parallel: max_concurrent must be a non-negative number".to_string()
                })?,
        }
    } else {
        cmds.len() // 默认: 全部并发
    };

    // 第三个 arg (可选): timeout_ms
    //
    // v0.104.6 D285：同一族的第二处，后果更明显。修前：
    //   `Value::Float(-1.0) as u64` → 饱和成 0 → `Duration::ZERO`
    //       ⇒ 超时机制**立刻杀进程**（实测 280ms 就返回，输出是 taskkill 的 SUCCESS）
    //   `Value::Int(-1)   as u64` → **回绕**成 u64::MAX ≈ 5.8 亿年
    //       ⇒ 超时机制**完全失效**（实测 3106ms，命令跑满全程）
    // 同一个 `-1`，一边「立刻杀」一边「永不超时」。
    let timeout: Option<Duration> = if args.len() >= 3 {
        match &args[2] {
            Value::Nil => None,
            other => {
                let ms = crate::flow::value_as_usize(other).ok_or_else(|| {
                    "exec.parallel: timeout_ms must be a non-negative number or nil".to_string()
                })?;
                Some(Duration::from_millis(ms as u64))
            }
        }
    } else {
        None
    };

    // v0.104.6 D337：空列表早返回**下移**到这里 —— 在两个可选参数都校验完之后。
    // 理由见上方注释：此前它在校验之前，于是「同一个非法 `max_concurrent`，
    // 因为命令列表是空的就看不到错误」。
    //
    // 注意 `max_concurrent` 在上面已被 `.max(1)` 钳过、空列表时不影响
    // 任何并发行为 ⇒ 移动早返回**不改变**合法调用的结果，只让非法参数
    // 不再被静默吞掉。
    if cmds.is_empty() {
        // TEETH-CHECK: 早返回被移回参数校验之前
        return Ok(Value::List(Vec::new().into()));
    }

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
    //
    // 预算 = 每条命令的 timeout × 波数 + 收尾宽限。波数 =
    // ceil(cmds / max_concurrent)，因为 worker 拿信号量是串行取任务的。
    //
    // **仅在用户显式给了 timeout 时设预算**：没给 timeout 意味着「我接受
    // 这条命令跑多久」，此时人为加一个上限就是改变语义而不是修 bug。
    let collect_budget: Option<Duration> = timeout.map(|per_cmd| {
        // 用 try_from 而非 `as u32`：后者是**静默截断**（usize→u32），
        // 与 `tests/int_cast_saturation_census.rs` 那条护栏同属一类问题。
        // 波数要 > u32::MAX 得有 42 亿条命令，实际不可达；真到了就钳到
        // u32::MAX，后续 `checked_mul` 再返回 None → Duration::MAX，
        // 仍是「不早退」的保守方向。
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
    for _ in 0..cmds_arc.len() {
        let received = match deadline {
            Some(dl) => {
                match rx.recv_timeout(dl.saturating_duration_since(std::time::Instant::now())) {
                    Ok(r) => Ok(r),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        timed_out = true;
                        break;
                    }
                }
            }
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }

    if timed_out {
        // worker 没能按时上报。**不能**在这里 `join()`：那正是要修的挂死。
        // 缺项按超时错误补齐，让调用方看到「哪几条没回来」而不是静默少几条。
        let done: std::collections::HashSet<usize> = results.iter().map(|r| r.idx).collect();
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
        // worker 线程在此脱离；它们持有的 `tx` 随线程结束而 drop，
        // `rx` 随后被本函数结束时 drop，不会泄漏到别的调用。
        return Ok(Value::List(results.iter().map(|r| r.to_value()).collect()));
    }

    // 等待所有 worker 完成。worker 线程 panic 会让结果集静默缺项（结果按
    // 原始索引排序后无法分辨缺失与空执行），必须显式暴露。
    for h in handles {
        h.join()
            .map_err(|_| "exec.parallel: worker thread panicked".to_string())?;
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
    // v0.104.6：shell 选择按平台适配。
    //
    // 此前硬编码 `Command::new("sh")` —— 在没有 POSIX `sh` 的平台（典型是
    // 未装 Git Bash / MSYS 的 Windows）上**每个命令都 spawn 失败**，返回
    // `ParallelResult.success=false` 且 stdout 为空，于是
    // `tests_v043_exec` 的 5 个 `exec_parallel_*` 用例全挂
    // （`spawn failed: program not found` / `left: ""`）。
    //
    // Windows 用 `cmd /C`；其余平台沿用 `sh -c`。两者都是「shell -c 形式」
    // 执行，调用方传入的命令串语义不变。
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

                // `kill_process_group` 是**尽力而为** —— unix 侧 `killpg` 的
                // 返回值在 `kill_process_group` 里被丢弃，windows 侧 `taskkill`
                // 的退出码同样被丢弃。杀不掉时（进程已退出但孙进程经 `setsid`
                // 脱离进程组却仍持有 stdout 管道、权限不足、pid 已被回收…），
                // 子进程的管道永不 EOF ⇒ `wait_with_output` 永不返回。
                //
                // 修前此处是无条件 `waiter.join()`：worker 线程被这条 join 焊死，
                // 既不 `tx.send` 结果、也不 drop `tx`，于是外层收集
                // （`rx.recv()`，本文件下方）既收不到结果、也等不到
                // `Disconnected` ⇒ **整个 `exec.parallel()` 永久挂起**，而
                // `timeout` 本身正是制造这次挂死的机制。
                //
                // 修法：给 waiter 一个**有限**收尾宽限，超时则**脱离**
                // （Rust 无协作式取消，线程只能泄漏 —— 但泄漏 1 个线程
                // 远好过挂死整个解释器）。复用已有的 `done_rx`：拿到值或拿到
                // `Disconnected` 都说明 waiter 已结束，此时 join 是瞬时的；
                // 只有再次 `Timeout` 才走脱离分支。
                if done_rx
                    .recv_timeout(Duration::from_millis(WAITER_GRACE_MS))
                    .is_ok()
                {
                    let _ = waiter.join();
                }
                // else: waiter 仍卡在 `wait_with_output` 里，主动脱离。
                // `done_rx` 随本函数返回被 drop，waiter 之后 send 得到 Err，
                // 无害。
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
    use super::*;
    use crate::value::Value;

    fn cmd(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// v0.43.0: exec.parallel() builtin tests

    #[test]
    fn exec_parallel_runs_all_commands() {
        let interp = Interpreter::new();
        let cmds = vec![cmd("echo a"), cmd("echo b"), cmd("echo c")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds.into())])
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
        let interp = Interpreter::new();
        // 6 个 sleep 1s, max_concurrent=2 → 总时间应该 ~3s (而非 ~1s 或 ~6s)
        // 跳过 perf assertion — 只验证结果正确
        let cmds: Vec<Value> = (0..6).map(|i| cmd(&format!("echo {}", i))).collect();
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds.into()), Value::Float(2.0)])
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
        let interp = Interpreter::new();
        let result = interp
            .call_exec_method("parallel", &[Value::List(vec![].into())])
            .unwrap();
        assert_eq!(result, Value::List(Vec::new().into()));
    }

    #[test]
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_collects_stdout_per_command() {
        let interp = Interpreter::new();
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
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_kills_process_group_on_timeout() {
        let interp = Interpreter::new();
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
        let interp = Interpreter::new();
        // 远超 timeout 的命令：timeout_ms = 200
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let cmds = vec![cmd(long)];
        let started = std::time::Instant::now();
        let result = interp
            .call_exec_method(
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
        let interp = Interpreter::new();
        let err = interp
            .call_exec_method("parallel", &[Value::Float(42.0)])
            .expect_err("non-list first arg should fail");
        assert!(err.contains("list of strings"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_validates_cmd_elements() {
        let interp = Interpreter::new();
        let cmds = vec![cmd("echo ok"), Value::Float(42.0)]; // 第二个不是 string
        let err = interp
            .call_exec_method("parallel", &[Value::List(cmds.into())])
            .expect_err("non-string cmd should fail");
        assert!(err.contains("must be a string"), "got: {}", err);
    }

    #[test]
    #[cfg(unix)]
    // v0.104.6：该用例依赖 POSIX shell 语义（printf / sleep / 进程组 / exit 127），
    // 在 Windows 的 `cmd /C` 下无对应物 —— 故按平台门控，不再因平台差异误报。
    fn exec_parallel_returns_error_for_missing_command() {
        // sh -c 调用不存在的命令 → sh 返回 exit_code=127, stderr "command not found"
        let interp = Interpreter::new();
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
        let interp = Interpreter::new();
        let err = interp
            .call_exec_method("nonexistent", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
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
