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
            let prev_in_len = net_in.len();
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
                    // If the engine consumed a TLS record from `net_in` (e.g. Schannel
                    // `SECBUFFER_EXTRA` across coalesced handshake records) and unparsed bytes
                    // remain in `net_in`, advance the state machine immediately before reading TCP.
                    if !net_in.is_empty() && net_in.len() < prev_in_len {
                        continue;
                    }
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

    /// Pump decrypted plaintext into `state.plain_buf` until its length exceeds `prev_plain_len`
    /// (or `state.peer_eof` is reached).
    ///
    /// Note: `engine.decrypt` MUST be polled even when `state.net_in` is empty because memory-BIO
    /// backends (OpenSSL/LibreSSL) drain coalesced post-handshake ciphertext (such as an immediate
    /// 1-RTT `ApplicationData` record arriving in the same TCP segment as `ClientFinished`) into
    /// their own internal BIO buffers during `handshake_step` / `decrypt`.
    async fn pump_decrypt(
        &self,
        prev_plain_len: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<()> {
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
                if st.plain_buf.len() > prev_plain_len || st.peer_eof {
                    return Ok(());
                }
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
                dec
            };

            if !outgoing_reply.is_empty() {
                self.tcp.write_bytes(&outgoing_reply, ctx).await?;
            }

            match status {
                DecryptStatus::Ok | DecryptStatus::PeerClosed => return Ok(()),
                DecryptStatus::NeedMoreData => {
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
            self.pump_decrypt(0, ctx).await?;
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
            let prev_plain_len = {
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
                st.plain_buf.len()
            };
            self.pump_decrypt(prev_plain_len, ctx).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::scheduler::TaskScheduler;
    use std::io::{Read, Write};
    use std::net::TcpStream as StdTcpStream;
    use std::sync::mpsc;

    fn load_test_tls_material() -> (String, String, String) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cert = std::fs::read_to_string(root.join("test_data/tls/test_server.pem"))
            .expect("read test_server.pem");
        let key = std::fs::read_to_string(root.join("test_data/tls/test_server_pkcs8.key"))
            .expect("read test_server_pkcs8.key");
        let ca = std::fs::read_to_string(root.join("test_data/tls/test_ca.pem"))
            .expect("read test_ca.pem");
        (cert, key, ca)
    }

    #[test]
    fn sans_io_coalesced_client_finished_appdata_and_close_notify() {
        let (cert_pem, key_pem, ca_pem) = load_test_tls_material();
        let srv_ctx = platform::build_server_context(&TlsServerConfig {
            cert_pem,
            key_pem,
            alpn_protocols: vec!["http/1.1".to_string()],
        })
        .expect("build_server_context");
        let mut srv = platform::create_server_session(srv_ctx).expect("create_server_session");
        let mut cli = platform::create_client_session(&TlsClientConfig {
            server_name: "localhost".to_string(),
            ca_pem: Some(ca_pem),
            alpn_protocols: vec!["http/1.1".to_string()],
            insecure_skip_verify: false,
        })
        .expect("create_client_session");

        let mut c2s = Vec::new();
        let mut s2c = Vec::new();
        let mut empty = Vec::new();

        // 1. ClientHello -> ServerHello..ServerFinished
        assert_eq!(
            cli.handshake_step(&mut empty, &mut c2s)
                .expect("client step 1"),
            HandshakeStatus::NeedMoreData
        );
        assert!(!c2s.is_empty());

        let srv_step1 = srv
            .handshake_step(&mut c2s, &mut s2c)
            .expect("server step 1");
        assert_eq!(srv_step1, HandshakeStatus::NeedMoreData);
        assert!(!s2c.is_empty());

        // Advance handshake until whichever side completes FIRST:
        // - TLS 1.3 (OpenSSL): `cli` completes first (1.5-RTT) and coalesces `[ClientFinished | AppData]` -> `srv`.
        // - TLS 1.2 (Schannel): `srv` completes first (2-RTT) and coalesces `[ServerFinished | AppData]` -> `cli`.
        let client_finished_first = loop {
            let cli_st = cli
                .handshake_step(&mut s2c, &mut c2s)
                .expect("client handshake step");
            if cli_st == HandshakeStatus::Complete {
                break true;
            }
            let srv_st = srv
                .handshake_step(&mut c2s, &mut s2c)
                .expect("server handshake step");
            if srv_st == HandshakeStatus::Complete {
                break false;
            }
        };

        if client_finished_first {
            // Coalesce `ClientFinished` + first `ApplicationData` record into ONE buffer `c2s`.
            cli.encrypt(b"hello-coalesced\n", &mut c2s)
                .expect("client 1-RTT encrypt");
            let srv_final = srv
                .handshake_step(&mut c2s, &mut s2c)
                .expect("server final handshake step");
            assert_eq!(srv_final, HandshakeStatus::Complete);

            // Without feeding any additional bytes into `c2s` (which OpenSSL has already drained to 0
            // bytes inside `self.rbio`), `srv.decrypt` MUST immediately yield `"hello-coalesced\n"`.
            let mut srv_plain = Vec::new();
            let mut srv_reply = Vec::new();
            let dec1 = srv
                .decrypt(&mut c2s, &mut srv_plain, &mut srv_reply)
                .expect("server decrypt coalesced appdata");
            assert_eq!(dec1, DecryptStatus::Ok);
            assert_eq!(srv_plain, b"hello-coalesced\n");
            s2c.extend_from_slice(&srv_reply);
        } else {
            // Coalesce `ServerFinished` + first `ApplicationData` record into ONE buffer `s2c`.
            srv.encrypt(b"hello-coalesced\n", &mut s2c)
                .expect("server 2-RTT encrypt");
            let cli_final = cli
                .handshake_step(&mut s2c, &mut c2s)
                .expect("client final handshake step");
            assert_eq!(cli_final, HandshakeStatus::Complete);

            let mut cli_early_plain = Vec::new();
            let mut cli_early_reply = Vec::new();
            let dec1 = cli
                .decrypt(&mut s2c, &mut cli_early_plain, &mut cli_early_reply)
                .expect("client decrypt coalesced appdata");
            assert_eq!(dec1, DecryptStatus::Ok);
            assert_eq!(cli_early_plain, b"hello-coalesced\n");
        }

        // Coalesce any post-handshake server frames (`NewSessionTicket`) + `ApplicationData`
        // + `close_notify` into ONE buffer `s2c` and verify client drains plaintext AND latches EOF.
        srv.encrypt(b"tail-without-newline", &mut s2c)
            .expect("server encrypt tail");
        srv.shutdown_alert(&mut s2c).expect("server shutdown_alert");

        let mut cli_plain = Vec::new();
        let mut cli_reply = Vec::new();
        let cli_dec = cli
            .decrypt(&mut s2c, &mut cli_plain, &mut cli_reply)
            .expect("client decrypt coalesced ticket + appdata + close_notify");
        assert_eq!(cli_plain, b"tail-without-newline");
        assert_eq!(cli_dec, DecryptStatus::PeerClosed);

        // Subsequent decrypt with empty input stays `PeerClosed`.
        let cli_dec_after = cli
            .decrypt(&mut empty, &mut cli_plain, &mut cli_reply)
            .expect("client decrypt after close_notify");
        assert_eq!(cli_dec_after, DecryptStatus::PeerClosed);
    }

    #[test]
    fn stream_coalesced_handshake_appdata_fragmented_readline_and_close_notify() {
        let (cert_pem, key_pem, ca_pem) = load_test_tls_material();
        let sched = TaskScheduler::shared();

        sched.block_on(async move {
            let ctx = TaskContext::root().with_timeout(Duration::from_secs(5));
            let listener = MoraTlsListener::bind(
                "127.0.0.1:0",
                &TlsServerConfig {
                    cert_pem,
                    key_pem,
                    alpn_protocols: vec!["http/1.1".to_string()],
                },
            )
            .await
            .expect("bind TLS listener");
            let addr = listener.local_addr().to_string();

            let (done_tx, done_rx) = mpsc::channel::<()>();
            let cli_thread = std::thread::spawn(move || {
                let mut raw = StdTcpStream::connect(&addr).expect("raw TCP connect");
                raw.set_nodelay(true).expect("set_nodelay");
                raw.set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set_read_timeout");

                let mut cli = platform::create_client_session(&TlsClientConfig {
                    server_name: "localhost".to_string(),
                    ca_pem: Some(ca_pem),
                    alpn_protocols: vec!["http/1.1".to_string()],
                    insecure_skip_verify: false,
                })
                .expect("create_client_session");

                let mut net_in = Vec::new();
                let mut net_out = Vec::new();
                let mut buf = [0u8; 16384];

                // Step 1: send ClientHello.
                assert_eq!(
                    cli.handshake_step(&mut net_in, &mut net_out)
                        .expect("cli step 1"),
                    HandshakeStatus::NeedMoreData
                );
                raw.write_all(&net_out).expect("write ClientHello");
                net_out.clear();

                // Drive until client handshake finishes, holding back any final `ClientFinished` in `net_out`.
                let mut hs_done = false;
                loop {
                    while !net_in.is_empty() {
                        let before = net_in.len();
                        let st = cli
                            .handshake_step(&mut net_in, &mut net_out)
                            .expect("cli handshake step");
                        if st == HandshakeStatus::Complete {
                            hs_done = true;
                            break;
                        }
                        if !net_out.is_empty() {
                            raw.write_all(&net_out).expect("write handshake step");
                            net_out.clear();
                        }
                        if net_in.len() == before {
                            break;
                        }
                    }
                    if hs_done {
                        break;
                    }
                    let n = raw.read(&mut buf).expect("read server handshake");
                    assert!(n > 0, "unexpected EOF during handshake");
                    net_in.extend_from_slice(&buf[..n]);
                }

                // Deterministically coalesce `[ClientFinished | AppData]` into ONE single `write_all`
                // and pause slightly before sending so server's `accept` reads all bytes in one recv.
                cli.encrypt(b"coalesced-1rtt-line\n", &mut net_out)
                    .expect("encrypt 1-RTT line");
                std::thread::sleep(Duration::from_millis(15));
                raw.write_all(&net_out)
                    .expect("write coalesced ClientFinished + AppData");
                net_out.clear();

                // Read server echo to confirm handshake + first line succeeded without deadlock.
                let mut echo_plain = Vec::new();
                let mut reply = Vec::new();
                while echo_plain != b"ACK:coalesced-1rtt-line\n" {
                    let st = cli
                        .decrypt(&mut net_in, &mut echo_plain, &mut reply)
                        .expect("cli decrypt echo");
                    if !reply.is_empty() {
                        raw.write_all(&reply).expect("write reply");
                        reply.clear();
                    }
                    if st == DecryptStatus::NeedMoreData {
                        let n = raw.read(&mut buf).expect("read echo ciphertext");
                        assert!(n > 0, "unexpected EOF waiting for echo");
                        net_in.extend_from_slice(&buf[..n]);
                    }
                }

                // Now send a single logical line fragmented across two distinct TLS records & packets.
                cli.encrypt(b"frag-alpha-", &mut net_out)
                    .expect("encrypt frag 1");
                raw.write_all(&net_out).expect("write frag 1");
                net_out.clear();
                std::thread::sleep(Duration::from_millis(20));

                cli.encrypt(b"frag-omega\n", &mut net_out)
                    .expect("encrypt frag 2");
                raw.write_all(&net_out).expect("write frag 2");
                net_out.clear();
                std::thread::sleep(Duration::from_millis(15));

                // Finally coalesce an unterminated tail + `close_notify` WITHOUT closing raw TCP yet!
                cli.encrypt(b"final-unterminated", &mut net_out)
                    .expect("encrypt tail");
                cli.shutdown_alert(&mut net_out).expect("emit close_notify");
                raw.write_all(&net_out).expect("write tail + close_notify");

                // Hold TCP socket open until server confirms observing both tail and TLS EOF.
                let _ = done_rx.recv_timeout(Duration::from_secs(5));
            });

            let srv_stream = listener
                .accept(Some(&ctx))
                .await
                .expect("server TLS accept");
            let line1 = srv_stream
                .read_line_bytes(4096, Some(&ctx))
                .await
                .expect("read coalesced line 1");
            assert_eq!(line1, b"coalesced-1rtt-line\n");

            srv_stream
                .write_bytes(b"ACK:coalesced-1rtt-line\n", Some(&ctx))
                .await
                .expect("write ACK");

            let line2 = srv_stream
                .read_line_bytes(4096, Some(&ctx))
                .await
                .expect("read fragmented line 2");
            assert_eq!(line2, b"frag-alpha-frag-omega\n");

            let tail = srv_stream
                .read_line_bytes(4096, Some(&ctx))
                .await
                .expect("read unterminated tail before close_notify");
            assert_eq!(tail, b"final-unterminated");

            let eof = srv_stream
                .read_line_bytes(4096, Some(&ctx))
                .await
                .expect("read EOF after close_notify while TCP still open");
            assert!(eof.is_empty());

            let _ = done_tx.send(());
            srv_stream.close().await;
            listener.close();
            cli_thread.join().expect("join client thread");
        });
    }
}
