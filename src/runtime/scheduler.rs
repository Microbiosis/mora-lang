//! Tokio-backed multi-thread task scheduler, structured concurrency groups, typed MPSC
//! channels, multi-channel fair `select`, and hierarchical cooperative cancellation (Wave 2 — Stage W2.4).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context as PollContext, Poll};

use parking_lot::Mutex;
use tokio::runtime::{Builder, Handle, Runtime, RuntimeFlavor};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, Instant};

use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::vm::{execute_method_named, execute_value, execute_value_named};
use crate::value::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CancelReason {
    Cancelled(String),
    TimedOut(String),
}

/// Hierarchical cooperative cancellation and deadline propagation token (`watch`-backed).
///
/// Parent cancellations and deadlines propagate downward to every descendant context,
/// while cancelling a child context leaves its parent unaffected.
#[derive(Clone, Debug)]
pub struct TaskContext {
    tx: Arc<watch::Sender<Option<CancelReason>>>,
    rx: watch::Receiver<Option<CancelReason>>,
    deadline: Option<Instant>,
    parent: Option<Arc<TaskContext>>,
}

impl Default for TaskContext {
    fn default() -> Self {
        Self::root()
    }
}

impl TaskContext {
    /// Create a root cancellation context with no deadline.
    pub fn root() -> Self {
        let (tx, rx) = watch::channel(None);
        Self {
            tx: Arc::new(tx),
            rx,
            deadline: None,
            parent: None,
        }
    }

    /// Derive a linked child context that inherits cancellation and deadlines from `self`.
    pub fn child(&self) -> Self {
        let (tx, rx) = watch::channel(None);
        Self {
            tx: Arc::new(tx),
            rx,
            deadline: None,
            parent: Some(Arc::new(self.clone())),
        }
    }

    /// Derive a linked child context bounded by a relative timeout duration.
    pub fn with_timeout(&self, duration: Duration) -> Self {
        let mut child = self.child();
        child.deadline = Instant::now().checked_add(duration);
        child
    }

    /// Trigger cooperative cancellation across this context and all descendant contexts.
    pub fn cancel(&self, reason: impl Into<String>) {
        let msg = reason.into();
        let _ = self.tx.send_if_modified(|slot| {
            if slot.is_some() {
                false
            } else {
                *slot = Some(CancelReason::Cancelled(msg.clone()));
                true
            }
        });
    }

    /// Compute the earliest effective deadline across `self` and all ancestor contexts.
    pub fn deadline(&self) -> Option<Instant> {
        let mut earliest = self.deadline;
        let mut cur = self.parent.as_deref();
        while let Some(ctx) = cur {
            earliest = match (earliest, ctx.deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            cur = ctx.parent.as_deref();
        }
        earliest
    }

    /// Synchronously inspect whether this context or any ancestor is cancelled or past deadline.
    pub fn status(&self) -> MoraResult<()> {
        let now = Instant::now();
        let mut chain: Vec<&TaskContext> = Vec::new();
        chain.push(self);
        let mut cur = self.parent.as_deref();
        while let Some(ctx) = cur {
            chain.push(ctx);
            cur = ctx.parent.as_deref();
        }
        for ctx in chain.into_iter().rev() {
            if let Some(reason) = ctx.rx.borrow().as_ref() {
                return Err(reason.to_error());
            }
            if let Some(dl) = ctx.deadline
                && now >= dl
            {
                let msg = "task deadline exceeded".to_string();
                let _ = ctx.tx.send_if_modified(|slot| {
                    if slot.is_some() {
                        false
                    } else {
                        *slot = Some(CancelReason::TimedOut(msg.clone()));
                        true
                    }
                });
                return Err(MoraError::Timeout(msg));
            }
        }
        Ok(())
    }

    /// Suspend until this context (or any ancestor) is cancelled or its deadline expires.
    pub async fn cancelled(&self) -> MoraError {
        self.cancelled_inner().await
    }

    /// Return true when both context handles refer to the same underlying cancellation cell.
    pub fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tx, &other.tx)
    }

    fn cancelled_inner(&self) -> Pin<Box<dyn Future<Output = MoraError> + Send + '_>> {
        Box::pin(async move {
            if let Err(err) = self.status() {
                return err;
            }
            let mut rx = self.rx.clone();
            let nearest_deadline = self.deadline();
            let parent_fut: Pin<Box<dyn Future<Output = MoraError> + Send + '_>> =
                match &self.parent {
                    Some(parent) => Box::pin(parent.cancelled()),
                    None => Box::pin(std::future::pending()),
                };
            let timeout_fut: Pin<Box<dyn Future<Output = ()> + Send>> = match nearest_deadline {
                Some(dl) => Box::pin(tokio::time::sleep_until(dl)),
                None => Box::pin(std::future::pending()),
            };

            tokio::select! {
                biased;
                res = rx.wait_for(|v| v.is_some()) => {
                    match res {
                        Ok(guard) => guard
                            .as_ref()
                            .map(CancelReason::to_error)
                            .unwrap_or_else(|| MoraError::Cancelled("context cancelled".to_string())),
                        Err(_) => self
                            .status()
                            .err()
                            .unwrap_or_else(|| MoraError::Cancelled("context sender dropped".to_string())),
                    }
                }
                parent_err = parent_fut => parent_err,
                () = timeout_fut => {
                    let msg = "task deadline exceeded".to_string();
                    let _ = self.tx.send_if_modified(|slot| {
                        if slot.is_some() {
                            false
                        } else {
                            *slot = Some(CancelReason::TimedOut(msg.clone()));
                            true
                        }
                    });
                    MoraError::Timeout(msg)
                }
            }
        })
    }
}

impl CancelReason {
    fn to_error(&self) -> MoraError {
        match self {
            Self::Cancelled(msg) => MoraError::Cancelled(msg.clone()),
            Self::TimedOut(msg) => MoraError::Timeout(msg.clone()),
        }
    }
}

enum SenderInner<T> {
    Bounded(mpsc::Sender<T>),
    Unbounded(mpsc::UnboundedSender<T>),
}

impl<T> Clone for SenderInner<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Bounded(tx) => Self::Bounded(tx.clone()),
            Self::Unbounded(tx) => Self::Unbounded(tx.clone()),
        }
    }
}

/// Multi-producer channel sender supporting bounded backpressure, unbounded queuing, and cancellation.
pub struct ChannelSender<T> {
    inner: SenderInner<T>,
}

