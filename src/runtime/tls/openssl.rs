//! Native Unix (Linux & macOS) TLS 1.2/1.3 engine built over dynamic OpenSSL / LibreSSL
//! (`libssl` + `libcrypto`) via [`crate::common::dynlib::DynLib`] and Memory-BIO (`BIO_s_mem()`).
//!
//! Zero external Cargo crates. Implements the sans-IO [`TlsEngine`] contract so all socket I/O
//! remains fully non-blocking and cancellable through Mora's `AsyncTcpStream` / `TaskContext`.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};
use std::net::IpAddr;
use std::ptr;
use std::sync::{Arc, OnceLock};

use crate::common::dynlib::DynLib;
use crate::error::{MoraError, MoraResult};

use super::pem::{parse_certificates_der, parse_private_key_pkcs8_der};
use super::{DecryptStatus, HandshakeStatus, TlsClientConfig, TlsEngine, TlsServerConfig};

// ---- Verified OpenSSL / LibreSSL C ABI constants ----

const SSL_ERROR_WANT_READ: c_int = 2;
const SSL_ERROR_WANT_WRITE: c_int = 3;
const SSL_ERROR_ZERO_RETURN: c_int = 6;

const SSL_VERIFY_NONE: c_int = 0;
const SSL_VERIFY_PEER: c_int = 1;

const SSL_CTRL_EXTRA_CHAIN_CERT: c_int = 14;
const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLSEXT_NAMETYPE_HOST_NAME: c_long = 0;
const TLS1_2_VERSION: c_long = 0x0303;

const BIO_CTRL_PENDING: c_int = 10;

const SSL_TLSEXT_ERR_OK: c_int = 0;
const SSL_TLSEXT_ERR_NOACK: c_int = 3;

// ---- Dynamic C ABI function pointers ----

type TlsMethodFn = unsafe extern "C" fn() -> *const c_void;
type SslCtxNewFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type SslCtxFreeFn = unsafe extern "C" fn(*mut c_void);
type SslCtxCtrlFn = unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long;
type SslCtxSetDefaultVerifyPathsFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SslCtxGetCertStoreFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type SslCtxUseCertificateFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type SslCtxUsePrivateKeyFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type SslCtxCheckPrivateKeyFn = unsafe extern "C" fn(*const c_void) -> c_int;
type AlpnSelectCb = unsafe extern "C" fn(
    *mut c_void,
    *mut *const c_uchar,
    *mut c_uchar,
    *const c_uchar,
    c_uint,
    *mut c_void,
) -> c_int;
type SslCtxSetAlpnSelectCbFn = unsafe extern "C" fn(*mut c_void, Option<AlpnSelectCb>, *mut c_void);

type SslNewFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type SslFreeFn = unsafe extern "C" fn(*mut c_void);
type SslSetBioFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type SslSetConnectStateFn = unsafe extern "C" fn(*mut c_void);
type SslSetAcceptStateFn = unsafe extern "C" fn(*mut c_void);
type SslSetVerifyFn = unsafe extern "C" fn(*mut c_void, c_int, *const c_void);
type SslCtrlFn = unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long;
type SslGet0ParamFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type SslDoHandshakeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SslReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int;
type SslWriteFn = unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> c_int;
type SslShutdownFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SslGetErrorFn = unsafe extern "C" fn(*const c_void, c_int) -> c_int;
type SslGetVersionFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
type SslGetCurrentCipherFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
type SslCipherGetNameFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
type SslSetAlpnProtosFn = unsafe extern "C" fn(*mut c_void, *const c_uchar, c_uint) -> c_int;
type SslGet0AlpnSelectedFn = unsafe extern "C" fn(*const c_void, *mut *const c_uchar, *mut c_uint);
type SslGetVerifyResultFn = unsafe extern "C" fn(*const c_void) -> c_long;

type BioSMemFn = unsafe extern "C" fn() -> *const c_void;
type BioNewFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type BioNewMemBufFn = unsafe extern "C" fn(*const c_void, c_int) -> *mut c_void;
type BioFreeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type BioReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int;
type BioWriteFn = unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> c_int;
type BioCtrlFn = unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long;

type PemReadBioX509Fn =
    unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *const c_void, *mut c_void) -> *mut c_void;
type PemReadBioPrivateKeyFn =
    unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *const c_void, *mut c_void) -> *mut c_void;
type X509FreeFn = unsafe extern "C" fn(*mut c_void);
type EvpPkeyFreeFn = unsafe extern "C" fn(*mut c_void);
type X509StoreAddCertFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type X509VerifyParamSet1HostFn = unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> c_int;
type X509VerifyParamSet1IpAscFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type ErrGetErrorFn = unsafe extern "C" fn() -> c_ulong;
type ErrErrorStringNFn = unsafe extern "C" fn(c_ulong, *mut c_char, usize);

