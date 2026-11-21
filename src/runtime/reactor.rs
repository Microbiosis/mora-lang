//! Zero-dependency non-blocking async I/O reactor (`sleep` / `timeout` timers + `std::net`
//! non-blocking TCP listeners and streams + cooperative `TaskContext` cancellation).

use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::net::{
    Shutdown, SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream, ToSocketAddrs,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::common::sync::Mutex;
use crate::error::{MoraError, MoraResult};
use crate::runtime::scheduler::{AsyncNotify, Select2, TaskContext, TaskScheduler, race2, sleep};

/// Async TCP listener backed by non-blocking [`std::net::TcpListener`] with cancellation support.
pub struct AsyncTcpListener {
    inner: StdTcpListener,
}

impl AsyncTcpListener {
    /// Bind a non-blocking TCP listener on `addr` (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> MoraResult<Self> {
        let inner = StdTcpListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        Ok(Self { inner })
    }

    /// Return the resolved local socket address bound by this listener.
    pub fn local_addr(&self) -> MoraResult<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }

    /// Accept the next incoming TCP connection, aborting cleanly if `ctx` is cancelled or times out.
    pub async fn accept(
        &self,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<(AsyncTcpStream, SocketAddr)> {
        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            match self.inner.accept() {
                Ok((stream, addr)) => {
                    stream.set_nonblocking(true)?;
                    return Ok((AsyncTcpStream { inner: stream }, addr));
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    if let Some(c) = ctx {
                        match race2(c.cancelled(), sleep(Duration::from_millis(1))).await {
                            Select2::First(err) => return Err(err),
                            Select2::Second(()) => {}
                        }
                    } else {
                        sleep(Duration::from_millis(1)).await;
                    }
                }
                Err(e) => return Err(MoraError::from(e)),
            }
        }
    }
}

/// Async buffered-free TCP stream wrapper over non-blocking [`std::net::TcpStream`].
pub struct AsyncTcpStream {
    inner: StdTcpStream,
}

impl AsyncTcpStream {
    /// Connect asynchronously to `addr`, respecting optional timeout and `TaskContext`.
    pub async fn connect(
        addr: &str,
        connect_timeout: Option<Duration>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Self> {
        if let Some(c) = ctx {
            c.status()?;
        }
        let addr_owned = addr.to_string();
        let sched = TaskScheduler::shared();
        let spawn_ctx = ctx.cloned().unwrap_or_else(TaskContext::root);
        let connect_handle = sched.spawn_blocking(&spawn_ctx, move |_c| {
            let mut addrs = addr_owned.to_socket_addrs().map_err(MoraError::from)?;
            let target = addrs.next().ok_or_else(|| {
                MoraError::Io(format!("could not resolve socket address: {addr_owned}"))
            })?;
            let stream = match connect_timeout {
                Some(dur) => StdTcpStream::connect_timeout(&target, dur).map_err(|e| {
                    if e.kind() == ErrorKind::TimedOut {
                        MoraError::Timeout(format!(
                            "TCP connect to {addr_owned} timed out after {dur:?}"
                        ))
                    } else {
                        MoraError::from(e)
                    }
                })?,
                None => StdTcpStream::connect(target).map_err(MoraError::from)?,
            };
            stream.set_nonblocking(true)?;
            Ok(stream)
        });

        let connect_fut = async move {
            match connect_timeout {
                Some(dur) => match race2(connect_handle.join(), sleep(dur)).await {
                    Select2::First(res) => res,
                    Select2::Second(()) => Err(MoraError::Timeout(format!(
                        "TCP connect to {addr} timed out after {dur:?}"
                    ))),
                },
                None => connect_handle.join().await,
            }
        };

        let stream = match ctx {
            Some(c) => match race2(c.cancelled(), connect_fut).await {
                Select2::First(err) => return Err(err),
                Select2::Second(res) => res?,
            },
            None => connect_fut.await?,
        };
        Ok(Self { inner: stream })
    }

    /// Write an entire byte slice asynchronously while honoring cancellation.
    pub async fn write_all(&mut self, data: &[u8], ctx: Option<&TaskContext>) -> MoraResult<()> {
        let mut offset = 0usize;
        while offset < data.len() {
            if let Some(c) = ctx {
                c.status()?;
            }
            match self.inner.write(&data[offset..]) {
                Ok(0) => {
                    return Err(MoraError::Io(
                        "TCP write_all wrote 0 bytes (connection closed)".to_string(),
                    ));
                }
                Ok(n) => offset += n,
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    if let Some(c) = ctx {
                        match race2(c.cancelled(), sleep(Duration::from_millis(1))).await {
                            Select2::First(err) => return Err(err),
                            Select2::Second(()) => {}
                        }
                    } else {
                        sleep(Duration::from_millis(1)).await;
                    }
                }
                Err(e) => return Err(MoraError::from(e)),
            }
        }
        Ok(())
    }

