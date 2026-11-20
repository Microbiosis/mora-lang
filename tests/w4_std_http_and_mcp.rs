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

#[test]
fn w4_std_net_http_router_params_wildcard_middleware_and_405() {
    let src = r#"
import "std/net/http"

let app = router()
assign app = app.use_middleware(fn(req: HttpRequest, stream: tcp_stream, next: any)
  if req.path == "/secret" then
    return response(401, "unauthorized")
  end
  let res = next(req, stream)
  if is_instance(res, "HttpResponse") then
    let hdrs = res.headers.set("X-Mw", "applied")
    return HttpResponse(status=res.status, headers=hdrs, body=res.body)
  end
  return res
end)

assign app = app.get("/users/:id/posts/:post_id", fn(req: HttpRequest, _s: tcp_stream)
  return json_response(200, {"uid": req.params.get("id"), "pid": req.params.get("post_id")})
end)

assign app = app.get("/files/*", fn(req: HttpRequest, _s: tcp_stream)
  return response(200, "file:" + str(req.params.get("*")))
end)

assign app = app.post("/items", fn(req: HttpRequest, _s: tcp_stream)
  return response(201, "created:" + req.body)
end)

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let srv = spawn app.serve_listener(listener, 5)

let r_user = get(addr, "/users/42/posts/99")
let r_user_j = json.parse(r_user.body)
let r_file = get(addr, "/files/a/b/c.txt")
let r_405 = get(addr, "/items")
let r_401 = get(addr, "/secret")
let r_404 = get(addr, "/unknown/path")

let served = srv.join()
listener.close()

[str(r_user.status), str(r_user_j.get("uid")), str(r_user_j.get("pid")), str(r_user.headers.get("x-mw")), r_file.body, str(r_405.status), str(r_405.headers.get("allow")), str(r_401.status), str(r_404.status), str(served)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("42".to_string()));
    assert_eq!(v[2], Value::String("99".to_string()));
    assert_eq!(v[3], Value::String("applied".to_string()));
    assert_eq!(v[4], Value::String("file:a/b/c.txt".to_string()));
    assert_eq!(v[5], Value::String("405".to_string()));
    assert_eq!(v[6], Value::String("POST".to_string()));
    assert_eq!(v[7], Value::String("401".to_string()));
    assert_eq!(v[8], Value::String("404".to_string()));
    assert_eq!(v[9], Value::String("5".to_string()));
}