struct OpenSslApi {
    _libssl: DynLib,
    _libcrypto: DynLib,
    tls_client_method: TlsMethodFn,
    tls_server_method: TlsMethodFn,
    ssl_ctx_new: SslCtxNewFn,
    ssl_ctx_free: SslCtxFreeFn,
    ssl_ctx_ctrl: SslCtxCtrlFn,
    ssl_ctx_set_default_verify_paths: SslCtxSetDefaultVerifyPathsFn,
    ssl_ctx_get_cert_store: SslCtxGetCertStoreFn,
    ssl_ctx_use_certificate: SslCtxUseCertificateFn,
    ssl_ctx_use_private_key: SslCtxUsePrivateKeyFn,
    ssl_ctx_check_private_key: SslCtxCheckPrivateKeyFn,
    ssl_ctx_set_alpn_select_cb: Option<SslCtxSetAlpnSelectCbFn>,
    ssl_new: SslNewFn,
    ssl_free: SslFreeFn,
    ssl_set_bio: SslSetBioFn,
    ssl_set_connect_state: SslSetConnectStateFn,
    ssl_set_accept_state: SslSetAcceptStateFn,
    ssl_set_verify: SslSetVerifyFn,
    ssl_ctrl: SslCtrlFn,
    ssl_get0_param: SslGet0ParamFn,
    ssl_do_handshake: SslDoHandshakeFn,
    ssl_read: SslReadFn,
    ssl_write: SslWriteFn,
    ssl_shutdown: SslShutdownFn,
    ssl_get_error: SslGetErrorFn,
    ssl_get_version: SslGetVersionFn,
    ssl_get_current_cipher: SslGetCurrentCipherFn,
    ssl_cipher_get_name: SslCipherGetNameFn,
    ssl_set_alpn_protos: Option<SslSetAlpnProtosFn>,
    ssl_get0_alpn_selected: Option<SslGet0AlpnSelectedFn>,
    ssl_get_verify_result: SslGetVerifyResultFn,
    bio_s_mem: BioSMemFn,
    bio_new: BioNewFn,
    bio_new_mem_buf: BioNewMemBufFn,
    bio_free: BioFreeFn,
    bio_read: BioReadFn,
    bio_write: BioWriteFn,
    bio_ctrl: BioCtrlFn,
    pem_read_bio_x509: PemReadBioX509Fn,
    pem_read_bio_private_key: PemReadBioPrivateKeyFn,
    x509_free: X509FreeFn,
    evp_pkey_free: EvpPkeyFreeFn,
    x509_store_add_cert: X509StoreAddCertFn,
    x509_verify_param_set1_host: X509VerifyParamSet1HostFn,
    x509_verify_param_set1_ip_asc: X509VerifyParamSet1IpAscFn,
    err_get_error: ErrGetErrorFn,
    err_error_string_n: ErrErrorStringNFn,
}

static OPENSSL_API: OnceLock<MoraResult<OpenSslApi>> = OnceLock::new();

fn api() -> MoraResult<&'static OpenSslApi> {
    let res = OPENSSL_API.get_or_init(load_openssl_api);
    match res {
        Ok(a) => Ok(a),
        Err(err) => Err(err.clone()),
    }
}

fn load_openssl_api() -> MoraResult<OpenSslApi> {
    const SSL_CANDIDATES: &[&str] = &[
        "libssl.so.3",
        "libssl.so.1.1",
        "libssl.so",
        "/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib",
        "/usr/local/opt/openssl@3/lib/libssl.3.dylib",
        "libssl.3.dylib",
        "libssl.48.dylib",
        "libssl.dylib",
    ];
    const CRYPTO_CANDIDATES: &[&str] = &[
        "libcrypto.so.3",
        "libcrypto.so.1.1",
        "libcrypto.so",
        "/opt/homebrew/opt/openssl@3/lib/libcrypto.3.dylib",
        "/usr/local/opt/openssl@3/lib/libcrypto.3.dylib",
        "libcrypto.3.dylib",
        "libcrypto.46.dylib",
        "libcrypto.dylib",
    ];

    let libssl = DynLib::open_any(SSL_CANDIDATES).ok_or_else(|| {
        MoraError::Io(
            "TLS (Unix): failed to dynamically load OpenSSL/LibreSSL (libssl.so.3 / libssl.so.1.1 / libssl.3.dylib)"
                .to_string(),
        )
    })?;
    let libcrypto = DynLib::open_any(CRYPTO_CANDIDATES).ok_or_else(|| {
        MoraError::Io(
            "TLS (Unix): failed to dynamically load OpenSSL/LibreSSL (libcrypto.so.3 / libcrypto.so.1.1 / libcrypto.3.dylib)"
                .to_string(),
        )
    })?;

    macro_rules! sym {
        ($lib:expr, $name:literal) => {{
            let resolved = {
                // SAFETY: Every symbol name matches the exact OpenSSL 1.1/3.x C ABI signature declared above.
                unsafe { $lib.get_symbol($name) }
            };
            resolved.ok_or_else(|| {
                MoraError::Io(format!(
                    "TLS (OpenSSL): missing C symbol {:?}",
                    std::str::from_utf8($name).unwrap_or("?")
                ))
            })?
        }};
    }

    macro_rules! opt_sym {
        ($lib:expr, $name:literal) => {{
            // SAFETY: Optional ALPN symbol matches OpenSSL 1.1/3.x C ABI signature.
            unsafe { $lib.get_symbol($name) }
        }};
    }

    Ok(OpenSslApi {
        tls_client_method: sym!(libssl, b"TLS_client_method\0"),
        tls_server_method: sym!(libssl, b"TLS_server_method\0"),
        ssl_ctx_new: sym!(libssl, b"SSL_CTX_new\0"),
        ssl_ctx_free: sym!(libssl, b"SSL_CTX_free\0"),
        ssl_ctx_ctrl: sym!(libssl, b"SSL_CTX_ctrl\0"),
        ssl_ctx_set_default_verify_paths: sym!(libssl, b"SSL_CTX_set_default_verify_paths\0"),
        ssl_ctx_get_cert_store: sym!(libssl, b"SSL_CTX_get_cert_store\0"),
        ssl_ctx_use_certificate: sym!(libssl, b"SSL_CTX_use_certificate\0"),
        ssl_ctx_use_private_key: sym!(libssl, b"SSL_CTX_use_PrivateKey\0"),
        ssl_ctx_check_private_key: sym!(libssl, b"SSL_CTX_check_private_key\0"),
        ssl_ctx_set_alpn_select_cb: opt_sym!(libssl, b"SSL_CTX_set_alpn_select_cb\0"),
        ssl_new: sym!(libssl, b"SSL_new\0"),
        ssl_free: sym!(libssl, b"SSL_free\0"),
        ssl_set_bio: sym!(libssl, b"SSL_set_bio\0"),
        ssl_set_connect_state: sym!(libssl, b"SSL_set_connect_state\0"),
        ssl_set_accept_state: sym!(libssl, b"SSL_set_accept_state\0"),
        ssl_set_verify: sym!(libssl, b"SSL_set_verify\0"),
        ssl_ctrl: sym!(libssl, b"SSL_ctrl\0"),
        ssl_get0_param: sym!(libssl, b"SSL_get0_param\0"),
        ssl_do_handshake: sym!(libssl, b"SSL_do_handshake\0"),
        ssl_read: sym!(libssl, b"SSL_read\0"),
        ssl_write: sym!(libssl, b"SSL_write\0"),
        ssl_shutdown: sym!(libssl, b"SSL_shutdown\0"),
        ssl_get_error: sym!(libssl, b"SSL_get_error\0"),
        ssl_get_version: sym!(libssl, b"SSL_get_version\0"),
        ssl_get_current_cipher: sym!(libssl, b"SSL_get_current_cipher\0"),
        ssl_cipher_get_name: sym!(libssl, b"SSL_CIPHER_get_name\0"),
        ssl_set_alpn_protos: opt_sym!(libssl, b"SSL_set_alpn_protos\0"),
        ssl_get0_alpn_selected: opt_sym!(libssl, b"SSL_get0_alpn_selected\0"),
        ssl_get_verify_result: sym!(libssl, b"SSL_get_verify_result\0"),
        bio_s_mem: sym!(libcrypto, b"BIO_s_mem\0"),
        bio_new: sym!(libcrypto, b"BIO_new\0"),
        bio_new_mem_buf: sym!(libcrypto, b"BIO_new_mem_buf\0"),
        bio_free: sym!(libcrypto, b"BIO_free\0"),
        bio_read: sym!(libcrypto, b"BIO_read\0"),
        bio_write: sym!(libcrypto, b"BIO_write\0"),
        bio_ctrl: sym!(libcrypto, b"BIO_ctrl\0"),
        pem_read_bio_x509: sym!(libcrypto, b"PEM_read_bio_X509\0"),
        pem_read_bio_private_key: sym!(libcrypto, b"PEM_read_bio_PrivateKey\0"),
        x509_free: sym!(libcrypto, b"X509_free\0"),
        evp_pkey_free: sym!(libcrypto, b"EVP_PKEY_free\0"),
        x509_store_add_cert: sym!(libcrypto, b"X509_STORE_add_cert\0"),
        x509_verify_param_set1_host: sym!(libcrypto, b"X509_VERIFY_PARAM_set1_host\0"),
        x509_verify_param_set1_ip_asc: sym!(libcrypto, b"X509_VERIFY_PARAM_set1_ip_asc\0"),
        err_get_error: sym!(libcrypto, b"ERR_get_error\0"),
        err_error_string_n: sym!(libcrypto, b"ERR_error_string_n\0"),
        _libssl: libssl,
        _libcrypto: libcrypto,
    })
}

