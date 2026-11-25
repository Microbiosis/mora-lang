//! P3 / v0.110.1 Integration Tests: Zero-Dependency Native Async TLS 1.2/1.3 & HTTPS
//!
//! Covers:
//! 1. Native OS TLS 1.2/1.3 transport (`tls_listen`, `tls_connect`, `tls_listener`, `tls_stream`,
//!    `std/net/tls`) with both PKCS#8 (`PRIVATE KEY`) and PKCS#1 (`RSA PRIVATE KEY`) keys.
//! 2. Strict X.509 certificate chain & hostname verification (trusted custom CA passes for both
//!    DNS `localhost` and IP `127.0.0.1`; untrusted CA rejected; wrong SNI rejected; explicit
//!    `insecure_skip_verify: true` opt-in passes).
//! 3. ALPN negotiation (`http/1.1`), multi-record (>32 KiB) encrypted framing, `.close()` wakeup
//!    of blocked `accept()`, static HM type enforcement (including `tcp_stream | tls_stream` union
//!    method checking), and LSP completions.
//! 4. Full pure-Mora HTTPS (`std/net/http`) over verified TLS: Router params/wildcard/middleware/405,
//!    Chunked Transfer-Encoding, HTTP/1.1 Keep-Alive connection reuse, 413 cap, and live SSE streaming.
//! 5. Full pure-Mora MCP-over-HTTPS (`std/mcp` `serve_https` + `call_https`) with `Struct::schema()`.

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
    let errors = check_program_witnesses(&witnesses);
    assert!(
        errors.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errors:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn compile_check_expect_runtime_err(src: &str) -> String {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errors = check_program_witnesses(&witnesses);
    assert!(
        errors.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errors:?}"
    );
    let mut core = CoreRuntime::default();
    match execute_program(Arc::new(prog), &mut core) {
        Ok((v, _)) => panic!("expected runtime error, got Ok({v:?}) for:\n{src}"),
        Err(e) => e.to_string(),
    }
}

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errors = check_program_witnesses(&witnesses);
    assert!(
        !errors.is_empty(),
        "expected static HM type error for:\n{src}"
    );
}

