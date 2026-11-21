//! Pure-Rust RFC 7468 PEM + Base64 + ASN.1 PKCS#1/SEC1 → PKCS#8 DER decoder.
//!
//! Zero external dependencies. Used by both Windows Schannel (which consumes DER
//! certificates and PKCS#8 private key blobs via CNG `NCryptImportKey`) and
//! Unix OpenSSL/LibreSSL (which validates PEM structure upfront before passing
//! to in-memory `BIO_new_mem_buf`).

use crate::error::{MoraError, MoraResult};

/// A decoded PEM block (`-----BEGIN <label>-----` .. `-----END <label>-----`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PemBlock {
    pub label: String,
    pub der: Vec<u8>,
}

/// Decode standard Base64 (RFC 4648 §4), ignoring ASCII whitespace (`\r`, `\n`, `' '`, `\t`).
pub(crate) fn decode_base64(input: &str) -> MoraResult<Vec<u8>> {
    let mut vals: Vec<u8> = Vec::with_capacity(input.len());
    let mut pad_count = 0usize;
    let mut seen_pad = false;

    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            seen_pad = true;
            pad_count += 1;
            if pad_count > 2 {
                return Err(MoraError::Io("TLS PEM: invalid base64 padding".to_string()));
            }
            continue;
        }
        if seen_pad {
            return Err(MoraError::Io(
                "TLS PEM: unexpected character after base64 padding".to_string(),
            ));
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => {
                return Err(MoraError::Io(format!(
                    "TLS PEM: invalid base64 byte 0x{b:02x}"
                )));
            }
        };
        vals.push(v);
    }

    let total_chars = vals.len() + pad_count;
    if !total_chars.is_multiple_of(4) || (vals.len() % 4 == 1) {
        return Err(MoraError::Io(
            "TLS PEM: invalid base64 payload length".to_string(),
        ));
    }

    let mut out = Vec::with_capacity((vals.len() * 3) / 4);
    let mut i = 0usize;
    while i + 4 <= vals.len() {
        let n = ((vals[i] as u32) << 18)
            | ((vals[i + 1] as u32) << 12)
            | ((vals[i + 2] as u32) << 6)
            | (vals[i + 3] as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        out.push(((n >> 8) & 0xFF) as u8);
        out.push((n & 0xFF) as u8);
        i += 4;
    }

    let rem = vals.len() - i;
    if rem == 2 {
        let n = ((vals[i] as u32) << 18) | ((vals[i + 1] as u32) << 12);
        out.push(((n >> 16) & 0xFF) as u8);
    } else if rem == 3 {
        let n =
            ((vals[i] as u32) << 18) | ((vals[i + 1] as u32) << 12) | ((vals[i + 2] as u32) << 6);
        out.push(((n >> 16) & 0xFF) as u8);
        out.push(((n >> 8) & 0xFF) as u8);
    }

    Ok(out)
}

/// Extract all RFC 7468 PEM blocks from `pem`.
pub(crate) fn parse_pem_blocks(pem: &str) -> MoraResult<Vec<PemBlock>> {
    let mut blocks = Vec::new();
    let mut current_label: Option<String> = None;
    let mut body = String::new();

    for raw_line in pem.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("-----BEGIN ")
            && let Some(label) = rest.strip_suffix("-----")
        {
            if current_label.is_some() {
                return Err(MoraError::Io(
                    "TLS PEM: nested BEGIN marker before END".to_string(),
                ));
            }
            current_label = Some(label.trim().to_string());
            body.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("-----END ")
            && let Some(label) = rest.strip_suffix("-----")
        {
            let expected = current_label.take().ok_or_else(|| {
                MoraError::Io("TLS PEM: unexpected END marker without matching BEGIN".to_string())
            })?;
            let end_label = label.trim();
            if expected != end_label {
                return Err(MoraError::Io(format!(
                    "TLS PEM: mismatched markers BEGIN '{expected}' / END '{end_label}'"
                )));
            }
            let der = decode_base64(&body)?;
            blocks.push(PemBlock {
                label: expected,
                der,
            });
            body.clear();
            continue;
        }
        if current_label.is_some() {
            body.push_str(line);
        }
    }

    if let Some(unclosed) = current_label {
        return Err(MoraError::Io(format!(
            "TLS PEM: unterminated block '-----BEGIN {unclosed}-----'"
        )));
    }
    Ok(blocks)
}

/// Parse one or more X.509 `CERTIFICATE` DER blobs from a PEM bundle.
pub(crate) fn parse_certificates_der(pem: &str) -> MoraResult<Vec<Vec<u8>>> {
    let blocks = parse_pem_blocks(pem)?;
    let certs: Vec<Vec<u8>> = blocks
        .into_iter()
        .filter(|b| b.label == "CERTIFICATE" || b.label == "X509 CERTIFICATE")
        .map(|b| b.der)
        .collect();
    if certs.is_empty() {
        return Err(MoraError::Io(
            "TLS PEM: no CERTIFICATE blocks found in certificate PEM".to_string(),
        ));
    }
    Ok(certs)
}

fn der_encode_tlv(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 5 + payload.len());
    out.push(tag);
    let len = payload.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else if len <= 0xFFFF {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push((len & 0xFF) as u8);
    } else {
        out.push(0x83);
        out.push((len >> 16) as u8);
        out.push(((len >> 8) & 0xFF) as u8);
        out.push((len & 0xFF) as u8);
    }
    out.extend_from_slice(payload);
    out
}

