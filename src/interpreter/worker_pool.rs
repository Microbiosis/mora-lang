//! v0.73: Persistent worker thread pool for parallel BSP EXEC.
//!
//! Mirrors the `exec_parallel` recipe (std::thread + mpsc + index
//! re-sorting) but as a reusable pool: worker threads are spawned once
//! and batch jobs are dispatched through a shared `Mutex<Vec<WorkerJob>>`
//! (dynamic work-stealing = partition) with results collected and
//! re-sorted by original index (the join = barrier).

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A unit of parallel work. `index` is the caller's ordering key;
/// results come back sorted by it (deterministic, order-preserving).
pub struct WorkerJob {
    pub index: usize,
    /// Arbitrary payload closure. Returns `Box<dyn Any + Send>` so the
    /// pool stays generic; the caller downcasts to its concrete type.
    pub task: Box<dyn FnOnce() -> Box<dyn std::any::Any + Send> + Send>,
}

/// Outcome of a job. Carries `index` so the pool can re-sort.
pub struct WorkerOutcome {
    pub index: usize,
    pub value: Box<dyn std::any::Any + Send>,
}

/// Batch message sent to all workers: a shared job queue + the result channel.
struct BatchMsg {
    jobs: Arc<Mutex<Vec<WorkerJob>>>,
    res_tx: Sender<WorkerOutcome>,
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
                                let value = (job.task)();
                                let _ = msg.res_tx.send(WorkerOutcome { index: job.index, value });
                            }
                            None => break, // queue drained → wait for next batch
                        }
                    }
                }
            }));
        }
        WorkerPool { tx: Some(tx), workers, num_workers }
    }

    /// Run a batch of jobs, blocking until all complete, returning
    /// outcomes sorted by `index` (deterministic order).
    ///
    /// This is the BSP barrier: SPAWN → (workers compute in parallel) → JOIN.
    pub fn run_batch(&self, jobs: Vec<WorkerJob>) -> Vec<WorkerOutcome> {
        let total = jobs.len();
        if total == 0 {
            return Vec::new();
        }
        let (res_tx, res_rx) = channel::<WorkerOutcome>();
        let shared_jobs = Arc::new(Mutex::new(jobs));
        // Broadcast the batch to every worker; each worker pulls from the
        // shared queue. Sending N copies means no worker misses the batch.
        if let Some(tx) = &self.tx {
            for _ in 0..self.num_workers {
                let _ = tx.send(BatchMsg {
                    jobs: shared_jobs.clone(),
                    res_tx: res_tx.clone(),
                });
            }
        }
        drop(res_tx); // workers hold their own clones

        // Collect exactly `total` outcomes (one per job).
        let mut outcomes: Vec<WorkerOutcome> = Vec::with_capacity(total);
        while outcomes.len() < total {
            match res_rx.recv() {
                Ok(out) => outcomes.push(out),
                Err(_) => break, // all senders dropped — shouldn't happen
            }
        }
        // Sort by index to restore deterministic order.
        outcomes.sort_by_key(|o| o.index);
        outcomes
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
                task: Box::new(move || Box::new(crate::value::Value::Int(i as i64)) as Box<dyn std::any::Any + Send>),
            })
            .collect();
        let outcomes = pool.run_batch(jobs);
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
        assert!(pool.run_batch(Vec::new()).is_empty());
    }

    #[test]
    fn pool_single_worker_preserves_order() {
        let pool = WorkerPool::new(1);
        let jobs: Vec<WorkerJob> = (0..8)
            .map(|i| WorkerJob {
                index: i,
                task: Box::new(move || Box::new(crate::value::Value::Int(i as i64 * 10)) as Box<dyn std::any::Any + Send>),
            })
            .collect();
        let outcomes = pool.run_batch(jobs);
        assert_eq!(outcomes.len(), 8);
        let v = outcomes[7].value.downcast_ref::<crate::value::Value>().unwrap();
        assert_eq!(*v, crate::value::Value::Int(70));
    }
}