impl<T> Clone for ChannelSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> ChannelSender<T> {
    /// Attempt to send without suspending, surfacing `MoraError::Channel` on full or closed endpoints.
    pub fn try_send(&self, value: T) -> MoraResult<()> {
        match &self.inner {
            SenderInner::Bounded(tx) => tx.try_send(value).map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => {
                    MoraError::Channel("channel is full".to_string())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    MoraError::Channel("send on closed channel".to_string())
                }
            }),
            SenderInner::Unbounded(tx) => tx
                .send(value)
                .map_err(|_| MoraError::Channel("send on closed channel".to_string())),
        }
    }

    /// Asynchronously send `value`, waiting on bounded capacity until delivered or closed.
    pub async fn send(&self, value: T) -> MoraResult<()> {
        match &self.inner {
            SenderInner::Bounded(tx) => tx
                .send(value)
                .await
                .map_err(|_| MoraError::Channel("send on closed channel".to_string())),
            SenderInner::Unbounded(tx) => tx
                .send(value)
                .map_err(|_| MoraError::Channel("send on closed channel".to_string())),
        }
    }

    /// Asynchronously send `value` while honouring cooperative cancellation or deadlines on `ctx`.
    pub async fn send_ctx(&self, value: T, ctx: &TaskContext) -> MoraResult<()> {
        ctx.status()?;
        tokio::select! {
            biased;
            err = ctx.cancelled() => Err(err),
            res = self.send(value) => res,
        }
    }

    /// Return `true` if the receiving half has been closed or dropped.
    pub fn is_closed(&self) -> bool {
        match &self.inner {
            SenderInner::Bounded(tx) => tx.is_closed(),
            SenderInner::Unbounded(tx) => tx.is_closed(),
        }
    }
}

enum ReceiverInner<T> {
    Bounded(mpsc::Receiver<T>),
    Unbounded(mpsc::UnboundedReceiver<T>),
}

/// Single-consumer channel receiver supporting async receive, non-blocking poll, close, and `select_channels`.
pub struct ChannelReceiver<T> {
    inner: ReceiverInner<T>,
}

impl<T> ChannelReceiver<T> {
    /// Poll the channel without suspending (`Ok(None)` when empty, `Err(Channel)` when disconnected).
    pub fn try_recv(&mut self) -> MoraResult<Option<T>> {
        let res = match &mut self.inner {
            ReceiverInner::Bounded(rx) => rx.try_recv(),
            ReceiverInner::Unbounded(rx) => rx.try_recv(),
        };
        match res {
            Ok(v) => Ok(Some(v)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                Err(MoraError::Channel("channel closed".to_string()))
            }
        }
    }

    /// Asynchronously receive the next item or return `MoraError::Channel` when closed.
    pub async fn recv(&mut self) -> MoraResult<T> {
        let next = match &mut self.inner {
            ReceiverInner::Bounded(rx) => rx.recv().await,
            ReceiverInner::Unbounded(rx) => rx.recv().await,
        };
        next.ok_or_else(|| MoraError::Channel("channel closed".to_string()))
    }

    /// Asynchronously receive while racing against cooperative cancellation/deadlines in `ctx`.
    pub async fn recv_ctx(&mut self, ctx: &TaskContext) -> MoraResult<T> {
        ctx.status()?;
        tokio::select! {
            biased;
            err = ctx.cancelled() => Err(err),
            res = self.recv() => res,
        }
    }

    /// Prevent further sends while still allowing buffered messages to drain via [`Self::recv`].
    pub fn close(&mut self) {
        match &mut self.inner {
            ReceiverInner::Bounded(rx) => rx.close(),
            ReceiverInner::Unbounded(rx) => rx.close(),
        }
    }

    fn poll_recv_item(&mut self, cx: &mut PollContext<'_>) -> Poll<Option<T>> {
        match &mut self.inner {
            ReceiverInner::Bounded(rx) => rx.poll_recv(cx),
            ReceiverInner::Unbounded(rx) => rx.poll_recv(cx),
        }
    }
}

/// Two-branch `select` outcome returned by [`TaskScheduler::select2`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Select2<A, B> {
    First(A),
    Second(B),
}

/// Handle to an independently spawned task executed by [`TaskScheduler`].
pub struct TaskHandle<T> {
    handle: JoinHandle<MoraResult<T>>,
    ctx: TaskContext,
}

impl<T> TaskHandle<T> {
    /// Await completion of the spawned task, mapping Tokio join panics/cancellations to [`MoraError`].
    pub async fn join(self) -> MoraResult<T> {
        match self.handle.await {
            Ok(res) => res,
            Err(join_err) => Err(MoraError::from(join_err)),
        }
    }

    /// Cancel the task's cooperative [`TaskContext`] and request Tokio task abort.
    pub fn abort(&self) {
        self.ctx.cancel("task aborted");
        self.handle.abort();
    }

    /// Inspect whether the underlying Tokio task has completed.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Borrow the cooperative [`TaskContext`] bound to this task.
    pub fn context(&self) -> &TaskContext {
        &self.ctx
    }
}

/// Structured concurrency task group backed by [`tokio::task::JoinSet`].
///
/// Dropping a `TaskGroup` automatically aborts any still-running child tasks, preventing
/// orphan background tasks and future leaks.
pub struct TaskGroup<T: 'static> {
    set: JoinSet<MoraResult<T>>,
    ctx: TaskContext,
    handle: Handle,
}