#[test]
fn w4_std_net_http_chunked_body_cap_and_keep_alive() {
    let src = r#"
import "std/net/http"

let app = router()
assign app = app.post("/chunked", fn(req: HttpRequest, _s: tcp_stream)
  return chunked_response(200, ["ACK:", req.body])
end)
assign app = app.get("/ping/:n", fn(req: HttpRequest, _s: tcp_stream)
  return response(200, "pong-" + str(req.params.get("n")))
end)
assign app = app.post("/echo", fn(req: HttpRequest, _s: tcp_stream)
  return response(200, req.body)
end)

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
-- Accept 4 connections with hard max_body_bytes = 32:
--   conn 1: chunked POST + chunked response
--   conn 2: oversized Content-Length POST -> 413
--   conn 3: oversized chunked POST -> 413
--   conn 4: persistent keep-alive connection sending 3 sequential HTTP/1.1 requests on ONE socket!
let srv = spawn app.serve_listener(listener, 4, nil, 32, 10)

-- 1. Chunked request + chunked response roundtrip
let r_chunk = post_chunked(addr, "/chunked", ["hello ", "chunked ", "world!"])

-- 2. Oversized Content-Length body (> 32 bytes) -> 413 Payload Too Large
let r_big_cl = post(addr, "/echo", "0123456789012345678901234567890123456789")

-- 3. Oversized chunked body (> 32 bytes) -> 413 Payload Too Large
let r_big_ch = post_chunked(addr, "/chunked", ["01234567890123456789", "01234567890123456789"])

-- 4. Persistent HTTP/1.1 Keep-Alive: 3 sequential requests on a SINGLE tcp_connect stream!
let ka_stream = tcp_connect(addr, 5000)
let k1 = send_request_on(ka_stream, "GET", addr, "/ping/1", "", "text/plain", true)
let k2 = send_request_on(ka_stream, "POST", addr, "/echo", "second-req", "text/plain", true)
let k3 = send_request_on(ka_stream, "GET", addr, "/ping/3", "", "text/plain", false)
ka_stream.close()

let total_reqs = srv.join()
listener.close()

[str(r_chunk.status), r_chunk.body, str(r_chunk.headers.get("transfer-encoding")), str(r_big_cl.status), str(r_big_ch.status), k1.body, str(k1.headers.get("connection")), k2.body, k3.body, str(k3.headers.get("connection")), str(total_reqs)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("200".to_string()));
    assert_eq!(v[1], Value::String("ACK:hello chunked world!".to_string()));
    assert_eq!(v[2], Value::String("chunked".to_string()));
    assert_eq!(v[3], Value::String("413".to_string()));
    assert_eq!(v[4], Value::String("413".to_string()));
    assert_eq!(v[5], Value::String("pong-1".to_string()));
    assert_eq!(v[6], Value::String("keep-alive".to_string()));
    assert_eq!(v[7], Value::String("second-req".to_string()));
    assert_eq!(v[8], Value::String("pong-3".to_string()));
    assert_eq!(v[9], Value::String("close".to_string()));
    // 1 chunked + 1 big_cl + 1 big_ch + 3 keep-alive requests = 6 total requests served across 4 connections!
    assert_eq!(v[10], Value::String("6".to_string()));
}

#[test]
fn w4_std_mcp_prompts_struct_schema_validation_and_stdio_transport() {
    let src = r#"
import "std/net/http"
import "std/mcp"

struct SearchInput
  query: string @tag(json="q", desc="Search query text")
  limit: int = 10 @tag(desc="Maximum number of results")
  tags: list<string> = [] @tag(desc="Filter tags")
  internal_secret: string = "hidden" @tag(json="-")
end

let srv = server("mora-mcp-p1", "2.0.0")
assign srv = srv.typed_tool("search", "Search docs", SearchInput::schema(), fn(args: dict<string, any>)
  let q = str(args.get("q"))
  let lim = if args.get("limit") != nil then str(args.get("limit")) else "10" end
  return "found:" + q + ":" + lim
end)
assign srv = srv.prompt("summarize", "Summarize topic", [prompt_arg("topic", "Topic name", true)], fn(args: dict<string, any>)
  return "Please summarize: " + str(args.get("topic"))
end)

-- 1. Verify Stdio NDJSON line batching (notifications produce no output line)
let batch_in: list<string> = []
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"q\":\"mora\",\"limit\":5}}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"limit\":5}}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"q\":\"mora\",\"limit\":\"bad\"}}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"prompts/list\",\"params\":{}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"prompts/get\",\"params\":{\"name\":\"summarize\",\"arguments\":{\"topic\":\"TensorComputeDag\"}}}")
assign batch_in = batch_in.push("{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"prompts/get\",\"params\":{\"name\":\"summarize\",\"arguments\":{}}}")
let replies = srv.serve_stdio_lines(batch_in)

let r_init = json.parse(replies.get(0))
let r_tlist = json.parse(replies.get(1))
let r_tok = json.parse(replies.get(2))
let r_tmiss = json.parse(replies.get(3))
let r_tbad = json.parse(replies.get(4))
let r_plist = json.parse(replies.get(5))
let r_pok = json.parse(replies.get(6))
let r_pmiss = json.parse(replies.get(7))

let tool_schema = r_tlist.get("result").get("tools").get(0).get("inputSchema")
let q_prop = tool_schema.get("properties").get("q")
let tags_prop = tool_schema.get("properties").get("tags")

