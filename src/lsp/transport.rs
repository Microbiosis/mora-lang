//! LSP 传输层：手写 JSON-RPC 2.0 over stdin/stdout
//!
//! 协议：每个 message 由 HTTP 风格 header + body 组成
//!   Content-Length: <N>\r\n
//!   \r\n
//!   <N bytes of UTF-8 JSON>
//!
//! 这里纯同步、单线程，不引入 tokio。LSP server 整体是"循环 read message, dispatch, write response"。
//! 对于编辑器这种场景吞吐够用；如果以后真要性能，再上 async。

use std::io::{self, Read, Write};

/// 读一条 LSP 消息（从 BufRead::lines() 状态机读 header + body）
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<Option<String>> {
    let mut header_lines: Vec<String> = Vec::new();
    let mut byte_buf = [0u8; 1];

    // 读 header（每个 header 一行 CRLF；空行 CRLF 结束）
    loop {
        let mut line = String::new();
        // 读一个 byte 序列直到 \n
        loop {
            match reader.read(&mut byte_buf) {
                Ok(0) => return Ok(None),  // EOF
                Ok(_) => {
                    if byte_buf[0] == b'\n' {
                        break;
                    }
                    if byte_buf[0] != b'\r' {
                        line.push(byte_buf[0] as char);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if line.is_empty() {
            // 空行 = header 结束
            break;
        }
        header_lines.push(line);
    }

    // 解析 Content-Length
    let mut content_length: Option<usize> = None;
    for line in &header_lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let len = match content_length {
        Some(n) => n,
        None => return Err(io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length")),
    };

    // 读 body
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let s = String::from_utf8(body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
}
