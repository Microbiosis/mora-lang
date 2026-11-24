//! Zero-dependency native multi-thread task scheduler, structured concurrency groups,
//! timer wheel/min-heap (`sleep` / `sleep_until`), typed MPSC & MPMC channels,
//! multi-channel fair `select`, and hierarchical cooperative cancellation (`TaskContext`).
//!
//! Built from first principles (`AGENTS.md §0`) directly on Rust's standard library
//! (`std::future`, `std::task`, `std::thread`, and `crate::common::sync`), with zero
//! external runtime dependencies.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar as StdCondvar, Mutex as StdMutex, OnceLock};
use std::task::{Context as PollContext, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use crate::common::sync::Mutex;
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::vm::{
    CallAction, VmEngine, VmOutcome, VmYield, resolve_method_call_named, resolve_value_call_named,
};
use crate::value::Value;

/// Cooperative VM instruction budget per M:N fiber timeslice on `ExecutorPool`.
const FIBER_STEP_BUDGET: usize = 2048;

/// Cooperative 1-poll yield future that hands the current M:N worker back to `ExecutorPool`.
struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

// ─── Native Min-Heap Timer Driver (`sleep` / `sleep_until`) ─────────────────

struct TimerState {
    next_id: u64,
    heap: BinaryHeap<Reverse<(Instant, u64)>>,
    wakers: HashMap<u64, Waker>,
}

struct TimerDriver {
    state: StdMutex<TimerState>,
    cv: StdCondvar,
}

static TIMER_DRIVER: OnceLock<Arc<TimerDriver>> = OnceLock::new();

fn timer_driver() -> &'static Arc<TimerDriver> {
    TIMER_DRIVER.get_or_init(|| {
        let driver = Arc::new(TimerDriver {
            state: StdMutex::new(TimerState {
                next_id: 1,
                heap: BinaryHeap::new(),
                wakers: HashMap::new(),
            }),
            cv: StdCondvar::new(),
        });
        let bg = Arc::clone(&driver);
        thread::Builder::new()
            .name("mora-timer".to_string())
            .spawn(move || timer_loop(bg))
            .expect("failed to spawn mora-timer thread");
        driver
    })
}

fn timer_loop(driver: Arc<TimerDriver>) {
    loop {
        let mut ready_wakers: Vec<Waker> = Vec::new();
        let wait_dur: Option<Duration> = {
            let mut st = driver.state.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            while let Some(&Reverse((deadline, id))) = st.heap.peek() {
                if deadline <= now {
                    st.heap.pop();
                    if let Some(w) = st.wakers.remove(&id) {
                        ready_wakers.push(w);
                    }
                } else {
                    break;
                }
            }
            if !ready_wakers.is_empty() {
                None
            } else if let Some(&Reverse((next_dl, _))) = st.heap.peek() {
                Some(next_dl.saturating_duration_since(now))
            } else {
                Some(Duration::from_secs(3600))
            }
        };

        for w in ready_wakers {
            w.wake();
        }

        if let Some(dur) = wait_dur {
            let st = driver.state.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            let should_wait = match st.heap.peek() {
                Some(&Reverse((dl, _))) => dl > now,
                None => true,
            };
            if should_wait {
                let _ = driver.cv.wait_timeout(st, dur);
            }
        }
    }
}

/// Future returned by [`sleep`] and [`sleep_until`].
pub struct SleepFuture {
    id: u64,
    deadline: Instant,
}

impl Future for SleepFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            if self.id != 0 {
                let drv = timer_driver();
                let mut st = drv.state.lock().unwrap_or_else(|p| p.into_inner());
                st.wakers.remove(&self.id);
                self.id = 0;
            }
            return Poll::Ready(());
        }
        let drv = timer_driver();
        let mut st = drv.state.lock().unwrap_or_else(|p| p.into_inner());
        let wake_thread = st
            .heap
            .peek()
            .is_none_or(|&Reverse((top_dl, _))| self.deadline < top_dl);
        if self.id == 0 {
            let id = st.next_id;
            st.next_id += 1;
            self.id = id;
            st.heap.push(Reverse((self.deadline, id)));
        }
        st.wakers.insert(self.id, cx.waker().clone());
        drop(st);
        if wake_thread {
            drv.cv.notify_one();
        }
        Poll::Pending
    }
}

impl Drop for SleepFuture {
    fn drop(&mut self) {
        if self.id != 0 {
            let drv = timer_driver();
            let mut st = drv.state.lock().unwrap_or_else(|p| p.into_inner());
            st.wakers.remove(&self.id);
        }
    }
}

/// Suspend asynchronously until `duration` has elapsed.
pub fn sleep(duration: Duration) -> SleepFuture {
    SleepFuture {
        id: 0,
        deadline: Instant::now()
            .checked_add(duration)
            .unwrap_or_else(Instant::now),
    }
}

/// Suspend asynchronously until `deadline` is reached.
pub(crate) fn sleep_until(deadline: Instant) -> SleepFuture {
    SleepFuture { id: 0, deadline }
}

// ─── Biased Future Race Combinators (first-ready branch wins) ────────────────

pub(crate) async fn race2<A, B, FA, FB>(fa: FA, fb: FB) -> Select2<A, B>
where
    FA: Future<Output = A>,
    FB: Future<Output = B>,
{
    let mut fa = std::pin::pin!(fa);
    let mut fb = std::pin::pin!(fb);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(a) = fa.as_mut().poll(cx) {
            return Poll::Ready(Select2::First(a));
        }
        if let Poll::Ready(b) = fb.as_mut().poll(cx) {
            return Poll::Ready(Select2::Second(b));
        }
        Poll::Pending
    })
    .await
}

