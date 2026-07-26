//! Mora LSP
//!
//!  binary  LSP
//!   1.  mora-lsp
//!   2.  initialize
//!   3.  initialized
//!   4.  textDocument/didOpen  typeck
//!   5.  publishDiagnostics
//!   6.  shutdown + exit
//!
//!  cargo run --example lsp_smoke  mora-lsp

use std::io::{BufReader, Read, Write};
use std::process::{Command, Stdio};

fn main() {
    let exe = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "mora-lsp".to_string());
    println!("[client] launching {}", exe);

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mora-lsp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
    write_msg(&mut stdin, init).expect("write initialize");
    let resp = read_msg(&mut reader);
    println!("[client] initialize response: {} bytes", resp.len());
    assert!(
        resp.contains("capabilities"),
        "expected capabilities in response"
    );

    // 2. initialized notification
    write_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
    )
    .expect("write initialized");

    // 3. didOpen with intentionally bad code
    let bad_code = "let x: number = \"hello\"\n";
    let did_open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///tmp/test.mora","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(bad_code)
    );
    write_msg(&mut stdin, &did_open).expect("write didOpen");

    // 4. read publishDiagnostics
    let diag = read_msg(&mut reader);
    println!("[client] diagnostics: {}", diag);
    assert!(
        diag.contains("publishDiagnostics"),
        "expected publishDiagnostics"
    );
    assert!(
        diag.contains("Type mismatch") || diag.contains("type mismatch"),
        "expected type mismatch diagnostic"
    );

    // 5. hover at line 0, char 4 (x identifier)
    let hover = r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///tmp/test.mora"},"position":{"line":0,"character":4}}}"#;
    write_msg(&mut stdin, hover).expect("write hover");
    let hover_resp = read_msg(&mut reader);
    println!("[client] hover: {} bytes", hover_resp.len());
    assert!(hover_resp.contains("\"id\":2"), "expected hover response");

    // 6. completion
    let comp = r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///tmp/test.mora"},"position":{"line":0,"character":0}}}"#;
    write_msg(&mut stdin, comp).expect("write completion");
    let comp_resp = read_msg(&mut reader);
    println!("[client] completion: {} bytes", comp_resp.len());
    assert!(
        comp_resp.contains("\"id\":3"),
        "expected completion response"
    );

    // 7. shutdown + exit
    let _ = write_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#,
    );
    let _ = read_msg(&mut reader);
    let _ = write_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"exit","params":null}"#,
    );

    drop(stdin);
    let _ = child.wait();

    println!("[client] ALL E2E CHECKS PASSED");
}

fn write_msg<W: Write>(w: &mut W, body: &str) -> std::io::Result<()> {
    let bytes = body.as_bytes();
    write!(w, "Content-Length: {}\r\n\r\n", bytes.len())?;
    w.write_all(bytes)?;
    w.flush()
}

fn read_msg<R: Read>(r: &mut R) -> String {
    //  header \r\n
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    let mut byte = [0u8; 1];

    loop {
        line.clear();
        loop {
            if r.read(&mut byte).unwrap() == 0 {
                return String::new();
            }
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                line.push(byte[0] as char);
            }
        }
        if line.is_empty() {
            //  = header
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("Content-Length")
        {
            content_length = value.trim().parse().ok();
        }
    }
    let len = content_length.unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

fn json_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}
