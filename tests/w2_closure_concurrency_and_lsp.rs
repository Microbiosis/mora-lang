//! Wave 2 Closure End-to-End Test Suite:
//! - `EngineShared` (`scheduler` + `reactor`) shared across isolated `CoreRuntime` instances
//! - Go-style concurrency primitives (`spawn`, `channel`, `select`, `context`, `sleep`, `task_handle`)
//! - HM static enforcement (channel invariance, select arm consistency, spawn arity & closure checks)
//! - LSP hover & completion unified with HM inference and canonical builtins/keywords

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::typeck::{TypeError, check_mir::check_program_witnesses, format_error};
use mora::value::Value;

fn compile_typecheck_run(source: &str) -> Value {
    let (program, witnesses) =
        ParserV3::compile(source).unwrap_or_else(|e| panic!("compile failed: {e}\n{source}"));
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let rendered: Vec<String> = errs.iter().map(format_error).collect();
        panic!(
            "typecheck failed:\n{}\n---\n{}",
            rendered.join("\n"),
            source
        );
    }
    let mut core = CoreRuntime::default();
    let prog = Arc::new(program);
    let (last, globals) = execute_program(Arc::clone(&prog), &mut core)
        .unwrap_or_else(|e| panic!("execute_program failed: {e}\n{source}"));
    let main_res = run_main_task(prog, globals, &mut core)
        .unwrap_or_else(|e| panic!("run_main_task failed: {e}\n{source}"));
    if main_res != Value::Nil {
        main_res
    } else {
        last
    }
}

fn compile_typecheck_run_err(source: &str) -> String {
    let (program, witnesses) =
        ParserV3::compile(source).unwrap_or_else(|e| panic!("compile failed: {e}\n{source}"));
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let rendered: Vec<String> = errs.iter().map(format_error).collect();
        panic!(
            "typecheck failed:\n{}\n---\n{}",
            rendered.join("\n"),
            source
        );
    }
    let mut core = CoreRuntime::default();
    let prog = Arc::new(program);
    let (_last, globals) = execute_program(Arc::clone(&prog), &mut core)
        .expect("program init should succeed before runtime failure");
    run_main_task(prog, globals, &mut core)
        .expect_err("expected runtime failure")
        .to_string()
}

fn compile_and_typecheck(source: &str) -> Vec<TypeError> {
    let (_program, witnesses) =
        ParserV3::compile(source).unwrap_or_else(|e| panic!("compile failed: {e}\n{source}"));
    check_program_witnesses(&witnesses)
}

#[test]
fn engine_shared_shares_scheduler_and_reactor_across_cloned_runtimes() {
    let mut core1 = CoreRuntime::default();
    let mut core2 = core1.clone();
    assert!(Arc::ptr_eq(core1.shared(), core2.shared()));
    assert!(Arc::ptr_eq(core1.scheduler(), core2.scheduler()));
    assert!(Arc::ptr_eq(core1.reactor(), core2.reactor()));

    let (p1, w1) = ParserV3::compile("let secret = 101i\nsecret").expect("p1");
    assert!(check_program_witnesses(&w1).is_empty());
    let (v1, _) = execute_program(Arc::new(p1), &mut core1).expect("exec1");
    assert_eq!(v1, Value::Int(101));

    let (p2, w2) = ParserV3::compile("let other = 202i\nother").expect("p2");
    assert!(check_program_witnesses(&w2).is_empty());
    let (v2, _) = execute_program(Arc::new(p2), &mut core2).expect("exec2");
    assert_eq!(v2, Value::Int(202));
}

