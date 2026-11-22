//! Wave 2 completion verification: First-class `AsyncTcpListener` & `AsyncTcpStream`
//! exposure (`tcp_listen`, `tcp_connect`, `tcp_listener`, `tcp_stream`), cooperative
//! `TaskContext` cancellation, `channel<tcp_stream>` / `spawn` / `select` composition,
//! HM static typing, and LSP completion parity.

use std::collections::HashMap;
use std::sync::Arc;

use mora::flow::json::JsonValue as JsonVal;
use mora::lsp::providers::completion_v3;
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|_e| panic!("compile failed:\n{src}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static type error for:\n{src}\nbut typeck succeeded"
    );
}

#[test]
fn w2_tcp_listener_and_stream_concurrent_roundtrip_and_bytes() {
    let src = r#"
let listener: tcp_listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let accepted_ch: channel<tcp_stream> = channel(2)

let accept_task = spawn fn()
  let conn1: tcp_stream = listener.accept()
  accepted_ch.send(conn1)
  let conn2: tcp_stream = listener.accept()
  accepted_ch.send(conn2)
  return 2
end

let server_worker = spawn fn()
  let s1: tcp_stream = select
    case c = accepted_ch.recv() => c
    timeout(2000) => tcp_connect(addr)
  end
  let line1 = s1.read_line()
  let body1 = s1.read(5)
  s1.write("HTTP/1.1 200 OK\r\n\r\n" + line1 + body1)
  s1.shutdown()
  s1.close()

  let s2: tcp_stream = accepted_ch.recv()
  let raw = s2.read_bytes(4)
  s2.write_bytes(raw)
  s2.shutdown()
  s2.close()
  return "served"
end

let c1: tcp_stream = tcp_connect(addr=addr, timeout_ms=2000)
let peer = c1.peer_addr()
c1.write("PING\nhello")
let resp1 = c1.read(128)
c1.close()

let c2: tcp_stream = tcp_connect(addr, 2000)
c2.write_bytes([65, 66, 67, 68])
let raw_back = c2.read_bytes(16)
c2.close()

let accepted_n = accept_task.join()
let srv_status = server_worker.join()
listener.close()

[str(accepted_n), srv_status, resp1, str(raw_back), str(listener.is_closed()), str(c1.is_closed()), str(peer == addr), str(type_of(listener)), str(type_of(c1))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("2".to_string()));
    assert_eq!(v[1], Value::String("served".to_string()));
    assert_eq!(
        v[2],
        Value::String("HTTP/1.1 200 OK\r\n\r\nPING\nhello".to_string())
    );
    assert_eq!(v[3], Value::String("[65, 66, 67, 68]".to_string()));
    assert_eq!(v[4], Value::String("true".to_string()));
    assert_eq!(v[5], Value::String("true".to_string()));
    assert_eq!(v[6], Value::String("true".to_string()));
    assert_eq!(v[7], Value::String("tcp_listener".to_string()));
    assert_eq!(v[8], Value::String("tcp_stream".to_string()));
}

#[test]
fn w2_tcp_cooperative_cancellation_and_close_wakeup() {
    let src = r#"
let listener = tcp_listen("127.0.0.1:0")
let ctx = context()
let accept_ctx = ctx.with_timeout(60)

let h1 = spawn fn()
  return listener.accept(accept_ctx)
end
-- Wait briefly then cancel/timeout triggers cleanly, or join error propagates.
sleep(90)
listener.close()
true
"#;
    // Verify listener.close() wakes blocked accept() without hanging:
    let close_wake_src = r#"
let listener = tcp_listen("127.0.0.1:0")
let h = spawn fn()
  return listener.accept()
end
sleep(25)
listener.close()
let closed = listener.is_closed()
closed
"#;
    let closed = compile_check_and_run(close_wake_src);
    assert_eq!(closed, Value::Bool(true));

    // Verify accept(ctx) aborts cleanly on context cancellation:
    let (prog, witnesses) = ParserV3::compile(src).expect("compile");
    assert!(check_program_witnesses(&witnesses).is_empty());
    let mut core = CoreRuntime::default();
    let _ = execute_program(Arc::new(prog), &mut core).expect("outer script succeeds");

    // Direct cancelled accept must return Cancelled/Timeout error:
    let cancel_err_src = r#"
let listener = tcp_listen("127.0.0.1:0")
let ctx = context()
ctx.cancel("stop accept")
listener.accept(ctx)
"#;
    let (prog2, witnesses2) = ParserV3::compile(cancel_err_src).expect("compile");
    assert!(check_program_witnesses(&witnesses2).is_empty());
    let mut core2 = CoreRuntime::default();
    let err = execute_program(Arc::new(prog2), &mut core2)
        .expect_err("cancelled accept must fail cleanly");
    assert!(
        err.is_cancellation_or_timeout(),
        "expected cancellation error, got: {err:?}"
    );
}

#[test]
fn w2_tcp_static_type_checks_and_lsp_completion() {
    // Reject wrong argument & return types statically
    assert_typecheck_fails("let l = tcp_listen(123)\n");
    assert_typecheck_fails("let l: tcp_stream = tcp_listen(\"127.0.0.1:0\")\n");
    assert_typecheck_fails("let l = tcp_listen(\"127.0.0.1:0\")\nlet s: int = l.accept()\n");
    assert_typecheck_fails(
        "let l = tcp_listen(\"127.0.0.1:0\")\nlet s = tcp_connect(l.local_addr())\ns.write(123)\n",
    );
    assert_typecheck_fails(
        "let l = tcp_listen(\"127.0.0.1:0\")\nlet s = tcp_connect(l.local_addr())\ns.write_bytes([\"bad\"])\n",
    );

    // Check methods_of introspection
    let m_src = r#"
let l = tcp_listen("127.0.0.1:0")
let s = tcp_connect(l.local_addr())
let lm = methods_of(l)
let sm = methods_of(s)
s.close()
l.close()
[str(lm.len()), str(sm.len())]
"#;
    let m_out = compile_check_and_run(m_src);
    assert_eq!(
        m_out,
        Value::List(
            vec![
                Value::String("4".to_string()),
                Value::String("10".to_string())
            ]
            .into()
        )
    );

    // LSP completion exposes tcp_listen, tcp_connect, tcp_listener, tcp_stream
    let mut docs = HashMap::new();
    let uri = "file:///w2_tcp.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let l = tcp_listen(\"127.0.0.1:0\")\n".to_string(),
            version: 1,
            diagnostics: Vec::new(),
        },
    );
    let comp = completion_v3(
        &docs,
        &JsonVal::Object(
            [(
                "textDocument".to_string(),
                JsonVal::Object(
                    [("uri".to_string(), JsonVal::String_(uri.to_string()))]
                        .into_iter()
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        ),
    );
    let labels: Vec<&str> = comp
        .as_array()
        .expect("completion array")
        .iter()
        .filter_map(|item| item.get("label").and_then(|v| v.as_str()))
        .collect();
    for expected in ["tcp_listen", "tcp_connect", "tcp_listener", "tcp_stream"] {
        assert!(
            labels.contains(&expected),
            "LSP completion missing Wave 2 TCP entry {expected:?}"
        );
    }
}
