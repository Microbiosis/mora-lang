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
                        let guard = rx.lock().unwrap();
                        guard.recv()
                    };
                    let Ok(msg) = msg else { break }; // channel closed → exit
                    // Grab jobs from the shared queue until empty.
                    loop {
                        let job = {
                            let mut queue = msg.jobs.lock().unwrap();
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
    fn drop(&mut self) {
        // Closing the channel makes workers exit their recv loop.
        self.tx.take();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
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
}