// ─── Native AsyncNotify (permit-based multi-waker wakeup) ───────────────────

#[derive(Default)]
struct NotifyInner {
    epoch: u64,
    permits: usize,
    next_waiter_id: u64,
    waiters: Vec<(u64, Option<Waker>, bool)>,
}

#[derive(Debug, Default)]
pub(crate) struct AsyncNotify {
    inner: Arc<Mutex<NotifyInner>>,
}

impl fmt::Debug for NotifyInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotifyInner")
            .field("epoch", &self.epoch)
            .field("permits", &self.permits)
            .finish()
    }
}

use std::fmt;

pub(crate) struct NotifiedFuture {
    inner: Arc<Mutex<NotifyInner>>,
    start_epoch: u64,
    waiter_id: u64,
}

impl AsyncNotify {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(NotifyInner::default())),
        }
    }

    pub(crate) fn notified(&self) -> NotifiedFuture {
        let mut st = self.inner.lock();
        st.next_waiter_id += 1;
        let wid = st.next_waiter_id;
        let epoch = st.epoch;
        st.waiters.push((wid, None, false));
        NotifiedFuture {
            inner: Arc::clone(&self.inner),
            start_epoch: epoch,
            waiter_id: wid,
        }
    }

    pub(crate) fn notify_one(&self) {
        let waker = {
            let mut st = self.inner.lock();
            if let Some(entry) = st.waiters.iter_mut().find(|(_, _, woken)| !*woken) {
                entry.2 = true;
                entry.1.take()
            } else {
                st.permits += 1;
                None
            }
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    pub(crate) fn notify_waiters(&self) {
        let wakers: Vec<Waker> = {
            let mut st = self.inner.lock();
            st.epoch = st.epoch.wrapping_add(1);
            let mut out = Vec::new();
            for entry in &mut st.waiters {
                entry.2 = true;
                if let Some(w) = entry.1.take() {
                    out.push(w);
                }
            }
            out
        };
        for w in wakers {
            w.wake();
        }
    }
}

impl Future for NotifiedFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<()> {
        let mut st = self.inner.lock();
        if st.epoch != self.start_epoch {
            let wid = self.waiter_id;
            st.waiters.retain(|(id, _, _)| *id != wid);
            drop(st);
            self.waiter_id = 0;
            return Poll::Ready(());
        }
        if let Some(pos) = st
            .waiters
            .iter()
            .position(|(id, _, _)| *id == self.waiter_id)
        {
            if st.waiters[pos].2 {
                st.waiters.swap_remove(pos);
                drop(st);
                self.waiter_id = 0;
                return Poll::Ready(());
            }
            if st.permits > 0 {
                st.permits -= 1;
                st.waiters.swap_remove(pos);
                drop(st);
                self.waiter_id = 0;
                return Poll::Ready(());
            }
            st.waiters[pos].1 = Some(cx.waker().clone());
        } else if st.permits > 0 {
            st.permits -= 1;
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

impl Drop for NotifiedFuture {
    fn drop(&mut self) {
        if self.waiter_id != 0 {
            let mut st = self.inner.lock();
            if let Some(pos) = st
                .waiters
                .iter()
                .position(|(id, _, _)| *id == self.waiter_id)
            {
                let was_woken = st.waiters[pos].2;
                st.waiters.swap_remove(pos);
                if was_woken {
                    if let Some(other) = st.waiters.iter_mut().find(|(_, _, w)| !*w) {
                        other.2 = true;
                        if let Some(w) = other.1.take() {
                            drop(st);
                            w.wake();
                        }
                    } else {
                        st.permits += 1;
                    }
                }
            }
        }
    }
}

// ─── Hierarchical Cooperative Cancellation (`TaskContext`) ──────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum CancelReason {
    Cancelled(String),
    TimedOut(String),
}

impl CancelReason {
    fn to_error(&self) -> MoraError {
        match self {
            Self::Cancelled(msg) => MoraError::Cancelled(msg.clone()),
            Self::TimedOut(msg) => MoraError::Timeout(msg.clone()),
        }
    }
}

#[derive(Debug, Default)]
struct CancelCell {
    reason: Mutex<Option<CancelReason>>,
    notify: AsyncNotify,
}

impl CancelCell {
    fn set_once(&self, reason: CancelReason) {
        let changed = {
            let mut slot = self.reason.lock();
            if slot.is_some() {
                false
            } else {
                *slot = Some(reason);
                true
            }
        };
        if changed {
            self.notify.notify_waiters();
        }
    }

    fn get(&self) -> Option<CancelReason> {
        self.reason.lock().clone()
    }
}

/// Hierarchical cooperative cancellation and deadline propagation token.
///
/// Parent cancellations and deadlines propagate downward to every descendant context,
/// while cancelling a child context leaves its parent unaffected.
#[derive(Clone, Debug)]
pub struct TaskContext {
    cell: Arc<CancelCell>,
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
        Self {
            cell: Arc::new(CancelCell::default()),
            deadline: None,
            parent: None,
        }
    }

    /// Derive a linked child context that inherits cancellation and deadlines from `self`.
    pub fn child(&self) -> Self {
        Self {
            cell: Arc::new(CancelCell::default()),
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
        self.cell.set_once(CancelReason::Cancelled(reason.into()));
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
            if let Some(reason) = ctx.cell.get() {
                return Err(reason.to_error());
            }
            if let Some(dl) = ctx.deadline
                && now >= dl
            {
                let msg = "task deadline exceeded".to_string();
                ctx.cell.set_once(CancelReason::TimedOut(msg.clone()));
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
        Arc::ptr_eq(&self.cell, &other.cell)
    }

    fn cancelled_inner(&self) -> Pin<Box<dyn Future<Output = MoraError> + Send + '_>> {
        Box::pin(async move {
            loop {
                if let Err(err) = self.status() {
                    return err;
                }
                let notified = self.cell.notify.notified();
                if let Err(err) = self.status() {
                    return err;
                }
                let parent_fut: Pin<Box<dyn Future<Output = MoraError> + Send + '_>> =
                    match &self.parent {
                        Some(parent) => Box::pin(parent.cancelled()),
                        None => Box::pin(std::future::pending()),
                    };
                let nearest_deadline = self.deadline();
                let timeout_fut: Pin<Box<dyn Future<Output = ()> + Send>> = match nearest_deadline {
                    Some(dl) => Box::pin(sleep_until(dl)),
                    None => Box::pin(std::future::pending()),
                };

                let self_or_parent = race2(notified, parent_fut);
                match race2(self_or_parent, timeout_fut).await {
                    Select2::First(Select2::First(())) => {
                        if let Err(err) = self.status() {
                            return err;
                        }
                    }
                    Select2::First(Select2::Second(parent_err)) => return parent_err,
                    Select2::Second(()) => {
                        let msg = "task deadline exceeded".to_string();
                        self.cell.set_once(CancelReason::TimedOut(msg.clone()));
                        return MoraError::Timeout(msg);
                    }
                }
            }
        })
    }
}

// ─── Native Async MPSC Channels (`ChannelSender` / `ChannelReceiver`) ───────

struct MpscState<T> {
    capacity: Option<usize>,
    queue: VecDeque<T>,
    closed: bool,
    sender_count: usize,
    recv_waker: Option<Waker>,
    send_wakers: VecDeque<(u64, Waker)>,
    next_send_id: u64,
}

/// Multi-producer channel sender supporting bounded backpressure, unbounded queuing, and cancellation.
pub struct ChannelSender<T> {
    state: Arc<Mutex<MpscState<T>>>,
}

impl<T> Clone for ChannelSender<T> {
    fn clone(&self) -> Self {
        self.state.lock().sender_count += 1;
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<T> Drop for ChannelSender<T> {
    fn drop(&mut self) {
        let wake = {
            let mut st = self.state.lock();
            st.sender_count = st.sender_count.saturating_sub(1);
            if st.sender_count == 0 {
                st.closed = true;
                st.recv_waker.take()
            } else {
                None
            }
        };
        if let Some(w) = wake {
            w.wake();
        }
    }
}

impl<T> ChannelSender<T> {
    /// Attempt to send without suspending, surfacing `MoraError::Channel` on full or closed endpoints.
    pub fn try_send(&self, value: T) -> MoraResult<()> {
        let wake = {
            let mut st = self.state.lock();
            if st.closed {
                return Err(MoraError::Channel("send on closed channel".to_string()));
            }
            if let Some(cap) = st.capacity
                && st.queue.len() >= cap
            {
                return Err(MoraError::Channel("channel is full".to_string()));
            }
            st.queue.push_back(value);
            st.recv_waker.take()
        };
        if let Some(w) = wake {
            w.wake();
        }
        Ok(())
    }

    /// Asynchronously send `value`, waiting on bounded capacity until delivered or closed.
    pub async fn send(&self, value: T) -> MoraResult<()> {
        let mut val_opt = Some(value);
        let mut waiter_id = 0u64;
        let res = std::future::poll_fn(|cx| {
            let mut st = self.state.lock();
            if st.closed {
                return Poll::Ready(Err(MoraError::Channel(
                    "send on closed channel".to_string(),
                )));
            }
            let can_send = st.capacity.is_none_or(|cap| st.queue.len() < cap);
            if can_send {
                if waiter_id != 0 {
                    st.send_wakers.retain(|(id, _)| *id != waiter_id);
                    waiter_id = 0;
                }
                st.queue.push_back(val_opt.take().expect("value present"));
                let rw = st.recv_waker.take();
                drop(st);
                if let Some(w) = rw {
                    w.wake();
                }
                return Poll::Ready(Ok(()));
            }
            if waiter_id == 0 {
                st.next_send_id += 1;
                waiter_id = st.next_send_id;
                st.send_wakers.push_back((waiter_id, cx.waker().clone()));
            } else if let Some(entry) = st.send_wakers.iter_mut().find(|(id, _)| *id == waiter_id) {
                entry.1 = cx.waker().clone();
            } else {
                st.send_wakers.push_back((waiter_id, cx.waker().clone()));
            }
            Poll::Pending
        })
        .await;
        if waiter_id != 0 {
            let mut st = self.state.lock();
            st.send_wakers.retain(|(id, _)| *id != waiter_id);
        }
        res
    }

    /// Asynchronously send `value` while honouring cooperative cancellation or deadlines on `ctx`.
    pub async fn send_ctx(&self, value: T, ctx: &TaskContext) -> MoraResult<()> {
        ctx.status()?;
        match race2(ctx.cancelled(), self.send(value)).await {
            Select2::First(err) => Err(err),
            Select2::Second(res) => res,
        }
    }

    /// Return `true` if the receiving half has been closed or dropped.
    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }
}

/// Single-consumer channel receiver supporting async receive, non-blocking poll, close, and `select_channels`.
pub struct ChannelReceiver<T> {
    state: Arc<Mutex<MpscState<T>>>,
}

impl<T> Drop for ChannelReceiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T> ChannelReceiver<T> {
    /// Poll the channel without suspending (`Ok(None)` when empty, `Err(Channel)` when disconnected).
    pub fn try_recv(&mut self) -> MoraResult<Option<T>> {
        let mut st = self.state.lock();
        if let Some(val) = st.queue.pop_front() {
            let sw = st.send_wakers.pop_front().map(|(_, w)| w);
            drop(st);
            if let Some(w) = sw {
                w.wake();
            }
            return Ok(Some(val));
        }
        if st.closed {
            Err(MoraError::Channel("channel closed".to_string()))
        } else {
            Ok(None)
        }
    }

    /// Asynchronously receive the next item or return `MoraError::Channel` when closed.
    pub async fn recv(&mut self) -> MoraResult<T> {
        let next = std::future::poll_fn(|cx| self.poll_recv_item(cx)).await;
        next.ok_or_else(|| MoraError::Channel("channel closed".to_string()))
    }

    /// Asynchronously receive while racing against cooperative cancellation/deadlines in `ctx`.
    pub async fn recv_ctx(&mut self, ctx: &TaskContext) -> MoraResult<T> {
        ctx.status()?;
        match race2(ctx.cancelled(), self.recv()).await {
            Select2::First(err) => Err(err),
            Select2::Second(res) => res,
        }
    }

    /// Prevent further sends while still allowing buffered messages to drain via [`Self::recv`].
    pub fn close(&mut self) {
        let wakers: Vec<Waker> = {
            let mut st = self.state.lock();
            st.closed = true;
            st.send_wakers.drain(..).map(|(_, w)| w).collect()
        };
        for w in wakers {
            w.wake();
        }
    }

    fn poll_recv_item(&mut self, cx: &mut PollContext<'_>) -> Poll<Option<T>> {
        let mut st = self.state.lock();
        if let Some(val) = st.queue.pop_front() {
            let sw = st.send_wakers.pop_front().map(|(_, w)| w);
            drop(st);
            if let Some(w) = sw {
                w.wake();
            }
            return Poll::Ready(Some(val));
        }
        if st.closed {
            return Poll::Ready(None);
        }
        st.recv_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Two-branch `select` outcome returned by [`TaskScheduler::select2`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Select2<A, B> {
    First(A),
    Second(B),
}

// ─── Native Work-Queue Executor, `TaskHandle` & `TaskGroup` ─────────────────

struct ThreadParkWaker {
    thread: Thread,
    unparked: AtomicBool,
}

impl Wake for ThreadParkWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.unparked.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

struct SharedTaskState<T> {
    done: AtomicBool,
    aborted: AtomicBool,
    result: Mutex<Option<MoraResult<T>>>,
    notify: AsyncNotify,
    future_slot: Mutex<Option<BoxFuture<()>>>,
}

impl<T> SharedTaskState<T> {
    fn complete(&self, res: MoraResult<T>) {
        {
            let mut g = self.result.lock();
            if g.is_none() {
                *g = Some(res);
            }
        }
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn abort(&self) {
        let was_aborted = self.aborted.swap(true, Ordering::AcqRel);
        // Immediately drop the boxed future if not currently being polled on a worker thread.
        if let Some(mut g) = self.future_slot.try_lock() {
            let _ = g.take();
        }
        if !was_aborted {
            self.complete(Err(MoraError::Cancelled("task aborted".to_string())));
        }
    }
}

struct ExecutorTaskState {
    queued: bool,
    running: bool,
    requeue: bool,
    future: Option<BoxFuture<()>>,
}

struct ExecutorTask {
    aborted: Arc<AtomicBool>,
    state: Mutex<ExecutorTaskState>,
    pool: Arc<ExecutorPool>,
}

struct ExecutorPool {
    worker_threads: usize,
    queue: StdMutex<VecDeque<Arc<ExecutorTask>>>,
    cv: StdCondvar,
    shutdown: AtomicBool,
    fiber_preemptions: AtomicU64,
    fiber_async_suspensions: AtomicU64,
}

impl ExecutorPool {
    fn new(worker_threads: usize) -> Arc<Self> {
        let count = worker_threads.max(1);
        let pool = Arc::new(Self {
            worker_threads: count,
            queue: StdMutex::new(VecDeque::new()),
            cv: StdCondvar::new(),
            shutdown: AtomicBool::new(false),
            fiber_preemptions: AtomicU64::new(0),
            fiber_async_suspensions: AtomicU64::new(0),
        });
        for i in 0..count {
            let p = Arc::clone(&pool);
            thread::Builder::new()
                .name(format!("mora-worker-{i}"))
                .spawn(move || worker_loop(p))
                .expect("failed to spawn mora-worker thread");
        }
        pool
    }

    fn enqueue(&self, task: Arc<ExecutorTask>) {
        let should_push = {
            let mut st = task.state.lock();
            if task.aborted.load(Ordering::Acquire) {
                st.future = None;
                false
            } else if st.running {
                st.requeue = true;
                false
            } else if st.future.is_some() && !st.queued {
                st.queued = true;
                true
            } else {
                false
            }
        };
        if should_push {
            let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.push_back(task);
            drop(q);
            self.cv.notify_one();
        }
    }
}

impl Drop for ExecutorPool {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.cv.notify_all();
    }
}

impl Wake for ExecutorTask {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.aborted.load(Ordering::Acquire) {
            if let Some(mut st) = self.state.try_lock() {
                st.future = None;
            }
            return;
        }
        self.pool.enqueue(Arc::clone(self));
    }
}

fn worker_loop(pool: Arc<ExecutorPool>) {
    loop {
        let task = {
            let mut q = pool.queue.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(t) = q.pop_front() {
                    break Some(t);
                }
                if pool.shutdown.load(Ordering::Acquire) {
                    break None;
                }
                q = pool.cv.wait(q).unwrap_or_else(|p| p.into_inner());
            }
        };
        let Some(task) = task else {
            return;
        };
        let mut fut_opt = {
            let mut st = task.state.lock();
            st.queued = false;
            st.requeue = false;
            if task.aborted.load(Ordering::Acquire) {
                st.future = None;
                None
            } else if let Some(f) = st.future.take() {
                st.running = true;
                Some(f)
            } else {
                None
            }
        };
        if let Some(mut fut) = fut_opt.take() {
            let waker = Waker::from(Arc::clone(&task));
            let mut cx = PollContext::from_waker(&waker);
            let poll_res = catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(&mut cx)));
            let requeue_now = {
                let mut st = task.state.lock();
                st.running = false;
                match poll_res {
                    Ok(Poll::Pending) if !task.aborted.load(Ordering::Acquire) => {
                        st.future = Some(fut);
                        if st.requeue && !st.queued {
                            st.requeue = false;
                            st.queued = true;
                            true
                        } else {
                            false
                        }
                    }
                    _ => {
                        st.requeue = false;
                        false
                    }
                }
            };
            if requeue_now {
                let mut q = pool.queue.lock().unwrap_or_else(|p| p.into_inner());
                q.push_back(task);
                drop(q);
                pool.cv.notify_one();
            }
        }
    }
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "task panicked".to_string()
    }
}

/// Handle to an independently spawned task executed by [`TaskScheduler`].
pub struct TaskHandle<T> {
    shared: Arc<SharedTaskState<T>>,
    ctx: TaskContext,
}

impl<T: Send + 'static> TaskHandle<T> {
    /// Await completion of the spawned task, mapping panics/cancellations to [`MoraError`].
    pub async fn join(self) -> MoraResult<T> {
        loop {
            let notified = self.shared.notify.notified();
            if let Some(res) = self.shared.result.lock().take() {
                return res;
            }
            notified.await;
        }
    }

    /// Cancel the task's cooperative [`TaskContext`] and request task abort.
    pub fn abort(&self) {
        self.ctx.cancel("task aborted");
        self.shared.abort();
    }

    /// Inspect whether the underlying task has completed.
    pub fn is_finished(&self) -> bool {
        self.shared.done.load(Ordering::Acquire)
    }

    /// Borrow the cooperative [`TaskContext`] bound to this task.
    pub fn context(&self) -> &TaskContext {
        &self.ctx
    }
}