impl<T: Send + 'static> TaskGroup<T> {
    /// Spawn an async future inside this structured concurrency group under a child `TaskContext`.
    pub fn spawn<F, Fut>(&mut self, build: F)
    where
        F: FnOnce(TaskContext) -> Fut,
        Fut: Future<Output = MoraResult<T>> + Send + 'static,
    {
        let child_ctx = self.ctx.child();
        self.set.spawn_on(build(child_ctx), &self.handle);
    }

    /// Spawn a synchronous/blocking computation onto the scheduler's blocking thread pool.
    pub fn spawn_blocking<F>(&mut self, f: F)
    where
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        let child_ctx = self.ctx.child();
        self.set
            .spawn_blocking_on(move || f(child_ctx), &self.handle);
    }

    /// Await the next completed task in completion order, aborting remaining tasks on cancellation.
    pub async fn join_next(&mut self) -> Option<MoraResult<T>> {
        if let Err(reason) = self.ctx.status() {
            self.set.abort_all();
            return Some(Err(reason));
        }
        tokio::select! {
            biased;
            err = self.ctx.cancelled() => {
                self.set.abort_all();
                Some(Err(err))
            }
            next = self.set.join_next() => {
                match next? {
                    Ok(res) => Some(res),
                    Err(join_err) => Some(Err(MoraError::from(join_err))),
                }
            }
        }
    }

    /// Drain all tasks in the group, cancelling siblings on the first failure.
    pub async fn join_all(mut self) -> MoraResult<Vec<T>> {
        let mut results = Vec::new();
        while let Some(res) = self.join_next().await {
            match res {
                Ok(v) => results.push(v),
                Err(err) => {
                    self.ctx.cancel(err.message());
                    self.set.abort_all();
                    return Err(err);
                }
            }
        }
        Ok(results)
    }

    /// Cooperatively cancel and abort all tasks currently registered in this group.
    pub fn abort_all(&mut self) {
        self.ctx.cancel("task group aborted");
        self.set.abort_all();
    }
}

/// Canonical multi-threaded Tokio task scheduler shared across [`CoreRuntime`] clones.
#[derive(Clone, Debug)]
pub struct TaskScheduler {
    runtime: Arc<Runtime>,
}

static SHARED_SCHEDULER: OnceLock<Arc<TaskScheduler>> = OnceLock::new();

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new().expect("default tokio multi-thread runtime must initialize")
    }
}