#[test]
fn p3_tls_native_handshake_alpn_pkcs1_strict_verification_and_lsp() {
    // 1. Live loopback TLS 1.2/1.3: PKCS#8 + PKCS#1 keys, DNS & IP verification, ALPN,
    //    multi-record (>32 KiB) encrypted roundtrip, binary read_bytes/write_bytes, and close wakeup.
    let src = r#"
import "std/net/tls"

let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pkcs8 = file.read_text("test_data/tls/test_server_pkcs8.key")
let key_pkcs1 = file.read_text("test_data/tls/test_server_pkcs1.key")
let ca_pem = file.read_text("test_data/tls/test_ca.pem")

-- A. PKCS#8 server + verified client ("localhost") + ALPN + large multi-record payload
let listener = listen("127.0.0.1:0", cert_pem, key_pkcs8, ["http/1.1"])
let addr = listener.local_addr()

let srv_task = spawn fn()
  let conn = listener.accept()
  let info = conn_info(conn)
  let line = conn.read_line()
  conn.write("ECHO:" + line)
  -- Read 40 KiB payload spanning multiple TLS 16 KiB records
  let big = ""
  while big.len() < 40960
    let chunk = conn.read(16384)
    if chunk == "" then break end
    assign big = big + chunk
  end
  conn.write(str(big.len()) + "\n")
  -- Binary byte frame test
  let raw_b = conn.read_bytes(4)
  conn.write_bytes(raw_b)
  conn.close()
  return info.tls_version + "|" + info.alpn_protocol
end

let cli = connect(addr, "localhost", ca_pem, ["http/1.1"], false, 5000)
let cli_info = conn_info(cli)
cli.write("hello-mora-tls\n")
let reply1 = cli.read_line().trim()

let block = "0123456789abcdef0123456789abcdef"
let big_payload = ""
let i = 0
while i < 1280
  assign big_payload = big_payload + block
  assign i = i + 1
end
cli.write(big_payload)
let reply_len = cli.read_line().trim()

cli.write_bytes([1, 2, 254, 255])
let echo_bytes = cli.read_bytes(4)
cli.close()

let srv_summary = srv_task.join()
listener.close()

-- B. PKCS#1 (BEGIN RSA PRIVATE KEY) server + verified IP ("127.0.0.1") + explicit insecure opt-in
let l2 = listen("127.0.0.1:0", cert_pem, key_pkcs1, ["http/1.1"])
let addr2 = l2.local_addr()
let s2 = spawn fn()
  let c1 = l2.accept()
  let m1 = c1.read_line().trim()
  c1.write("P1:" + m1 + "\n")
  c1.close()
  let c2 = l2.accept()
  let m2 = c2.read_line().trim()
  c2.write("INSEC:" + m2 + "\n")
  c2.close()
  return true
end

let c_ip = connect(addr2, "127.0.0.1", ca_pem, ["http/1.1"], false, 5000)
c_ip.write("ip-ok\n")
let r_ip = c_ip.read_line().trim()
c_ip.close()

let c_insec = connect(addr2, "wrong.example.com", "", ["http/1.1"], true, 5000)
c_insec.write("skip-ok\n")
let r_insec = c_insec.read_line().trim()
c_insec.close()

let s2_ok = s2.join()
l2.close()

let proto_ok = cli_info.tls_version == "TLSv1.2" or cli_info.tls_version == "TLSv1.3"
let cipher_ok = cli_info.cipher_suite != ""
let alpn_ok = cli_info.alpn_protocol == "http/1.1"
let bytes_ok = echo_bytes == [1, 2, 254, 255]

[str(proto_ok), str(cipher_ok), str(alpn_ok), reply1, reply_len, str(bytes_ok), str(srv_summary), r_ip, r_insec, str(s2_ok), str(listener.is_closed())]
"#;

    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("true".to_string()), "proto_ok: {v:?}");
    assert_eq!(v[1], Value::String("true".to_string()), "cipher_ok: {v:?}");
    assert_eq!(v[2], Value::String("true".to_string()), "alpn_ok: {v:?}");
    assert_eq!(v[3], Value::String("ECHO:hello-mora-tls".to_string()));
    assert_eq!(v[4], Value::String("40960".to_string()));
    assert_eq!(v[5], Value::String("true".to_string()), "bytes_ok: {v:?}");
    let Value::String(ref srv_sum) = v[6] else {
        panic!("expected string srv_summary");
    };
    assert!(
        srv_sum.starts_with("TLSv1.2|http/1.1") || srv_sum.starts_with("TLSv1.3|http/1.1"),
        "unexpected server summary: {srv_sum}"
    );
    assert_eq!(v[7], Value::String("P1:ip-ok".to_string()));
    assert_eq!(v[8], Value::String("INSEC:skip-ok".to_string()));
    assert_eq!(v[9], Value::String("true".to_string()));
    assert_eq!(v[10], Value::String("true".to_string()));

    // 2. Strict security test: untrusted CA MUST be rejected during handshake!
    let untrusted_err = compile_check_expect_runtime_err(
        r#"
import "std/net/tls"
let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pem = file.read_text("test_data/tls/test_server_pkcs8.key")
let bad_ca = file.read_text("test_data/tls/test_untrusted_ca.pem")
let l = listen("127.0.0.1:0", cert_pem, key_pem)
let addr = l.local_addr()
let _srv = spawn fn()
  let c = l.accept()
  c.close()
  return nil
end
let s = connect(addr, "localhost", bad_ca, ["http/1.1"], false, 3000)
s.close()
l.close()
"#,
    );
    assert!(
        untrusted_err.to_lowercase().contains("verification")
            || untrusted_err.to_lowercase().contains("certificate")
            || untrusted_err.to_lowercase().contains("untrusted")
            || untrusted_err.to_lowercase().contains("handshake"),
        "expected certificate verification failure, got: {untrusted_err}"
    );

    // 3. Strict security test: wrong SNI hostname MUST be rejected even with trusted CA!
    let wrong_sni_err = compile_check_expect_runtime_err(
        r#"
import "std/net/tls"
let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pem = file.read_text("test_data/tls/test_server_pkcs8.key")
let ca_pem = file.read_text("test_data/tls/test_ca.pem")
let l = listen("127.0.0.1:0", cert_pem, key_pem)
let addr = l.local_addr()
let _srv = spawn fn()
  let c = l.accept()
  c.close()
  return nil
end
let s = connect(addr, "evil.example.com", ca_pem, ["http/1.1"], false, 3000)
s.close()
l.close()
"#,
    );
    assert!(
        wrong_sni_err.to_lowercase().contains("mismatch")
            || wrong_sni_err.to_lowercase().contains("verification")
            || wrong_sni_err.to_lowercase().contains("certificate")
            || wrong_sni_err.to_lowercase().contains("handshake"),
        "expected hostname verification failure, got: {wrong_sni_err}"
    );

    // 3b. Cooperative cancellation test: context timeout immediately aborts blocked tls_listener.accept(ctx)
    let cancel_err = compile_check_expect_runtime_err(
        r#"
import "std/net/tls"
let l = listen_files("127.0.0.1:0", "test_data/tls/test_server.pem", "test_data/tls/test_server_pkcs8.key")
let ctx = context().with_timeout(40)
let _c = l.accept(ctx)
l.close()
"#,
    );
    assert!(
        cancel_err.to_lowercase().contains("timeout")
            || cancel_err.to_lowercase().contains("cancel")
            || cancel_err.to_lowercase().contains("deadline"),
        "expected cancellation/timeout error on blocked tls accept, got: {cancel_err}"
    );

    // 4. Static HM type checking & union-stream method enforcement
    assert_typecheck_fails(
        r#"
let bad: string = tls_listen("127.0.0.1:0", {})
"#,
    );
    assert_typecheck_fails(
        r#"
let bad: string = tls_connect("127.0.0.1:443")
"#,
    );
    assert_typecheck_fails(
        r#"
task bad_union_method(s: tcp_stream | tls_stream): string
  return s.tls_version()
end
"#,
    );
    assert_typecheck_fails(
        r#"
import "std/net/http"
let _ = read_request("not-a-stream")
"#,
    );

    // 5. LSP completion exposes `tls_listener` and `tls_stream`
    let mut docs = HashMap::new();
    let uri = "file:///p3_tls.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "import \"std/net/tls\"\n".to_string(),
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
    assert!(labels.contains(&"tls_listener"));
    assert!(labels.contains(&"tls_stream"));
}