    /// Read up to `buf.len()` bytes asynchronously while honoring cancellation (`0` on EOF).
    pub async fn read(&mut self, buf: &mut [u8], ctx: Option<&TaskContext>) -> MoraResult<usize> {
        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            match self.inner.read(buf) {
                Ok(n) => return Ok(n),
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    if let Some(c) = ctx {
                        match race2(c.cancelled(), sleep(Duration::from_millis(1))).await {
                            Select2::First(err) => return Err(err),
                            Select2::Second(()) => {}
                        }
                    } else {
                        sleep(Duration::from_millis(1)).await;
                    }
                }
                Err(e) => return Err(MoraError::from(e)),
            }
        }
    }

    /// Read up to `max_bytes` until EOF while honoring cancellation.
    pub async fn read_to_limit(
        &mut self,
        max_bytes: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        while out.len() < max_bytes {
            let remaining = max_bytes - out.len();
            let to_read = remaining.min(scratch.len());
            let n = self.read(&mut scratch[..to_read], ctx).await?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&scratch[..n]);
        }
        Ok(out)
    }

    /// Return the local socket address bound to this TCP stream.
    pub fn local_addr(&self) -> MoraResult<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }

    /// Return the remote peer socket address of this TCP stream.
    pub fn peer_addr(&self) -> MoraResult<SocketAddr> {
        Ok(self.inner.peer_addr()?)
    }

    /// Half-close the write side of the TCP connection cleanly.
    pub async fn shutdown(&mut self) -> MoraResult<()> {
        match self.inner.shutdown(Shutdown::Write) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotConnected => Ok(()),
            Err(e) => Err(MoraError::from(e)),
        }
    }
}

/// First-class Mora runtime wrapper over [`AsyncTcpListener`] supporting cooperative
/// [`TaskContext`] cancellation and explicit `.close()` wakeups without holding locks across `.await`.
pub struct MoraTcpListener {
    inner: AsyncTcpListener,
    local_addr: String,
    closed: AtomicBool,
    close_notify: AsyncNotify,
}

impl std::fmt::Debug for MoraTcpListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoraTcpListener")
            .field("local_addr", &self.local_addr)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl MoraTcpListener {
    /// Bind a non-blocking TCP listener on `addr` (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> MoraResult<Arc<Self>> {
        let inner = AsyncTcpListener::bind(addr).await?;
        let local_addr = inner.local_addr()?.to_string();
        Ok(Arc::new(Self {
            inner,
            local_addr,
            closed: AtomicBool::new(false),
            close_notify: AsyncNotify::new(),
        }))
    }

    /// Return the resolved local socket address string.
    pub fn local_addr(&self) -> &str {
        &self.local_addr
    }

    /// Close the listener and wake any pending `accept()` callers.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.close_notify.notify_waiters();
        }
    }

    /// Return whether this listener has been closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Accept the next incoming TCP connection, honoring both `listener.close()` and `ctx`.
    pub async fn accept(&self, ctx: Option<&TaskContext>) -> MoraResult<Arc<MoraTcpStream>> {
        if self.is_closed() {
            return Err(MoraError::Io("TCP listener is closed".to_string()));
        }
        let notified = self.close_notify.notified();
        if self.is_closed() {
            return Err(MoraError::Io("TCP listener is closed".to_string()));
        }
        match race2(notified, self.inner.accept(ctx)).await {
            Select2::First(()) => Err(MoraError::Io("TCP listener is closed".to_string())),
            Select2::Second(res) => {
                let (stream, _peer) = res?;
                MoraTcpStream::from_async(stream)
            }
        }
    }
}

/// First-class Mora runtime wrapper over [`AsyncTcpStream`] with buffered line/chunk reads,
/// async writes, and lock-free `.close()` wakeup + [`TaskContext`] cancellation support.
pub struct MoraTcpStream {
    raw: Mutex<Option<StdTcpStream>>,
    read_buf: Mutex<Vec<u8>>,
    local_addr: String,
    peer_addr: String,
    closed: AtomicBool,
    close_notify: AsyncNotify,
}