impl TaskScheduler {
    /// Construct an isolated multi-thread Tokio scheduler.
    pub fn new() -> MoraResult<Self> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .thread_name("mora-worker")
            .build()
            .map_err(|e| MoraError::Task(format!("failed to create tokio runtime: {e}")))?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    /// Construct a multi-thread scheduler with an explicit worker thread count (>= 1).
    pub fn with_worker_threads(worker_threads: usize) -> MoraResult<Self> {
        let threads = worker_threads.max(1);
        let runtime = Builder::new_multi_thread()
            .worker_threads(threads)
            .enable_all()
            .thread_name("mora-worker")
            .build()
            .map_err(|e| MoraError::Task(format!("failed to create tokio runtime: {e}")))?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    /// Process-wide lazily initialized shared scheduler used by `CoreRuntime::default()`.
    pub fn shared() -> Arc<Self> {
        Arc::clone(
            SHARED_SCHEDULER
                .get_or_init(|| Arc::new(Self::new().expect("shared TaskScheduler init failed"))),
        )
    }

    /// Borrow the underlying Tokio runtime [`Handle`].
    pub fn handle(&self) -> Handle {
        self.runtime.handle().clone()
    }

    /// Drive an async future to completion safely from synchronous contexts, multi-thread
    /// Tokio worker threads (`block_in_place`), or current-thread Tokio runtimes (scoped bridge).
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        match Handle::try_current() {
            Ok(current) if current.runtime_flavor() == RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| self.runtime.block_on(future))
            }
            Ok(_) => std::thread::scope(|scope| {
                let rt = Arc::clone(&self.runtime);
                scope
                    .spawn(move || rt.block_on(future))
                    .join()
                    .expect("scoped tokio bridge thread panicked")
            }),
            Err(_) => self.runtime.block_on(future),
        }
    }

    /// Create a bounded FIFO channel returning `(ChannelSender, ChannelReceiver)`.
    pub fn channel<T>(
        &self,
        capacity: usize,
    ) -> MoraResult<(ChannelSender<T>, ChannelReceiver<T>)> {
        if capacity == 0 {
            return Err(MoraError::Channel(
                "bounded channel capacity must be at least 1".to_string(),
            ));
        }
        let (tx, rx) = mpsc::channel(capacity);
        Ok((
            ChannelSender {
                inner: SenderInner::Bounded(tx),
            },
            ChannelReceiver {
                inner: ReceiverInner::Bounded(rx),
            },
        ))
    }

    /// Create an unbounded FIFO channel returning `(ChannelSender, ChannelReceiver)`.
    pub fn unbounded_channel<T>(&self) -> (ChannelSender<T>, ChannelReceiver<T>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            ChannelSender {
                inner: SenderInner::Unbounded(tx),
            },
            ChannelReceiver {
                inner: ReceiverInner::Unbounded(rx),
            },
        )
    }

    /// Create a new structured concurrency [`TaskGroup`] bound to `ctx` (or root context).
    pub fn group<T: Send + 'static>(&self, ctx: Option<TaskContext>) -> TaskGroup<T> {
        TaskGroup {
            set: JoinSet::new(),
            ctx: ctx.unwrap_or_else(TaskContext::root),
            handle: self.handle(),
        }
    }

    /// Spawn an async future with an attached child [`TaskContext`].
    pub fn spawn<T, F, Fut>(&self, ctx: &TaskContext, build: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> Fut,
        Fut: Future<Output = MoraResult<T>> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let fut = build(child_ctx.clone());
        let handle = self.runtime.spawn(fut);
        TaskHandle {
            handle,
            ctx: child_ctx,
        }
    }

    /// Offload blocking CPU work onto Tokio's blocking pool with cooperative cancellation context.
    pub fn spawn_blocking<T, F>(&self, ctx: &TaskContext, f: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let inner_ctx = child_ctx.clone();
        let handle = self.runtime.spawn_blocking(move || f(inner_ctx));
        TaskHandle {
            handle,
            ctx: child_ctx,
        }
    }

    /// Execute a first-class Mora callable [`Value`] on an isolated [`CoreRuntime`] clone
    /// inside the blocking pool, propagating cancellation/deadlines cooperatively into `VmEngine`.
    pub fn spawn_mora_task(
        &self,
        callee: Value,
        args: Vec<Value>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> TaskHandle<Value> {
        let mut isolated = core.clone();
        isolated.autograd = crate::runtime::autograd::AutogradContext::new();
        self.spawn_blocking(ctx, move |task_ctx| {
            task_ctx.status()?;
            isolated.task_context = Some(task_ctx);
            execute_value(callee, args, &mut isolated)
        })
    }

    /// Run a batch of synchronous jobs concurrently and collect results in exact input index order.
    ///
    /// Used by `VmEngine`'s `parallel` worker execution as well as structured fan-out workloads.
    pub fn run_indexed_batch<T, F>(
        &self,
        jobs: Vec<F>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<T>>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        let n = jobs.len();
        let group_ctx = ctx
            .map(TaskContext::child)
            .unwrap_or_else(TaskContext::root);
        group_ctx.status()?;
        let mut group: TaskGroup<(usize, T)> = self.group(Some(group_ctx.clone()));
        for (idx, job) in jobs.into_iter().enumerate() {
            group.spawn_blocking(move |task_ctx| job(task_ctx).map(|val| (idx, val)));
        }
        self.block_on(async move {
            let mut slots: Vec<Option<T>> = (0..n).map(|_| None).collect();
            while let Some(next) = group.join_next().await {
                match next {
                    Ok((idx, val)) => slots[idx] = Some(val),
                    Err(err) => {
                        group.abort_all();
                        return Err(err);
                    }
                }
            }
            Ok(slots
                .into_iter()
                .map(|slot| slot.expect("completed indexed worker slot must be populated"))
                .collect())
        })
    }

    /// Race two async computations and return whichever completes first.
    pub async fn select2<A, B, FA, FB>(&self, fa: FA, fb: FB) -> Select2<A, B>
    where
        FA: Future<Output = A>,
        FB: Future<Output = B>,
    {
        tokio::select! {
            a = fa => Select2::First(a),
            b = fb => Select2::Second(b),
        }
    }

    /// Perform fair round-robin `select` across multiple open [`ChannelReceiver`]s,
    /// optionally gated by a cooperative [`TaskContext`].
    pub async fn select_channels<T>(
        &self,
        receivers: &mut [ChannelReceiver<T>],
        start_hint: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<(usize, T)> {
        if receivers.is_empty() {
            return Err(MoraError::Channel(
                "select_channels requires at least one receiver".to_string(),
            ));
        }
        if let Some(c) = ctx {
            c.status()?;
        }
        let len = receivers.len();
        let mut closed = vec![false; len];
        let recv_fut = std::future::poll_fn(|cx| {
            for offset in 0..len {
                let idx = (start_hint + offset) % len;
                if closed[idx] {
                    continue;
                }
                match receivers[idx].poll_recv_item(cx) {
                    Poll::Ready(Some(val)) => return Poll::Ready(Ok((idx, val))),
                    Poll::Ready(None) => closed[idx] = true,
                    Poll::Pending => {}
                }
            }
            if closed.iter().all(|c| *c) {
                Poll::Ready(Err(MoraError::Channel(
                    "all selected channels are closed".to_string(),
                )))
            } else {
                Poll::Pending
            }
        });

        match ctx {
            Some(c) => {
                tokio::select! {
                    biased;
                    reason = c.cancelled() => Err(reason),
                    res = recv_fut => res,
                }
            }
            None => recv_fut.await,
        }
    }

    /// Spawn a first-class Mora callable returning a cloneable [`MoraTaskHandle`].
    pub fn spawn_mora_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        self.spawn_mora_value_named(callee, args, Vec::new(), core, ctx)
    }

    /// Spawn a callable Mora value with named keyword arguments on an isolated VM task.
    pub fn spawn_mora_value_named(
        &self,
        callee: Value,
        args: Vec<Value>,
        named_args: Vec<(String, Value)>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        let mut isolated = core.clone();
        isolated.autograd = crate::runtime::autograd::AutogradContext::new();
        self.spawn_blocking_mora(ctx, move |task_ctx| {
            task_ctx.status()?;
            isolated.task_context = Some(task_ctx);
            execute_value_named(callee, args, named_args, &mut isolated)
        })
    }

    /// Spawn a method invocation (`receiver.method(args...)`) on an isolated VM task.
    pub fn spawn_mora_method(
        &self,
        receiver: Value,
        method: String,
        args: Vec<Value>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        self.spawn_mora_method_named(receiver, method, args, Vec::new(), core, ctx)
    }

    /// Spawn a method invocation with named keyword arguments on an isolated VM task.
    pub fn spawn_mora_method_named(
        &self,
        receiver: Value,
        method: String,
        args: Vec<Value>,
        named_args: Vec<(String, Value)>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        let mut isolated = core.clone();
        isolated.autograd = crate::runtime::autograd::AutogradContext::new();
        self.spawn_blocking_mora(ctx, move |task_ctx| {
            task_ctx.status()?;
            isolated.task_context = Some(task_ctx);
            execute_method_named(receiver, &method, args, named_args, &mut isolated)
        })
    }

    fn spawn_blocking_mora<F>(&self, ctx: &TaskContext, f: F) -> Arc<MoraTaskHandle>
    where
        F: FnOnce(TaskContext) -> MoraResult<Value> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let inner_ctx = child_ctx.clone();
        let blocking_handle = self.runtime.spawn_blocking(move || f(inner_ctx));
        let task_handle = Arc::new(MoraTaskHandle {
            ctx: child_ctx,
            done: AtomicBool::new(false),
            result: Mutex::new(None),
            waiter: Notify::new(),
            watcher_handle: Mutex::new(None),
        });
        let watcher_shared = Arc::clone(&task_handle);
        let watcher = self.runtime.spawn(async move {
            let outcome = match blocking_handle.await {
                Ok(res) => res,
                Err(join_err) => Err(MoraError::from(join_err)),
            };
            {
                let mut g = watcher_shared.result.lock();
                if g.is_none() {
                    *g = Some(outcome);
                }
            }
            watcher_shared.done.store(true, Ordering::Release);
            watcher_shared.waiter.notify_waiters();
        });
        *task_handle.watcher_handle.lock() = Some(watcher);
        task_handle
    }
}

/// Shareable handle to a spawned Mora concurrent task (`spawn expr`).
#[derive(Debug)]
pub struct MoraTaskHandle {
    ctx: TaskContext,
    done: AtomicBool,
    result: Mutex<Option<MoraResult<Value>>>,
    waiter: Notify,
    watcher_handle: Mutex<Option<JoinHandle<()>>>,
}

