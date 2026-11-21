//! Zero-dependency native async TLS 1.2/1.3 transport layer (`MoraTlsListener` & `MoraTlsStream`).
//!
//! Architecture:
//! - Pure sans-IO [`TlsEngine`] abstraction implemented by native OS TLS stacks via dynamic FFI:
//!   - Windows (`#[cfg(windows)]`): Win32 Schannel (`secur32.dll`) + CryptoAPI (`crypt32.dll`) + CNG (`ncrypt.dll`)
//!   - Linux & macOS (`#[cfg(not(windows))]`): OpenSSL / LibreSSL (`libssl` + `libcrypto`) Memory-BIO
//! - Built directly atop Mora's non-blocking [`MoraTcpListener`] / [`MoraTcpStream`], so every
//!   TLS handshake, read, line read, write, and close honors [`TaskContext`] cooperative cancellation,
//!   timeouts, and lock-free `.close()` wakeups without ever holding a mutex across `.await`.

pub(crate) mod pem;

#[cfg(windows)]
mod schannel;
#[cfg(windows)]
use schannel as platform;

#[cfg(not(windows))]
mod openssl;
#[cfg(not(windows))]
use openssl as platform;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::common::sync::Mutex;
use crate::error::{MoraError, MoraResult};
use crate::runtime::reactor::{MoraTcpListener, MoraTcpStream};
use crate::runtime::scheduler::{Select2, TaskContext, race2, sleep};

/// Status returned by one sans-IO TLS handshake step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandshakeStatus {
    Complete,
    NeedMoreData,
}

/// Status returned by one sans-IO TLS record decryption step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecryptStatus {
    Ok,
    NeedMoreData,
    PeerClosed,
}

/// Unified sans-IO contract implemented by both Windows Schannel and Unix OpenSSL/LibreSSL.
pub(crate) trait TlsEngine: Send {
    fn handshake_step(
        &mut self,
        incoming: &mut Vec<u8>,
        outgoing: &mut Vec<u8>,
    ) -> MoraResult<HandshakeStatus>;
    fn encrypt(&mut self, plaintext: &[u8], outgoing: &mut Vec<u8>) -> MoraResult<()>;
    fn decrypt(
        &mut self,
        incoming: &mut Vec<u8>,
        plaintext_out: &mut Vec<u8>,
        outgoing_response: &mut Vec<u8>,
    ) -> MoraResult<DecryptStatus>;
    fn shutdown_alert(&mut self, outgoing: &mut Vec<u8>) -> MoraResult<()>;
    fn tls_version(&self) -> String;
    fn cipher_suite(&self) -> String;
    fn alpn_protocol(&self) -> Option<String>;
}

/// Configuration for a TLS server listener (`tls_listen`).
#[derive(Debug, Clone)]
pub(crate) struct TlsServerConfig {
    pub cert_pem: String,
    pub key_pem: String,
    pub alpn_protocols: Vec<String>,
}

/// Configuration for a TLS client connection (`tls_connect`).
#[derive(Debug, Clone)]
pub(crate) struct TlsClientConfig {
    pub server_name: String,
    pub ca_pem: Option<String>,
    pub alpn_protocols: Vec<String>,
    pub insecure_skip_verify: bool,
}

/// First-class Mora async TLS listener (`tls_listen(addr, config)`).
pub struct MoraTlsListener {
    tcp: Arc<MoraTcpListener>,
    server_ctx: Arc<platform::PlatformServerContext>,
}

impl std::fmt::Debug for MoraTlsListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoraTlsListener")
            .field("local_addr", &self.tcp.local_addr())
            .field("closed", &self.tcp.is_closed())
            .finish()
    }
}

impl MoraTlsListener {
    /// Validate certificates/key, compile native server TLS credentials, and bind TCP listener.
    pub(crate) async fn bind(addr: &str, cfg: &TlsServerConfig) -> MoraResult<Arc<Self>> {
        let server_ctx = platform::build_server_context(cfg)?;
        let tcp = MoraTcpListener::bind(addr).await?;
        Ok(Arc::new(Self { tcp, server_ctx }))
    }

    pub(crate) fn local_addr(&self) -> &str {
        self.tcp.local_addr()
    }

    pub(crate) fn close(&self) {
        self.tcp.close();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.tcp.is_closed()
    }