#[test]
fn p3_std_net_http_over_tls_router_chunked_keepalive_and_sse() {
    let src = r#"
import "std/net/http"
import "std/net/tls"

let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pem = file.read_text("test_data/tls/test_server_pkcs8.key")
let ca_pem = file.read_text("test_data/tls/test_ca.pem")

let r = router()
assign r = r.use_middleware(fn(req: HttpRequest, stream: tcp_stream | tls_stream, next: any)
  let res = next(req, stream)
  if is_instance(res, "HttpResponse") then
    return HttpResponse(status=res.status, headers=res.headers.set("X-TLS-MW", "verified"), body=res.body)
  end
  return res
end)
assign r = r.get("/models/:id", fn(req: HttpRequest, _s: tcp_stream | tls_stream)
  return json_response(200, {"id": req.params.get("id"), "q": req.query})
end)
assign r = r.get("/assets/*", fn(req: HttpRequest, _s: tcp_stream | tls_stream)
  return response(200, "asset:" + str(req.params.get("*")))
end)
assign r = r.post("/chunked", fn(req: HttpRequest, _s: tcp_stream | tls_stream)
  return chunked_response(200, ["tls:", req.body, ":ok"])
end)
assign r = r.get("/events", fn(_req: HttpRequest, stream: tcp_stream | tls_stream)
  sse_start(stream)
  sse_send(stream, "token", "hello", "1")
  sse_send(stream, "token", "secure-world", "2")
  return nil
end)

let listener = listen("127.0.0.1:0", cert_pem, key_pem, ["http/1.1"])
let addr = listener.local_addr()

-- Serve 5 TLS connections:
-- 1) Keep-Alive reuse (2 requests on 1 tls_stream: /models/qwen3?mode=fp8 + /assets/v1/w.gguf)
-- 2) 405 Method Not Allowed on DELETE /models/qwen3
-- 3) Chunked POST + Chunked response
-- 4) 413 Payload Too Large (max_body_bytes = 32)
-- 5) Live HTTPS SSE stream
let srv = spawn fn()
  return r.serve_listener(listener, 5, nil, 32, 10)