#[test]
fn buffered_channels_try_ops_len_close_and_multi_spawn_fanin() {
    let src = r#"
task producer(ch: channel<int>, base: int)
  ch.send(base + 1i)
  ch.send(base + 2i)
  base
end

task main()
  let ch = channel(4i)
  let cap = ch.capacity()
  let h1 = spawn producer(ch, 10i)
  let h2 = spawn do
    ch.send(30i)
    true
  end
  let r1 = h1.join()
  let r2 = h2.join()
  let n_ready = len(ch)
  let total = ch.recv() + ch.recv() + ch.recv()
  let empty_recv = ch.try_recv()
  let full_ch = channel(1i)
  let s1 = full_ch.try_send(1i)
  let s2 = full_ch.try_send(2i)
  ch.close()
  let h_closed = spawn ch.is_closed()
  let closed = h_closed.join()
  str(cap) + "," + str(r1) + "," + str(r2) + "," + str(n_ready) + "," + str(total) + "," + str(empty_recv) + "," + str(s1) + "," + str(s2) + "," + str(closed)
end
"#;
    let out = compile_typecheck_run(src);
    assert_eq!(
        out,
        Value::String("4,10,true,3,53,nil,true,false,true".to_string())
    );

    let err = compile_typecheck_run_err(
        r#"
task main()
  let ch = channel(1i)
  ch.close()
  ch.send(1i)
end
"#,
    );
    assert!(
        err.contains("closed"),
        "expected send on closed channel error, got: {err}"
    );
}

#[test]
fn unbuffered_channel_rendezvous_and_spawn_method() {
    let src = r#"
task main()
  let ch = channel()
  let h = spawn do
    ch.send("ping")
    ch.recv()
  end
  let msg = ch.recv()
  ch.send(msg + "-pong")
  let reply = h.join()
  reply
end
"#;
    let out = compile_typecheck_run(src);
    assert_eq!(out, Value::String("ping-pong".to_string()));
}

#[test]
fn select_handles_recv_send_timeout_and_default_arms() {
    let src_recv_and_send = r#"
task main()
  let c1 = channel(1i)
  let blocking_unbuf = channel()
  let c2 = channel(1i)
  c1.send(42i)
  let a = select
    case v = c1.recv() -> v + 1i
    case blocking_unbuf.send(99i) -> 0i
    case timeout(500) -> -1i
  end
  let b = select {
    case c2.send(77i) -> c2.recv(),
    case timeout(500) -> -2i
  }
  [a, b]
end
"#;
    assert_eq!(
        compile_typecheck_run(src_recv_and_send),
        Value::List(vec![Value::Int(43), Value::Int(77)].into())
    );

    let src_default_and_timeout = r#"
task main()
  let empty = channel(1i)
  let d = select
    case v = empty.recv() -> v
    default -> 123i
  end
  let t = select
    case v = empty.recv() -> v
    case timeout(15) -> 456i
  end
  [d, t]
end
"#;
    assert_eq!(
        compile_typecheck_run(src_default_and_timeout),
        Value::List(vec![Value::Int(123), Value::Int(456)].into())
    );
}

#[test]
fn context_cancellation_timeout_spawn_and_sleep() {
    let src_ok = r#"
task work(ctx: context, out: channel<int>)
  sleep(5)
  if ctx.is_cancelled() then
    out.send(-1i)
  else
    out.send(88i)
  end
end

task main()
  let root = context()
  let child = root.child()
  let before = child.is_cancelled()
  let ch = channel(1i)
  let h = child.spawn(work, child, ch)
  h.join()
  let got = ch.recv()
  root.cancel("done")
  let after = child.is_cancelled()
  str(before) + "," + str(got) + "," + str(after)
end
"#;
    assert_eq!(
        compile_typecheck_run(src_ok),
        Value::String("false,88,true".to_string())
    );

    let src_timeout_err = r#"
task main()
  let never = channel()
  let ctx = context().with_timeout(15)
  never.recv_ctx(ctx)
end
"#;
    let err = compile_typecheck_run_err(src_timeout_err);
    assert!(
        err.to_lowercase().contains("deadline") || err.to_lowercase().contains("cancel"),
        "expected deadline/cancellation error, got: {err}"
    );
}