-- 2. Verify concurrent Go-channel Stdio transport (`serve_stdio_channels`)
let in_ch: channel<string> = channel(4)
let out_ch: channel<string> = channel(4)
let worker = spawn srv.serve_stdio_channels(in_ch, out_ch, 2)
in_ch.send("{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"ping\",\"params\":{}}")
in_ch.send("{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"prompts/get\",\"params\":{\"name\":\"summarize\",\"arguments\":{\"topic\":\"KVAllocator\"}}}")
let ch_r1 = json.parse(out_ch.recv())
let ch_r2 = json.parse(out_ch.recv())
let ch_count = worker.join()

-- 3. Verify live stream NDJSON Stdio transport (`serve_stdio_stream`) over TCP loopback
let slistener = tcp_listen("127.0.0.1:0")
let saddr = slistener.local_addr()
let stream_srv = spawn fn()
  let st = slistener.accept()
  let n = srv.serve_stdio_stream(st)
  st.close()
  return n
end
let cli = tcp_connect(saddr, 5000)
cli.write("{\"jsonrpc\":\"2.0\",\"id\":20,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"q\":\"simd\"}}}\n")
let sline = cli.read_line(8192)
cli.close()
let stream_n = stream_srv.join()
slistener.close()
let sreply = json.parse(sline)

[str(replies.len()), str(r_init.get("result").get("capabilities").get("prompts") != nil), str(tool_schema.get("title")), str(q_prop.get("type")), str(q_prop.get("description")), str(tags_prop.get("type")), str(tags_prop.get("items").get("type")), str(tool_schema.get("required")), str(r_tok.get("result").get("content").get(0).get("text")), str(r_tmiss.get("error").get("code")), str(r_tbad.get("error").get("code")), str(r_plist.get("result").get("prompts").get(0).get("name")), str(r_pok.get("result").get("messages").get(0).get("content").get("text")), str(r_pmiss.get("error").get("code")), str(ch_count), str(ch_r1.get("id")), str(ch_r2.get("result").get("messages").get(0).get("content").get("text")), str(stream_n), str(sreply.get("result").get("content").get(0).get("text")), str(tool_schema.get("properties").get("internal_secret") == nil)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("8".to_string())); // 9 inputs - 1 notification = 8 replies!
    assert_eq!(v[1], Value::String("true".to_string())); // prompts capability advertised
    assert_eq!(v[2], Value::String("SearchInput".to_string())); // Struct::schema() title
    assert_eq!(v[3], Value::String("string".to_string())); // @tag(json="q") mapped type
    assert_eq!(v[4], Value::String("Search query text".to_string())); // @tag(desc=...) propagated
    assert_eq!(v[5], Value::String("array".to_string())); // list<string> -> array
    assert_eq!(v[6], Value::String("string".to_string())); // items: string
    assert_eq!(v[7], Value::String("[q]".to_string())); // only non-default field required
    assert_eq!(v[8], Value::String("found:mora:5".to_string())); // valid call succeeded
    assert_eq!(v[9], Value::String("-32602".to_string())); // missing required 'q' rejected
    assert_eq!(v[10], Value::String("-32602".to_string())); // bad type for 'limit' rejected
    assert_eq!(v[11], Value::String("summarize".to_string())); // prompts/list
    assert_eq!(
        v[12],
        Value::String("Please summarize: TensorComputeDag".to_string())
    );
    assert_eq!(v[13], Value::String("-32602".to_string())); // missing prompt arg rejected
    assert_eq!(v[14], Value::String("2".to_string())); // serve_stdio_channels count
    assert_eq!(v[15], Value::String("10".to_string())); // ping reply id
    assert_eq!(
        v[16],
        Value::String("Please summarize: KVAllocator".to_string())
    );
    assert_eq!(v[17], Value::String("1".to_string())); // serve_stdio_stream count
    assert_eq!(v[18], Value::String("found:simd:10".to_string())); // live NDJSON stream reply
    assert_eq!(v[19], Value::String("true".to_string())); // @tag(json="-") field excluded from JSON Schema
}