/// Structured concurrency task group.
///
/// Dropping a `TaskGroup` automatically aborts any still-running child tasks, preventing
/// orphan background tasks and future leaks.
pub struct TaskGroup<T: Send + 'static> {
    handles: VecDeque<TaskHandle<T>>,
    group_notify: Arc<AsyncNotify>,
    ctx: TaskContext,
    scheduler: TaskScheduler,
}

impl<T: Send + 'static> Drop for TaskGroup<T> {
    fn drop(&mut self) {
        self.abort_all();
    }
}

impl<T: Send + 'static> TaskGroup<T> {
    /// Spawn an async future inside this structured concurrency group under a child `TaskContext`.
    pub fn spawn<F, Fut>(&mut self, build: F)
    where
        F: FnOnce(TaskContext) -> Fut,
        Fut: Future<Output = MoraResult<T>> + Send + 'static,
    {
        let g_notify = Arc::clone(&self.group_notify);
        let handle = self
            .scheduler
            .spawn_with_notify(&self.ctx, build, Some(g_notify));
        self.handles.push_back(handle);
    }

    /// Spawn a synchronous/blocking computation onto the scheduler's blocking thread pool.
    pub fn spawn_blocking<F>(&mut self, f: F)
    where
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        let g_notify = Arc::clone(&self.group_notify);
        let handle = self
            .scheduler
            .spawn_blocking_with_notify(&self.ctx, f, Some(g_notify));
        self.handles.push_back(handle);
    }

    /// Await the next completed task in completion order, aborting remaining tasks on cancellation.
    pub async fn join_next(&mut self) -> Option<MoraResult<T>> {
        if self.handles.is_empty() {
            return None;
        }
        if let Err(reason) = self.ctx.status() {
            self.abort_all();
            return Some(Err(reason));
        }
        loop {
            let notified = self.group_notify.notified();
            if let Some(pos) = self.handles.iter().position(|h| h.is_finished()) {
                let h = self.handles.remove(pos).expect("index valid");
                let res = h
                    .shared
                    .result
                    .lock()
                    .take()
                    .unwrap_or_else(|| Err(MoraError::Cancelled("task aborted".to_string())));
                return Some(res);
            }
            if self.handles.is_empty() {
                return None;
            }
            match race2(self.ctx.cancelled(), notified).await {
                Select2::First(err) => {
                    self.abort_all();
                    return Some(Err(err));
                }
                Select2::Second(()) => {}
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
                    self.abort_all();
                    return Err(err);
                }
            }
        }
        Ok(results)
    }

    /// Cooperatively cancel and abort all tasks currently registered in this group.
    pub fn abort_all(&mut self) {
        self.ctx.cancel("task group aborted");
        while let Some(h) = self.handles.pop_front() {
            h.abort();
        }
    }
}

