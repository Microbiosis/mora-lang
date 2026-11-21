//! Native Windows TLS 1.2/1.3 engine built over dynamic Win32 Schannel (`secur32.dll`),
//! CryptoAPI (`crypt32.dll`), and CNG (`ncrypt.dll`) via [`crate::common::dynlib::DynLib`].
//!
//! Zero external Cargo crates. Implements the sans-IO [`TlsEngine`] contract so all socket I/O
//! remains fully non-blocking and cancellable through Mora's `AsyncTcpStream` / `TaskContext`.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::common::dynlib::DynLib;
use crate::error::{MoraError, MoraResult};

use super::pem::{parse_certificates_der, parse_private_key_pkcs8_der};
use super::{DecryptStatus, HandshakeStatus, TlsClientConfig, TlsEngine, TlsServerConfig};

// ---- Win32 SSPI / Schannel / CryptoAPI / CNG verified ABI constants ----

const SEC_E_OK: i32 = 0;
const SEC_I_CONTINUE_NEEDED: i32 = 0x0009_0312;
const SEC_I_COMPLETE_NEEDED: i32 = 0x0009_0313;
const SEC_I_COMPLETE_AND_CONTINUE: i32 = 0x0009_0314;
const SEC_I_CONTEXT_EXPIRED: i32 = 0x0009_0317;
const SEC_I_RENEGOTIATE: i32 = 0x0009_0321;
const SEC_E_INCOMPLETE_MESSAGE: i32 = 0x8009_0318u32 as i32;

const SECPKG_CRED_INBOUND: u32 = 0x0000_0001;
const SECPKG_CRED_OUTBOUND: u32 = 0x0000_0002;
const SECURITY_NATIVE_DREP: u32 = 0x0000_0010;

const SCHANNEL_CRED_VERSION: u32 = 4;
const SCHANNEL_SHUTDOWN: u32 = 1;
const SCH_CRED_NO_SYSTEM_MAPPER: u32 = 0x0000_0002;
const SCH_CRED_MANUAL_CRED_VALIDATION: u32 = 0x0000_0008;
const SCH_CRED_NO_DEFAULT_CREDS: u32 = 0x0000_0010;
const SCH_CRED_IGNORE_NO_REVOCATION_CHECK: u32 = 0x0000_0800;
const SCH_USE_STRONG_CRYPTO: u32 = 0x0040_0000;

const SP_PROT_TLS1_2_SERVER: u32 = 0x0000_0400;
const SP_PROT_TLS1_2_CLIENT: u32 = 0x0000_0800;
const SP_PROT_TLS1_3_SERVER: u32 = 0x0000_1000;
const SP_PROT_TLS1_3_CLIENT: u32 = 0x0000_2000;

const ISC_REQ_SEQUENCE_DETECT: u32 = 0x0000_0008;
const ISC_REQ_REPLAY_DETECT: u32 = 0x0000_0004;
const ISC_REQ_CONFIDENTIALITY: u32 = 0x0000_0010;
const ISC_REQ_ALLOCATE_MEMORY: u32 = 0x0000_0100;
const ISC_REQ_EXTENDED_ERROR: u32 = 0x0000_4000;
const ISC_REQ_STREAM: u32 = 0x0000_8000;

const ASC_REQ_REPLAY_DETECT: u32 = 0x0000_0004;
const ASC_REQ_SEQUENCE_DETECT: u32 = 0x0000_0008;
const ASC_REQ_CONFIDENTIALITY: u32 = 0x0000_0010;
const ASC_REQ_ALLOCATE_MEMORY: u32 = 0x0000_0100;
const ASC_REQ_EXTENDED_ERROR: u32 = 0x0000_8000;
const ASC_REQ_STREAM: u32 = 0x0001_0000;

const SECBUFFER_VERSION: u32 = 0;
const SECBUFFER_EMPTY: u32 = 0;
const SECBUFFER_DATA: u32 = 1;
const SECBUFFER_TOKEN: u32 = 2;
const SECBUFFER_EXTRA: u32 = 5;
const SECBUFFER_STREAM_TRAILER: u32 = 6;
const SECBUFFER_STREAM_HEADER: u32 = 7;
const SECBUFFER_APPLICATION_PROTOCOLS: u32 = 18;

const SECPKG_ATTR_STREAM_SIZES: u32 = 4;
const SECPKG_ATTR_APPLICATION_PROTOCOL: u32 = 35;
const SECPKG_ATTR_REMOTE_CERT_CONTEXT: u32 = 83;
const SECPKG_ATTR_CONNECTION_INFO: u32 = 90;

const SEC_APPLICATION_PROTOCOL_NEGOTIATION_STATUS_SUCCESS: u32 = 1;
const SEC_APPLICATION_PROTOCOL_NEGOTIATION_EXT_ALPN: u32 = 2;

const X509_ASN_ENCODING: u32 = 0x0000_0001;
const PKCS_7_ASN_ENCODING: u32 = 0x0001_0000;
const CERT_STORE_PROV_MEMORY: *const c_void = 2usize as *const c_void;
const CERT_STORE_ADD_REPLACE_EXISTING: u32 = 3;
const CERT_KEY_PROV_INFO_PROP_ID: u32 = 2;
const CERT_CHAIN_POLICY_SSL: *const c_void = 4usize as *const c_void;
const AUTHTYPE_SERVER: u32 = 2;

const NCRYPTBUFFER_VERSION: u32 = 0;
const NCRYPTBUFFER_PKCS_KEY_NAME: u32 = 45;
const NCRYPT_SILENT_FLAG: u32 = 0x0000_0040;
const NCRYPT_OVERWRITE_KEY_FLAG: u32 = 0x0000_0080;
const NCRYPT_DO_NOT_FINALIZE_FLAG: u32 = 0x0000_0400;
const NCRYPT_ALLOW_DECRYPT_FLAG: u32 = 0x0000_0001;
const NCRYPT_ALLOW_SIGNING_FLAG: u32 = 0x0000_0002;
const NCRYPT_ALLOW_KEY_AGREEMENT_FLAG: u32 = 0x0000_0004;

static KEY_CONTAINER_SEQ: AtomicU64 = AtomicU64::new(1);

// ---- C ABI structures ----

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SecHandle {
    dw_lower: usize,
    dw_upper: usize,
}

