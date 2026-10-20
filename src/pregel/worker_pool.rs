//! v0.73: Persistent worker thread pool for parallel BSP EXEC.
//!
//! Mirrors the `exec_parallel` recipe (std::thread + mpsc + index
//! re-sorting) but as a reusable pool: worker threads are spawned once
//! and batch jobs are dispatched through a shared `Mutex<Vec<WorkerJob>>`
//! (dynamic work-stealing = partition) with results collected and
//! re-sorted by original index (the join = barrier).

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A unit of parallel work. `index` is the caller's ordering key;
/// results come back sorted by it (deterministic, order-preserving).
pub struct WorkerJob {
    pub index: usize,
    /// Arbitrary payload closure. Returns `Result<Box<dyn Any + Send>, String>`
    /// so a failing job reports an error instead of panicking the worker
    /// thread (which previously caused `run_batch` to hang forever).
    pub task: Box<dyn FnOnce() -> Result<Box<dyn std::any::Any + Send>, String> + Send>,
}

/// Outcome of a job. Carries `index` so the pool can re-sort.
pub struct WorkerOutcome {
    pub index: usize,
    pub value: Box<dyn std::any::Any + Send>,
}

/// Result of a batch run: outcomes (sorted by index) plus a timeout flag.
pub struct BatchResult {
    pub outcomes: Vec<WorkerOutcome>,
    /// True when the batch did not complete within the deadline.
    /// A timed-out job's outcome is absent; its worker thread is leaked
    /// (Rust has no cooperative thread cancellation) and the pool must
    /// be rebuilt to reclaim it.
    pub timed_out: bool,
}

/// Batch message sent to all workers: a shared job queue + the result channel.
/// Workers send `Ok(WorkerOutcome)` on success or `Err(String)` on job failure.
struct BatchMsg {
    jobs: Arc<Mutex<Vec<WorkerJob>>>,
    res_tx: Sender<Result<WorkerOutcome, String>>,
}

/// Persistent thread pool.
pub struct WorkerPool {
    tx: Option<Sender<BatchMsg>>,
    workers: Vec<JoinHandle<()>>,
    /// Number of alive workers (for introspection/tests).
    num_workers: usize,
}

