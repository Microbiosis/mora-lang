//! Tokio-backed non-blocking async I/O reactor (`tokio::time` timers + `tokio::net` TCP listeners
//! and streams + `tokio::io` async read/write multiplexing with cooperative cancellation) (Wave 2 — Stage W2.4).

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tokio::time::{Duration, sleep, timeout};

use crate::error::{MoraError, MoraResult};
use crate::runtime::scheduler::{TaskContext, TaskScheduler};

/// Async TCP listener backed by [`tokio::net::TcpListener`] with cancellation support.
pub struct AsyncTcpListener {
    inner: TcpListener,
}

impl AsyncTcpListener {
    /// Bind a non-blocking TCP listener on `addr` (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> MoraResult<Self> {
        let inner = TcpListener::bind(addr).await?;
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
        if let Some(c) = ctx {
            c.status()?;
            tokio::select! {
                biased;
                err = c.cancelled() => Err(err),
                accepted = self.inner.accept() => {
                    let (stream, addr) = accepted?;
                    Ok((AsyncTcpStream { inner: stream }, addr))
                }
            }
        } else {
            let (stream, addr) = self.inner.accept().await?;
            Ok((AsyncTcpStream { inner: stream }, addr))
        }
    }
}

/// Async buffered-free TCP stream wrapper over [`tokio::net::TcpStream`].
pub struct AsyncTcpStream {
    inner: TcpStream,
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
        let connect_fut = async {
            match connect_timeout {
                Some(dur) => match timeout(dur, TcpStream::connect(addr)).await {
                    Ok(res) => res.map_err(MoraError::from),
                    Err(_) => Err(MoraError::Timeout(format!(
                        "TCP connect to {addr} timed out after {dur:?}"
                    ))),
                },
                None => TcpStream::connect(addr).await.map_err(MoraError::from),
            }
        };

        let stream = match ctx {
            Some(c) => {
                tokio::select! {
                    biased;
                    err = c.cancelled() => return Err(err),
                    res = connect_fut => res?,
                }
            }
            None => connect_fut.await?,
        };
        Ok(Self { inner: stream })
    }

    /// Write an entire byte slice asynchronously while honoring cancellation.
    pub async fn write_all(&mut self, data: &[u8], ctx: Option<&TaskContext>) -> MoraResult<()> {
        if let Some(c) = ctx {
            c.status()?;
            tokio::select! {
                biased;
                err = c.cancelled() => Err(err),
                res = self.inner.write_all(data) => Ok(res?),
            }
        } else {
            Ok(self.inner.write_all(data).await?)
        }
    }

    /// Read up to `buf.len()` bytes asynchronously while honoring cancellation (`0` on EOF).
    pub async fn read(&mut self, buf: &mut [u8], ctx: Option<&TaskContext>) -> MoraResult<usize> {
        if let Some(c) = ctx {
            c.status()?;
            tokio::select! {
                biased;
                err = c.cancelled() => Err(err),
                res = self.inner.read(buf) => Ok(res?),
            }
        } else {
            Ok(self.inner.read(buf).await?)
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
        Ok(self.inner.shutdown().await?)
    }
}

/// First-class Mora runtime wrapper over [`AsyncTcpListener`] supporting cooperative
/// [`TaskContext`] cancellation and explicit `.close()` wakeups without holding locks across `.await`.
pub struct MoraTcpListener {
    inner: AsyncTcpListener,
    local_addr: String,
    closed: AtomicBool,
    close_notify: Notify,
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
            close_notify: Notify::new(),
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
        tokio::pin!(notified);
        if self.is_closed() {
            return Err(MoraError::Io("TCP listener is closed".to_string()));
        }
        tokio::select! {
            biased;
            () = &mut notified => Err(MoraError::Io("TCP listener is closed".to_string())),
            res = self.inner.accept(ctx) => {
                let (stream, _peer) = res?;
                MoraTcpStream::from_async(stream)
            }
        }
    }
}

struct TcpStreamState {
    stream: Option<AsyncTcpStream>,
    read_buf: Vec<u8>,
}