/// Canonical multi-threaded native task scheduler shared across [`CoreRuntime`] clones.
#[derive(Clone)]
pub struct TaskScheduler {
    pool: Arc<ExecutorPool>,
}

impl fmt::Debug for TaskScheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskScheduler").finish()
    }
}

static SHARED_SCHEDULER: OnceLock<Arc<TaskScheduler>> = OnceLock::new();

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new().expect("default TaskScheduler must initialize")
    }
}

impl TaskScheduler {
    /// Construct an isolated multi-thread scheduler.
    pub fn new() -> MoraResult<Self> {
        let threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 16);
        Self::with_worker_threads(threads)
    }

    /// Construct a multi-thread scheduler with an explicit worker thread count (>= 1).
    pub fn with_worker_threads(worker_threads: usize) -> MoraResult<Self> {
        Ok(Self {
            pool: ExecutorPool::new(worker_threads.max(1)),
        })
    }

    /// Process-wide lazily initialized shared scheduler used by `CoreRuntime::default()`.
    pub fn shared() -> Arc<Self> {
        Arc::clone(
            SHARED_SCHEDULER
                .get_or_init(|| Arc::new(Self::new().expect("shared TaskScheduler init failed"))),
        )
    }

    /// Drive an async future to completion safely on the current thread.
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future,
    {
        let park_waker = Arc::new(ThreadParkWaker {
            thread: thread::current(),
            unparked: AtomicBool::new(false),
        });
        let waker = Waker::from(Arc::clone(&park_waker));
        let mut cx = PollContext::from_waker(&waker);
        let mut fut = std::pin::pin!(future);
        loop {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return out;
            }
            while !park_waker.unparked.swap(false, Ordering::Acquire) {
                thread::park();
            }
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
        let state = Arc::new(Mutex::new(MpscState {
            capacity: Some(capacity),
            queue: VecDeque::with_capacity(capacity.min(64)),
            closed: false,
            sender_count: 1,
            recv_waker: None,
            send_wakers: VecDeque::new(),
            next_send_id: 0,
        }));
        Ok((
            ChannelSender {
                state: Arc::clone(&state),
            },
            ChannelReceiver { state },
        ))
    }

    /// Create an unbounded FIFO channel returning `(ChannelSender, ChannelReceiver)`.
    pub fn unbounded_channel<T>(&self) -> (ChannelSender<T>, ChannelReceiver<T>) {
        let state = Arc::new(Mutex::new(MpscState {
            capacity: None,
            queue: VecDeque::new(),
            closed: false,
            sender_count: 1,
            recv_waker: None,
            send_wakers: VecDeque::new(),
            next_send_id: 0,
        }));
        (
            ChannelSender {
                state: Arc::clone(&state),
            },
            ChannelReceiver { state },
        )
    }

    /// Create a new structured concurrency [`TaskGroup`] bound to `ctx` (or root context).
    pub fn group<T: Send + 'static>(&self, ctx: Option<TaskContext>) -> TaskGroup<T> {
        TaskGroup {
            handles: VecDeque::new(),
            group_notify: Arc::new(AsyncNotify::new()),
            ctx: ctx.unwrap_or_else(TaskContext::root),
            scheduler: self.clone(),
        }
    }

    /// Spawn an async future with an attached child [`TaskContext`].
    pub fn spawn<T, F, Fut>(&self, ctx: &TaskContext, build: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> Fut,
        Fut: Future<Output = MoraResult<T>> + Send + 'static,
    {
        self.spawn_with_notify(ctx, build, None)
    }

    fn spawn_with_notify<T, F, Fut>(
        &self,
        ctx: &TaskContext,
        build: F,
        group_notify: Option<Arc<AsyncNotify>>,
    ) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> Fut,
        Fut: Future<Output = MoraResult<T>> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let fut = build(child_ctx.clone());
        let aborted = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(SharedTaskState {
            done: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            result: Mutex::new(None),
            notify: AsyncNotify::new(),
            future_slot: Mutex::new(None),
        });
        let shared_for_task = Arc::clone(&shared);
        let aborted_for_task = Arc::clone(&aborted);
        let wrapped: BoxFuture<()> = Box::pin(async move {
            let mut inner = Box::pin(fut);
            let outcome = std::future::poll_fn(|cx| {
                if shared_for_task.aborted.load(Ordering::Acquire) {
                    aborted_for_task.store(true, Ordering::Release);
                    return Poll::Ready(Err(MoraError::Cancelled("task aborted".to_string())));
                }
                match catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
                    Ok(Poll::Ready(res)) => Poll::Ready(res),
                    Ok(Poll::Pending) => Poll::Pending,
                    Err(payload) => Poll::Ready(Err(MoraError::Task(format!(
                        "task panicked: {}",
                        panic_payload_message(payload)
                    )))),
                }
            })
            .await;
            shared_for_task.complete(outcome);
            if let Some(gn) = group_notify {
                gn.notify_waiters();
            }
        });
        let exec_task = Arc::new(ExecutorTask {
            aborted,
            state: Mutex::new(ExecutorTaskState {
                queued: false,
                running: false,
                requeue: false,
                future: Some(wrapped),
            }),
            pool: Arc::clone(&self.pool),
        });
        self.pool.enqueue(exec_task);
        TaskHandle {
            shared,
            ctx: child_ctx,
        }
    }

    /// Offload blocking CPU work onto a dedicated worker thread with cooperative cancellation context.
    pub fn spawn_blocking<T, F>(&self, ctx: &TaskContext, f: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        self.spawn_blocking_with_notify(ctx, f, None)
    }

    fn spawn_blocking_with_notify<T, F>(
        &self,
        ctx: &TaskContext,
        f: F,
        group_notify: Option<Arc<AsyncNotify>>,
    ) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(TaskContext) -> MoraResult<T> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let inner_ctx = child_ctx.clone();
        let shared = Arc::new(SharedTaskState {
            done: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            result: Mutex::new(None),
            notify: AsyncNotify::new(),
            future_slot: Mutex::new(None),
        });
        let shared_bg = Arc::clone(&shared);
        thread::spawn(move || {
            if shared_bg.aborted.load(Ordering::Acquire) {
                shared_bg.complete(Err(MoraError::Cancelled("task aborted".to_string())));
            } else {
                let outcome = match catch_unwind(AssertUnwindSafe(|| f(inner_ctx))) {
                    Ok(res) => res,
                    Err(payload) => Err(MoraError::Task(format!(
                        "task panicked: {}",
                        panic_payload_message(payload)
                    ))),
                };
                shared_bg.complete(outcome);
            }
            if let Some(gn) = group_notify {
                gn.notify_waiters();
            }
        });
        TaskHandle {
            shared,
            ctx: child_ctx,
        }
    }

    /// Return `(worker_threads, fiber_preemptions, fiber_async_suspensions)` telemetry
    /// for the M:N cooperative VM fiber executor pool.
    pub fn fiber_stats(&self) -> (usize, u64, u64) {
        (
            self.pool.worker_threads,
            self.pool.fiber_preemptions.load(Ordering::Relaxed),
            self.pool.fiber_async_suspensions.load(Ordering::Relaxed),
        )
    }

    /// Execute a first-class Mora callable [`Value`] on an isolated [`CoreRuntime`] clone
    /// as a cooperative M:N VM fiber scheduled on `ExecutorPool` (zero extra OS threads).
    pub fn spawn_mora_task(
        &self,
        callee: Value,
        args: Vec<Value>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> TaskHandle<Value> {
        let mut isolated = core.fork_for_fiber();
        let pool = Arc::clone(&self.pool);
        self.spawn(ctx, move |task_ctx| async move {
            isolated.session.task_context = Some(task_ctx.clone());
            run_mora_fiber_loop(&mut isolated, &task_ctx, &pool, |rt| {
                resolve_value_call_named(callee, args, Vec::new(), rt)
            })
            .await
        })
    }

    /// Run a batch of synchronous jobs concurrently and collect results in exact input index order.
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
        let mut group: TaskGroup<(usize, T)> = self.group(Some(group_ctx));
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
        race2(fa, fb).await
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
            Some(c) => match race2(c.cancelled(), recv_fut).await {
                Select2::First(reason) => Err(reason),
                Select2::Second(res) => res,
            },
            None => recv_fut.await,
        }
    }

    /// Spawn a first-class Mora callable returning a cloneable [`MoraTaskHandle`].
    pub(crate) fn spawn_mora_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        self.spawn_mora_value_named(callee, args, Vec::new(), core, ctx)
    }

    /// Spawn a callable Mora value with named keyword arguments as a cooperative M:N fiber.
    pub(crate) fn spawn_mora_value_named(
        &self,
        callee: Value,
        args: Vec<Value>,
        named_args: Vec<(String, Value)>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        let mut isolated = core.fork_for_fiber();
        let pool = Arc::clone(&self.pool);
        self.spawn_mora_fiber(ctx, move |task_ctx| async move {
            isolated.session.task_context = Some(task_ctx.clone());
            run_mora_fiber_loop(&mut isolated, &task_ctx, &pool, |rt| {
                resolve_value_call_named(callee, args, named_args, rt)
            })
            .await
        })
    }

    /// Spawn a method invocation with named keyword arguments as a cooperative M:N fiber.
    pub(crate) fn spawn_mora_method_named(
        &self,
        receiver: Value,
        method: String,
        args: Vec<Value>,
        named_args: Vec<(String, Value)>,
        core: &CoreRuntime,
        ctx: &TaskContext,
    ) -> Arc<MoraTaskHandle> {
        let mut isolated = core.fork_for_fiber();
        let pool = Arc::clone(&self.pool);
        self.spawn_mora_fiber(ctx, move |task_ctx| async move {
            isolated.session.task_context = Some(task_ctx.clone());
            run_mora_fiber_loop(&mut isolated, &task_ctx, &pool, |rt| {
                resolve_method_call_named(
                    rt,
                    receiver,
                    &method,
                    args,
                    named_args,
                    crate::common::Span::default(),
                )
            })
            .await
        })
    }

    /// P5-3: Spawn an arbitrary native async Rust future onto the cooperative M:N scheduler
    /// returning a first-class cloneable [`MoraTaskHandle`] (used by `ProcessGroup.all_reduce_async`).
    pub(crate) fn spawn_native_future<F, Fut>(
        &self,
        ctx: &TaskContext,
        build: F,
    ) -> Arc<MoraTaskHandle>
    where
        F: FnOnce(TaskContext) -> Fut + Send + 'static,
        Fut: Future<Output = MoraResult<Value>> + Send + 'static,
    {
        self.spawn_mora_fiber(ctx, build)
    }

    fn spawn_mora_fiber<F, Fut>(&self, ctx: &TaskContext, build: F) -> Arc<MoraTaskHandle>
    where
        F: FnOnce(TaskContext) -> Fut + Send + 'static,
        Fut: Future<Output = MoraResult<Value>> + Send + 'static,
    {
        let child_ctx = ctx.child();
        let inner_ctx = child_ctx.clone();
        let task_handle = Arc::new(MoraTaskHandle {
            ctx: child_ctx,
            done: AtomicBool::new(false),
            result: Mutex::new(None),
            waiter: AsyncNotify::new(),
        });
        let watcher_shared = Arc::clone(&task_handle);
        let fut = build(inner_ctx);
        let aborted = Arc::new(AtomicBool::new(false));
        let wrapped: BoxFuture<()> = Box::pin(async move {
            let mut inner = Box::pin(fut);
            let outcome = std::future::poll_fn(|cx| {
                match catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
                    Ok(Poll::Ready(res)) => Poll::Ready(res),
                    Ok(Poll::Pending) => Poll::Pending,
                    Err(payload) => Poll::Ready(Err(MoraError::Task(format!(
                        "task panicked: {}",
                        panic_payload_message(payload)
                    )))),
                }
            })
            .await;
            {
                let mut g = watcher_shared.result.lock();
                if g.is_none() {
                    *g = Some(outcome);
                }
            }
            watcher_shared.done.store(true, Ordering::Release);
            watcher_shared.waiter.notify_waiters();
        });
        let exec_task = Arc::new(ExecutorTask {
            aborted,
            state: Mutex::new(ExecutorTaskState {
                queued: false,
                running: false,
                requeue: false,
                future: Some(wrapped),
            }),
            pool: Arc::clone(&self.pool),
        });
        self.pool.enqueue(exec_task);
        task_handle
    }
}

