//! Tokio-backed non-blocking async I/O reactor (`tokio::time` timers + `tokio::net` TCP listeners
//! and streams + `tokio::io` async read/write multiplexing with cooperative cancellation) (Wave 2 — Stage W2.4).

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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

    /// Half-close the write side of the TCP connection cleanly.
    pub async fn shutdown(&mut self) -> MoraResult<()> {
        Ok(self.inner.shutdown().await?)
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