impl MoraTaskHandle {
    /// Await task completion, propagating cancellation from `caller_ctx` if cancelled.
    pub async fn join_async(&self, caller_ctx: Option<&TaskContext>) -> MoraResult<Value> {
        if let Some(ctx) = caller_ctx {
            ctx.status()?;
        }
        loop {
            let notified = self.waiter.notified();
            {
                let guard = self.result.lock();
                if let Some(res) = guard.as_ref() {
                    return res.clone();
                }
            }
            match caller_ctx {
                Some(ctx) => {
                    tokio::select! {
                        biased;
                        err = ctx.cancelled() => return Err(err),
                        () = notified => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    /// Cancel the task's cooperative [`TaskContext`] and wake any `.join()` waiters.
    pub fn abort(&self) {
        self.ctx.cancel("task aborted");
        {
            let mut guard = self.result.lock();
            if guard.is_none() {
                *guard = Some(Err(MoraError::Cancelled("task aborted".to_string())));
            }
        }
        self.done.store(true, Ordering::Release);
        self.waiter.notify_waiters();
        if let Some(h) = self.watcher_handle.lock().take() {
            h.abort();
        }
    }

    /// Return whether the spawned task has settled.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Return a handle to the child task's cooperative [`TaskContext`].
    pub fn context(&self) -> TaskContext {
        self.ctx.clone()
    }
}

// ─── First-Class Go-Style MPMC Channel & Multi-Arm Select ───────────────

static NEXT_SELECT_TICKET_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
enum TicketOutcome {
    Waiting,
    Claimed { arm: usize, value: Value },
    Error { message: String },
    Cancelled,
}

#[derive(Debug)]
struct SelectTicket {
    id: u64,
    state: Mutex<TicketOutcome>,
    notify: Notify,
}

impl SelectTicket {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_SELECT_TICKET_ID.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(TicketOutcome::Waiting),
            notify: Notify::new(),
        })
    }

    fn is_waiting(&self) -> bool {
        matches!(*self.state.lock(), TicketOutcome::Waiting)
    }

    fn try_claim_single(&self, arm: usize, value: Value) -> bool {
        let mut g = self.state.lock();
        if matches!(*g, TicketOutcome::Waiting) {
            *g = TicketOutcome::Claimed { arm, value };
            drop(g);
            self.notify.notify_one();
            true
        } else {
            false
        }
    }

    fn try_fail(&self, message: impl Into<String>) -> bool {
        let mut g = self.state.lock();
        if matches!(*g, TicketOutcome::Waiting) {
            *g = TicketOutcome::Error {
                message: message.into(),
            };
            drop(g);
            self.notify.notify_one();
            true
        } else {
            false
        }
    }

    fn cancel_if_waiting(&self) {
        let mut g = self.state.lock();
        if matches!(*g, TicketOutcome::Waiting) {
            *g = TicketOutcome::Cancelled;
            drop(g);
            self.notify.notify_waiters();
        }
    }

    fn take_outcome(&self) -> Option<MoraResult<(usize, Value)>> {
        let g = self.state.lock();
        match &*g {
            TicketOutcome::Waiting | TicketOutcome::Cancelled => None,
            TicketOutcome::Claimed { arm, value } => Some(Ok((*arm, value.clone()))),
            TicketOutcome::Error { message, .. } => Some(Err(MoraError::Channel(message.clone()))),
        }
    }
}

/// Atomically pair two distinct `SelectTicket`s in strict ascending ticket-ID order
/// (guaranteed cycle-free lock hierarchy: channel -> min_id -> max_id).
fn try_claim_ticket_pair(
    sender: &SelectTicket,
    sender_arm: usize,
    receiver: &SelectTicket,
    receiver_arm: usize,
    value: &Value,
) -> bool {
    if sender.id == receiver.id {
        return false;
    }
    if sender.id < receiver.id {
        let mut sg = sender.state.lock();
        let mut rg = receiver.state.lock();
        if matches!(*sg, TicketOutcome::Waiting) && matches!(*rg, TicketOutcome::Waiting) {
            *sg = TicketOutcome::Claimed {
                arm: sender_arm,
                value: Value::Nil,
            };
            *rg = TicketOutcome::Claimed {
                arm: receiver_arm,
                value: value.clone(),
            };
            drop(rg);
            drop(sg);
            sender.notify.notify_one();
            receiver.notify.notify_one();
            true
        } else {
            false
        }
    } else {
        let mut rg = receiver.state.lock();
        let mut sg = sender.state.lock();
        if matches!(*sg, TicketOutcome::Waiting) && matches!(*rg, TicketOutcome::Waiting) {
            *rg = TicketOutcome::Claimed {
                arm: receiver_arm,
                value: value.clone(),
            };
            *sg = TicketOutcome::Claimed {
                arm: sender_arm,
                value: Value::Nil,
            };
            drop(sg);
            drop(rg);
            receiver.notify.notify_one();
            sender.notify.notify_one();
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct SendWaiter {
    arm: usize,
    value: Value,
    ticket: Arc<SelectTicket>,
}

#[derive(Debug)]
struct RecvWaiter {
    arm: usize,
    ticket: Arc<SelectTicket>,
}

#[derive(Debug)]
struct ChannelBufferState {
    capacity: usize,
    closed: bool,
    queue: VecDeque<Value>,
    send_waiters: VecDeque<SendWaiter>,
    recv_waiters: VecDeque<RecvWaiter>,
}

/// First-class MPMC typed channel supporting unbuffered (`capacity == 0`) rendezvous,
/// bounded buffering (`capacity > 0`), explicit `.close()`, cooperative `TaskContext`
/// cancellation, and multi-arm fair `select`.
#[derive(Debug)]
pub struct MoraChannel {
    state: Mutex<ChannelBufferState>,
}

enum PollSendResult {
    Completed,
    Closed,
    Parked,
    AlreadyClaimed,
}

enum PollRecvResult {
    Completed(Value),
    Closed,
    Parked,
    AlreadyClaimed,
}

impl MoraChannel {
    /// Construct a new MPMC channel (`0` = unbuffered Go rendezvous, `> 0` = buffered FIFO).
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(ChannelBufferState {
                capacity,
                closed: false,
                queue: VecDeque::with_capacity(capacity.min(64)),
                send_waiters: VecDeque::new(),
                recv_waiters: VecDeque::new(),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.state.lock().capacity
    }

    pub fn len(&self) -> usize {
        self.state.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.lock().queue.is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    /// Close the channel, failing all pending sends and waking empty-channel receivers.
    pub fn close(&self) {
        let mut st = self.state.lock();
        st.closed = true;
        while let Some(sw) = st.send_waiters.pop_front() {
            sw.ticket.try_fail("send on closed channel");
        }
        while let Some(rw) = st.recv_waiters.pop_front() {
            rw.ticket.try_fail("channel closed");
        }
    }

    /// Non-blocking send: delivers immediately if receiver is waiting or buffer has capacity.
    pub fn try_send(&self, val: Value) -> MoraResult<bool> {
        let mut st = self.state.lock();
        if st.closed {
            return Err(MoraError::Channel("send on closed channel".to_string()));
        }
        while let Some(rw) = st.recv_waiters.pop_front() {
            if rw.ticket.try_claim_single(rw.arm, val.clone()) {
                return Ok(true);
            }
        }
        if st.queue.len() < st.capacity {
            st.queue.push_back(val);
            return Ok(true);
        }
        Ok(false)
    }

    /// Non-blocking receive: pops buffered item or completes an unbuffered sender rendezvous.
    pub fn try_recv(&self) -> MoraResult<Option<Value>> {
        let mut st = self.state.lock();
        if let Some(val) = st.queue.pop_front() {
            while let Some(sw) = st.send_waiters.pop_front() {
                if sw.ticket.try_claim_single(sw.arm, Value::Nil) {
                    st.queue.push_back(sw.value);
                    break;
                }
            }
            return Ok(Some(val));
        }
        while let Some(sw) = st.send_waiters.pop_front() {
            if sw.ticket.try_claim_single(sw.arm, Value::Nil) {
                return Ok(Some(sw.value));
            }
        }
        if st.closed {
            Err(MoraError::Channel("channel closed".to_string()))
        } else {
            Ok(None)
        }
    }

    fn poll_or_park_send(
        &self,
        ticket: &Arc<SelectTicket>,
        arm: usize,
        val: Value,
    ) -> PollSendResult {
        let mut st = self.state.lock();
        if !ticket.is_waiting() {
            return PollSendResult::AlreadyClaimed;
        }
        if st.closed {
            return PollSendResult::Closed;
        }
        let mut skipped_recvs: Vec<RecvWaiter> = Vec::new();
        while let Some(rw) = st.recv_waiters.pop_front() {
            if rw.ticket.id == ticket.id {
                skipped_recvs.push(rw);
                continue;
            }
            if try_claim_ticket_pair(ticket, arm, &rw.ticket, rw.arm, &val) {
                for r in skipped_recvs.into_iter().rev() {
                    st.recv_waiters.push_front(r);
                }
                return PollSendResult::Completed;
            }
            if !ticket.is_waiting() {
                for r in skipped_recvs.into_iter().rev() {
                    st.recv_waiters.push_front(r);
                }
                return PollSendResult::AlreadyClaimed;
            }
        }
        for r in skipped_recvs.into_iter().rev() {
            st.recv_waiters.push_front(r);
        }
        if st.queue.len() < st.capacity {
            if ticket.try_claim_single(arm, Value::Nil) {
                st.queue.push_back(val);
                return PollSendResult::Completed;
            }
            return PollSendResult::AlreadyClaimed;
        }
        st.send_waiters.push_back(SendWaiter {
            arm,
            value: val,
            ticket: Arc::clone(ticket),
        });
        PollSendResult::Parked
    }

    fn poll_or_park_recv(&self, ticket: &Arc<SelectTicket>, arm: usize) -> PollRecvResult {
        let mut st = self.state.lock();
        if !ticket.is_waiting() {
            return PollRecvResult::AlreadyClaimed;
        }
        if !st.queue.is_empty() {
            if !ticket.try_claim_single(arm, st.queue.front().cloned().unwrap_or(Value::Nil)) {
                return PollRecvResult::AlreadyClaimed;
            }
            let val = st.queue.pop_front().unwrap_or(Value::Nil);
            while let Some(sw) = st.send_waiters.pop_front() {
                if sw.ticket.try_claim_single(sw.arm, Value::Nil) {
                    st.queue.push_back(sw.value);
                    break;
                }
            }
            return PollRecvResult::Completed(val);
        }
        let mut skipped_sends: Vec<SendWaiter> = Vec::new();
        while let Some(sw) = st.send_waiters.pop_front() {
            if sw.ticket.id == ticket.id {
                skipped_sends.push(sw);
                continue;
            }
            if try_claim_ticket_pair(&sw.ticket, sw.arm, ticket, arm, &sw.value) {
                for s in skipped_sends.into_iter().rev() {
                    st.send_waiters.push_front(s);
                }
                return PollRecvResult::Completed(sw.value);
            }
            if !ticket.is_waiting() {
                for s in skipped_sends.into_iter().rev() {
                    st.send_waiters.push_front(s);
                }
                return PollRecvResult::AlreadyClaimed;
            }
        }
        for s in skipped_sends.into_iter().rev() {
            st.send_waiters.push_front(s);
        }
        if st.closed {
            return PollRecvResult::Closed;
        }
        st.recv_waiters.push_back(RecvWaiter {
            arm,
            ticket: Arc::clone(ticket),
        });
        PollRecvResult::Parked
    }

    fn prune_inactive(&self, ticket_id: u64) {
        let mut st = self.state.lock();
        st.send_waiters
            .retain(|w| w.ticket.id != ticket_id && w.ticket.is_waiting());
        st.recv_waiters
            .retain(|w| w.ticket.id != ticket_id && w.ticket.is_waiting());
    }

    /// Asynchronously send `val`, blocking until delivered or cancelled via `ctx`.
    pub async fn send_async(&self, val: Value, ctx: Option<&TaskContext>) -> MoraResult<()> {
        if let Some(c) = ctx {
            c.status()?;
        }
        let ticket = SelectTicket::new();
        match self.poll_or_park_send(&ticket, 0, val) {
            PollSendResult::Completed => return Ok(()),
            PollSendResult::Closed => {
                return Err(MoraError::Channel("send on closed channel".to_string()));
            }
            PollSendResult::AlreadyClaimed => {
                return ticket
                    .take_outcome()
                    .unwrap_or(Ok((0, Value::Nil)))
                    .map(|_| ());
            }
            PollSendResult::Parked => {}
        }
        let _guard = TicketCleanupGuard {
            ticket: Arc::clone(&ticket),
            channels: vec![self],
        };
        let notified = ticket.notify.notified();
        if let Some(outcome) = ticket.take_outcome() {
            return outcome.map(|_| ());
        }
        match ctx {
            Some(c) => {
                tokio::select! {
                    biased;
                    () = notified => ticket.take_outcome().unwrap_or(Ok((0, Value::Nil))).map(|_| ()),
                    err = c.cancelled() => {
                        ticket.cancel_if_waiting();
                        if let Some(outcome) = ticket.take_outcome() {
                            outcome.map(|_| ())
                        } else {
                            Err(err)
                        }
                    }
                }
            }
            None => {
                notified.await;
                ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string())))
                    .map(|_| ())
            }
        }
    }

    /// Asynchronously receive next value, blocking until delivered, closed, or cancelled.
    pub async fn recv_async(&self, ctx: Option<&TaskContext>) -> MoraResult<Value> {
        if let Some(c) = ctx {
            c.status()?;
        }
        let ticket = SelectTicket::new();
        match self.poll_or_park_recv(&ticket, 0) {
            PollRecvResult::Completed(v) => return Ok(v),
            PollRecvResult::Closed => {
                return Err(MoraError::Channel("channel closed".to_string()));
            }
            PollRecvResult::AlreadyClaimed => {
                return ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string())))
                    .map(|(_, v)| v);
            }
            PollRecvResult::Parked => {}
        }
        let _guard = TicketCleanupGuard {
            ticket: Arc::clone(&ticket),
            channels: vec![self],
        };
        let notified = ticket.notify.notified();
        if let Some(outcome) = ticket.take_outcome() {
            return outcome.map(|(_, v)| v);
        }
        match ctx {
            Some(c) => {
                tokio::select! {
                    biased;
                    () = notified => ticket
                        .take_outcome()
                        .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string())))
                        .map(|(_, v)| v),
                    err = c.cancelled() => {
                        ticket.cancel_if_waiting();
                        if let Some(outcome) = ticket.take_outcome() {
                            outcome.map(|(_, v)| v)
                        } else {
                            Err(err)
                        }
                    }
                }
            }
            None => {
                notified.await;
                ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string())))
                    .map(|(_, v)| v)
            }
        }
    }
}