fn drain_openssl_errors(a: &OpenSslApi) -> String {
    let mut parts = Vec::new();
    loop {
        // SAFETY: `ERR_get_error` reads and pops the thread-local OpenSSL error queue.
        let code = unsafe { (a.err_get_error)() };
        if code == 0 {
            break;
        }
        let mut buf = [0 as c_char; 256];
        // SAFETY: `buf` is a 256-byte writable buffer for `ERR_error_string_n`.
        unsafe {
            (a.err_error_string_n)(code, buf.as_mut_ptr(), buf.len());
        }
        // SAFETY: `ERR_error_string_n` always NUL-terminates `buf`.
        let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        parts.push(msg);
    }
    if parts.is_empty() {
        "unknown OpenSSL error".to_string()
    } else {
        parts.join("; ")
    }
}

fn encode_alpn_wire(protocols: &[String]) -> Vec<u8> {
    let mut wire = Vec::new();
    for p in protocols {
        let bytes = p.as_bytes();
        if bytes.is_empty() || bytes.len() > 255 {
            continue;
        }
        wire.push(bytes.len() as u8);
        wire.extend_from_slice(bytes);
    }
    wire
}

// SAFETY: Invoked by OpenSSL during server ALPN negotiation with valid client wire buffer `in_ptr[..in_len]`
// and `arg` pointing to our pinned `Vec<u8>` server ALPN preference list owned by `PlatformServerContext`.
unsafe extern "C" fn server_alpn_select_cb(
    _ssl: *mut c_void,
    out: *mut *const c_uchar,
    out_len: *mut c_uchar,
    in_ptr: *const c_uchar,
    in_len: c_uint,
    arg: *mut c_void,
) -> c_int {
    if arg.is_null() || in_ptr.is_null() || out.is_null() || out_len.is_null() {
        return SSL_TLSEXT_ERR_NOACK;
    }
    // SAFETY: `arg` points to the server's `Box<Vec<u8>>` ALPN wire list alive for the entire `SSL_CTX` lifetime.
    let server_wire = unsafe { &*(arg as *const Vec<u8>) };
    // SAFETY: OpenSSL guarantees `in_ptr` is valid for `in_len` bytes during the callback.
    let client_wire = unsafe { std::slice::from_raw_parts(in_ptr, in_len as usize) };

    let mut s_pos = 0usize;
    while s_pos < server_wire.len() {
        let s_len = server_wire[s_pos] as usize;
        s_pos += 1;
        if s_pos + s_len > server_wire.len() {
            break;
        }
        let s_proto = &server_wire[s_pos..s_pos + s_len];
        s_pos += s_len;

        let mut c_pos = 0usize;
        while c_pos < client_wire.len() {
            let c_len = client_wire[c_pos] as usize;
            c_pos += 1;
            if c_pos + c_len > client_wire.len() {
                break;
            }
            if &client_wire[c_pos..c_pos + c_len] == s_proto {
                // SAFETY: `out` and `out_len` are valid non-null out-pointers provided by OpenSSL.
                unsafe {
                    *out = in_ptr.add(c_pos);
                    *out_len = c_len as u8;
                }
                return SSL_TLSEXT_ERR_OK;
            }
            c_pos += c_len;
        }
    }
    SSL_TLSEXT_ERR_NOACK
}