#[test]
fn hm_statically_rejects_invalid_concurrency_usage() {
    let errs_send = compile_and_typecheck(
        r#"
let ch = channel(2i)
ch.send(1i)
ch.send("wrong")
"#,
    );
    assert!(
        !errs_send.is_empty(),
        "mixing string into channel<int> must be rejected"
    );

    let errs_invariant = compile_and_typecheck(
        r#"
task needs_float(c: channel<float>)
  c.recv()
end
let ci = channel(1i)
ci.send(1i)
needs_float(ci)
"#,
    );
    assert!(
        !errs_invariant.is_empty(),
        "Channel<T> must remain invariant (channel<int> != channel<float>)"
    );

    let errs_select = compile_and_typecheck(
        r#"
let ch = channel(1i)
ch.send(5i)
select
  case v = ch.recv() -> v + "bad"
end
"#,
    );
    assert!(
        !errs_select.is_empty(),
        "select recv binding must preserve element type int"
    );

    let errs_spawn_arity = compile_and_typecheck(
        r#"
task f(a: int, b: int)
  a + b
end
spawn f(1i)
"#,
    );
    assert!(
        !errs_spawn_arity.is_empty(),
        "spawn call with wrong arity must be rejected"
    );

    let errs_spawn_not_closure = compile_and_typecheck("spawn 123i");
    assert!(
        !errs_spawn_not_closure.is_empty()
            && errs_spawn_not_closure.iter().any(|e| {
                let s = format_error(e).to_lowercase();
                s.contains("int")
                    && (s.contains("call") || s.contains("function") || s.contains("closure"))
            }),
        "spawn 123i must report non-callable error, got: {:?}",
        errs_spawn_not_closure
    );
}

fn lsp_frame(json: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
}

#[test]
fn lsp_hover_and_completion_surface_hm_types_and_concurrency_builtins() {
    let text = concat!(
        "let ch = channel(4i)\n",
        "ch.send(1i)\n",
        "let ctx = context()\n",
        "task worker(c: channel<int>)\n",
        "  select\n",
        "    case v = c.recv() -> v + 1i\n",
        "    case timeout(50) -> 0i\n",
        "  end\n",
        "end\n",
        "let h = spawn worker(ch)\n",
    );
    let uri = "file:///w2_lsp_test.mora";
    let did_open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":"{}"}}}}}}"#,
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    );

    let reqs = [
        lsp_frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#),
        lsp_frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
        lsp_frame(&did_open),
        // id 10: `ch` on line 0 (`let ch = ...`)
        lsp_frame(&format!(
            r#"{{"jsonrpc":"2.0","id":10,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":0,"character":4}}}}}}"#
        )),
        // id 11: `ctx` on line 2 (`let ctx = ...`)
        lsp_frame(&format!(
            r#"{{"jsonrpc":"2.0","id":11,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":2,"character":4}}}}}}"#
        )),
        // id 12: `worker` on line 3 (`task worker(...)`)
        lsp_frame(&format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":3,"character":6}}}}}}"#
        )),
        // id 13: `h` on line 9 (`let h = spawn worker(ch)`)
        lsp_frame(&format!(
            r#"{{"jsonrpc":"2.0","id":13,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":9,"character":4}}}}}}"#
        )),
        // id 20: completion on line 9
        lsp_frame(&format!(
            r#"{{"jsonrpc":"2.0","id":20,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":9,"character":0}}}}}}"#
        )),
        lsp_frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#),
        lsp_frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    ]
    .concat();

    let mut child = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mora-lsp");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(reqs.as_bytes())
        .expect("write requests");
    let out = child.wait_with_output().expect("wait mora-lsp");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let msg_for = |id: u32| -> String {
        stdout
            .split("Content-Length:")
            .find(|chunk| chunk.contains(&format!(r#""id":{id},"#)))
            .unwrap_or_else(|| panic!("missing response for id={id}:\n{stdout}"))
            .to_string()
    };

    assert!(
        msg_for(10).contains("let ch: channel<int>"),
        "expected ch: channel<int>, got: {}",
        msg_for(10)
    );
    assert!(
        msg_for(11).contains("let ctx: context"),
        "expected ctx: context, got: {}",
        msg_for(11)
    );
    assert!(
        msg_for(12).contains("fn worker: fn (channel<int>) -> int"),
        "expected worker signature, got: {}",
        msg_for(12)
    );
    assert!(
        msg_for(13).contains("let h: task_handle<int>"),
        "expected h: task_handle<int>, got: {}",
        msg_for(13)
    );

    let comp = msg_for(20);
    for expected in [
        r#""label":"spawn""#,
        r#""label":"select""#,
        r#""label":"case""#,
        r#""label":"default""#,
        r#""label":"timeout""#,
        r#""label":"channel""#,
        r#""label":"context""#,
        r#""label":"sleep""#,
        r#""label":"math""#,
        r#""label":"ch""#,
        r#""detail":"channel<int>""#,
        r#""detail":"task_handle<int>""#,
    ] {
        assert!(
            comp.contains(expected),
            "completion missing {expected}:\n{comp}"
        );
    }
}
