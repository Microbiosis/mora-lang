//! Wave 4 Step 3 verification: Pure-Mora `std/net/http` (including SSE) and `std/mcp`
//! standard libraries, cross-module `struct`/`impl`/function export propagation, and
//! live concurrent loopback HTTP/SSE/MCP server-client roundtrips.

use std::sync::Arc;

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

#[test]
fn w4_std_net_http_and_sse_live_roundtrip() {
    let src = r#"
import "std/net/http"

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()

task app_handler(req: HttpRequest, stream: tcp_stream): any
  if req.path == "/echo" and req.method == "POST" then
    let parsed = json.parse(req.body)
    return json_response(200, {"echo": parsed.get("msg"), "q": req.query})
  end
  if req.path == "/events" and req.method == "GET" then
    sse_start(stream)
    sse_send(stream, "token", "hello", "1")
    sse_send(stream, "done", "world", "2")
    return nil
  end
  return response(404, "missing")
end

let srv = spawn serve_listener(listener, app_handler, 3)

let post_resp = post(addr, "/echo?mode=fast", json.stringify({"msg": "hi-http"}))
let post_json = json.parse(post_resp.body)

let sse_stream = open_sse(addr, "/events")
let ev1 = sse_read_event(sse_stream)
let ev2 = sse_read_event(sse_stream)
sse_stream.close()

let miss_resp = get(addr, "/nope")
let served = srv.join()
listener.close()

[str(post_resp.status), str(post_json.get("echo")), str(post_json.get("q")), ev1.event, ev1.data, ev1.id, ev2.event, ev2.data, str(miss_resp.status), str(served)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("hi-http".to_string()));
    assert_eq!(v[2], Value::String("mode=fast".to_string()));
    assert_eq!(v[3], Value::String("token".to_string()));
    assert_eq!(v[4], Value::String("hello".to_string()));
    assert_eq!(v[5], Value::String("1".to_string()));
    assert_eq!(v[6], Value::String("done".to_string()));
    assert_eq!(v[7], Value::String("world".to_string()));
    assert_eq!(v[8], Value::String("404".to_string()));
    assert_eq!(v[9], Value::String("3".to_string()));
}

#[test]
fn w4_std_mcp_server_http_sse_and_jsonrpc_roundtrip() {
    let src = r#"
import "std/net/http"
import "std/mcp"

let srv = server("mora-demo", "1.0.0")
let schema: dict<string, any> = json.parse("{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"integer\"},\"b\":{\"type\":\"integer\"}}}")
assign srv = srv.tool("add", "Add two integers", schema, fn(args: dict<string, any>) int(str(args.get("a"))) + int(str(args.get("b"))) end)
assign srv = srv.resource("mora://status", "status", "Runtime status", "text/plain", fn(uri: string) "ready:" + uri end)

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let h = spawn srv.serve_listener(listener, 6)

let health = get(addr, "/health")
let sse_ch = open_sse(addr, "/sse")
let ep_ev = sse_read_event(sse_ch)
sse_ch.close()

let empty_params: dict<string, any> = {}
let init_res = call_http(addr, "initialize", empty_params, 1)
let list_res = call_http(addr, "tools/list", empty_params, 2)
let call_params: dict<string, any> = {}
assign call_params = call_params.set("name", "add").set("arguments", {"a": 19, "b": 23})
let call_res = call_http(addr, "tools/call", call_params, 3)
let res_read = call_http(addr, "resources/read", {"uri": "mora://status"}, 4)

let served = h.join()
listener.close()

let server_name = init_res.get("result").get("serverInfo").get("name")
let tool_name = list_res.get("result").get("tools").get(0).get("name")
let tool_out = call_res.get("result").get("content").get(0).get("text")
let res_text = res_read.get("result").get("contents").get(0).get("text")

[str(health.status), ep_ev.event, ep_ev.data, str(server_name), str(tool_name), str(tool_out), str(res_text), str(served)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("endpoint".to_string()));
    assert_eq!(v[2], Value::String("/mcp".to_string()));
    assert_eq!(v[3], Value::String("mora-demo".to_string()));
    assert_eq!(v[4], Value::String("add".to_string()));
    assert_eq!(v[5], Value::String("42".to_string()));
    assert_eq!(v[6], Value::String("ready:mora://status".to_string()));
    assert_eq!(v[7], Value::String("6".to_string()));
}