pub(crate) struct PlatformServerContext {
    ssl_ctx: *mut c_void,
    #[allow(clippy::box_collection)]
    _alpn_wire: Option<Box<Vec<u8>>>,
}

// SAFETY: An initialized OpenSSL `SSL_CTX` is thread-safe for concurrent `SSL_new(ctx)` calls
// (OpenSSL 1.1+/3.x internal reference counting & locking).
unsafe impl Send for PlatformServerContext {}
// SAFETY: Shared read-only `SSL_CTX` used to spawn per-connection `SSL` objects across threads.
unsafe impl Sync for PlatformServerContext {}

impl Drop for PlatformServerContext {
    fn drop(&mut self) {
        if !self.ssl_ctx.is_null()
            && let Ok(a) = api()
        {
            // SAFETY: `self.ssl_ctx` was created by `SSL_CTX_new` and is freed once on Drop.
            unsafe {
                (a.ssl_ctx_free)(self.ssl_ctx);
            }
        }
    }
}

pub(crate) fn build_server_context(
    cfg: &TlsServerConfig,
) -> MoraResult<Arc<PlatformServerContext>> {
    // Upfront strict PEM validation identical to Windows Schannel.
    let _ = parse_certificates_der(&cfg.cert_pem)?;
    let _ = parse_private_key_pkcs8_der(&cfg.key_pem)?;

    let a = api()?;
    let method = {
        // SAFETY: `TLS_server_method` returns a static `const SSL_METHOD*`.
        unsafe { (a.tls_server_method)() }
    };
    let ctx = {
        // SAFETY: `method` is valid from `TLS_server_method`.
        unsafe { (a.ssl_ctx_new)(method) }
    };
    if ctx.is_null() {
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_CTX_new(server) failed: {}",
            drain_openssl_errors(a)
        )));
    }

    // Enforce minimum TLS 1.2
    // SAFETY: `ctx` is a valid `SSL_CTX*`; `SSL_CTRL_SET_MIN_PROTO_VERSION = 123`.
    let _ = unsafe {
        (a.ssl_ctx_ctrl)(
            ctx,
            SSL_CTRL_SET_MIN_PROTO_VERSION,
            TLS1_2_VERSION,
            ptr::null_mut(),
        )
    };

    // Load certificate chain from memory BIO
    let cert_bio = {
        // SAFETY: `cfg.cert_pem` bytes remain live for the duration of `PEM_read_bio_X509` calls.
        unsafe {
            (a.bio_new_mem_buf)(
                cfg.cert_pem.as_ptr() as *const c_void,
                cfg.cert_pem.len() as c_int,
            )
        }
    };
    if cert_bio.is_null() {
        // SAFETY: Free `ctx` on failure.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(
            "TLS (OpenSSL): BIO_new_mem_buf(cert) failed".to_string(),
        ));
    }

    let leaf = {
        // SAFETY: `cert_bio` is a valid memory BIO.
        unsafe { (a.pem_read_bio_x509)(cert_bio, ptr::null_mut(), ptr::null(), ptr::null_mut()) }
    };
    if leaf.is_null() {
        // SAFETY: Release BIO and CTX on error.
        unsafe {
            (a.bio_free)(cert_bio);
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): failed to parse leaf certificate PEM: {}",
            drain_openssl_errors(a)
        )));
    }
    let use_ok = {
        // SAFETY: `ctx` and `leaf` are valid OpenSSL objects.
        unsafe { (a.ssl_ctx_use_certificate)(ctx, leaf) }
    };
    // SAFETY: `SSL_CTX_use_certificate` increments refcount, so we release our local ref.
    unsafe {
        (a.x509_free)(leaf);
    }
    if use_ok != 1 {
        // SAFETY: Release BIO and CTX on error.
        unsafe {
            (a.bio_free)(cert_bio);
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_CTX_use_certificate failed: {}",
            drain_openssl_errors(a)
        )));
    }

    // Load optional intermediate chain certificates
    loop {
        let extra = {
            // SAFETY: `cert_bio` is valid; returns null at EOF.
            unsafe {
                (a.pem_read_bio_x509)(cert_bio, ptr::null_mut(), ptr::null(), ptr::null_mut())
            }
        };
        if extra.is_null() {
            let _ = drain_openssl_errors(a);
            break;
        }
        // SAFETY: `SSL_CTRL_EXTRA_CHAIN_CERT` takes ownership of `extra` on success.
        let added = unsafe { (a.ssl_ctx_ctrl)(ctx, SSL_CTRL_EXTRA_CHAIN_CERT, 0, extra) };
        if added != 1 {
            // SAFETY: Free `extra` if OpenSSL did not take ownership.
            unsafe {
                (a.x509_free)(extra);
            }
        }
    }
    // SAFETY: Finished reading cert chain from `cert_bio`.
    unsafe {
        (a.bio_free)(cert_bio);
    }

    // Load private key from memory BIO
    let key_bio = {
        // SAFETY: `cfg.key_pem` bytes remain live for `PEM_read_bio_PrivateKey`.
        unsafe {
            (a.bio_new_mem_buf)(
                cfg.key_pem.as_ptr() as *const c_void,
                cfg.key_pem.len() as c_int,
            )
        }
    };
    if key_bio.is_null() {
        // SAFETY: Free `ctx` on failure.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(
            "TLS (OpenSSL): BIO_new_mem_buf(key) failed".to_string(),
        ));
    }
    let pkey = {
        // SAFETY: `key_bio` is a valid memory BIO.
        unsafe {
            (a.pem_read_bio_private_key)(key_bio, ptr::null_mut(), ptr::null(), ptr::null_mut())
        }
    };
    // SAFETY: Finished reading key from `key_bio`.
    unsafe {
        (a.bio_free)(key_bio);
    }
    if pkey.is_null() {
        // SAFETY: Free `ctx` on error.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): failed to parse private key PEM: {}",
            drain_openssl_errors(a)
        )));
    }
    let key_ok = {
        // SAFETY: `ctx` and `pkey` are valid OpenSSL handles.
        unsafe { (a.ssl_ctx_use_private_key)(ctx, pkey) }
    };
    // SAFETY: `SSL_CTX_use_PrivateKey` increments refcount; free local ref.
    unsafe {
        (a.evp_pkey_free)(pkey);
    }
    if key_ok != 1 {
        // SAFETY: Free `ctx` on error.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_CTX_use_PrivateKey failed: {}",
            drain_openssl_errors(a)
        )));
    }
    let check_ok = {
        // SAFETY: `ctx` now holds both certificate and private key.
        unsafe { (a.ssl_ctx_check_private_key)(ctx) }
    };
    if check_ok != 1 {
        // SAFETY: Free `ctx` on key mismatch.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): certificate and private key do not match: {}",
            drain_openssl_errors(a)
        )));
    }

    let wire = encode_alpn_wire(&cfg.alpn_protocols);
    let alpn_box = if !wire.is_empty()
        && let Some(set_cb) = a.ssl_ctx_set_alpn_select_cb
    {
        let mut boxed = Box::new(wire);
        let arg_ptr = (&raw mut *boxed).cast::<c_void>();
        // SAFETY: `boxed` is kept alive inside `PlatformServerContext` for the full lifetime of `ctx`.
        unsafe {
            set_cb(ctx, Some(server_alpn_select_cb), arg_ptr);
        }
        Some(boxed)
    } else {
        None
    };

    Ok(Arc::new(PlatformServerContext {
        ssl_ctx: ctx,
        _alpn_wire: alpn_box,
    }))
}