/// First-class Mora runtime wrapper over [`AsyncTcpStream`] with buffered line/chunk reads,
/// async writes, and [`TaskContext`] cancellation support.
pub struct MoraTcpStream {
    state: Mutex<TcpStreamState>,
    local_addr: String,
    peer_addr: String,
    closed: AtomicBool,
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
            state: Mutex::new(TcpStreamState {
                stream: Some(stream),
                read_buf: Vec::new(),
            }),
            local_addr,
            peer_addr,
            closed: AtomicBool::new(false),
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
        let mut guard = self.state.lock().await;
        if !guard.read_buf.is_empty() {
            let take = max_bytes.min(guard.read_buf.len());
            let chunk: Vec<u8> = guard.read_buf.drain(..take).collect();
            return Ok(chunk);
        }
        let Some(stream) = guard.stream.as_mut() else {
            return Err(MoraError::Io("TCP stream is closed".to_string()));
        };
        let mut buf = vec![0u8; max_bytes.min(65536)];
        let n = stream.read(&mut buf, ctx).await?;
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
        let mut guard = self.state.lock().await;
        loop {
            if let Some(pos) = guard.read_buf.iter().position(|&b| b == b'\n') {
                let end = (pos + 1).min(max_bytes);
                let line: Vec<u8> = guard.read_buf.drain(..end).collect();
                return Ok(line);
            }
            if guard.read_buf.len() >= max_bytes {
                let line: Vec<u8> = guard.read_buf.drain(..max_bytes).collect();
                return Ok(line);
            }
            let Some(stream) = guard.stream.as_mut() else {
                if !guard.read_buf.is_empty() {
                    return Ok(std::mem::take(&mut guard.read_buf));
                }
                return Err(MoraError::Io("TCP stream is closed".to_string()));
            };
            let mut scratch = [0u8; 4096];
            let n = stream.read(&mut scratch, ctx).await?;
            if n == 0 {
                return Ok(std::mem::take(&mut guard.read_buf));
            }
            guard.read_buf.extend_from_slice(&scratch[..n]);
        }
    }

    /// Write all bytes in `data`, returning total byte count written.
    pub async fn write_bytes(&self, data: &[u8], ctx: Option<&TaskContext>) -> MoraResult<usize> {
        if let Some(c) = ctx {
            c.status()?;
        }
        let mut guard = self.state.lock().await;
        let Some(stream) = guard.stream.as_mut() else {
            return Err(MoraError::Io("TCP stream is closed".to_string()));
        };
        stream.write_all(data, ctx).await?;
        Ok(data.len())
    }

    /// Half-close the write side of the TCP connection cleanly.
    pub async fn shutdown(&self) -> MoraResult<()> {
        let mut guard = self.state.lock().await;
        let Some(stream) = guard.stream.as_mut() else {
            return Ok(());
        };
        stream.shutdown().await
    }

    /// Close and drop the underlying TCP stream.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let mut guard = self.state.lock().await;
        if let Some(mut s) = guard.stream.take() {
            let _ = s.shutdown().await;
        }
        guard.read_buf.clear();
    }
}

/// Async I/O reactor pairing Tokio timers, sockets, and structured connection multiplexing
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
            tokio::select! {
                biased;
                err = c.cancelled() => Err(err),
                () = sleep(duration) => Ok(()),
            }
        } else {
            sleep(duration).await;
            Ok(())
        }
    }

    /// Execute a blocking I/O closure on the scheduler's Tokio blocking thread pool without
    /// starving async worker threads, interrupting cleanly if `ctx` is cancelled/timed out.
    pub async fn run_blocking_io<T, F>(&self, ctx: Option<&TaskContext>, op: F) -> MoraResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> MoraResult<T> + Send + 'static,
    {
        if let Some(c) = ctx {
            c.status()?;
        }
        let mut handle = self.scheduler.handle().spawn_blocking(op);
        let joined = if let Some(c) = ctx {
            tokio::select! {
                biased;
                err = c.cancelled() => return Err(err),
                res = &mut handle => res,
            }
        } else {
            handle.await
        };
        match joined {
            Ok(inner) => inner,
            Err(join_err) => Err(MoraError::Io(format!("async I/O task aborted: {join_err}"))),
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
        match timeout(duration, future).await {
            Ok(inner) => inner,
            Err(_) => Err(MoraError::Timeout(format!(
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
                Err(err) if err.is_cancellation_or_timeout() => {
                    group.abort_all();
                    return Err(err);
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