impl WorkerPool {
    /// Spawn `num_workers` worker threads that wait for batches.
    ///
    /// `Receiver` is not Clone, so workers share the receiver via
    /// `Arc<Mutex<Receiver>>` — the mutex is uncontended because each
    /// worker blocks on `recv` (which releases the lock).
    pub fn new(num_workers: usize) -> Self {
        let (tx, rx) = channel::<BatchMsg>();
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let rx = rx.clone();
            workers.push(std::thread::spawn(move || {
                loop {
                    // Block for the next batch (releases mutex while waiting).
                    let msg = {
                        let guard = rx.lock().expect("worker 结果通道不变量已破损（结果可能丢失且永不补发） — 恢复会掩盖已破损的不变量，见 src/common/lock.rs");
                        guard.recv()
                    };
                    let Ok(msg) = msg else { break }; // channel closed → exit
                    // Grab jobs from the shared queue until empty.
                    loop {
                        let job = {
                            let mut queue = msg.jobs.lock().expect("worker 任务队列不变量已破损（任务可能已出队但未被执行） — 恢复会掩盖已破损的不变量，见 src/common/lock.rs");
                            queue.pop()
                        };
                        match job {
                            Some(job) => {
                                let outcome = match (job.task)() {
                                    Ok(value) => Ok(WorkerOutcome {
                                        index: job.index,
                                        value,
                                    }),
                                    Err(e) => Err(e),
                                };
                                if let Err(e) = msg.res_tx.send(outcome) {
                                    eprintln!("[warn] worker pool: failed to send outcome: {}", e);
                                }
                            }
                            None => break, // queue drained → wait for next batch
                        }
                    }
                }
            }));
        }
        WorkerPool {
            tx: Some(tx),
            workers,
            num_workers,
        }
    }

    /// Run a batch of jobs, blocking until all complete, returning
    /// outcomes sorted by `index` (deterministic order).
    ///
    /// This is the BSP barrier: SPAWN → (workers compute in parallel) → JOIN.
    pub fn run_batch(&self, jobs: Vec<WorkerJob>) -> Result<Vec<WorkerOutcome>, String> {
        Ok(self.run_batch_with_timeout(jobs, None)?.outcomes)
    }

    /// Run a batch with an optional per-batch deadline.
    ///
    /// - Returns `Err(String)` if any job failed (worker threads stay alive).
    /// - `BatchResult.timed_out == true` when the deadline elapsed before all
    ///   jobs reported. Timed-out jobs' outcomes are absent and their worker
    ///   threads are leaked (no cancellation) — rebuild the pool to reclaim.
    pub fn run_batch_with_timeout(
        &self,
        jobs: Vec<WorkerJob>,
        timeout: Option<std::time::Duration>,
    ) -> Result<BatchResult, String> {
        let total = jobs.len();
        if total == 0 {
            return Ok(BatchResult {
                outcomes: Vec::new(),
                timed_out: false,
            });
        }
        let (res_tx, res_rx) = channel::<Result<WorkerOutcome, String>>();
        let shared_jobs = Arc::new(Mutex::new(jobs));
        // Broadcast the batch to every worker; each worker pulls from the
        // shared queue. Sending N copies means no worker misses the batch.
        if let Some(tx) = &self.tx {
            for _ in 0..self.num_workers {
                if let Err(e) = tx.send(BatchMsg {
                    jobs: shared_jobs.clone(),
                    res_tx: res_tx.clone(),
                }) {
                    eprintln!("[warn] worker pool: failed to broadcast batch: {}", e);
                }
            }
        }
        drop(res_tx); // workers hold their own clones

        // Collect up to `total` outcomes (one per job) or hit the deadline.
        let mut outcomes: Vec<WorkerOutcome> = Vec::with_capacity(total);
        let mut timed_out = false;
        while outcomes.len() < total {
            let recv = match timeout {
                Some(d) => match res_rx.recv_timeout(d) {
                    Ok(r) => Ok(r),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        timed_out = true;
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err("worker pool channel disconnected".to_string());
                    }
                },
                None => res_rx
                    .recv()
                    .map_err(|_| "worker pool channel closed".to_string()),
            };
            match recv {
                Ok(Ok(out)) => outcomes.push(out),
                Ok(Err(e)) => return Err(e), // a job failed
                Err(e) => return Err(e),
            }
        }
        // Sort by index to restore deterministic order.
        outcomes.sort_by_key(|o| o.index);
        Ok(BatchResult {
            outcomes,
            timed_out,
        })
    }

    pub fn num_workers(&self) -> usize {
        self.num_workers
    }
}