enum CtxOwner {
    ClientOwned(*mut c_void),
    ServerShared(Arc<PlatformServerContext>),
}

pub(crate) struct PlatformTlsSession {
    ssl: *mut c_void,
    rbio: *mut c_void, // borrowed by `ssl` (freed by `SSL_free`)
    wbio: *mut c_void, // borrowed by `ssl` (freed by `SSL_free`)
    ctx_owner: CtxOwner,
    handshake_done: bool,
    peer_closed: bool,
}

// SAFETY: Owned exclusively behind a `Mutex` in `MoraTlsStream`; never accessed concurrently.
unsafe impl Send for PlatformTlsSession {}

impl Drop for PlatformTlsSession {
    fn drop(&mut self) {
        if let Ok(a) = api() {
            if !self.ssl.is_null() {
                // SAFETY: `SSL_free` also frees attached `rbio` and `wbio`.
                unsafe {
                    (a.ssl_free)(self.ssl);
                }
            }
            match &self.ctx_owner {
                CtxOwner::ClientOwned(ctx) if !ctx.is_null() => {
                    // SAFETY: Client `SSL_CTX` is uniquely owned and freed after `SSL_free`.
                    unsafe {
                        (a.ssl_ctx_free)(*ctx);
                    }
                }
                CtxOwner::ServerShared(shared) => {
                    // Keep shared `SSL_CTX` alive until after `SSL_free` above.
                    let _ = shared.ssl_ctx;
                }
                CtxOwner::ClientOwned(_) => {}
            }
        }
    }
}

fn attach_mem_bios(a: &OpenSslApi, ssl: *mut c_void) -> MoraResult<(*mut c_void, *mut c_void)> {
    let bio_method = {
        // SAFETY: `BIO_s_mem` returns a static `const BIO_METHOD*`.
        unsafe { (a.bio_s_mem)() }
    };
    let rbio = {
        // SAFETY: Creates a fresh memory BIO.
        unsafe { (a.bio_new)(bio_method) }
    };
    let wbio = {
        // SAFETY: Creates a fresh memory BIO.
        unsafe { (a.bio_new)(bio_method) }
    };
    if rbio.is_null() || wbio.is_null() {
        // SAFETY: Free any successfully created BIOs before returning error.
        unsafe {
            if !rbio.is_null() {
                (a.bio_free)(rbio);
            }
            if !wbio.is_null() {
                (a.bio_free)(wbio);
            }
        }
        return Err(MoraError::Io(
            "TLS (OpenSSL): BIO_new(BIO_s_mem) failed".to_string(),
        ));
    }
    // SAFETY: `SSL_set_bio` transfers ownership of `rbio` and `wbio` to `ssl`.
    unsafe {
        (a.ssl_set_bio)(ssl, rbio, wbio);
    }
    Ok((rbio, wbio))
}