end

let tls_cfg: dict<string, any> = {}
assign tls_cfg = tls_cfg.set("ca_pem", ca_pem).set("alpn", ["http/1.1"])

-- 1. Reused Keep-Alive HTTPS connection
let ka = tls_connect(addr, "localhost", tls_cfg, 5000)
let r1 = send_request_on(ka, "GET", "localhost", "/models/qwen3?mode=fp8", "", "text/plain", true)
let r2 = send_request_on(ka, "GET", "localhost", "/assets/v1/w.gguf", "", "text/plain", false)
ka.close()

-- 2. 405 Method Not Allowed over HTTPS
let r405 = https_request("DELETE", addr, "/models/qwen3", "localhost", "", "text/plain", tls_cfg)

-- 3. Chunked POST + Chunked response over HTTPS
let rchk = https_post_chunked(addr, "/chunked", ["mora-", "https-", "chunk"], "localhost", "text/plain", tls_cfg)

-- 4. 413 Payload Too Large over HTTPS
let r413 = https_post(addr, "/chunked", "0123456789012345678901234567890123456789", "localhost", "text/plain", tls_cfg)

-- 5. Live SSE over HTTPS
let sse_s = open_https_sse(addr, "/events", "localhost", tls_cfg)
let ev1 = sse_read_event(sse_s)
let ev2 = sse_read_event(sse_s)
sse_s.close()

let total_reqs = srv.join()
listener.close()

let r1_json = json.parse(r1.body)
[str(r1.status), str(r1.headers.get("x-tls-mw")), str(r1_json.get("id")), str(r1_json.get("q")), str(r2.status), r2.body, str(r405.status), str(rchk.status), rchk.body, str(r413.status), ev1.event + ":" + ev1.data + ":" + ev1.id, ev2.event + ":" + ev2.data + ":" + ev2.id, str(total_reqs)]
"#;

    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("verified".to_string()));
    assert_eq!(v[2], Value::String("qwen3".to_string()));
    assert_eq!(v[3], Value::String("mode=fp8".to_string()));
    assert_eq!(v[4], Value::String("200".to_string()));
    assert_eq!(v[5], Value::String("asset:v1/w.gguf".to_string()));
    assert_eq!(v[6], Value::String("405".to_string()));
    assert_eq!(v[7], Value::String("200".to_string()));
    assert_eq!(v[8], Value::String("tls:mora-https-chunk:ok".to_string()));
    assert_eq!(v[9], Value::String("413".to_string()));
    assert_eq!(v[10], Value::String("token:hello:1".to_string()));
    assert_eq!(v[11], Value::String("token:secure-world:2".to_string()));
    assert_eq!(v[12], Value::String("6".to_string())); // 2 keep-alive + 4 single = 6 requests
}