/// Cooperative M:N fiber driver executing a Mora VM call stack across step-budget preemptions
/// and non-blocking async I/O / timer / channel suspensions on `ExecutorPool`.
async fn run_mora_fiber_loop<F>(
    isolated: &mut CoreRuntime,
    task_ctx: &TaskContext,
    pool: &ExecutorPool,
    init_action: F,
) -> MoraResult<Value>
where
    F: FnOnce(&mut CoreRuntime) -> MoraResult<CallAction>,
{
    task_ctx.status()?;
    let action = init_action(isolated)?;
    let mut outcome = VmEngine::new(isolated).run_action_with_budget(action, FIBER_STEP_BUDGET)?;
    loop {
        match outcome {
            VmOutcome::Completed(val) => return Ok(val),
            VmOutcome::Suspended {
                frames,
                yield_reason: VmYield::Preempt,
            } => {
                pool.fiber_preemptions.fetch_add(1, Ordering::Relaxed);
                yield_now().await;
                task_ctx.status()?;
                outcome =
                    VmEngine::with_frames(isolated, frames).run_with_budget(FIBER_STEP_BUDGET)?;
            }
            VmOutcome::Suspended {
                frames,
                yield_reason: VmYield::AwaitFuture(fut),
            } => {
                pool.fiber_async_suspensions.fetch_add(1, Ordering::Relaxed);
                let awaited = match race2(task_ctx.cancelled(), fut).await {
                    Select2::First(err) => Err(err),
                    Select2::Second(res) => res,
                };
                outcome = match awaited {
                    Ok(val) => VmEngine::with_frames(isolated, frames)
                        .resume_with_value(val, FIBER_STEP_BUDGET)?,
                    Err(err) => VmEngine::with_frames(isolated, frames)
                        .resume_with_error(err, FIBER_STEP_BUDGET)?,
                };
            }
        }
    }
}