pub(crate) fn create_client_session(cfg: &TlsClientConfig) -> MoraResult<PlatformTlsSession> {
    if !cfg.insecure_skip_verify
        && let Some(ca_pem) = &cfg.ca_pem
    {
        let _ = parse_certificates_der(ca_pem)?;
    }
    let c_host = if !cfg.server_name.is_empty() {
        Some(
            CString::new(cfg.server_name.as_str())
                .map_err(|_| MoraError::Io("TLS: server_name contains interior NUL".to_string()))?,
        )
    } else {
        None
    };

    let a = api()?;
    let method = {
        // SAFETY: `TLS_client_method` returns a static `const SSL_METHOD*`.
        unsafe { (a.tls_client_method)() }
    };
    let ctx = {
        // SAFETY: `method` is valid from `TLS_client_method`.
        unsafe { (a.ssl_ctx_new)(method) }
    };
    if ctx.is_null() {
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_CTX_new(client) failed: {}",
            drain_openssl_errors(a)
        )));
    }
    // Enforce TLS 1.2 minimum
    // SAFETY: `ctx` is valid.
    let _ = unsafe {
        (a.ssl_ctx_ctrl)(
            ctx,
            SSL_CTRL_SET_MIN_PROTO_VERSION,
            TLS1_2_VERSION,
            ptr::null_mut(),
        )
    };

    if !cfg.insecure_skip_verify {
        if let Some(ca_pem) = &cfg.ca_pem {
            let store = {
                // SAFETY: Returns the `X509_STORE*` owned by `ctx`.
                unsafe { (a.ssl_ctx_get_cert_store)(ctx) }
            };
            let bio = {
                // SAFETY: `ca_pem` bytes remain live while reading X.509 certificates.
                unsafe {
                    (a.bio_new_mem_buf)(ca_pem.as_ptr() as *const c_void, ca_pem.len() as c_int)
                }
            };
            if store.is_null() || bio.is_null() {
                // SAFETY: Release allocated handles on error.
                unsafe {
                    if !bio.is_null() {
                        (a.bio_free)(bio);
                    }
                    (a.ssl_ctx_free)(ctx);
                }
                return Err(MoraError::Io(
                    "TLS (OpenSSL): failed to initialize CA store".to_string(),
                ));
            }
            let mut added = 0usize;
            loop {
                let x = {
                    // SAFETY: Reads next PEM certificate from memory BIO.
                    unsafe {
                        (a.pem_read_bio_x509)(bio, ptr::null_mut(), ptr::null(), ptr::null_mut())
                    }
                };
                if x.is_null() {
                    let _ = drain_openssl_errors(a);
                    break;
                }
                // SAFETY: Adds `x` to `store` and frees our local ref.
                let ok = unsafe { (a.x509_store_add_cert)(store, x) };
                // SAFETY: `X509_STORE_add_cert` increments refcount; release local ref.
                unsafe {
                    (a.x509_free)(x);
                }
                if ok == 1 {
                    added += 1;
                }
            }
            // SAFETY: Done reading CA PEM BIO.
            unsafe {
                (a.bio_free)(bio);
            }
            if added == 0 {
                // SAFETY: Free `ctx` on failure.
                unsafe {
                    (a.ssl_ctx_free)(ctx);
                }
                return Err(MoraError::Io(
                    "TLS (OpenSSL): no valid CA certificates in ca_pem".to_string(),
                ));
            }
        } else {
            // SAFETY: Loads OS default CA certificates into `ctx`.
            let _ = unsafe { (a.ssl_ctx_set_default_verify_paths)(ctx) };
        }
    }

    let ssl = {
        // SAFETY: `ctx` is a valid `SSL_CTX*`.
        unsafe { (a.ssl_new)(ctx) }
    };
    if ssl.is_null() {
        // SAFETY: Free `ctx` on failure.
        unsafe {
            (a.ssl_ctx_free)(ctx);
        }
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_new(client) failed: {}",
            drain_openssl_errors(a)
        )));
    }

    let (rbio, wbio) = match attach_mem_bios(a, ssl) {
        Ok(pair) => pair,
        Err(e) => {
            // SAFETY: Free `ssl` and `ctx` on error.
            unsafe {
                (a.ssl_free)(ssl);
                (a.ssl_ctx_free)(ctx);
            }
            return Err(e);
        }
    };

    // SAFETY: Marks `ssl` as TLS client state machine.
    unsafe {
        (a.ssl_set_connect_state)(ssl);
    }

    if cfg.insecure_skip_verify {
        // SAFETY: Explicit user opt-in to skip peer certificate verification.
        unsafe {
            (a.ssl_set_verify)(ssl, SSL_VERIFY_NONE, ptr::null());
        }
    } else {
        // SAFETY: Enforce strict peer certificate chain verification.
        unsafe {
            (a.ssl_set_verify)(ssl, SSL_VERIFY_PEER, ptr::null());
        }
        let param = {
            // SAFETY: Returns `X509_VERIFY_PARAM*` owned by `ssl`.
            unsafe { (a.ssl_get0_param)(ssl) }
        };
        if !param.is_null()
            && let Some(ref c_host) = c_host
        {
            let ok = if cfg.server_name.parse::<IpAddr>().is_ok() {
                // SAFETY: `param` and NUL-terminated IP string are valid.
                unsafe { (a.x509_verify_param_set1_ip_asc)(param, c_host.as_ptr()) }
            } else {
                // SAFETY: `param` and hostname buffer are valid.
                unsafe {
                    (a.x509_verify_param_set1_host)(param, c_host.as_ptr(), cfg.server_name.len())
                }
            };
            if ok != 1 {
                // SAFETY: Free `ssl` and `ctx` on error.
                unsafe {
                    (a.ssl_free)(ssl);
                    (a.ssl_ctx_free)(ctx);
                }
                return Err(MoraError::Io(format!(
                    "TLS (OpenSSL): failed to configure hostname verification for '{}'",
                    cfg.server_name
                )));
            }
        }
    }

    // Configure SNI hostname extension for DNS names (RFC 6066 prohibits raw IP literals in SNI).
    if let Some(ref c_host) = c_host
        && cfg.server_name.parse::<IpAddr>().is_err()
    {
        // SAFETY: `SSL_CTRL_SET_TLSEXT_HOSTNAME` copies `c_host` internally.
        unsafe {
            (a.ssl_ctrl)(
                ssl,
                SSL_CTRL_SET_TLSEXT_HOSTNAME,
                TLSEXT_NAMETYPE_HOST_NAME,
                c_host.as_ptr() as *mut c_void,
            );
        }
    }

    // Configure client ALPN protocols if requested.
    let wire = encode_alpn_wire(&cfg.alpn_protocols);
    if !wire.is_empty()
        && let Some(set_alpn) = a.ssl_set_alpn_protos
    {
        // SAFETY: `wire` is valid length-prefixed ALPN wire bytes.
        let _ = unsafe { set_alpn(ssl, wire.as_ptr(), wire.len() as c_uint) };
    }

    Ok(PlatformTlsSession {
        ssl,
        rbio,
        wbio,
        ctx_owner: CtxOwner::ClientOwned(ctx),
        handshake_done: false,
        peer_closed: false,
    })
}