impl Drop for WorkerPool {
    /// **刻意不 join**。
    ///
    /// 修前这里逐个 `w.join()`，而 join 是无界的：worker 正卡在
    /// `(job.task)()` 里执行用户 step 时，它不会去 `recv`，也就看不到
    /// 「channel 已关闭」而退出。于是超时路径
    /// （`pregel/mod.rs`：`self.worker_pool = None; return Err(超时)`）
    /// 在**调用线程**上同步 join 那个正在超时执行的 step —— 重建 pool 的
    /// 动作本身把这一步焊死，所谓「drop 掉泄漏线程」变成了永久挂起。
    ///
    /// std 的 `JoinHandle::join` 没有超时，无法在这里既等待又兜底。两种
    /// 结局只能选一个：
    /// - join  → 卡住的那个 worker 让整个 Pregel 永久挂起（本缺陷）
    /// - 不 join → 卡住的 worker 线程晚些自己结束；空闲的 worker 立刻
    ///   退出（`tx.take()` 关闭 channel ⇒ `recv` 立即返回 `Err` ⇒ break）
    ///
    /// 泄漏 1 个线程远好过挂死整个解释器，故取后者。空闲 worker 的退出
    /// 不需要 join 同步：它们被唤醒后自行结束，没有任何资源依赖调用方。
    fn drop(&mut self) {
        // 关闭 channel：空闲 worker 的 recv 立即返回 Err 并退出。
        self.tx.take();
        // 丢弃 JoinHandle 即「脱离」，不阻塞。
        self.workers.clear();
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_returns_sorted_outcomes() {
        let pool = WorkerPool::new(4);
        let jobs: Vec<WorkerJob> = (0..32)
            .map(|i| WorkerJob {
                index: i,
                task: Box::new(move || {
                    Ok(Box::new(crate::value::Value::Int(i as i64))
                        as Box<dyn std::any::Any + Send>)
                }),
            })
            .collect();
        let outcomes = pool.run_batch(jobs).unwrap();
        assert_eq!(outcomes.len(), 32);
        // Deterministic order by index.
        for (pos, out) in outcomes.iter().enumerate() {
            assert_eq!(out.index, pos);
            let v = out.value.downcast_ref::<crate::value::Value>().unwrap();
            assert_eq!(*v, crate::value::Value::Int(pos as i64));
        }
    }

    #[test]
    fn pool_handles_empty_batch() {
        let pool = WorkerPool::new(2);
        assert!(pool.run_batch(Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn pool_single_worker_preserves_order() {
        let pool = WorkerPool::new(1);
        let jobs: Vec<WorkerJob> = (0..8)
            .map(|i| WorkerJob {
                index: i,
                task: Box::new(move || {
                    Ok(Box::new(crate::value::Value::Int(i as i64 * 10))
                        as Box<dyn std::any::Any + Send>)
                }),
            })
            .collect();
        let outcomes = pool.run_batch(jobs).unwrap();
        assert_eq!(outcomes.len(), 8);
        let v = outcomes[7]
            .value
            .downcast_ref::<crate::value::Value>()
            .unwrap();
        assert_eq!(*v, crate::value::Value::Int(70));
    }

    #[test]
    fn pool_propagates_job_error() {
        let pool = WorkerPool::new(2);
        let jobs: Vec<WorkerJob> = vec![
            WorkerJob {
                index: 0,
                task: Box::new(|| Err("boom".to_string())),
            },
            WorkerJob {
                index: 1,
                task: Box::new(|| Ok(Box::new(42u8))),
            },
        ];
        match pool.run_batch(jobs) {
            Err(e) => assert_eq!(e, "boom"),
            Ok(_) => panic!("batch with failing job must return Err"),
        }
    }

    #[test]
    fn pool_timeout_reports_timed_out() {
        let pool = WorkerPool::new(2);
        let jobs: Vec<WorkerJob> = vec![WorkerJob {
            index: 0,
            task: Box::new(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                Ok(Box::new(1u8))
            }),
        }];
        let res = pool
            .run_batch_with_timeout(jobs, Some(std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(res.timed_out, "short deadline must report timeout");
        assert!(res.outcomes.is_empty(), "timed-out job has no outcome");
    }

    /// 回归：`WorkerPool` 被 drop 时**不得**阻塞在还没跑完的 job 上。
    ///
    /// 修前 `Drop` 逐个 `w.join()`，而 `join` 无超时：超时路径
    /// （`pregel/mod.rs` 的 `self.worker_pool = None; return Err(超时)`）
    /// 会在**调用线程**上同步 join 那个正在超时执行的 step。所谓的
    /// 「drop 掉泄漏线程以重建 pool」因此变成永久挂起 —— 重建动作本身
    /// 把这一步焊死，错误信息永远返回不出去。
    ///
    /// 本用例在**修复前会永久挂起**（而非断言失败），故整个测试套件会卡死；
    /// 这正是该缺陷在 CI 上的表现形态。修复后 drop 立即返回，
    /// 那个 3s 的 worker 线程在后台自行结束。
    #[test]
    fn drop_does_not_block_on_an_in_flight_job() {
        let pool = WorkerPool::new(1);

        // job **不自己结束** —— 它一直等到本测试显式放行为止。这才是缺陷的
        // 真实形态：卡住的是用户 step 本身，不是「跑得久一点」。
        // 修前 drop 会 join 它 ⇒ 永久挂起 ⇒ 整个测试套件卡死（CI 上就
        // 表现为「某个测试不动了」，没有任何断言失败信息）。
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let jobs: Vec<WorkerJob> = vec![WorkerJob {
            index: 0,
            task: Box::new(move || {
                let _ = release_rx.recv();
                Ok(Box::new(1u8))
            }),
        }];
        let res = pool
            .run_batch_with_timeout(jobs, Some(std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(res.timed_out, "前置条件：job 必须处于超时状态");
        assert!(res.outcomes.is_empty(), "前置条件：尚无 outcome");

        let started = std::time::Instant::now();
        drop(pool);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "drop 在 in-flight job 上阻塞了 {:?} —— 修前是永久挂起",
            elapsed
        );

        // 放行后台线程，避免测试自身留下永久阻塞的线程。
        let _ = release_tx.send(());
    }
}