    /// Accept the next incoming TCP connection and perform the non-blocking server TLS handshake.
    pub(crate) async fn accept(&self, ctx: Option<&TaskContext>) -> MoraResult<Arc<MoraTlsStream>> {
        let tcp_stream = self.tcp.accept(ctx).await?;
        let session = platform::create_server_session(Arc::clone(&self.server_ctx))?;
        MoraTlsStream::from_handshake(tcp_stream, session, ctx).await
    }
}

struct TlsStreamState {
    engine: platform::PlatformTlsSession,
    net_in: Vec<u8>,
    plain_buf: Vec<u8>,
    peer_eof: bool,
}

/// First-class Mora async TLS 1.2/1.3 stream (`tls_connect` / `tls_listener.accept()`).
pub struct MoraTlsStream {
    tcp: Arc<MoraTcpStream>,
    state: Mutex<TlsStreamState>,
    closed: AtomicBool,
    tls_version: String,
    cipher_suite: String,
    alpn_protocol: Option<String>,
}

impl std::fmt::Debug for MoraTlsStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoraTlsStream")
            .field("local_addr", &self.tcp.local_addr())
            .field("peer_addr", &self.tcp.peer_addr())
            .field("tls_version", &self.tls_version)
            .field("cipher_suite", &self.cipher_suite)
            .field("alpn", &self.alpn_protocol)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl MoraTlsStream {
    /// Connect asynchronously over TCP and complete a verified TLS 1.2/1.3 client handshake.
    pub(crate) async fn connect(
        addr: &str,
        cfg: &TlsClientConfig,
        connect_timeout: Option<Duration>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        let session = platform::create_client_session(cfg)?;
        let tcp = MoraTcpStream::connect(addr, connect_timeout, ctx).await?;
        let handshake_fut = Self::from_handshake(tcp, session, ctx);
        match connect_timeout {
            Some(dur) => match race2(handshake_fut, sleep(dur)).await {
                Select2::First(res) => res,
                Select2::Second(()) => Err(MoraError::Timeout(format!(
                    "TLS handshake to {addr} timed out after {dur:?}"
                ))),
            },
            None => handshake_fut.await,
        }
    }

    async fn from_handshake(
        tcp: Arc<MoraTcpStream>,
        mut engine: platform::PlatformTlsSession,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        let mut net_in: Vec<u8> = Vec::new();
        let mut net_out: Vec<u8> = Vec::new();

        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            let step = engine.handshake_step(&mut net_in, &mut net_out);
            if !net_out.is_empty() {
                let to_send = std::mem::take(&mut net_out);
                // Always flush any generated alert/handshake bytes even if `step` returned Err.
                let write_res = tcp.write_bytes(&to_send, ctx).await;
                if step.is_ok() {
                    write_res?;
                }
            }
            match step? {
                HandshakeStatus::Complete => break,
                HandshakeStatus::NeedMoreData => {
                    let chunk = tcp.read_bytes(16384, ctx).await?;
                    if chunk.is_empty() {
                        return Err(MoraError::Io(
                            "TLS handshake aborted: peer closed connection unexpectedly"
                                .to_string(),
                        ));
                    }
                    net_in.extend_from_slice(&chunk);
                }
            }
        }

        let tls_version = engine.tls_version();
        let cipher_suite = engine.cipher_suite();
        let alpn_protocol = engine.alpn_protocol();

        Ok(Arc::new(Self {
            tcp,
            state: Mutex::new(TlsStreamState {
                engine,
                net_in,
                plain_buf: Vec::new(),
                peer_eof: false,
            }),
            closed: AtomicBool::new(false),
            tls_version,
            cipher_suite,
            alpn_protocol,
        }))
    }

    pub(crate) fn local_addr(&self) -> &str {
        self.tcp.local_addr()
    }

    pub(crate) fn peer_addr(&self) -> &str {
        self.tcp.peer_addr()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.tcp.is_closed()
    }

    pub(crate) fn tls_version(&self) -> &str {
        &self.tls_version
    }

    pub(crate) fn cipher_suite(&self) -> &str {
        &self.cipher_suite
    }

    pub(crate) fn alpn_protocol(&self) -> Option<&str> {
        self.alpn_protocol.as_deref()
    }

    /// Pump at least one decrypted plaintext chunk into `state.plain_buf` (or set `peer_eof`).
    async fn pump_decrypt(&self, ctx: Option<&TaskContext>) -> MoraResult<()> {
        loop {
            if self.is_closed() {
                return Err(MoraError::Io("TLS stream is closed".to_string()));
            }
            if let Some(c) = ctx {
                c.status()?;
            }

            let mut outgoing_reply = Vec::new();
            let status = {
                let mut st = self.state.lock();
                if !st.plain_buf.is_empty() || st.peer_eof {
                    return Ok(());
                }
                if !st.net_in.is_empty() {
                    let TlsStreamState {
                        engine,
                        net_in,
                        plain_buf,
                        peer_eof,
                    } = &mut *st;
                    let dec = engine.decrypt(net_in, plain_buf, &mut outgoing_reply)?;
                    if dec == DecryptStatus::PeerClosed {
                        *peer_eof = true;
                    }
                    Some(dec)
                } else {
                    None
                }
            };

            if !outgoing_reply.is_empty() {
                let _ = self.tcp.write_bytes(&outgoing_reply, ctx).await;
            }

            match status {
                Some(DecryptStatus::Ok | DecryptStatus::PeerClosed) => return Ok(()),
                Some(DecryptStatus::NeedMoreData) | None => {
                    let chunk = self.tcp.read_bytes(16384, ctx).await?;
                    if chunk.is_empty() {
                        let mut st = self.state.lock();
                        st.peer_eof = true;
                        return Ok(());
                    }
                    let mut st = self.state.lock();
                    st.net_in.extend_from_slice(&chunk);
                }
            }
        }
    }

    /// Read up to `max_bytes` decrypted bytes (empty on EOF).
    pub(crate) async fn read_bytes(
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
                let mut st = self.state.lock();
                if !st.plain_buf.is_empty() {
                    let take = max_bytes.min(st.plain_buf.len());
                    return Ok(st.plain_buf.drain(..take).collect());
                }
                if st.peer_eof {
                    return Ok(Vec::new());
                }
            }
            self.pump_decrypt(ctx).await?;
        }
    }

    /// Read decrypted bytes up to and including the next `\n` (or EOF / `max_bytes` limit).
    pub(crate) async fn read_line_bytes(
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
                let mut st = self.state.lock();
                if let Some(pos) = st.plain_buf.iter().position(|&b| b == b'\n') {
                    let end = (pos + 1).min(max_bytes);
                    return Ok(st.plain_buf.drain(..end).collect());
                }
                if st.plain_buf.len() >= max_bytes {
                    return Ok(st.plain_buf.drain(..max_bytes).collect());
                }
                if st.peer_eof {
                    return Ok(std::mem::take(&mut st.plain_buf));
                }
            }
            self.pump_decrypt(ctx).await?;
        }
    }

    /// Encrypt and write all plaintext bytes in `data`, returning total plaintext byte count.
    pub(crate) async fn write_bytes(
        &self,
        data: &[u8],
        ctx: Option<&TaskContext>,
    ) -> MoraResult<usize> {
        if self.is_closed() {
            return Err(MoraError::Io("TLS stream is closed".to_string()));
        }
        if let Some(c) = ctx {
            c.status()?;
        }
        if data.is_empty() {
            return Ok(0);
        }
        let mut encrypted = Vec::with_capacity(data.len() + 64);
        {
            let mut st = self.state.lock();
            st.engine.encrypt(data, &mut encrypted)?;
        }
        self.tcp.write_bytes(&encrypted, ctx).await?;
        Ok(data.len())
    }

    /// Emit a TLS `close_notify` alert and half-close the write side of the TCP socket.
    pub(crate) async fn shutdown(&self) -> MoraResult<()> {
        let mut alert = Vec::new();
        {
            let mut st = self.state.lock();
            let _ = st.engine.shutdown_alert(&mut alert);
        }
        if !alert.is_empty() {
            let _ = self.tcp.write_bytes(&alert, None).await;
        }
        self.tcp.shutdown().await
    }

    /// Send a clean TLS `close_notify` alert (best effort) and close the underlying TCP socket.
    pub(crate) async fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let mut alert = Vec::new();
            {
                let mut st = self.state.lock();
                let _ = st.engine.shutdown_alert(&mut alert);
            }
            if !alert.is_empty() {
                let _ = self.tcp.write_bytes(&alert, None).await;
            }
            self.tcp.close().await;
        }
    }
}