pub(crate) fn create_server_session(
    ctx: Arc<PlatformServerContext>,
) -> MoraResult<PlatformTlsSession> {
    let a = api()?;
    let ssl = {
        // SAFETY: `ctx.ssl_ctx` is a valid shared server `SSL_CTX*`.
        unsafe { (a.ssl_new)(ctx.ssl_ctx) }
    };
    if ssl.is_null() {
        return Err(MoraError::Io(format!(
            "TLS (OpenSSL): SSL_new(server) failed: {}",
            drain_openssl_errors(a)
        )));
    }
    let (rbio, wbio) = match attach_mem_bios(a, ssl) {
        Ok(pair) => pair,
        Err(e) => {
            // SAFETY: Free `ssl` on BIO setup failure.
            unsafe {
                (a.ssl_free)(ssl);
            }
            return Err(e);
        }
    };
    // SAFETY: Marks `ssl` as TLS server state machine.
    unsafe {
        (a.ssl_set_accept_state)(ssl);
    }

    Ok(PlatformTlsSession {
        ssl,
        rbio,
        wbio,
        ctx_owner: CtxOwner::ServerShared(ctx),
        handshake_done: false,
        peer_closed: false,
    })
}

impl PlatformTlsSession {
    fn rbio_pending(&self, a: &OpenSslApi) -> usize {
        // SAFETY: Queries pending unread ciphertext bytes in `self.rbio`.
        let pending = unsafe { (a.bio_ctrl)(self.rbio, BIO_CTRL_PENDING, 0, ptr::null_mut()) };
        pending.max(0) as usize
    }

    fn feed_incoming(&self, a: &OpenSslApi, incoming: &mut Vec<u8>) {
        if incoming.is_empty() {
            return;
        }
        // SAFETY: Writes incoming ciphertext bytes into OpenSSL memory `rbio`.
        let written = unsafe {
            (a.bio_write)(
                self.rbio,
                incoming.as_ptr() as *const c_void,
                incoming.len() as c_int,
            )
        };
        if written > 0 {
            incoming.drain(..written as usize);
        }
    }

    fn drain_outgoing(&self, a: &OpenSslApi, outgoing: &mut Vec<u8>) {
        loop {
            // SAFETY: Queries pending bytes in `wbio`.
            let pending = unsafe { (a.bio_ctrl)(self.wbio, BIO_CTRL_PENDING, 0, ptr::null_mut()) };
            if pending <= 0 {
                break;
            }
            let take = (pending as usize).min(65536);
            let start = outgoing.len();
            outgoing.resize(start + take, 0);
            // SAFETY: Reads up to `take` pending ciphertext bytes from `wbio`.
            let n = unsafe {
                (a.bio_read)(
                    self.wbio,
                    outgoing[start..].as_mut_ptr() as *mut c_void,
                    take as c_int,
                )
            };
            if n <= 0 {
                outgoing.truncate(start);
                break;
            }
            outgoing.truncate(start + n as usize);
        }
    }
}

impl TlsEngine for PlatformTlsSession {
    fn handshake_step(
        &mut self,
        incoming: &mut Vec<u8>,
        outgoing: &mut Vec<u8>,
    ) -> MoraResult<HandshakeStatus> {
        if self.handshake_done {
            return Ok(HandshakeStatus::Complete);
        }
        let a = api()?;
        self.feed_incoming(a, incoming);

        let rc = {
            // SAFETY: Advances OpenSSL state machine against memory BIOs.
            unsafe { (a.ssl_do_handshake)(self.ssl) }
        };
        self.drain_outgoing(a, outgoing);

        if rc == 1 {
            self.handshake_done = true;
            return Ok(HandshakeStatus::Complete);
        }
        let err = {
            // SAFETY: Queries non-blocking OpenSSL status code for `rc`.
            unsafe { (a.ssl_get_error)(self.ssl, rc) }
        };
        if err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE {
            return Ok(HandshakeStatus::NeedMoreData);
        }
        let verify_rc = {
            // SAFETY: Queries peer certificate verification result code.
            unsafe { (a.ssl_get_verify_result)(self.ssl) }
        };
        let detail = drain_openssl_errors(a);
        if verify_rc != 0 {
            return Err(MoraError::Io(format!(
                "TLS certificate verification failed (X509 verify code {verify_rc}): {detail}"
            )));
        }
        Err(MoraError::Io(format!(
            "TLS (OpenSSL) handshake failed (err={err}): {detail}"
        )))
    }