impl std::fmt::Debug for MoraTcpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoraTcpStream")
            .field("local_addr", &self.local_addr)
            .field("peer_addr", &self.peer_addr)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl MoraTcpStream {
    /// Wrap an accepted [`AsyncTcpStream`].
    pub fn from_async(stream: AsyncTcpStream) -> MoraResult<Arc<Self>> {
        let local_addr = stream.local_addr()?.to_string();
        let peer_addr = stream.peer_addr()?.to_string();
        Ok(Arc::new(Self {
            raw: Mutex::new(Some(stream.inner)),
            read_buf: Mutex::new(Vec::new()),
            local_addr,
            peer_addr,
            closed: AtomicBool::new(false),
            close_notify: AsyncNotify::new(),
        }))
    }

    /// Connect asynchronously to `addr`, honoring optional timeout and `TaskContext`.
    pub async fn connect(
        addr: &str,
        connect_timeout: Option<Duration>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        let stream = AsyncTcpStream::connect(addr, connect_timeout, ctx).await?;
        Self::from_async(stream)
    }

    /// Return the local socket address string.
    pub fn local_addr(&self) -> &str {
        &self.local_addr
    }

    /// Return the remote peer socket address string.
    pub fn peer_addr(&self) -> &str {
        &self.peer_addr
    }

    /// Return whether this stream has been closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    async fn read_some(&self, buf: &mut [u8], ctx: Option<&TaskContext>) -> MoraResult<usize> {
        loop {
            if self.is_closed() {
                return Err(MoraError::Io("TCP stream is closed".to_string()));
            }
            if let Some(c) = ctx {
                c.status()?;
            }
            let res = {
                let mut g = self.raw.lock();
                let Some(s) = g.as_mut() else {
                    return Err(MoraError::Io("TCP stream is closed".to_string()));
                };
                s.read(buf)
            };
            match res {
                Ok(n) => return Ok(n),
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    let close_fut = self.close_notify.notified();
                    if self.is_closed() {
                        return Err(MoraError::Io("TCP stream is closed".to_string()));
                    }
                    let wait_tick = race2(close_fut, sleep(Duration::from_millis(1)));
                    if let Some(c) = ctx {
                        match race2(c.cancelled(), wait_tick).await {
                            Select2::First(err) => return Err(err),
                            Select2::Second(Select2::First(())) => {
                                return Err(MoraError::Io("TCP stream is closed".to_string()));
                            }
                            Select2::Second(Select2::Second(())) => {}
                        }
                    } else if let Select2::First(()) = wait_tick.await {
                        return Err(MoraError::Io("TCP stream is closed".to_string()));
                    }
                }
                Err(e) => return Err(MoraError::from(e)),
            }
        }
    }

    /// Read up to `max_bytes` bytes (draining internal line-buffer bytes first; empty on EOF).
    pub async fn read_bytes(
        &self,
        max_bytes: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<u8>> {
        if let Some(c) = ctx {
            c.status()?;
        }
        if max_bytes == 0 {
            return Ok(Vec::new());
        }
        {
            let mut rb = self.read_buf.lock();
            if !rb.is_empty() {
                let take = max_bytes.min(rb.len());
                let chunk: Vec<u8> = rb.drain(..take).collect();
                return Ok(chunk);
            }
        }
        let mut buf = vec![0u8; max_bytes.min(65536)];
        let n = self.read_some(&mut buf, ctx).await?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Read bytes up to and including the next `\n` (or EOF / `max_bytes` limit).
    pub async fn read_line_bytes(
        &self,
        max_bytes: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<u8>> {
        if let Some(c) = ctx {
            c.status()?;
        }
        if max_bytes == 0 {
            return Ok(Vec::new());
        }
        loop {
            {
                let mut rb = self.read_buf.lock();
                if let Some(pos) = rb.iter().position(|&b| b == b'\n') {
                    let end = (pos + 1).min(max_bytes);
                    let line: Vec<u8> = rb.drain(..end).collect();
                    return Ok(line);
                }
                if rb.len() >= max_bytes {
                    let line: Vec<u8> = rb.drain(..max_bytes).collect();
                    return Ok(line);
                }
            }
            let mut scratch = [0u8; 4096];
            match self.read_some(&mut scratch, ctx).await {
                Ok(0) => {
                    let mut rb = self.read_buf.lock();
                    return Ok(std::mem::take(&mut *rb));
                }
                Ok(n) => {
                    let mut rb = self.read_buf.lock();
                    rb.extend_from_slice(&scratch[..n]);
                }
                Err(err) => {
                    let mut rb = self.read_buf.lock();
                    if !rb.is_empty() && !err.is_cancellation_or_timeout() {
                        return Ok(std::mem::take(&mut *rb));
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Write all bytes in `data`, returning total byte count written.
    pub async fn write_bytes(&self, data: &[u8], ctx: Option<&TaskContext>) -> MoraResult<usize> {
        if let Some(c) = ctx {
            c.status()?;
        }
        let mut offset = 0usize;
        while offset < data.len() {
            if self.is_closed() {
                return Err(MoraError::Io("TCP stream is closed".to_string()));
            }
            if let Some(c) = ctx {
                c.status()?;
            }
            let res = {
                let mut g = self.raw.lock();
                let Some(s) = g.as_mut() else {
                    return Err(MoraError::Io("TCP stream is closed".to_string()));
                };
                s.write(&data[offset..])
            };
            match res {
                Ok(0) => {
                    return Err(MoraError::Io("TCP stream write returned 0".to_string()));
                }
                Ok(n) => offset += n,
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    let close_fut = self.close_notify.notified();
                    let wait_tick = race2(close_fut, sleep(Duration::from_millis(1)));
                    if let Some(c) = ctx {
                        match race2(c.cancelled(), wait_tick).await {
                            Select2::First(err) => return Err(err),
                            Select2::Second(Select2::First(())) => {
                                return Err(MoraError::Io("TCP stream is closed".to_string()));
                            }
                            Select2::Second(Select2::Second(())) => {}
                        }
                    } else if let Select2::First(()) = wait_tick.await {
                        return Err(MoraError::Io("TCP stream is closed".to_string()));
                    }
                }
                Err(e) => return Err(MoraError::from(e)),
            }
        }
        Ok(data.len())
    }

    /// Half-close the write side of the TCP connection cleanly.
    pub async fn shutdown(&self) -> MoraResult<()> {
        let mut g = self.raw.lock();
        let Some(s) = g.as_mut() else {
            return Ok(());
        };
        match s.shutdown(Shutdown::Write) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotConnected => Ok(()),
            Err(e) => Err(MoraError::from(e)),
        }
    }

    /// Close and drop the underlying TCP stream, waking any pending readers/writers immediately.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(s) = self.raw.lock().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        self.read_buf.lock().clear();
        self.close_notify.notify_waiters();
    }
}

/// Async I/O reactor pairing native timers, sockets, and structured connection multiplexing
/// onto a shared [`TaskScheduler`].
#[derive(Clone, Debug)]
pub struct AsyncIoReactor {
    scheduler: Arc<TaskScheduler>,
}

impl Default for AsyncIoReactor {
    fn default() -> Self {
        Self::new(TaskScheduler::shared())
    }
}

impl AsyncIoReactor {
    /// Construct an `AsyncIoReactor` bound to `scheduler`.
    pub fn new(scheduler: Arc<TaskScheduler>) -> Self {
        Self { scheduler }
    }

    /// Borrow the underlying shared [`TaskScheduler`].
    pub fn scheduler(&self) -> &Arc<TaskScheduler> {
        &self.scheduler
    }

    /// Suspend asynchronously for `duration`, interrupting immediately on `ctx` cancellation/timeout.
    pub async fn sleep(&self, duration: Duration, ctx: Option<&TaskContext>) -> MoraResult<()> {
        if let Some(c) = ctx {
            c.status()?;
            match race2(c.cancelled(), sleep(duration)).await {
                Select2::First(err) => Err(err),
                Select2::Second(()) => Ok(()),
            }
        } else {
            sleep(duration).await;
            Ok(())
        }
    }

    /// Execute a blocking I/O closure on the scheduler's blocking thread pool without
    /// starving async worker threads, interrupting cleanly if `ctx` is cancelled/timed out.
    pub async fn run_blocking_io<T, F>(&self, ctx: Option<&TaskContext>, op: F) -> MoraResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> MoraResult<T> + Send + 'static,
    {
        if let Some(c) = ctx {
            c.status()?;
        }
        let base_ctx = ctx.cloned().unwrap_or_else(TaskContext::root);
        let handle = self.scheduler.spawn_blocking(&base_ctx, move |_c| op());
        if let Some(c) = ctx {
            match race2(c.cancelled(), handle.join()).await {
                Select2::First(err) => Err(err),
                Select2::Second(res) => res,
            }
        } else {
            handle.join().await
        }
    }

    /// Synchronous bridge for executing non-blocking filesystem I/O onto [`Self::run_blocking_io`].
    pub fn run_fs<T, F>(&self, ctx: Option<&TaskContext>, op: F) -> MoraResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> MoraResult<T> + Send + 'static,
    {
        self.scheduler.block_on(self.run_blocking_io(ctx, op))
    }

    /// Bound an arbitrary async operation by `duration`, mapping expiration to [`MoraError::Timeout`].
    pub async fn timeout<T, F>(&self, duration: Duration, future: F) -> MoraResult<T>
    where
        F: Future<Output = MoraResult<T>>,
    {
        let _ = Instant::now();
        match race2(future, sleep(duration)).await {
            Select2::First(inner) => inner,
            Select2::Second(()) => Err(MoraError::Timeout(format!(
                "async I/O operation timed out after {duration:?}"
            ))),
        }
    }

    /// Execute a single bounded TCP request-response exchange against `addr`.
    pub async fn request_response(
        &self,
        addr: &str,
        payload: &[u8],
        max_bytes: usize,
        op_timeout: Duration,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<u8>> {
        self.timeout(op_timeout, async {
            let mut stream = AsyncTcpStream::connect(addr, Some(op_timeout), ctx).await?;
            stream.write_all(payload, ctx).await?;
            stream.shutdown().await?;
            stream.read_to_limit(max_bytes, ctx).await
        })
        .await
    }

    /// Serve incoming TCP connections concurrently on `listener` until either `max_connections`
    /// connections have been accepted and drained, or `ctx` cancels.
    pub async fn serve_connections<F, Fut>(
        &self,
        listener: AsyncTcpListener,
        max_connections: Option<usize>,
        ctx: &TaskContext,
        handler: F,
    ) -> MoraResult<usize>
    where
        F: Fn(AsyncTcpStream, SocketAddr, TaskContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = MoraResult<()>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let mut group = self.scheduler.group::<()>(Some(ctx.child()));
        let mut accepted_count = 0usize;

        while max_connections.is_none_or(|limit| accepted_count < limit) {
            match listener.accept(Some(ctx)).await {
                Ok((stream, peer)) => {
                    accepted_count += 1;
                    let h = Arc::clone(&handler);
                    group.spawn(move |conn_ctx| async move { h(stream, peer, conn_ctx).await });
                }
                Err(err) => {
                    group.abort_all();
                    return Err(err);
                }
            }
        }

        group.join_all().await?;
        Ok(accepted_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactor_sleep_timeout_and_cancellation() {
        let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
        let reactor = AsyncIoReactor::new(Arc::clone(&sched));
        assert!(Arc::ptr_eq(reactor.scheduler(), &sched));

        let ctx = TaskContext::root();
        let cancel_ctx = ctx.clone();
        sched.block_on(async {
            reactor
                .sleep(Duration::from_millis(5), Some(&ctx))
                .await
                .expect("short sleep succeeds");

            let expired = reactor
                .timeout::<(), _>(Duration::from_millis(10), async {
                    sleep(Duration::from_millis(200)).await;
                    Ok(())
                })
                .await
                .unwrap_err();
            assert_eq!(expired.kind_name(), "timeout");

            let h = sched.spawn(&cancel_ctx, move |c| async move {
                reactor.sleep(Duration::from_secs(5), Some(&c)).await
            });
            sleep(Duration::from_millis(15)).await;
            cancel_ctx.cancel("stop sleep");
            assert_eq!(h.join().await.unwrap_err().kind_name(), "cancelled");
        });
    }

    #[test]
    fn reactor_tcp_multiplexed_echo_roundtrip() {
        let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
        let reactor = AsyncIoReactor::new(Arc::clone(&sched));
        let ctx = TaskContext::root();

        sched.block_on(async {
            let listener = AsyncTcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let addr = listener.local_addr().expect("local_addr").to_string();

            let srv_reactor = reactor.clone();
            let server = sched.spawn(&ctx, move |srv_ctx| async move {
                srv_reactor
                    .serve_connections(
                        listener,
                        Some(2),
                        &srv_ctx,
                        |mut stream, _peer, conn_ctx| async move {
                            let req = stream.read_to_limit(1024, Some(&conn_ctx)).await?;
                            stream.write_all(&req, Some(&conn_ctx)).await?;
                            stream.shutdown().await?;
                            Ok(())
                        },
                    )
                    .await
            });

            let r1 = reactor
                .request_response(&addr, b"alpha", 64, Duration::from_secs(2), Some(&ctx))
                .await
                .expect("client 1");
            let r2 = reactor
                .request_response(&addr, b"beta", 64, Duration::from_secs(2), Some(&ctx))
                .await
                .expect("client 2");

            assert_eq!(r1, b"alpha");
            assert_eq!(r2, b"beta");
            let served = server.join().await.expect("server join");
            assert_eq!(served, 2);
        });
    }
}