#[test]
fn p3_std_mcp_over_https_live_roundtrip() {
    let src = r#"
import "std/net/http"
import "std/net/tls"
import "std/mcp"

struct ScaleArgs
  x: int @tag(desc="Input value")
  k: int = 2 @tag(desc="Scale factor")
end

let srv = server("mora-https-mcp", "1.0.0")
assign srv = srv.typed_tool("scale", "Multiply x by k", ScaleArgs::schema(), fn(args: dict<string, any>)
  return int(str(args.get("x"))) * int(str(args.get("k")))
end)
assign srv = srv.resource("mora://status", "status", "TLS MCP status", "text/plain", fn(_u: string) "tls-mcp-ready" end)

let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pem = file.read_text("test_data/tls/test_server_pkcs8.key")
let ca_pem = file.read_text("test_data/tls/test_ca.pem")

let listener = listen("127.0.0.1:0", cert_pem, key_pem, ["http/1.1"])
let addr = listener.local_addr()
let srv_task = spawn fn()
  return srv.serve_listener(listener, 4)
end

let tls_cfg: dict<string, any> = {}
assign tls_cfg = tls_cfg.set("ca_pem", ca_pem).set("alpn", ["http/1.1"])

-- 1. HTTPS /health
let health = https_get(addr, "/health", "localhost", tls_cfg)

-- 2. HTTPS JSON-RPC tools/call (valid)
let call_args: dict<string, any> = {}
assign call_args = call_args.set("x", 21).set("k", 2)
let call_params: dict<string, any> = {}
assign call_params = call_params.set("name", "scale").set("arguments", call_args)
let rpc_call = call_https(addr, "tools/call", call_params, "localhost", tls_cfg, 10).unwrap()

-- 3. HTTPS JSON-RPC resources/read
let res_params: dict<string, any> = {}
assign res_params = res_params.set("uri", "mora://status")
let rpc_res = call_https(addr, "resources/read", res_params, "localhost", tls_cfg, 11).unwrap()

-- 4. HTTPS /sse MCP endpoint discovery
let sse = open_https_sse(addr, "/sse", "localhost", tls_cfg)
let ev = sse_read_event(sse)
sse.close()

let served = srv_task.join()
listener.close()

let tool_text = rpc_call.get("result").get("content").get(0).get("text")
let res_text = rpc_res.get("result").get("contents").get(0).get("text")

[str(health.status), str(json.parse(health.body).get("status")), str(tool_text), str(res_text), ev.event + ":" + ev.data, str(served)]
"#;

    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("ok".to_string()));
    assert_eq!(v[2], Value::String("42".to_string()));
    assert_eq!(v[3], Value::String("tls-mcp-ready".to_string()));
    assert_eq!(v[4], Value::String("endpoint:/mcp".to_string()));
    assert_eq!(v[5], Value::String("4".to_string()));
}

#[test]
fn p3_tls_coalesced_handshake_fragmented_readline_and_close_notify() {
    // Regression test for:
    // 1. Coalesced `[ClientFinished | 1-RTT ApplicationData]` drained into OpenSSL `rbio`
    // 2. Multi-record fragmented `read_line()` (`prev_plain_len > 0` across delayed TLS records)
    // 3. Unterminated final line followed immediately by `close_notify`
    let src = r#"
import "std/net/tls"

let cert_pem = file.read_text("test_data/tls/test_server.pem")
let key_pem = file.read_text("test_data/tls/test_server_pkcs8.key")
let ca_pem = file.read_text("test_data/tls/test_ca.pem")

let listener = listen("127.0.0.1:0", cert_pem, key_pem, ["http/1.1"])
let addr = listener.local_addr()

let srv_task = spawn fn()
  let round = 0
  while round < 6
    let c = listener.accept()
    let first = c.read_line().trim()
    let frag = c.read_line().trim()
    c.write("OK:" + first + "|" + frag + "\n")
    c.write("tail-" + str(round))
    c.close()
    assign round = round + 1
  end
  return round
end

let last_reply = ""
let last_tail = ""
let eof_ok = true
let i = 0
while i < 6
  let cli = connect(addr, "localhost", ca_pem, ["http/1.1"], false, 5000)
  -- Immediate 1-RTT write right after handshake completion
  cli.write("burst-" + str(i) + "\n")
  -- Multi-record fragmented line without newline in first two records
  cli.write("segA-")
  sleep(10)
  cli.write("segB-")
  sleep(10)
  cli.write("segC\n")
  assign last_reply = cli.read_line().trim()
  assign last_tail = cli.read_line()
  let eof_check = cli.read_line()
  if eof_check != "" then
    assign eof_ok = false
  end
  cli.close()
  assign i = i + 1
end

let rounds = srv_task.join()
listener.close()
[str(rounds), last_reply, last_tail, str(eof_ok)]
"#;

    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("6".to_string()));
    assert_eq!(v[1], Value::String("OK:burst-5|segA-segB-segC".to_string()));
    assert_eq!(v[2], Value::String("tail-5".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
}