/// Wrap raw PKCS#1 RSA private key DER (`-----BEGIN RSA PRIVATE KEY-----`) into
/// RFC 5208 unencrypted PKCS#8 `PrivateKeyInfo` DER so Windows CNG `NCRYPT_PKCS8_PRIVATE_KEY_BLOB`
/// accepts it directly alongside native PKCS#8 keys.
fn wrap_pkcs1_rsa_in_pkcs8(pkcs1_der: &[u8]) -> Vec<u8> {
    // version INTEGER 0: 02 01 00
    // AlgorithmIdentifier: SEQUENCE { OID rsaEncryption (1.2.840.113549.1.1.1), NULL }
    const VERSION_0: &[u8] = &[0x02, 0x01, 0x00];
    const RSA_ALG_ID: &[u8] = &[
        0x30, 0x0D, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let octet_key = der_encode_tlv(0x04, pkcs1_der);
    let mut seq = Vec::with_capacity(VERSION_0.len() + RSA_ALG_ID.len() + octet_key.len());
    seq.extend_from_slice(VERSION_0);
    seq.extend_from_slice(RSA_ALG_ID);
    seq.extend_from_slice(&octet_key);
    der_encode_tlv(0x30, &seq)
}

/// Wrap SEC1 EC private key DER (`-----BEGIN EC PRIVATE KEY-----`) into RFC 5208 PKCS#8
/// `PrivateKeyInfo` DER for standard NIST curves (P-256, P-384, P-521).
fn wrap_sec1_ec_in_pkcs8(sec1_der: &[u8]) -> MoraResult<Vec<u8>> {
    const OID_P256: &[u8] = &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
    const OID_P384: &[u8] = &[0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x22];
    const OID_P521: &[u8] = &[0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x23];
    const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];

    let curve_oid = if sec1_der.windows(OID_P256.len()).any(|w| w == OID_P256) {
        OID_P256
    } else if sec1_der.windows(OID_P384.len()).any(|w| w == OID_P384) {
        OID_P384
    } else if sec1_der.windows(OID_P521.len()).any(|w| w == OID_P521) {
        OID_P521
    } else {
        return Err(MoraError::Io(
            "TLS PEM: EC PRIVATE KEY curve OID not recognized (supported: P-256, P-384, P-521; or supply PKCS#8 PRIVATE KEY)".to_string(),
        ));
    };

    let mut alg_inner = Vec::with_capacity(OID_EC_PUBLIC_KEY.len() + curve_oid.len());
    alg_inner.extend_from_slice(OID_EC_PUBLIC_KEY);
    alg_inner.extend_from_slice(curve_oid);
    let alg_id = der_encode_tlv(0x30, &alg_inner);
    let octet_key = der_encode_tlv(0x04, sec1_der);

    let mut seq = Vec::with_capacity(3 + alg_id.len() + octet_key.len());
    seq.extend_from_slice(&[0x02, 0x01, 0x00]);
    seq.extend_from_slice(&alg_id);
    seq.extend_from_slice(&octet_key);
    Ok(der_encode_tlv(0x30, &seq))
}

/// Extract an unencrypted private key from PEM and normalize it into RFC 5208 PKCS#8 DER.
/// Supports `PRIVATE KEY` (PKCS#8), `RSA PRIVATE KEY` (PKCS#1), and `EC PRIVATE KEY` (SEC1).
pub(crate) fn parse_private_key_pkcs8_der(pem: &str) -> MoraResult<Vec<u8>> {
    let blocks = parse_pem_blocks(pem)?;
    for block in blocks {
        match block.label.as_str() {
            "PRIVATE KEY" => return Ok(block.der),
            "RSA PRIVATE KEY" => return Ok(wrap_pkcs1_rsa_in_pkcs8(&block.der)),
            "EC PRIVATE KEY" => return wrap_sec1_ec_in_pkcs8(&block.der),
            "ENCRYPTED PRIVATE KEY" => {
                return Err(MoraError::Io(
                    "TLS PEM: passphrase-encrypted private keys are not supported; supply unencrypted PKCS#8/PKCS#1/SEC1 PEM".to_string(),
                ));
            }
            _ => {}
        }
    }
    Err(MoraError::Io(
        "TLS PEM: no PRIVATE KEY / RSA PRIVATE KEY / EC PRIVATE KEY block found".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_pem_roundtrip() {
        assert_eq!(decode_base64("SGVsbG8=").expect("b64"), b"Hello");
        assert_eq!(
            decode_base64("SGVs\r\nbG8g\nV29ybGQ=").expect("b64"),
            b"Hello World"
        );
        assert_eq!(decode_base64("QQ==").expect("b64"), b"A");
        assert!(decode_base64("!!!").is_err());

        let pem = "-----BEGIN CERTIFICATE-----\nSGVsbG8=\n-----END CERTIFICATE-----\n";
        let certs = parse_certificates_der(pem).expect("certs");
        assert_eq!(certs, vec![b"Hello".to_vec()]);
    }

    #[test]
    fn pkcs1_wraps_into_valid_pkcs8_header() {
        let pkcs1 = vec![0x30, 0x03, 0x02, 0x01, 0x00];
        let pkcs8 = wrap_pkcs1_rsa_in_pkcs8(&pkcs1);
        assert_eq!(pkcs8[0], 0x30); // SEQUENCE
        // Contains rsaEncryption OID 1.2.840.113549.1.1.1
        assert!(
            pkcs8
                .windows(9)
                .any(|w| w == [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01])
        );
        assert!(pkcs8.ends_with(&pkcs1));
    }
}