impl SecHandle {
    fn is_valid(&self) -> bool {
        self.dw_lower != 0 || self.dw_upper != 0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SecBuffer {
    cb_buffer: u32,
    buffer_type: u32,
    pv_buffer: *mut c_void,
}

impl SecBuffer {
    fn empty() -> Self {
        Self {
            cb_buffer: 0,
            buffer_type: SECBUFFER_EMPTY,
            pv_buffer: ptr::null_mut(),
        }
    }
}

#[repr(C)]
struct SecBufferDesc {
    ul_version: u32,
    c_buffers: u32,
    p_buffers: *mut SecBuffer,
}

#[repr(C)]
struct SchannelCred {
    dw_version: u32,
    c_creds: u32,
    pa_cred: *const *const CertContext,
    h_root_store: *mut c_void,
    c_mappers: u32,
    aph_mappers: *mut *mut c_void,
    c_supported_algs: u32,
    palg_supported_algs: *mut u32,
    grbit_enabled_protocols: u32,
    dw_minimum_cipher_strength: u32,
    dw_maximum_cipher_strength: u32,
    dw_session_lifespan: u32,
    dw_flags: u32,
    dw_cred_format: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SecPkgContextStreamSizes {
    cb_header: u32,
    cb_trailer: u32,
    cb_maximum_message: u32,
    c_buffers: u32,
    cb_block_size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SecPkgContextConnectionInfo {
    dw_protocol: u32,
    ai_cipher: u32,
    c_cipher_strength: u32,
    ai_hash: u32,
    c_hash_strength: u32,
    ai_exch: u32,
    c_exch_strength: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SecPkgContextApplicationProtocol {
    proto_nego_status: u32,
    proto_nego_ext: u32,
    protocol_id_size: u8,
    protocol_id: [u8; 255],
}

impl Default for SecPkgContextApplicationProtocol {
    fn default() -> Self {
        Self {
            proto_nego_status: 0,
            proto_nego_ext: 0,
            protocol_id_size: 0,
            protocol_id: [0u8; 255],
        }
    }
}

#[repr(C)]
struct CertContext {
    dw_cert_encoding_type: u32,
    pb_cert_encoded: *mut u8,
    cb_cert_encoded: u32,
    p_cert_info: *mut c_void,
    h_cert_store: *mut c_void,
}

#[repr(C)]
struct CryptKeyProvInfo {
    pwsz_container_name: *const u16,
    pwsz_prov_name: *const u16,
    dw_prov_type: u32,
    dw_flags: u32,
    c_prov_param: u32,
    rg_prov_param: *mut c_void,
    dw_key_spec: u32,
}

#[repr(C)]
struct NCryptBuffer {
    cb_buffer: u32,
    buffer_type: u32,
    pv_buffer: *mut c_void,
}

#[repr(C)]
struct NCryptBufferDesc {
    ul_version: u32,
    c_buffers: u32,
    p_buffers: *mut NCryptBuffer,
}

#[repr(C)]
struct CtlUsage {
    c_usage_identifier: u32,
    rgpsz_usage_identifier: *mut *const i8,
}

#[repr(C)]
struct CertUsageMatch {
    dw_type: u32,
    usage: CtlUsage,
}

#[repr(C)]
struct CertChainPara {
    cb_size: u32,
    requested_usage: CertUsageMatch,
    requested_issuance_policy: CertUsageMatch,
    dw_url_retrieval_timeout: u32,
    f_check_revocation_freshness_time: i32,
    dw_revocation_freshness_time: u32,
    pft_cache_resync: *mut c_void,
    p_strong_sign_para: *mut c_void,
    dw_strong_sign_flags: u32,
}

#[repr(C)]
struct CertChainEngineConfig {
    cb_size: u32,
    h_restricted_root: *mut c_void,
    h_restricted_trust: *mut c_void,
    h_restricted_other: *mut c_void,
    c_additional_store: u32,
    rgp_additional_store: *mut *mut c_void,
    dw_flags: u32,
    dw_url_retrieval_timeout: u32,
    maximum_cached_certificates: u32,
    cycle_detection_modulus: u32,
    h_exclusive_root: *mut c_void,
    h_exclusive_trusted_people: *mut c_void,
    dw_exclusive_flags: u32,
}

#[repr(C)]
struct SslExtraCertChainPolicyPara {
    cb_size: u32,
    dw_auth_type: u32,
    fdw_checks: u32,
    pwsz_server_name: *const u16,
}

#[repr(C)]
struct CertChainPolicyPara {
    cb_size: u32,
    dw_flags: u32,
    pv_extra_policy_para: *mut c_void,
}

#[repr(C)]
struct CertChainPolicyStatus {
    cb_size: u32,
    dw_error: u32,
    l_chain_index: i32,
    l_element_index: i32,
    pv_extra_policy_status: *mut c_void,
}

// ---- Dynamic DLL function pointer tables ----

type AcquireCredentialsHandleWFn = unsafe extern "system" fn(
    *const u16,
    *const u16,
    u32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut SecHandle,
    *mut i64,
) -> i32;
type FreeCredentialsHandleFn = unsafe extern "system" fn(*mut SecHandle) -> i32;
type InitializeSecurityContextWFn = unsafe extern "system" fn(
    *mut SecHandle,
    *mut SecHandle,
    *const u16,
    u32,
    u32,
    u32,
    *mut SecBufferDesc,
    u32,
    *mut SecHandle,
    *mut SecBufferDesc,
    *mut u32,
    *mut i64,
) -> i32;
type AcceptSecurityContextFn = unsafe extern "system" fn(
    *mut SecHandle,
    *mut SecHandle,
    *mut SecBufferDesc,
    u32,
    u32,
    *mut SecHandle,
    *mut SecBufferDesc,
    *mut u32,
    *mut i64,
) -> i32;
type CompleteAuthTokenFn = unsafe extern "system" fn(*mut SecHandle, *mut SecBufferDesc) -> i32;
type DeleteSecurityContextFn = unsafe extern "system" fn(*mut SecHandle) -> i32;
type FreeContextBufferFn = unsafe extern "system" fn(*mut c_void) -> i32;
type EncryptMessageFn =
    unsafe extern "system" fn(*mut SecHandle, u32, *mut SecBufferDesc, u32) -> i32;
type DecryptMessageFn =
    unsafe extern "system" fn(*mut SecHandle, *mut SecBufferDesc, u32, *mut u32) -> i32;
type ApplyControlTokenFn = unsafe extern "system" fn(*mut SecHandle, *mut SecBufferDesc) -> i32;
type QueryContextAttributesWFn = unsafe extern "system" fn(*mut SecHandle, u32, *mut c_void) -> i32;

type CertOpenStoreFn =
    unsafe extern "system" fn(*const c_void, u32, usize, u32, *const c_void) -> *mut c_void;
type CertCloseStoreFn = unsafe extern "system" fn(*mut c_void, u32) -> i32;
type CertAddEncodedCertificateToStoreFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    *const u8,
    u32,
    u32,
    *mut *const CertContext,
) -> i32;
type CertSetCertificateContextPropertyFn =
    unsafe extern "system" fn(*const CertContext, u32, u32, *const c_void) -> i32;
type CertFreeCertificateContextFn = unsafe extern "system" fn(*const CertContext) -> i32;
type CertCreateCertificateChainEngineFn =
    unsafe extern "system" fn(*const CertChainEngineConfig, *mut *mut c_void) -> i32;
type CertFreeCertificateChainEngineFn = unsafe extern "system" fn(*mut c_void);
type CertGetCertificateChainFn = unsafe extern "system" fn(
    *mut c_void,
    *const CertContext,
    *mut c_void,
    *mut c_void,
    *const CertChainPara,
    u32,
    *mut c_void,
    *mut *const c_void,
) -> i32;
type CertFreeCertificateChainFn = unsafe extern "system" fn(*const c_void);
type CertVerifyCertificateChainPolicyFn = unsafe extern "system" fn(
    *const c_void,
    *const c_void,
    *const CertChainPolicyPara,
    *mut CertChainPolicyStatus,
) -> i32;

type NCryptOpenStorageProviderFn = unsafe extern "system" fn(*mut usize, *const u16, u32) -> i32;
type NCryptImportKeyFn = unsafe extern "system" fn(
    usize,
    usize,
    *const u16,
    *const NCryptBufferDesc,
    *mut usize,
    *const u8,
    u32,
    u32,
) -> i32;
type NCryptSetPropertyFn = unsafe extern "system" fn(usize, *const u16, *const u8, u32, u32) -> i32;
type NCryptFinalizeKeyFn = unsafe extern "system" fn(usize, u32) -> i32;
type NCryptDeleteKeyFn = unsafe extern "system" fn(usize, u32) -> i32;
type NCryptFreeObjectFn = unsafe extern "system" fn(usize) -> i32;

struct WinTlsApi {
    _secur32: DynLib,
    _crypt32: DynLib,
    _ncrypt: DynLib,
    acquire_credentials_handle_w: AcquireCredentialsHandleWFn,
    free_credentials_handle: FreeCredentialsHandleFn,
    initialize_security_context_w: InitializeSecurityContextWFn,
    accept_security_context: AcceptSecurityContextFn,
    complete_auth_token: CompleteAuthTokenFn,
    delete_security_context: DeleteSecurityContextFn,
    free_context_buffer: FreeContextBufferFn,
    encrypt_message: EncryptMessageFn,
    decrypt_message: DecryptMessageFn,
    apply_control_token: ApplyControlTokenFn,
    query_context_attributes_w: QueryContextAttributesWFn,
    cert_open_store: CertOpenStoreFn,
    cert_close_store: CertCloseStoreFn,
    cert_add_encoded_certificate_to_store: CertAddEncodedCertificateToStoreFn,
    cert_set_certificate_context_property: CertSetCertificateContextPropertyFn,
    cert_free_certificate_context: CertFreeCertificateContextFn,
    cert_create_certificate_chain_engine: CertCreateCertificateChainEngineFn,
    cert_free_certificate_chain_engine: CertFreeCertificateChainEngineFn,
    cert_get_certificate_chain: CertGetCertificateChainFn,
    cert_free_certificate_chain: CertFreeCertificateChainFn,
    cert_verify_certificate_chain_policy: CertVerifyCertificateChainPolicyFn,
    ncrypt_open_storage_provider: NCryptOpenStorageProviderFn,
    ncrypt_import_key: NCryptImportKeyFn,
    ncrypt_set_property: NCryptSetPropertyFn,
    ncrypt_finalize_key: NCryptFinalizeKeyFn,
    ncrypt_delete_key: NCryptDeleteKeyFn,
    ncrypt_free_object: NCryptFreeObjectFn,
}

static WIN_TLS_API: OnceLock<Result<WinTlsApi, String>> = OnceLock::new();

fn api() -> MoraResult<&'static WinTlsApi> {
    let res = WIN_TLS_API.get_or_init(load_win_tls_api);
    match res {
        Ok(a) => Ok(a),
        Err(msg) => Err(MoraError::Io(msg.clone())),
    }
}

fn load_win_tls_api() -> Result<WinTlsApi, String> {
    let secur32 = DynLib::open("secur32.dll")
        .ok_or_else(|| "TLS (Windows): failed to load secur32.dll".to_string())?;
    let crypt32 = DynLib::open("crypt32.dll")
        .ok_or_else(|| "TLS (Windows): failed to load crypt32.dll".to_string())?;
    let ncrypt = DynLib::open("ncrypt.dll")
        .ok_or_else(|| "TLS (Windows): failed to load ncrypt.dll".to_string())?;

    macro_rules! sym {
        ($lib:expr, $name:literal) => {{
            let resolved = {
                // SAFETY: Every symbol name matches the exact Win32 SDK C ABI signature declared above.
                unsafe { $lib.get_symbol($name) }
            };
            resolved.ok_or_else(|| {
                format!(
                    "TLS (Windows): missing Win32 symbol {:?}",
                    std::str::from_utf8($name).unwrap_or("?")
                )
            })?
        }};
    }

    Ok(WinTlsApi {
        acquire_credentials_handle_w: sym!(secur32, b"AcquireCredentialsHandleW\0"),
        free_credentials_handle: sym!(secur32, b"FreeCredentialsHandle\0"),
        initialize_security_context_w: sym!(secur32, b"InitializeSecurityContextW\0"),
        accept_security_context: sym!(secur32, b"AcceptSecurityContext\0"),
        complete_auth_token: sym!(secur32, b"CompleteAuthToken\0"),
        delete_security_context: sym!(secur32, b"DeleteSecurityContext\0"),
        free_context_buffer: sym!(secur32, b"FreeContextBuffer\0"),
        encrypt_message: sym!(secur32, b"EncryptMessage\0"),
        decrypt_message: sym!(secur32, b"DecryptMessage\0"),
        apply_control_token: sym!(secur32, b"ApplyControlToken\0"),
        query_context_attributes_w: sym!(secur32, b"QueryContextAttributesW\0"),
        cert_open_store: sym!(crypt32, b"CertOpenStore\0"),
        cert_close_store: sym!(crypt32, b"CertCloseStore\0"),
        cert_add_encoded_certificate_to_store: sym!(crypt32, b"CertAddEncodedCertificateToStore\0"),
        cert_set_certificate_context_property: sym!(
            crypt32,
            b"CertSetCertificateContextProperty\0"
        ),
        cert_free_certificate_context: sym!(crypt32, b"CertFreeCertificateContext\0"),
        cert_create_certificate_chain_engine: sym!(crypt32, b"CertCreateCertificateChainEngine\0"),
        cert_free_certificate_chain_engine: sym!(crypt32, b"CertFreeCertificateChainEngine\0"),
        cert_get_certificate_chain: sym!(crypt32, b"CertGetCertificateChain\0"),
        cert_free_certificate_chain: sym!(crypt32, b"CertFreeCertificateChain\0"),
        cert_verify_certificate_chain_policy: sym!(crypt32, b"CertVerifyCertificateChainPolicy\0"),
        ncrypt_open_storage_provider: sym!(ncrypt, b"NCryptOpenStorageProvider\0"),
        ncrypt_import_key: sym!(ncrypt, b"NCryptImportKey\0"),
        ncrypt_set_property: sym!(ncrypt, b"NCryptSetProperty\0"),
        ncrypt_finalize_key: sym!(ncrypt, b"NCryptFinalizeKey\0"),
        ncrypt_delete_key: sym!(ncrypt, b"NCryptDeleteKey\0"),
        ncrypt_free_object: sym!(ncrypt, b"NCryptFreeObject\0"),
        _secur32: secur32,
        _crypt32: crypt32,
        _ncrypt: ncrypt,
    })
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn encode_schannel_alpn_buffer(protocols: &[String]) -> Option<Vec<u8>> {
    if protocols.is_empty() {
        return None;
    }
    let mut wire = Vec::new();
    for p in protocols {
        let bytes = p.as_bytes();
        if bytes.is_empty() || bytes.len() > 255 {
            continue;
        }
        wire.push(bytes.len() as u8);
        wire.extend_from_slice(bytes);
    }
    if wire.is_empty() {
        return None;
    }
    let list_size = (4 + 2 + wire.len()) as u32;
    let mut out = Vec::with_capacity(4 + list_size as usize);
    out.extend_from_slice(&list_size.to_ne_bytes());
    out.extend_from_slice(&SEC_APPLICATION_PROTOCOL_NEGOTIATION_EXT_ALPN.to_ne_bytes());
    out.extend_from_slice(&(wire.len() as u16).to_ne_bytes());
    out.extend_from_slice(&wire);
    Some(out)
}

fn decode_protocol_name(dw_protocol: u32) -> String {
    if (dw_protocol & (SP_PROT_TLS1_3_CLIENT | SP_PROT_TLS1_3_SERVER)) != 0 {
        "TLSv1.3".to_string()
    } else if (dw_protocol & (SP_PROT_TLS1_2_CLIENT | SP_PROT_TLS1_2_SERVER)) != 0 {
        "TLSv1.2".to_string()
    } else {
        format!("TLS(0x{dw_protocol:08x})")
    }
}

fn decode_cipher_suite(info: &SecPkgContextConnectionInfo) -> String {
    let cipher = match info.ai_cipher {
        0x660E => "AES_128",
        0x660F => "AES_192",
        0x6610 => "AES_256",
        0x6611 => "AES",
        _ => "CIPHER",
    };
    let hash = match info.ai_hash {
        0x800C => "SHA256",
        0x800D => "SHA384",
        0x800E => "SHA512",
        _ => "AEAD",
    };
    format!("{cipher}_{}_{hash}", info.c_cipher_strength)
}

// ---- Reusable server TLS credential (shared across all accepted connections) ----

pub(crate) struct PlatformServerContext {
    cred: SecHandle,
    cert_ctx: *const CertContext,
    cert_store: *mut c_void,
    ncrypt_prov: usize,
    ncrypt_key: usize,
    alpn_protocols: Vec<String>,
}

// SAFETY: Windows Schannel `CredHandle`, `HCERTSTORE`, `PCCERT_CONTEXT`, and CNG provider/key
// handles are safe to share across threads for concurrent `AcceptSecurityContext` calls and are
// freed exactly once when the owning `Arc<PlatformServerContext>` drops.
unsafe impl Send for PlatformServerContext {}
// SAFETY: Schannel inbound credential handles are explicitly documented as thread-safe for
// concurrent `AcceptSecurityContext` handshakes across worker threads.
unsafe impl Sync for PlatformServerContext {}

impl Drop for PlatformServerContext {
    fn drop(&mut self) {
        if let Ok(a) = api() {
            if self.cred.is_valid() {
                // SAFETY: `self.cred` was obtained from `AcquireCredentialsHandleW` and is freed once.
                unsafe {
                    (a.free_credentials_handle)(&mut self.cred);
                }
            }
            if !self.cert_ctx.is_null() {
                // SAFETY: `self.cert_ctx` was obtained from `CertAddEncodedCertificateToStore` and is freed once.
                unsafe {
                    (a.cert_free_certificate_context)(self.cert_ctx);
                }
            }
            if !self.cert_store.is_null() {
                // SAFETY: `self.cert_store` was opened with `CertOpenStore` and is closed once.
                unsafe {
                    (a.cert_close_store)(self.cert_store, 0);
                }
            }
            if self.ncrypt_key != 0 {
                // SAFETY: `self.ncrypt_key` is a live CNG key handle created for this ephemeral container;
                // `NCryptDeleteKey` deletes the temporary key container AND frees the handle.
                unsafe {
                    (a.ncrypt_delete_key)(self.ncrypt_key, NCRYPT_SILENT_FLAG);
                }
            }
            if self.ncrypt_prov != 0 {
                // SAFETY: `self.ncrypt_prov` was opened by `NCryptOpenStorageProvider` and is freed once.
                unsafe {
                    (a.ncrypt_free_object)(self.ncrypt_prov);
                }
            }
        }
    }
}

pub(crate) fn build_server_context(
    cfg: &TlsServerConfig,
) -> MoraResult<Arc<PlatformServerContext>> {
    let a = api()?;
    let cert_ders = parse_certificates_der(&cfg.cert_pem)?;
    let pkcs8_der = parse_private_key_pkcs8_der(&cfg.key_pem)?;

    let ksp_name = to_wide_nul("Microsoft Software Key Storage Provider");
    let seq = KEY_CONTAINER_SEQ.fetch_add(1, Ordering::Relaxed);
    let container_name_str = format!("mora-tls-{}-{seq}", std::process::id());
    let mut container_name_w = to_wide_nul(&container_name_str);

    let mut h_prov: usize = 0;
    let st_prov = {
        // SAFETY: `h_prov` and `ksp_name` are valid aligned pointers for `NCryptOpenStorageProvider`.
        unsafe { (a.ncrypt_open_storage_provider)(&mut h_prov, ksp_name.as_ptr(), 0) }
    };
    if st_prov != 0 {
        return Err(MoraError::Io(format!(
            "TLS (Schannel): NCryptOpenStorageProvider failed: 0x{:08x}",
            st_prov as u32
        )));
    }

    let mut name_buf = NCryptBuffer {
        cb_buffer: (container_name_w.len() * std::mem::size_of::<u16>()) as u32,
        buffer_type: NCRYPTBUFFER_PKCS_KEY_NAME,
        pv_buffer: container_name_w.as_mut_ptr() as *mut c_void,
    };
    let param_desc = NCryptBufferDesc {
        ul_version: NCRYPTBUFFER_VERSION,
        c_buffers: 1,
        p_buffers: &mut name_buf,
    };
    let blob_type = to_wide_nul("PKCS8_PRIVATEKEY");

    let mut h_key: usize = 0;
    let st_imp = {
        // SAFETY: All pointers (`h_prov`, `blob_type`, `param_desc`, `h_key`, `pkcs8_der`) remain valid
        // for the synchronous `NCryptImportKey` call.
        unsafe {
            (a.ncrypt_import_key)(
                h_prov,
                0,
                blob_type.as_ptr(),
                &param_desc,
                &mut h_key,
                pkcs8_der.as_ptr(),
                pkcs8_der.len() as u32,
                NCRYPT_DO_NOT_FINALIZE_FLAG | NCRYPT_OVERWRITE_KEY_FLAG | NCRYPT_SILENT_FLAG,
            )
        }
    };
    if st_imp != 0 {
        // SAFETY: `h_prov` was successfully opened above and is released on error.
        unsafe {
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(format!(
            "TLS (Schannel): NCryptImportKey(PKCS8_PRIVATEKEY) failed: 0x{:08x}",
            st_imp as u32
        )));
    }

    let key_usage: u32 =
        NCRYPT_ALLOW_DECRYPT_FLAG | NCRYPT_ALLOW_SIGNING_FLAG | NCRYPT_ALLOW_KEY_AGREEMENT_FLAG;
    let usage_prop = to_wide_nul("Key Usage");
    let st_usage = {
        // SAFETY: `h_key`, `usage_prop`, and `&key_usage` (4 bytes) are valid for `NCryptSetProperty`.
        unsafe {
            (a.ncrypt_set_property)(
                h_key,
                usage_prop.as_ptr(),
                (&raw const key_usage).cast::<u8>(),
                std::mem::size_of::<u32>() as u32,
                NCRYPT_SILENT_FLAG,
            )
        }
    };
    if st_usage != 0 {
        // SAFETY: Cleanup opened CNG key and provider on failure.
        unsafe {
            (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(format!(
            "TLS (Schannel): NCryptSetProperty(Key Usage) failed: 0x{:08x}",
            st_usage as u32
        )));
    }

    let st_fin = {
        // SAFETY: `h_key` is an unfinalized imported CNG key handle.
        unsafe { (a.ncrypt_finalize_key)(h_key, NCRYPT_SILENT_FLAG) }
    };
    if st_fin != 0 {
        // SAFETY: Cleanup opened CNG key and provider on failure.
        unsafe {
            (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(format!(
            "TLS (Schannel): NCryptFinalizeKey failed: 0x{:08x}",
            st_fin as u32
        )));
    }

    let cert_store = {
        // SAFETY: `CERT_STORE_PROV_MEMORY` creates an isolated in-memory CryptoAPI store.
        unsafe {
            (a.cert_open_store)(
                CERT_STORE_PROV_MEMORY,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                0,
                ptr::null(),
            )
        }
    };
    if cert_store.is_null() {
        // SAFETY: Cleanup CNG handles on failure.
        unsafe {
            (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(
            "TLS (Schannel): CertOpenStore(MEMORY) failed".to_string(),
        ));
    }

    let mut leaf_cert: *const CertContext = ptr::null();
    for (idx, der) in cert_ders.iter().enumerate() {
        let mut out_ctx: *const CertContext = ptr::null();
        let ok = {
            // SAFETY: `cert_store` is a valid memory store and `der` is a valid byte slice.
            unsafe {
                (a.cert_add_encoded_certificate_to_store)(
                    cert_store,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    der.as_ptr(),
                    der.len() as u32,
                    CERT_STORE_ADD_REPLACE_EXISTING,
                    if idx == 0 {
                        &mut out_ctx
                    } else {
                        ptr::null_mut()
                    },
                )
            }
        };
        if ok == 0 {
            // SAFETY: Release allocated Win32 resources on failure.
            unsafe {
                if !leaf_cert.is_null() {
                    (a.cert_free_certificate_context)(leaf_cert);
                }
                (a.cert_close_store)(cert_store, 0);
                (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
                (a.ncrypt_free_object)(h_prov);
            }
            return Err(MoraError::Io(format!(
                "TLS (Schannel): failed to parse certificate #{idx} DER into memory store"
            )));
        }
        if idx == 0 {
            leaf_cert = out_ctx;
        }
    }

    let prov_info = CryptKeyProvInfo {
        pwsz_container_name: container_name_w.as_ptr(),
        pwsz_prov_name: ksp_name.as_ptr(),
        dw_prov_type: 0,
        dw_flags: 0,
        c_prov_param: 0,
        rg_prov_param: ptr::null_mut(),
        dw_key_spec: 0,
    };
    let ok_bind = {
        // SAFETY: `leaf_cert` is valid and `prov_info` references live NUL-terminated UTF-16 buffers.
        unsafe {
            (a.cert_set_certificate_context_property)(
                leaf_cert,
                CERT_KEY_PROV_INFO_PROP_ID,
                0,
                (&raw const prov_info).cast::<c_void>(),
            )
        }
    };
    if ok_bind == 0 {
        // SAFETY: Release allocated Win32 resources on failure.
        unsafe {
            (a.cert_free_certificate_context)(leaf_cert);
            (a.cert_close_store)(cert_store, 0);
            (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(
            "TLS (Schannel): CertSetCertificateContextProperty(KEY_PROV_INFO) failed".to_string(),
        ));
    }

    let pkg_name = to_wide_nul("Microsoft Unified Security Protocol Provider");
    let cert_arr = [leaf_cert];
    let mut sch_cred = SchannelCred {
        dw_version: SCHANNEL_CRED_VERSION,
        c_creds: 1,
        pa_cred: cert_arr.as_ptr(),
        h_root_store: ptr::null_mut(),
        c_mappers: 0,
        aph_mappers: ptr::null_mut(),
        c_supported_algs: 0,
        palg_supported_algs: ptr::null_mut(),
        grbit_enabled_protocols: 0, // OS strong defaults (TLS 1.2 + TLS 1.3 on Win11+)
        dw_minimum_cipher_strength: 0,
        dw_maximum_cipher_strength: 0,
        dw_session_lifespan: 0,
        dw_flags: SCH_CRED_NO_SYSTEM_MAPPER | SCH_USE_STRONG_CRYPTO,
        dw_cred_format: 0,
    };

    let mut cred_handle = SecHandle::default();
    let mut expiry: i64 = 0;
    let st_cred = {
        // SAFETY: `pkg_name`, `sch_cred`, `cred_handle`, and `expiry` are valid for `AcquireCredentialsHandleW`.
        unsafe {
            (a.acquire_credentials_handle_w)(
                ptr::null(),
                pkg_name.as_ptr(),
                SECPKG_CRED_INBOUND,
                ptr::null_mut(),
                (&raw mut sch_cred).cast::<c_void>(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut cred_handle,
                &mut expiry,
            )
        }
    };
    if st_cred != SEC_E_OK {
        // SAFETY: Release allocated Win32 resources on failure.
        unsafe {
            (a.cert_free_certificate_context)(leaf_cert);
            (a.cert_close_store)(cert_store, 0);
            (a.ncrypt_delete_key)(h_key, NCRYPT_SILENT_FLAG);
            (a.ncrypt_free_object)(h_prov);
        }
        return Err(MoraError::Io(format!(
            "TLS (Schannel): AcquireCredentialsHandleW(server) failed: 0x{:08x}",
            st_cred as u32
        )));
    }

    Ok(Arc::new(PlatformServerContext {
        cred: cred_handle,
        cert_ctx: leaf_cert,
        cert_store,
        ncrypt_prov: h_prov,
        ncrypt_key: h_key,
        alpn_protocols: cfg.alpn_protocols.clone(),
    }))
}

// ---- Active Schannel TLS session (Client or Server) ----

enum CredOwner {
    ClientOwned(SecHandle),
    ServerShared(Arc<PlatformServerContext>),
}

impl CredOwner {
    fn cred_ptr(&mut self) -> *mut SecHandle {
        match self {
            CredOwner::ClientOwned(h) => &raw mut *h,
            // Schannel reads the inbound credential handle during AcceptSecurityContext without mutating it.
            CredOwner::ServerShared(arc) => (&raw const arc.cred).cast_mut(),
        }
    }
}

pub(crate) struct PlatformTlsSession {
    is_client: bool,
    cred: CredOwner,
    ctx: SecHandle,
    ctx_initialized: bool,
    handshake_done: bool,
    server_name: Option<String>,
    ca_pem: Option<String>,
    insecure_skip_verify: bool,
    alpn_buf: Option<Vec<u8>>,
    stream_sizes: SecPkgContextStreamSizes,
    conn_info: SecPkgContextConnectionInfo,
    negotiated_alpn: Option<String>,
}

// SAFETY: A `PlatformTlsSession` is owned exclusively behind a `Mutex` inside `MoraTlsStream`;
// its Win32 `CtxtHandle` / `CredHandle` are never accessed concurrently from two threads.
unsafe impl Send for PlatformTlsSession {}

impl Drop for PlatformTlsSession {
    fn drop(&mut self) {
        if let Ok(a) = api() {
            if self.ctx_initialized && self.ctx.is_valid() {
                // SAFETY: `self.ctx` was created by `InitializeSecurityContextW` / `AcceptSecurityContext`.
                unsafe {
                    (a.delete_security_context)(&mut self.ctx);
                }
            }
            if let CredOwner::ClientOwned(ref mut h) = self.cred
                && h.is_valid()
            {
                // SAFETY: Client credential handle is uniquely owned and freed once on Drop.
                unsafe {
                    (a.free_credentials_handle)(h);
                }
            }
        }
    }
}

pub(crate) fn create_client_session(cfg: &TlsClientConfig) -> MoraResult<PlatformTlsSession> {
    let a = api()?;
    let pkg_name = to_wide_nul("Microsoft Unified Security Protocol Provider");
    let mut sch_cred = SchannelCred {
        dw_version: SCHANNEL_CRED_VERSION,
        c_creds: 0,
        pa_cred: ptr::null(),
        h_root_store: ptr::null_mut(),
        c_mappers: 0,
        aph_mappers: ptr::null_mut(),
        c_supported_algs: 0,
        palg_supported_algs: ptr::null_mut(),
        grbit_enabled_protocols: 0, // OS strong defaults (TLS 1.2 + TLS 1.3)
        dw_minimum_cipher_strength: 0,
        dw_maximum_cipher_strength: 0,
        dw_session_lifespan: 0,
        dw_flags: SCH_CRED_MANUAL_CRED_VALIDATION
            | SCH_CRED_NO_DEFAULT_CREDS
            | SCH_CRED_IGNORE_NO_REVOCATION_CHECK
            | SCH_USE_STRONG_CRYPTO,
        dw_cred_format: 0,
    };

    let mut cred = SecHandle::default();
    let mut expiry: i64 = 0;
    let st = {
        // SAFETY: Valid pointers passed to `AcquireCredentialsHandleW` for outbound client handle.
        unsafe {
            (a.acquire_credentials_handle_w)(
                ptr::null(),
                pkg_name.as_ptr(),
                SECPKG_CRED_OUTBOUND,
                ptr::null_mut(),
                (&raw mut sch_cred).cast::<c_void>(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut cred,
                &mut expiry,
            )
        }
    };
    if st != SEC_E_OK {
        return Err(MoraError::Io(format!(
            "TLS (Schannel): AcquireCredentialsHandleW(client) failed: 0x{:08x}",
            st as u32
        )));
    }

    Ok(PlatformTlsSession {
        is_client: true,
        cred: CredOwner::ClientOwned(cred),
        ctx: SecHandle::default(),
        ctx_initialized: false,
        handshake_done: false,
        server_name: Some(cfg.server_name.clone()),
        ca_pem: cfg.ca_pem.clone(),
        insecure_skip_verify: cfg.insecure_skip_verify,
        alpn_buf: encode_schannel_alpn_buffer(&cfg.alpn_protocols),
        stream_sizes: SecPkgContextStreamSizes::default(),
        conn_info: SecPkgContextConnectionInfo::default(),
        negotiated_alpn: None,
    })
}

pub(crate) fn create_server_session(
    ctx: Arc<PlatformServerContext>,
) -> MoraResult<PlatformTlsSession> {
    let alpn_buf = encode_schannel_alpn_buffer(&ctx.alpn_protocols);
    Ok(PlatformTlsSession {
        is_client: false,
        cred: CredOwner::ServerShared(ctx),
        ctx: SecHandle::default(),
        ctx_initialized: false,
        handshake_done: false,
        server_name: None,
        ca_pem: None,
        insecure_skip_verify: false,
        alpn_buf,
        stream_sizes: SecPkgContextStreamSizes::default(),
        conn_info: SecPkgContextConnectionInfo::default(),
        negotiated_alpn: None,
    })
}

impl PlatformTlsSession {
    fn query_post_handshake_attrs(&mut self) -> MoraResult<()> {
        let a = api()?;
        let st_sizes = {
            // SAFETY: `self.ctx` is a completed Schannel context and `stream_sizes` has exact ABI layout.
            unsafe {
                (a.query_context_attributes_w)(
                    &mut self.ctx,
                    SECPKG_ATTR_STREAM_SIZES,
                    (&raw mut self.stream_sizes).cast::<c_void>(),
                )
            }
        };
        if st_sizes != SEC_E_OK {
            return Err(MoraError::Io(format!(
                "TLS (Schannel): QueryContextAttributesW(STREAM_SIZES) failed: 0x{:08x}",
                st_sizes as u32
            )));
        }

        // SAFETY: `self.ctx` is valid and `conn_info` has exact `SecPkgContext_ConnectionInfo` layout.
        let st_info = unsafe {
            (a.query_context_attributes_w)(
                &mut self.ctx,
                SECPKG_ATTR_CONNECTION_INFO,
                (&raw mut self.conn_info).cast::<c_void>(),
            )
        };
        if st_info == SEC_E_OK
            && (self.conn_info.dw_protocol
                & (SP_PROT_TLS1_2_CLIENT
                    | SP_PROT_TLS1_2_SERVER
                    | SP_PROT_TLS1_3_CLIENT
                    | SP_PROT_TLS1_3_SERVER))
                == 0
        {
            return Err(MoraError::Io(format!(
                "TLS (Schannel): negotiated protocol 0x{:08x} is below minimum TLS 1.2",
                self.conn_info.dw_protocol
            )));
        }

        let mut alpn_res = SecPkgContextApplicationProtocol::default();
        // SAFETY: `self.ctx` is valid and `alpn_res` has exact `SecPkgContext_ApplicationProtocol` layout.
        let st_alpn = unsafe {
            (a.query_context_attributes_w)(
                &mut self.ctx,
                SECPKG_ATTR_APPLICATION_PROTOCOL,
                (&raw mut alpn_res).cast::<c_void>(),
            )
        };
        if st_alpn == SEC_E_OK
            && alpn_res.proto_nego_status == SEC_APPLICATION_PROTOCOL_NEGOTIATION_STATUS_SUCCESS
            && alpn_res.protocol_id_size > 0
        {
            let len = alpn_res.protocol_id_size as usize;
            if let Ok(proto) = std::str::from_utf8(&alpn_res.protocol_id[..len]) {
                self.negotiated_alpn = Some(proto.to_string());
            }
        }
        Ok(())
    }

    fn verify_server_certificate(&mut self) -> MoraResult<()> {
        if !self.is_client || self.insecure_skip_verify {
            return Ok(());
        }
        let a = api()?;
        let mut peer_cert: *const CertContext = ptr::null();
        let st = {
            // SAFETY: `self.ctx` is a completed client Schannel context.
            unsafe {
                (a.query_context_attributes_w)(
                    &mut self.ctx,
                    SECPKG_ATTR_REMOTE_CERT_CONTEXT,
                    (&raw mut peer_cert).cast::<c_void>(),
                )
            }
        };
        if st != SEC_E_OK || peer_cert.is_null() {
            return Err(MoraError::Io(format!(
                "TLS (Schannel): failed to query remote server certificate: 0x{:08x}",
                st as u32
            )));
        }

        // Optional exclusive CA trust store when `ca_pem` is provided.
        let mut custom_store: *mut c_void = ptr::null_mut();
        let mut custom_engine: *mut c_void = ptr::null_mut();
        let verify_result = (|| -> MoraResult<()> {
            if let Some(ca_pem) = &self.ca_pem {
                let ca_ders = parse_certificates_der(ca_pem)?;
                custom_store = {
                    // SAFETY: Opens an isolated in-memory store for user-specified trust anchors.
                    unsafe {
                        (a.cert_open_store)(
                            CERT_STORE_PROV_MEMORY,
                            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                            0,
                            0,
                            ptr::null(),
                        )
                    }
                };
                if custom_store.is_null() {
                    return Err(MoraError::Io(
                        "TLS (Schannel): CertOpenStore(CA) failed".to_string(),
                    ));
                }
                for der in &ca_ders {
                    let ok = {
                        // SAFETY: `custom_store` is valid and `der` contains DER-encoded X.509 certificate bytes.
                        unsafe {
                            (a.cert_add_encoded_certificate_to_store)(
                                custom_store,
                                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                                der.as_ptr(),
                                der.len() as u32,
                                CERT_STORE_ADD_REPLACE_EXISTING,
                                ptr::null_mut(),
                            )
                        }
                    };
                    if ok == 0 {
                        return Err(MoraError::Io(
                            "TLS (Schannel): failed to add CA certificate to trust store"
                                .to_string(),
                        ));
                    }
                }
                let engine_cfg = CertChainEngineConfig {
                    cb_size: std::mem::size_of::<CertChainEngineConfig>() as u32,
                    h_restricted_root: ptr::null_mut(),
                    h_restricted_trust: ptr::null_mut(),
                    h_restricted_other: ptr::null_mut(),
                    c_additional_store: 0,
                    rgp_additional_store: ptr::null_mut(),
                    dw_flags: 0,
                    dw_url_retrieval_timeout: 0,
                    maximum_cached_certificates: 0,
                    cycle_detection_modulus: 0,
                    h_exclusive_root: custom_store,
                    h_exclusive_trusted_people: ptr::null_mut(),
                    dw_exclusive_flags: 0,
                };
                let ok_eng = {
                    // SAFETY: `engine_cfg` has exact `cb_size` and valid `h_exclusive_root`.
                    unsafe {
                        (a.cert_create_certificate_chain_engine)(&engine_cfg, &mut custom_engine)
                    }
                };
                if ok_eng == 0 {
                    return Err(MoraError::Io(
                        "TLS (Schannel): CertCreateCertificateChainEngine failed".to_string(),
                    ));
                }
            }

            let chain_para = CertChainPara {
                cb_size: std::mem::size_of::<CertChainPara>() as u32,
                requested_usage: CertUsageMatch {
                    dw_type: 0,
                    usage: CtlUsage {
                        c_usage_identifier: 0,
                        rgpsz_usage_identifier: ptr::null_mut(),
                    },
                },
                requested_issuance_policy: CertUsageMatch {
                    dw_type: 0,
                    usage: CtlUsage {
                        c_usage_identifier: 0,
                        rgpsz_usage_identifier: ptr::null_mut(),
                    },
                },
                dw_url_retrieval_timeout: 0,
                f_check_revocation_freshness_time: 0,
                dw_revocation_freshness_time: 0,
                pft_cache_resync: ptr::null_mut(),
                p_strong_sign_para: ptr::null_mut(),
                dw_strong_sign_flags: 0,
            };

            let peer_store = {
                // SAFETY: `peer_cert` is non-null and returned by Schannel `QueryContextAttributesW`.
                unsafe { (*peer_cert).h_cert_store }
            };
            let mut chain_ctx: *const c_void = ptr::null();
            let ok_chain = {
                // SAFETY: `custom_engine` (or null for system default), `peer_cert`, `peer_store`,
                // and `chain_para` are valid for `CertGetCertificateChain`.
                unsafe {
                    (a.cert_get_certificate_chain)(
                        custom_engine,
                        peer_cert,
                        ptr::null_mut(),
                        peer_store,
                        &chain_para,
                        0,
                        ptr::null_mut(),
                        &mut chain_ctx,
                    )
                }
            };
            if ok_chain == 0 || chain_ctx.is_null() {
                return Err(MoraError::Io(
                    "TLS (Schannel): CertGetCertificateChain failed".to_string(),
                ));
            }

            let server_name_w = self.server_name.as_deref().map(to_wide_nul);
            let mut ssl_extra = SslExtraCertChainPolicyPara {
                cb_size: std::mem::size_of::<SslExtraCertChainPolicyPara>() as u32,
                dw_auth_type: AUTHTYPE_SERVER,
                fdw_checks: 0,
                pwsz_server_name: server_name_w.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            };
            let policy_para = CertChainPolicyPara {
                cb_size: std::mem::size_of::<CertChainPolicyPara>() as u32,
                dw_flags: 0,
                pv_extra_policy_para: (&raw mut ssl_extra).cast::<c_void>(),
            };
            let mut policy_status = CertChainPolicyStatus {
                cb_size: std::mem::size_of::<CertChainPolicyStatus>() as u32,
                dw_error: 0,
                l_chain_index: -1,
                l_element_index: -1,
                pv_extra_policy_status: ptr::null_mut(),
            };
            let ok_pol = {
                // SAFETY: `chain_ctx`, `policy_para`, and `policy_status` are valid for `CertVerifyCertificateChainPolicy`.
                unsafe {
                    (a.cert_verify_certificate_chain_policy)(
                        CERT_CHAIN_POLICY_SSL,
                        chain_ctx,
                        &policy_para,
                        &mut policy_status,
                    )
                }
            };
            // SAFETY: `chain_ctx` was returned by `CertGetCertificateChain` and is freed once.
            unsafe {
                (a.cert_free_certificate_chain)(chain_ctx);
            }
            if ok_pol == 0 {
                return Err(MoraError::Io(
                    "TLS (Schannel): CertVerifyCertificateChainPolicy call failed".to_string(),
                ));
            }
            if policy_status.dw_error != 0 {
                let reason = match policy_status.dw_error {
                    0x800B_0109 => "untrusted root CA (CERT_E_UNTRUSTEDROOT)",
                    0x800B_010F => "certificate hostname mismatch (CERT_E_CN_NO_MATCH)",
                    0x800B_0101 => "certificate expired or not yet valid (CERT_E_EXPIRED)",
                    0x800B_010A => "incomplete certificate chain (CERT_E_CHAINING)",
                    _ => "certificate chain policy violation",
                };
                return Err(MoraError::Io(format!(
                    "TLS certificate verification failed for '{}': {reason} (0x{:08x})",
                    self.server_name.as_deref().unwrap_or(""),
                    policy_status.dw_error
                )));
            }
            Ok(())
        })();

        // SAFETY: Always free remote certificate context, custom engine, and custom store.
        unsafe {
            if !custom_engine.is_null() {
                (a.cert_free_certificate_chain_engine)(custom_engine);
            }
            if !custom_store.is_null() {
                (a.cert_close_store)(custom_store, 0);
            }
            (a.cert_free_certificate_context)(peer_cert);
        }
        verify_result
    }

    fn run_handshake_call(
        &mut self,
        incoming: &mut Vec<u8>,
        outgoing: &mut Vec<u8>,
    ) -> MoraResult<HandshakeStatus> {
        let a = api()?;

        // Prepare input buffers
        let mut in_bufs = [SecBuffer::empty(), SecBuffer::empty()];
        let mut in_count = 0u32;
        let first_call = !self.ctx_initialized;

        if first_call && self.is_client {
            if let Some(ref mut alpn) = self.alpn_buf {
                in_bufs[0] = SecBuffer {
                    cb_buffer: alpn.len() as u32,
                    buffer_type: SECBUFFER_APPLICATION_PROTOCOLS,
                    pv_buffer: alpn.as_mut_ptr() as *mut c_void,
                };
                in_count = 1;
            }
        } else {
            if incoming.is_empty() {
                return Ok(HandshakeStatus::NeedMoreData);
            }
            in_bufs[0] = SecBuffer {
                cb_buffer: incoming.len() as u32,
                buffer_type: SECBUFFER_TOKEN,
                pv_buffer: incoming.as_mut_ptr() as *mut c_void,
            };
            in_count = 2;
            if first_call
                && !self.is_client
                && let Some(ref mut alpn) = self.alpn_buf
            {
                in_bufs[1] = SecBuffer {
                    cb_buffer: alpn.len() as u32,
                    buffer_type: SECBUFFER_APPLICATION_PROTOCOLS,
                    pv_buffer: alpn.as_mut_ptr() as *mut c_void,
                };
            }
        }

        let mut in_desc = SecBufferDesc {
            ul_version: SECBUFFER_VERSION,
            c_buffers: in_count,
            p_buffers: in_bufs.as_mut_ptr(),
        };
        let in_desc_ptr = if in_count > 0 {
            &raw mut in_desc
        } else {
            ptr::null_mut()
        };

        let mut out_bufs = [SecBuffer::empty(), SecBuffer::empty()];
        out_bufs[0].buffer_type = SECBUFFER_TOKEN;
        let mut out_desc = SecBufferDesc {
            ul_version: SECBUFFER_VERSION,
            c_buffers: 2,
            p_buffers: out_bufs.as_mut_ptr(),
        };

        let mut attrs: u32 = 0;
        let mut expiry: i64 = 0;
        let mut new_ctx = self.ctx;
        let prev_ctx_ptr = if self.ctx_initialized {
            &raw mut self.ctx
        } else {
            ptr::null_mut()
        };

        let status = if self.is_client {
            let target_w = self.server_name.as_deref().map(to_wide_nul);
            let target_ptr = target_w.as_ref().map_or(ptr::null(), |v| v.as_ptr());
            let flags = ISC_REQ_SEQUENCE_DETECT
                | ISC_REQ_REPLAY_DETECT
                | ISC_REQ_CONFIDENTIALITY
                | ISC_REQ_ALLOCATE_MEMORY
                | ISC_REQ_EXTENDED_ERROR
                | ISC_REQ_STREAM;
            // SAFETY: All SSPI handles, buffer descriptors, and UTF-16 target name are valid.
            unsafe {
                (a.initialize_security_context_w)(
                    self.cred.cred_ptr(),
                    prev_ctx_ptr,
                    target_ptr,
                    flags,
                    0,
                    SECURITY_NATIVE_DREP,
                    in_desc_ptr,
                    0,
                    &mut new_ctx,
                    &mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            }
        } else {
            let flags = ASC_REQ_SEQUENCE_DETECT
                | ASC_REQ_REPLAY_DETECT
                | ASC_REQ_CONFIDENTIALITY
                | ASC_REQ_ALLOCATE_MEMORY
                | ASC_REQ_EXTENDED_ERROR
                | ASC_REQ_STREAM;
            // SAFETY: All SSPI handles and buffer descriptors are valid for `AcceptSecurityContext`.
            unsafe {
                (a.accept_security_context)(
                    self.cred.cred_ptr(),
                    prev_ctx_ptr,
                    in_desc_ptr,
                    flags,
                    SECURITY_NATIVE_DREP,
                    &mut new_ctx,
                    &mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            }
        };

        if status == SEC_E_INCOMPLETE_MESSAGE {
            for b in &out_bufs {
                if !b.pv_buffer.is_null() {
                    // SAFETY: Buffer was allocated by Schannel via `ALLOCATE_MEMORY`.
                    unsafe {
                        (a.free_context_buffer)(b.pv_buffer);
                    }
                }
            }
            return Ok(HandshakeStatus::NeedMoreData);
        }

        if new_ctx.is_valid() {
            self.ctx = new_ctx;
            self.ctx_initialized = true;
        }

        if status == SEC_I_COMPLETE_NEEDED || status == SEC_I_COMPLETE_AND_CONTINUE {
            // SAFETY: `self.ctx` and `out_desc` are valid per SSPI completion contract.
            let st_comp = unsafe { (a.complete_auth_token)(&mut self.ctx, &mut out_desc) };
            if st_comp != SEC_E_OK {
                for b in &out_bufs {
                    if !b.pv_buffer.is_null() {
                        // SAFETY: Free Schannel-allocated output token on error.
                        unsafe {
                            (a.free_context_buffer)(b.pv_buffer);
                        }
                    }
                }
                return Err(MoraError::Io(format!(
                    "TLS (Schannel): CompleteAuthToken failed: 0x{:08x}",
                    st_comp as u32
                )));
            }
        }

        // Copy any outgoing handshake tokens (including alert tokens on failure!) and free SSPI buffers.
        for b in &out_bufs {
            if !b.pv_buffer.is_null() {
                if b.cb_buffer > 0 {
                    // SAFETY: `b.pv_buffer` points to `b.cb_buffer` readable bytes allocated by Schannel.
                    let slice = unsafe {
                        std::slice::from_raw_parts(b.pv_buffer as *const u8, b.cb_buffer as usize)
                    };
                    outgoing.extend_from_slice(slice);
                }
                // SAFETY: Buffer was allocated by Schannel via `ALLOCATE_MEMORY` and freed once.
                unsafe {
                    (a.free_context_buffer)(b.pv_buffer);
                }
            }
        }

        // Advance `incoming` cursor using SECBUFFER_EXTRA if input was consumed.
        if !(first_call && self.is_client) {
            let mut extra_len: Option<usize> = None;
            for b in &in_bufs {
                if b.buffer_type == SECBUFFER_EXTRA {
                    extra_len = Some(b.cb_buffer as usize);
                }
            }
            if let Some(rem) = extra_len {
                let consumed = incoming.len().saturating_sub(rem);
                incoming.drain(..consumed);
            } else if status == SEC_E_OK
                || status == SEC_I_CONTINUE_NEEDED
                || status == SEC_I_COMPLETE_NEEDED
                || status == SEC_I_COMPLETE_AND_CONTINUE
            {
                incoming.clear();
            }
        }

        match status {
            SEC_E_OK | SEC_I_COMPLETE_NEEDED => {
                self.query_post_handshake_attrs()?;
                self.verify_server_certificate()?;
                self.handshake_done = true;
                Ok(HandshakeStatus::Complete)
            }
            SEC_I_CONTINUE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE => {
                Ok(HandshakeStatus::NeedMoreData)
            }
            other => Err(MoraError::Io(format!(
                "TLS (Schannel) handshake failed: 0x{:08x}",
                other as u32
            ))),
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
        self.run_handshake_call(incoming, outgoing)
    }

    fn encrypt(&mut self, plaintext: &[u8], outgoing: &mut Vec<u8>) -> MoraResult<()> {
        if !self.handshake_done {
            return Err(MoraError::Io(
                "TLS (Schannel): encrypt called before handshake completed".to_string(),
            ));
        }
        let a = api()?;
        let max_chunk = (self.stream_sizes.cb_maximum_message as usize).max(16384);
        let hdr_len = self.stream_sizes.cb_header as usize;
        let trl_len = self.stream_sizes.cb_trailer as usize;

        for chunk in plaintext.chunks(max_chunk) {
            let mut record = vec![0u8; hdr_len + chunk.len() + trl_len];
            record[hdr_len..hdr_len + chunk.len()].copy_from_slice(chunk);

            let mut bufs = [
                SecBuffer {
                    cb_buffer: hdr_len as u32,
                    buffer_type: SECBUFFER_STREAM_HEADER,
                    pv_buffer: record.as_mut_ptr() as *mut c_void,
                },
                SecBuffer {
                    cb_buffer: chunk.len() as u32,
                    buffer_type: SECBUFFER_DATA,
                    pv_buffer: record[hdr_len..].as_mut_ptr() as *mut c_void,
                },
                SecBuffer {
                    cb_buffer: trl_len as u32,
                    buffer_type: SECBUFFER_STREAM_TRAILER,
                    pv_buffer: record[hdr_len + chunk.len()..].as_mut_ptr() as *mut c_void,
                },
                SecBuffer::empty(),
            ];
            let mut desc = SecBufferDesc {
                ul_version: SECBUFFER_VERSION,
                c_buffers: 4,
                p_buffers: bufs.as_mut_ptr(),
            };
            let st = {
                // SAFETY: `self.ctx` is an established context and `bufs` point to disjoint regions of `record`.
                unsafe { (a.encrypt_message)(&mut self.ctx, 0, &mut desc, 0) }
            };
            if st != SEC_E_OK {
                return Err(MoraError::Io(format!(
                    "TLS (Schannel): EncryptMessage failed: 0x{:08x}",
                    st as u32
                )));
            }
            let total = (bufs[0].cb_buffer + bufs[1].cb_buffer + bufs[2].cb_buffer) as usize;
            outgoing.extend_from_slice(&record[..total]);
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
                "TLS (Schannel): decrypt called before handshake completed".to_string(),
            ));
        }
        let a = api()?;
        let mut produced_any = false;

        while !incoming.is_empty() {
            let mut bufs = [
                SecBuffer {
                    cb_buffer: incoming.len() as u32,
                    buffer_type: SECBUFFER_DATA,
                    pv_buffer: incoming.as_mut_ptr() as *mut c_void,
                },
                SecBuffer::empty(),
                SecBuffer::empty(),
                SecBuffer::empty(),
            ];
            let mut desc = SecBufferDesc {
                ul_version: SECBUFFER_VERSION,
                c_buffers: 4,
                p_buffers: bufs.as_mut_ptr(),
            };

            let st = {
                // SAFETY: `self.ctx` is established and `bufs[0]` references `incoming`'s live slice.
                unsafe { (a.decrypt_message)(&mut self.ctx, &mut desc, 0, ptr::null_mut()) }
            };

            if st == SEC_E_INCOMPLETE_MESSAGE {
                return if produced_any {
                    Ok(DecryptStatus::Ok)
                } else {
                    Ok(DecryptStatus::NeedMoreData)
                };
            }

            let mut data_span: Option<(usize, usize)> = None;
            let mut extra_len: usize = 0;
            let base_ptr = incoming.as_ptr() as usize;
            for b in &bufs {
                if b.buffer_type == SECBUFFER_DATA && b.cb_buffer > 0 && !b.pv_buffer.is_null() {
                    let off = (b.pv_buffer as usize).saturating_sub(base_ptr);
                    data_span = Some((off, b.cb_buffer as usize));
                } else if b.buffer_type == SECBUFFER_EXTRA {
                    extra_len = b.cb_buffer as usize;
                }
            }

            if let Some((off, len)) = data_span
                && off + len <= incoming.len()
            {
                plaintext_out.extend_from_slice(&incoming[off..off + len]);
                produced_any = true;
            }

            let consumed = incoming.len().saturating_sub(extra_len);
            incoming.drain(..consumed);

            match st {
                SEC_E_OK => {}
                SEC_I_CONTEXT_EXPIRED => {
                    return if produced_any {
                        Ok(DecryptStatus::Ok)
                    } else {
                        Ok(DecryptStatus::PeerClosed)
                    };
                }
                SEC_I_RENEGOTIATE => {
                    // TLS 1.3 post-handshake messages (e.g. NewSessionTicket) or TLS 1.2 renegotiation.
                    self.handshake_done = false;
                    let _ = self.run_handshake_call(incoming, outgoing_response)?;
                    if !self.handshake_done {
                        return Ok(DecryptStatus::NeedMoreData);
                    }
                }
                other => {
                    return Err(MoraError::Io(format!(
                        "TLS (Schannel): DecryptMessage failed: 0x{:08x}",
                        other as u32
                    )));
                }
            }
        }

        if produced_any {
            Ok(DecryptStatus::Ok)
        } else {
            Ok(DecryptStatus::NeedMoreData)
        }
    }

    fn shutdown_alert(&mut self, outgoing: &mut Vec<u8>) -> MoraResult<()> {
        if !self.ctx_initialized {
            return Ok(());
        }
        let a = api()?;
        let mut shutdown_token: u32 = SCHANNEL_SHUTDOWN;
        let mut ctrl_buf = SecBuffer {
            cb_buffer: std::mem::size_of::<u32>() as u32,
            buffer_type: SECBUFFER_TOKEN,
            pv_buffer: (&raw mut shutdown_token).cast::<c_void>(),
        };
        let mut ctrl_desc = SecBufferDesc {
            ul_version: SECBUFFER_VERSION,
            c_buffers: 1,
            p_buffers: &mut ctrl_buf,
        };
        // SAFETY: `self.ctx` is a valid security context and `ctrl_desc` points to `SCHANNEL_SHUTDOWN`.
        let st = unsafe { (a.apply_control_token)(&mut self.ctx, &mut ctrl_desc) };
        if st != SEC_E_OK {
            return Ok(());
        }

        let mut out_buf = SecBuffer {
            cb_buffer: 0,
            buffer_type: SECBUFFER_TOKEN,
            pv_buffer: ptr::null_mut(),
        };
        let mut out_desc = SecBufferDesc {
            ul_version: SECBUFFER_VERSION,
            c_buffers: 1,
            p_buffers: &mut out_buf,
        };
        let mut attrs: u32 = 0;
        let mut expiry: i64 = 0;

        let _ = if self.is_client {
            // SAFETY: Valid handles and buffer descriptor for emitting close_notify alert.
            unsafe {
                (a.initialize_security_context_w)(
                    self.cred.cred_ptr(),
                    &mut self.ctx,
                    ptr::null(),
                    ISC_REQ_ALLOCATE_MEMORY | ISC_REQ_STREAM,
                    0,
                    SECURITY_NATIVE_DREP,
                    ptr::null_mut(),
                    0,
                    &mut self.ctx,
                    &mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            }
        } else {
            // SAFETY: Valid handles and buffer descriptor for emitting close_notify alert.
            unsafe {
                (a.accept_security_context)(
                    self.cred.cred_ptr(),
                    &mut self.ctx,
                    ptr::null_mut(),
                    ASC_REQ_ALLOCATE_MEMORY | ASC_REQ_STREAM,
                    SECURITY_NATIVE_DREP,
                    &mut self.ctx,
                    &mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            }
        };

        if !out_buf.pv_buffer.is_null() {
            if out_buf.cb_buffer > 0 {
                // SAFETY: `out_buf.pv_buffer` holds `out_buf.cb_buffer` bytes allocated by Schannel.
                let slice = unsafe {
                    std::slice::from_raw_parts(
                        out_buf.pv_buffer as *const u8,
                        out_buf.cb_buffer as usize,
                    )
                };
                outgoing.extend_from_slice(slice);
            }
            // SAFETY: Buffer allocated by Schannel is freed once.
            unsafe {
                (a.free_context_buffer)(out_buf.pv_buffer);
            }
        }
        Ok(())
    }

    fn tls_version(&self) -> String {
        decode_protocol_name(self.conn_info.dw_protocol)
    }

    fn cipher_suite(&self) -> String {
        decode_cipher_suite(&self.conn_info)
    }

    fn alpn_protocol(&self) -> Option<String> {
        self.negotiated_alpn.clone()
    }
}
