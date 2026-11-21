//! LSP 传输层：手写 JSON-RPC 2.0 over stdin/stdout
//!
//! 协议：每个 message 由 HTTP 风格 header + body 组成
//!   `Content-Length: <N>\r\n`
//!   `\r\n`
//!   `<N bytes of UTF-8 JSON>`
//!
//! 这里采用纯同步单线程 stdio 消息循环（read message → dispatch → write response），并强制校验 `MAX_CONTENT_LENGTH` 防 OOM。
//! 对于编辑器这种场景吞吐够用；如果以后真要性能，再上 async。

use std::io::{self, Read, Write};

/// 单条 LSP 消息 body 的硬上限（16 MiB），防止恶意或损坏的 `Content-Length` 触发 OOM。
pub const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024;
/// 单条 LSP 消息允许的最大 header 行数。
pub const MAX_HEADER_LINES: usize = 64;
/// 单行 LSP header 的最大字节数（8 KiB）。
pub const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;

/// 读一条 LSP 消息（流式读受限 header 行，校验 `Content-Length` 后再分配 body）。
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<Option<String>> {
    let mut header_lines: Vec<String> = Vec::new();
    let mut byte_buf = [0u8; 1];

    // 读 header（每个 header 一行 CRLF/LF；空行结束）
    loop {
        let mut raw_line: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut byte_buf) {
                Ok(0) => {
                    if header_lines.is_empty() && raw_line.is_empty() {
                        return Ok(None); // 流在消息边界 cleanly EOF
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected EOF while reading LSP header",
                    ));
                }
                Ok(_) => {
                    if byte_buf[0] == b'\n' {
                        break;
                    }
                    if raw_line.len() >= MAX_HEADER_LINE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "LSP header line exceeds maximum of {} bytes",
                                MAX_HEADER_LINE_BYTES
                            ),
                        ));
                    }
                    raw_line.push(byte_buf[0]);
                }
                Err(e) => return Err(e),
            }
        }
        if raw_line.last() == Some(&b'\r') {
            raw_line.pop();
        }
        let line = String::from_utf8(raw_line).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid UTF-8 in LSP header: {e}"),
            )
        })?;
        if line.is_empty() {
            break;
        }
        if header_lines.len() >= MAX_HEADER_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("LSP header count exceeds maximum of {}", MAX_HEADER_LINES),
            ));
        }
        header_lines.push(line);
    }

    // 解析并校验 Content-Length（禁止冲突值、非法数值与超大分配）
    let mut content_length: Option<usize> = None;
    for line in &header_lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("Content-Length")
        {
            let parsed = value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length header")
            })?;
            if parsed > MAX_CONTENT_LENGTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Content-Length {} exceeds maximum allowed {} bytes",
                        parsed, MAX_CONTENT_LENGTH
                    ),
                ));
            }
            if let Some(prev) = content_length
                && prev != parsed
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conflicting Content-Length headers",
                ));
            }
            content_length = Some(parsed);
        }
    }
    let len = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;

    // 读 body
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let s = String::from_utf8(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(s))
}

/// 写一条 LSP 消息
pub fn write_message<W: Write>(writer: &mut W, body: &str) -> io::Result<()> {
    let bytes = body.as_bytes();
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_message() {
        let original = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let mut buf = Vec::new();
        write_message(&mut buf, original).unwrap();

        let mut cursor = Cursor::new(buf);
        let received = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(received, original);
    }

    #[test]
    fn read_message_with_garbage_before() {
        // 真实 LSP client 可能发奇怪的 byte
        let mut payload = b"Content-Length: 5\r\n\r\nhello".to_vec();
        let mut cursor = Cursor::new(&mut payload[..]);
        let s = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn rejects_oversized_content_length_without_allocating() {
        let payload = format!("Content-Length: {}\r\n\r\n", MAX_CONTENT_LENGTH + 1);
        let mut cursor = Cursor::new(payload.as_bytes());
        let err = read_message(&mut cursor).expect_err("oversized Content-Length must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn rejects_conflicting_content_length_and_header_floods() {
        let conflicting = b"Content-Length: 2\r\nContent-Length: 5\r\n\r\n{}";
        let mut c = Cursor::new(&conflicting[..]);
        assert!(read_message(&mut c).is_err());

        let mut flood = Vec::new();
        for i in 0..=MAX_HEADER_LINES {
            flood.extend_from_slice(format!("X-Pad-{i}: v\r\n").as_bytes());
        }
        flood.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        let mut c2 = Cursor::new(&flood[..]);
        assert!(read_message(&mut c2).is_err());

        let long_line = format!(
            "X-Long: {}\r\nContent-Length: 2\r\n\r\n{{}}",
            "a".repeat(MAX_HEADER_LINE_BYTES + 1)
        );
        let mut c3 = Cursor::new(long_line.as_bytes());
        assert!(read_message(&mut c3).is_err());
    }
}