    fn encrypt(&mut self, plaintext: &[u8], outgoing: &mut Vec<u8>) -> MoraResult<()> {
        if !self.handshake_done {
            return Err(MoraError::Io(
                "TLS (OpenSSL): encrypt called before handshake completed".to_string(),
            ));
        }
        let a = api()?;
        let mut off = 0usize;
        while off < plaintext.len() {
            let chunk = &plaintext[off..(off + 16384).min(plaintext.len())];
            let n = {
                // SAFETY: Encrypts plaintext chunk into `wbio`.
                unsafe {
                    (a.ssl_write)(
                        self.ssl,
                        chunk.as_ptr() as *const c_void,
                        chunk.len() as c_int,
                    )
                }
            };
            self.drain_outgoing(a, outgoing);
            if n > 0 {
                off += n as usize;
            } else {
                let err = {
                    // SAFETY: Queries OpenSSL error for failed `SSL_write`.
                    unsafe { (a.ssl_get_error)(self.ssl, n) }
                };
                return Err(MoraError::Io(format!(
                    "TLS (OpenSSL): SSL_write failed (err={err}): {}",
                    drain_openssl_errors(a)
                )));
            }
        }
        Ok(())
    }

    fn decrypt(
        &mut self,
        incoming: &mut Vec<u8>,
        plaintext_out: &mut Vec<u8>,
        outgoing_response: &mut Vec<u8>,
    ) -> MoraResult<DecryptStatus> {
        if !self.handshake_done {
            return Err(MoraError::Io(
                "TLS (OpenSSL): decrypt called before handshake completed".to_string(),
            ));
        }
        if self.peer_closed {
            return Ok(DecryptStatus::PeerClosed);
        }
        let a = api()?;
        self.feed_incoming(a, incoming);

        let mut produced = false;
        let mut scratch = [0u8; 16384];
        loop {
            let rbio_before = self.rbio_pending(a);
            let n = {
                // SAFETY: Decrypts available application data from `rbio` into `scratch`.
                unsafe {
                    (a.ssl_read)(
                        self.ssl,
                        scratch.as_mut_ptr() as *mut c_void,
                        scratch.len() as c_int,
                    )
                }
            };
            self.drain_outgoing(a, outgoing_response);
            if n > 0 {
                plaintext_out.extend_from_slice(&scratch[..n as usize]);
                produced = true;
                continue;
            }
            let err = {
                // SAFETY: Queries OpenSSL error status for `n <= 0`.
                unsafe { (a.ssl_get_error)(self.ssl, n) }
            };
            if err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE {
                let rbio_after = self.rbio_pending(a);
                // If `SSL_read` consumed a non-application record (e.g. TLS 1.3 `NewSessionTicket`
                // or `KeyUpdate`) without `SSL_MODE_AUTO_RETRY` and unread ciphertext still remains
                // in `rbio`, continue draining immediately instead of stalling on socket I/O.
                if rbio_after > 0 && rbio_after < rbio_before {
                    continue;
                }
                return if produced {
                    Ok(DecryptStatus::Ok)
                } else {
                    Ok(DecryptStatus::NeedMoreData)
                };
            }
            if err == SSL_ERROR_ZERO_RETURN {
                // Latch `close_notify` immediately even if preceding `ApplicationData` records were
                // decrypted in this same batch (`MoraTlsStream` always drains `plain_buf` before
                // surfacing EOF).
                self.peer_closed = true;
                return Ok(DecryptStatus::PeerClosed);
            }
            return Err(MoraError::Io(format!(
                "TLS (OpenSSL): SSL_read failed (err={err}): {}",
                drain_openssl_errors(a)
            )));
        }
    }

    fn shutdown_alert(&mut self, outgoing: &mut Vec<u8>) -> MoraResult<()> {
        if !self.handshake_done {
            return Ok(());
        }
        let a = api()?;
        // SAFETY: Emits TLS `close_notify` alert into `wbio`.
        let _ = unsafe { (a.ssl_shutdown)(self.ssl) };
        self.drain_outgoing(a, outgoing);
        Ok(())
    }

    fn tls_version(&self) -> String {
        if let Ok(a) = api() {
            let ptr = {
                // SAFETY: `SSL_get_version` returns a static NUL-terminated ASCII string.
                unsafe { (a.ssl_get_version)(self.ssl) }
            };
            if !ptr.is_null() {
                // SAFETY: Returned pointer is a valid NUL-terminated static C string.
                return unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned();
            }
        }
        "TLS".to_string()
    }

    fn cipher_suite(&self) -> String {
        if let Ok(a) = api() {
            let cipher = {
                // SAFETY: Returns current negotiated `const SSL_CIPHER*` or null.
                unsafe { (a.ssl_get_current_cipher)(self.ssl) }
            };
            if !cipher.is_null() {
                let name = {
                    // SAFETY: `cipher` is non-null from `SSL_get_current_cipher`.
                    unsafe { (a.ssl_cipher_get_name)(cipher) }
                };
                if !name.is_null() {
                    // SAFETY: Returned pointer is a valid static C string.
                    return unsafe { CStr::from_ptr(name) }
                        .to_string_lossy()
                        .into_owned();
                }
            }
        }
        "UNKNOWN".to_string()
    }

    fn alpn_protocol(&self) -> Option<String> {
        let a = api().ok()?;
        let get_alpn = a.ssl_get0_alpn_selected?;
        let mut data: *const c_uchar = ptr::null();
        let mut len: c_uint = 0;
        // SAFETY: Queries negotiated ALPN slice borrowed from `self.ssl`.
        unsafe {
            get_alpn(self.ssl, &mut data, &mut len);
        }
        if data.is_null() || len == 0 {
            return None;
        }
        // SAFETY: `data` is valid for `len` bytes until `self.ssl` is modified/freed.
        let slice = unsafe { std::slice::from_raw_parts(data, len as usize) };
        std::str::from_utf8(slice).ok().map(ToString::to_string)
    }
}