struct TicketCleanupGuard<'a> {
    ticket: Arc<SelectTicket>,
    channels: Vec<&'a MoraChannel>,
}

impl Drop for TicketCleanupGuard<'_> {
    fn drop(&mut self) {
        self.ticket.cancel_if_waiting();
        for ch in &self.channels {
            ch.prune_inactive(self.ticket.id);
        }
    }
}

/// Branch descriptor passed to [`select_arms`].
#[derive(Debug, Clone)]
pub enum SelectBranch {
    Recv(Arc<MoraChannel>),
    Send(Arc<MoraChannel>, Value),
    Timeout(Duration),
    Default,
}

/// Execute Go-style fair multi-branch `select` across channel recv/send, timeout, and default arms.
pub async fn select_arms(
    arms: Vec<SelectBranch>,
    start_hint: usize,
    ctx: Option<&TaskContext>,
) -> MoraResult<(usize, Value)> {
    if arms.is_empty() {
        return Err(MoraError::Runtime(
            "select requires at least one arm".to_string(),
        ));
    }
    if let Some(c) = ctx {
        c.status()?;
    }

    let len = arms.len();
    let start = start_hint % len;
    let ticket = SelectTicket::new();
    let mut watched_channels: Vec<&MoraChannel> = Vec::new();
    let mut default_arm: Option<usize> = None;
    let mut earliest_timeout: Option<(usize, Instant)> = None;
    let now = Instant::now();

    // First record default and earliest timeout deadline without blocking channels.
    for (idx, arm) in arms.iter().enumerate() {
        match arm {
            SelectBranch::Default if default_arm.is_none() => default_arm = Some(idx),
            SelectBranch::Timeout(dur) => {
                let dl = now.checked_add(*dur).unwrap_or(now);
                match earliest_timeout {
                    Some((_, cur)) if dl >= cur => {}
                    _ => earliest_timeout = Some((idx, dl)),
                }
            }
            _ => {}
        }
    }

    // Notice! To guard against self-rendezvous when a single `select` specifies BOTH
    // Send(ch, v) and Recv(ch) on the same unbuffered channel, only temporarily register
    // after probing — Wait: we handle that via `try_claim_ticket_pair` checking `sender.id != receiver.id`,
    // AND wait: `poll_or_park_recv` previously skipped & dropped self send_waiters via `pop_front` if it popped them!
    // Check that! When probing channels here, a channel appearing in both Send and Recv of the same `select`
    // only leaves non-self waiters untouched if we probe all arms cleanly without dropping same-ticket waiters!
    // Wait: let's handle same-ticket waiters safely inside `poll_or_park_recv`: don't drop same-ticket waiters!
    let mut cleanup_guard = TicketCleanupGuard {
        ticket: Arc::clone(&ticket),
        channels: Vec::new(),
    };

    for offset in 0..len {
        let idx = (start + offset) % len;
        match &arms[idx] {
            SelectBranch::Recv(ch) => {
                watched_channels.push(ch.as_ref());
                match ch.poll_or_park_recv(&ticket, idx) {
                    PollRecvResult::Completed(v) => return Ok((idx, v)),
                    PollRecvResult::Closed => {
                        ticket.cancel_if_waiting();
                        cleanup_guard.channels = watched_channels;
                        return Err(MoraError::Channel("channel closed".to_string()));
                    }
                    PollRecvResult::AlreadyClaimed => {
                        cleanup_guard.channels = watched_channels;
                        return ticket.take_outcome().unwrap_or_else(|| {
                            Err(MoraError::Channel("channel closed".to_string()))
                        });
                    }
                    PollRecvResult::Parked => {}
                }
            }
            SelectBranch::Send(ch, val) => {
                watched_channels.push(ch.as_ref());
                match ch.poll_or_park_send(&ticket, idx, val.clone()) {
                    PollSendResult::Completed => return Ok((idx, Value::Nil)),
                    PollSendResult::Closed => {
                        ticket.cancel_if_waiting();
                        cleanup_guard.channels = watched_channels;
                        return Err(MoraError::Channel("send on closed channel".to_string()));
                    }
                    PollSendResult::AlreadyClaimed => {
                        cleanup_guard.channels = watched_channels;
                        return ticket.take_outcome().unwrap_or(Ok((idx, Value::Nil)));
                    }
                    PollSendResult::Parked => {}
                }
            }
            SelectBranch::Timeout(_) | SelectBranch::Default => {}
        }
    }
    cleanup_guard.channels = watched_channels;

    // If a 0ms timeout already elapsed and no channel arm won synchronously, fire timeout first.
    if let Some((t_idx, dl)) = earliest_timeout
        && Instant::now() >= dl
    {
        ticket.cancel_if_waiting();
        if let Some(outcome) = ticket.take_outcome() {
            return outcome;
        }
        return Ok((t_idx, Value::Nil));
    }

    // If `default` is present and no channel arm was immediately ready, pick `default`.
    if let Some(d_idx) = default_arm {
        ticket.cancel_if_waiting();
        if let Some(outcome) = ticket.take_outcome() {
            return outcome;
        }
        return Ok((d_idx, Value::Nil));
    }

    let notified = ticket.notify.notified();
    if let Some(outcome) = ticket.take_outcome() {
        return outcome;
    }

    let timeout_fut: Pin<Box<dyn Future<Output = usize> + Send>> = match earliest_timeout {
        Some((t_idx, dl)) => Box::pin(async move {
            tokio::time::sleep_until(dl).await;
            t_idx
        }),
        None => Box::pin(std::future::pending()),
    };

    match ctx {
        Some(c) => {
            tokio::select! {
                biased;
                () = notified => ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string()))),
                t_idx = timeout_fut => {
                    ticket.cancel_if_waiting();
                    if let Some(outcome) = ticket.take_outcome() {
                        outcome
                    } else {
                        Ok((t_idx, Value::Nil))
                    }
                }
                err = c.cancelled() => {
                    ticket.cancel_if_waiting();
                    if let Some(outcome) = ticket.take_outcome() {
                        outcome
                    } else {
                        Err(err)
                    }
                }
            }
        }
        None => {
            tokio::select! {
                biased;
                () = notified => ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string()))),
                t_idx = timeout_fut => {
                    ticket.cancel_if_waiting();
                    if let Some(outcome) = ticket.take_outcome() {
                        outcome
                    } else {
                        Ok((t_idx, Value::Nil))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_parent_child_cancellation_propagation() {
        let root = TaskContext::root();
        let child = root.child();
        let grandchild = child.child();

        assert!(root.status().is_ok());
        assert!(child.status().is_ok());
        assert!(grandchild.status().is_ok());

        // Cancelling a sibling child must not cancel root or other children.
        let sibling = root.child();
        sibling.cancel("sibling stopped");
        assert!(sibling.status().is_err());
        assert!(root.status().is_ok());
        assert!(child.status().is_ok());

        // Cancelling root propagates through all descendants.
        root.cancel("shutdown");
        assert_eq!(
            child.status().unwrap_err(),
            MoraError::Cancelled("shutdown".to_string())
        );
        assert_eq!(
            grandchild.status().unwrap_err(),
            MoraError::Cancelled("shutdown".to_string())
        );
    }

    #[test]
    fn channel_bounded_backpressure_and_close() {
        let sched = TaskScheduler::with_worker_threads(2).expect("scheduler");
        assert!(sched.channel::<i32>(0).is_err());

        let (tx, mut rx) = sched.channel::<i32>(2).expect("bounded");
        tx.try_send(1).expect("slot 1");
        tx.try_send(2).expect("slot 2");
        let full = tx.try_send(3).unwrap_err();
        assert_eq!(full.kind_name(), "channel");

        assert_eq!(rx.try_recv().unwrap(), Some(1));
        assert_eq!(rx.try_recv().unwrap(), Some(2));
        assert_eq!(rx.try_recv().unwrap(), None);

        rx.close();
        assert!(tx.is_closed());
        assert_eq!(tx.try_send(4).unwrap_err().kind_name(), "channel");
    }

    #[test]
    fn select2_and_select_channels_and_unbounded() {
        let sched = TaskScheduler::with_worker_threads(2).expect("scheduler");
        let (tx1, rx1) = sched.unbounded_channel::<&'static str>();
        let (tx2, rx2) = sched.channel::<&'static str>(4).expect("chan2");
        let ctx = TaskContext::root();

        sched.block_on(async {
            tx1.send_ctx("one", &ctx).await.expect("send1");
            tx2.send("two").await.expect("send2");
            let mut rxs = [rx1, rx2];
            let first = sched
                .select_channels(&mut rxs, 0, Some(&ctx))
                .await
                .expect("first");
            let second = sched
                .select_channels(&mut rxs, 1, Some(&ctx))
                .await
                .expect("second");
            assert_eq!(first, (0, "one"));
            assert_eq!(second, (1, "two"));

            let s = sched
                .select2(
                    async {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        "later"
                    },
                    async { "immediate" },
                )
                .await;
            assert_eq!(s, Select2::Second("immediate"));

            tx2.send("three").await.expect("send3");
            assert_eq!(rxs[1].recv_ctx(&ctx).await.expect("recv3"), "three");
            let cancel_child = ctx.child();
            cancel_child.cancel("stop recv");
            assert_eq!(
                rxs[1]
                    .recv_ctx(&cancel_child)
                    .await
                    .expect_err("cancelled")
                    .kind_name(),
                "cancelled"
            );
            drop(tx1);
            drop(tx2);
            let drain = sched.select_channels(&mut rxs, 0, None).await;
            assert_eq!(drain.unwrap_err().kind_name(), "channel");
        });
    }

    #[test]
    fn task_handle_and_group_cancellation_and_deadline() {
        let sched = TaskScheduler::with_worker_threads(2).expect("scheduler");
        let root = TaskContext::root();
        let deadline_ctx = root.with_timeout(Duration::from_millis(200));
        assert!(deadline_ctx.deadline().is_some());

        let handle = sched.spawn(&root, |ctx| async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.status()?;
            Ok(99usize)
        });
        assert!(!handle.is_finished());
        let _ = handle.context().status();
        handle.abort();
        let joined = sched.block_on(handle.join());
        assert!(joined.is_err());

        let group_res = sched.block_on(async {
            let mut group = sched.group::<usize>(Some(root.clone()));
            group.spawn(|_c| async { Ok(10) });
            group.spawn_blocking(|_c| Ok(20));
            group.join_all().await
        });
        let mut vals = group_res.expect("group join");
        vals.sort_unstable();
        assert_eq!(vals, vec![10, 20]);
    }
}