/// Shareable handle to a spawned Mora concurrent task (`spawn expr`).
#[derive(Debug)]
pub struct MoraTaskHandle {
    ctx: TaskContext,
    done: AtomicBool,
    result: Mutex<Option<MoraResult<Value>>>,
    waiter: AsyncNotify,
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
                Some(ctx) => match race2(ctx.cancelled(), notified).await {
                    Select2::First(err) => return Err(err),
                    Select2::Second(()) => {}
                },
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
    notify: AsyncNotify,
}

impl SelectTicket {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_SELECT_TICKET_ID.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(TicketOutcome::Waiting),
            notify: AsyncNotify::new(),
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
            TicketOutcome::Error { message } => Some(Err(MoraError::Channel(message.clone()))),
        }
    }
}

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
            Some(c) => match race2(notified, c.cancelled()).await {
                Select2::First(()) => ticket
                    .take_outcome()
                    .unwrap_or(Ok((0, Value::Nil)))
                    .map(|_| ()),
                Select2::Second(err) => {
                    ticket.cancel_if_waiting();
                    if let Some(outcome) = ticket.take_outcome() {
                        outcome.map(|_| ())
                    } else {
                        Err(err)
                    }
                }
            },
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
            Some(c) => match race2(notified, c.cancelled()).await {
                Select2::First(()) => ticket
                    .take_outcome()
                    .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string())))
                    .map(|(_, v)| v),
                Select2::Second(err) => {
                    ticket.cancel_if_waiting();
                    if let Some(outcome) = ticket.take_outcome() {
                        outcome.map(|(_, v)| v)
                    } else {
                        Err(err)
                    }
                }
            },
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

    if let Some((t_idx, dl)) = earliest_timeout
        && Instant::now() >= dl
    {
        ticket.cancel_if_waiting();
        if let Some(outcome) = ticket.take_outcome() {
            return outcome;
        }
        return Ok((t_idx, Value::Nil));
    }

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
            sleep_until(dl).await;
            t_idx
        }),
        None => Box::pin(std::future::pending()),
    };

    match ctx {
        Some(c) => match race2(race2(notified, timeout_fut), c.cancelled()).await {
            Select2::First(Select2::First(())) => ticket
                .take_outcome()
                .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string()))),
            Select2::First(Select2::Second(t_idx)) => {
                ticket.cancel_if_waiting();
                if let Some(outcome) = ticket.take_outcome() {
                    outcome
                } else {
                    Ok((t_idx, Value::Nil))
                }
            }
            Select2::Second(err) => {
                ticket.cancel_if_waiting();
                if let Some(outcome) = ticket.take_outcome() {
                    outcome
                } else {
                    Err(err)
                }
            }
        },
        None => match race2(notified, timeout_fut).await {
            Select2::First(()) => ticket
                .take_outcome()
                .unwrap_or_else(|| Err(MoraError::Channel("channel closed".to_string()))),
            Select2::Second(t_idx) => {
                ticket.cancel_if_waiting();
                if let Some(outcome) = ticket.take_outcome() {
                    outcome
                } else {
                    Ok((t_idx, Value::Nil))
                }
            }
        },
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

        let sibling = root.child();
        sibling.cancel("sibling stopped");
        assert!(sibling.status().is_err());
        assert!(root.status().is_ok());
        assert!(child.status().is_ok());

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
                        sleep(Duration::from_millis(5)).await;
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
            sleep(Duration::from_millis(200)).await;
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
