//! Wave 2.5 — Security & boundary closure integration suite:
//! - Bounded LSP stdio transport (OOM / header-flood / conflicting length defense)
//! - Unified iterative RFC 8259 JSON parser (`MAX_JSON_DEPTH = 256`) + wire-format parity
//! - Cross-module `import` typeck enforcement (compile-time & `ControlInst::Import` VM gate)
//! - `FsPolicy::Confined` + `CheckedPath` + isolated `virtual_cwd` + `AsyncIoReactor` cancellation
//! - First-class `HostObject` trait / `Value::External(Arc<dyn HostObject>)` dispatch & JSON

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Cursor};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use mora::error::{MoraError, MoraResult};
use mora::flow::{MAX_JSON_DEPTH, json_to_value, type_name, value_to_json};
use mora::lsp::json::{Value as LspJsonValue, parse as lsp_parse, to_string as lsp_to_string};
use mora::lsp::transport::{
    MAX_CONTENT_LENGTH, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, read_message,
};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::scheduler::TaskContext;
use mora::runtime::vm::{execute_program, execute_value};
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::{HostObject, Value};

fn eval_with_core(src: &str, core: &mut CoreRuntime) -> MoraResult<Value> {
    let (prog, _w) = ParserV3::compile(src)?;
    let (val, _) = execute_program(Arc::new(prog), core)?;
    Ok(val)
}

#[cfg(unix)]
fn create_dir_link(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create unix dir symlink");
}

#[cfg(windows)]
fn create_dir_link(link: &Path, target: &Path) {
    // NTFS directory junctions work on Windows without Developer Mode or elevation.
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .expect("spawn cmd mklink /J");
    assert!(
        out.status.success(),
        "mklink /J failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
fn try_create_file_symlink(link: &Path, target: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).expect("create unix file symlink");
    true
}

#[cfg(windows)]
fn try_create_file_symlink(link: &Path, target: &Path) -> bool {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(1314) => false,
        Err(e) => panic!("unexpected windows symlink_file error: {e}"),
    }
}

// ─── 1. LSP Transport Hardening ─────────────────────────────────────────────────

#[test]
fn transport_rejects_oversized_content_length_without_oom() {
    for bogus_len in [MAX_CONTENT_LENGTH + 1, 999_999_999usize, usize::MAX] {
        let frame = format!("Content-Length: {bogus_len}\r\n\r\n");
        let mut cursor = Cursor::new(frame.into_bytes());
        let err = read_message(&mut cursor)
            .expect_err("oversized Content-Length must be rejected before body read/alloc");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "err={err}");
        assert!(
            err.to_string().contains("exceeds maximum allowed"),
            "unexpected message: {err}"
        );
    }
}

#[test]
fn transport_rejects_conflicting_content_length_and_header_floods() {
    let conflicting = b"Content-Length: 2\r\nContent-Length: 5\r\n\r\n{}";
    let err = read_message(&mut Cursor::new(&conflicting[..]))
        .expect_err("conflicting Content-Length must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    let oversized_line = format!(
        "X-Pad: {}\r\nContent-Length: 2\r\n\r\n{{}}",
        "a".repeat(MAX_HEADER_LINE_BYTES + 16)
    );
    let err = read_message(&mut Cursor::new(oversized_line.into_bytes()))
        .expect_err("oversized header line must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    let mut flooded = String::new();
    for i in 0..(MAX_HEADER_LINES + 1) {
        flooded.push_str(&format!("X-H-{i}: 1\r\n"));
    }
    flooded.push_str("Content-Length: 2\r\n\r\n{}");
    let err =
        read_message(&mut Cursor::new(flooded.into_bytes())).expect_err("header flood must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

// ─── 2. Unified JSON Depth & Wire Format Parity ────────────────────────────────

#[test]
fn json_enforces_exact_max_depth_iteratively_across_flow_and_lsp() {
    let exact_ok = format!(
        "{}1{}",
        "[".repeat(MAX_JSON_DEPTH),
        "]".repeat(MAX_JSON_DEPTH)
    );
    let too_deep = format!(
        "{}1{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "]".repeat(MAX_JSON_DEPTH + 1)
    );

    assert!(
        json_to_value(&exact_ok).is_ok(),
        "flow::json must accept depth == MAX_JSON_DEPTH"
    );
    assert!(
        lsp_parse(&exact_ok).is_ok(),
        "lsp::json must accept depth == MAX_JSON_DEPTH"
    );

    let flow_err = json_to_value(&too_deep).expect_err("flow::json must reject depth > 256");
    assert!(
        matches!(flow_err, MoraError::Serialization(_)),
        "got: {flow_err:?}"
    );
    let lsp_err = lsp_parse(&too_deep).expect_err("lsp::json must reject depth > 256");
    assert!(
        matches!(lsp_err, MoraError::Serialization(_)),
        "got: {lsp_err:?}"
    );
}

#[test]
fn json_rejects_malformed_numbers_and_trailing_garbage_consistently() {
    for bad in ["007", "5.", "-.", "{\"a\":1}xyz", "[1,]"] {
        assert!(
            json_to_value(bad).is_err(),
            "flow::json should reject malformed input `{bad}`"
        );
        assert!(
            lsp_parse(bad).is_err(),
            "lsp::json should reject malformed input `{bad}`"
        );
    }
}

#[test]
fn json_preserves_flow_vs_lsp_number_wire_formats() {
    assert_eq!(value_to_json(&Value::Float(6.0)), "6.0");
    assert_eq!(value_to_json(&Value::Int(6)), "6");
    assert_eq!(lsp_to_string(&LspJsonValue::Number(6.0)), "6");
    assert_eq!(lsp_to_string(&LspJsonValue::Number(f64::NAN)), "null");
}

// ─── 3. Cross-Module Import Typeck Closure ──────────────────────────────────────

#[test]
fn typeck_and_vm_import_both_reject_type_errors_in_imported_module() {
    let dir = TempDir::new("w2_5_import_typeck");
    let bad_mod = dir.join("bad_mod.mora");
    std::fs::write(
        &bad_mod,
        "file.write_text(\"executed.txt\", \"should_not_run\")\nexport let broken = 1i + \"oops\"\n",
    )
    .expect("write bad_mod.mora");

    // 3a. Compile-time / `--check` path (`typeck::imports`)
    let abs_for_mora = bad_mod.to_string_lossy().replace('\\', "/");
    let main_abs = format!("import \"{abs_for_mora}\"\n1i\n");
    let (_prog, witnesses) = ParserV3::compile(&main_abs).expect("compile main");
    let type_errs = check_program_witnesses(&witnesses);
    assert!(
        !type_errs.is_empty(),
        "check_program_witnesses must report type error inside imported module"
    );

    // 3b. Runtime `ControlInst::Import` gate even when caller executes directly without pre-typeck
    let mut core = CoreRuntime::default()
        .with_fs_root(dir.path())
        .expect("confined root");
    let main_rel = "import \"bad_mod.mora\"\n1i\n";
    let err = eval_with_core(main_rel, &mut core)
        .expect_err("ControlInst::Import must reject untypechecked module before execution");
    assert!(
        matches!(err, MoraError::Typeck(_)),
        "expected MoraError::Typeck, got: {err:?}"
    );
    assert!(
        !dir.join("executed.txt").exists(),
        "imported module body must not execute when its typeck fails"
    );
}

// ─── 4. Filesystem Confinement, Virtual CWD, Symlink Escape, Cancellation ──────

#[test]
fn fs_confined_blocks_lexical_traversal_chdir_escape_and_keeps_process_cwd_intact() {
    let dir = TempDir::new("w2_5_fs_confine");
    let sandbox = dir.join("sandbox");
    let sub = sandbox.join("sub");
    std::fs::create_dir_all(&sub).expect("create sandbox/sub");
    std::fs::write(dir.join("outside.txt"), "top_secret").expect("write outside.txt");
    std::fs::write(dir.join("outside.mora"), "export let secret = 42i\n")
        .expect("write outside.mora");
    std::fs::write(sub.join("inside.txt"), "ok_inside").expect("write inside.txt");

    let host_cwd_before = std::env::current_dir().expect("host cwd");

    let mut core_a = CoreRuntime::default()
        .with_fs_root(&sandbox)
        .expect("confine A");
    let mut core_b = CoreRuntime::default()
        .with_fs_root(&sandbox)
        .expect("confine B");

    // Lexical traversal escapes, NUL injection, and home_dir leak blocked across file.* and import
    let err = eval_with_core("file.read_text(\"../outside.txt\")", &mut core_a)
        .expect_err(".. escape read_text must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );

    let err = eval_with_core("file.write_text(\"../pwn.txt\", \"bad\")", &mut core_a)
        .expect_err(".. escape write_text must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );
    assert!(!dir.join("pwn.txt").exists());

    let err = eval_with_core("import \"../outside.mora\"\n1i", &mut core_a)
        .expect_err(".. escape import must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );

    let err =
        eval_with_core("file.chdir(\"..\")", &mut core_a).expect_err("chdir above root must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );

    let err = eval_with_core("file.home_dir()", &mut core_a)
        .expect_err("home_dir must be denied under confined FsPolicy");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("sandbox denied")),
        "got: {err:?}"
    );

    let err = core_a
        .fs_context()
        .resolve_path("bad\0path.txt", "file.read_text")
        .expect_err("NUL byte must be rejected");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("NUL")),
        "got: {err:?}"
    );

    // Isolated per-runtime virtual_cwd: moving core_a into "sub" must not affect core_b or host cwd
    assert_eq!(
        eval_with_core("file.chdir(\"sub\")", &mut core_a).expect("chdir sub"),
        Value::Nil
    );
    let new_cwd = eval_with_core("file.cwd()", &mut core_a).expect("cwd sub");
    let canon_sub = sub.canonicalize().expect("canon sub");
    assert_eq!(new_cwd, Value::String(canon_sub.display().to_string()));
    assert_eq!(
        eval_with_core("file.read_text(\"inside.txt\")", &mut core_a).expect("read inside sub"),
        Value::String("ok_inside".to_string())
    );

    let canon_sandbox = sandbox.canonicalize().expect("canon sandbox");
    assert_eq!(
        eval_with_core("file.cwd()", &mut core_b).expect("core_b cwd"),
        Value::String(canon_sandbox.display().to_string())
    );
    assert_eq!(
        std::env::current_dir().expect("host cwd after"),
        host_cwd_before,
        "file.chdir must never mutate process-wide std::env::current_dir"
    );
}

#[test]
fn fs_confined_blocks_symlink_junction_and_dangling_target_escape() {
    let dir = TempDir::new("w2_5_fs_symlink");
    let sandbox = dir.join("sandbox");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&sandbox).expect("create sandbox");
    std::fs::create_dir_all(&outside).expect("create outside");
    std::fs::write(outside.join("secret.txt"), "secret_data").expect("write outside secret");

    // Directory link/junction into `outside` tests BOTH existing targets and nonexistent child writes.
    let escape_dir_link = sandbox.join("escape_dir");
    create_dir_link(&escape_dir_link, &outside);

    let mut core = CoreRuntime::default()
        .with_fs_root(&sandbox)
        .expect("confine");

    let err = eval_with_core("file.read_text(\"escape_dir/secret.txt\")", &mut core)
        .expect_err("read through symlink/junction outside sandbox must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );

    let err = eval_with_core(
        "file.write_text(\"escape_dir/dangling_new.txt\", \"pwn\")",
        &mut core,
    )
    .expect_err("write to non-existent leaf through escaping dir link must fail");
    assert!(
        matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
        "got: {err:?}"
    );
    assert!(!outside.join("dangling_new.txt").exists());

    // Direct dangling file symlink when OS grants symlink privilege
    let dangling_file = sandbox.join("dangling.txt");
    let outside_dangling_target = outside.join("created_via_dangling.txt");
    if try_create_file_symlink(&dangling_file, &outside_dangling_target) {
        let err = eval_with_core("file.write_text(\"dangling.txt\", \"pwn\")", &mut core)
            .expect_err("write through dangling file symlink outside sandbox must fail");
        assert!(
            matches!(&err, MoraError::Runtime(m) if m.contains("escapes")),
            "got: {err:?}"
        );
        assert!(!outside_dangling_target.exists());
    }
}

#[test]
fn fs_reactor_and_file_builtin_honor_task_context_cancellation() {
    let dir = TempDir::new("w2_5_fs_cancel");
    std::fs::write(dir.join("inside.txt"), "hello").expect("write inside.txt");

    let ctx = TaskContext::root();
    ctx.cancel("aborted by user");

    let mut core = CoreRuntime::default()
        .with_fs_root(dir.path())
        .expect("confine")
        .with_task_context(ctx.clone());

    let reactor_err = core
        .reactor()
        .run_fs(Some(&ctx), || Ok(1usize))
        .expect_err("cancelled TaskContext must abort reactor.run_fs");
    assert_eq!(reactor_err.kind_name(), "cancelled");
    assert!(matches!(reactor_err, MoraError::Cancelled(_)));

    let file_err = eval_with_core("file.read_text(\"inside.txt\")", &mut core)
        .expect_err("cancelled TaskContext must abort file.read_text");
    assert_eq!(file_err.kind_name(), "cancelled");
    assert!(matches!(file_err, MoraError::Cancelled(_)));
}

// ─── 5. HostObject Extension Mechanism ──────────────────────────────────────────

#[derive(Debug)]
struct CounterHost {
    label: String,
    count: AtomicI64,
    project_json: bool,
}

impl CounterHost {
    fn new(label: &str, init: i64, project_json: bool) -> Self {
        Self {
            label: label.to_string(),
            count: AtomicI64::new(init),
            project_json,
        }
    }
}

impl HostObject for CounterHost {
    fn type_name(&self) -> &'static str {
        "counter"
    }

    fn display(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "counter({}:{})",
            self.label,
            self.count.load(Ordering::SeqCst)
        )
    }

    fn methods(&self) -> &[&'static str] {
        &["get", "inc", "cwd"]
    }

    fn call_method(
        &self,
        method: &str,
        args: &[Value],
        core: &mut CoreRuntime,
    ) -> MoraResult<Value> {
        match method {
            "get" => Ok(Value::Int(self.count.load(Ordering::SeqCst))),
            "inc" => {
                let delta = match args.first() {
                    Some(Value::Int(n)) => *n,
                    Some(other) => {
                        return Err(MoraError::Runtime(format!(
                            "counter.inc expects int, got {}",
                            type_name(other)
                        )));
                    }
                    None => 1,
                };
                let next = self.count.fetch_add(delta, Ordering::SeqCst) + delta;
                Ok(Value::Int(next))
            }
            "cwd" => Ok(Value::String(core.fs_context().cwd().display().to_string())),
            other => Err(MoraError::Runtime(format!(
                "unknown counter method: {other}"
            ))),
        }
    }

    fn to_json(&self) -> Option<Value> {
        if !self.project_json {
            return None;
        }
        let mut map = HashMap::new();
        map.insert("label".to_string(), Value::String(self.label.clone()));
        map.insert(
            "count".to_string(),
            Value::Int(self.count.load(Ordering::SeqCst)),
        );
        Some(Value::Dict(map))
    }

    fn dyn_eq(&self, other: &dyn HostObject) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|rhs| {
            self.label == rhs.label
                && self.count.load(Ordering::SeqCst) == rhs.count.load(Ordering::SeqCst)
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn external_host_object_supports_display_eq_methods_vm_dispatch_and_json() {
    let dir = TempDir::new("w2_5_host_object");
    let mut core = CoreRuntime::default()
        .with_fs_root(dir.path())
        .expect("confine");

    let h1: Arc<dyn HostObject> = Arc::new(CounterHost::new("hits", 10, true));
    let h1_same: Arc<dyn HostObject> = Arc::new(CounterHost::new("hits", 10, false));
    let h_diff: Arc<dyn HostObject> = Arc::new(CounterHost::new("hits", 99, true));
    let h_opaque: Arc<dyn HostObject> = Arc::new(CounterHost::new("opaque\"tag", 0, false));

    let v1 = Value::External(Arc::clone(&h1));
    let v1_ptr = Value::External(Arc::clone(&h1));
    let v1_eq = Value::External(h1_same);
    let v_diff = Value::External(h_diff);
    let v_opaque = Value::External(h_opaque);

    assert_eq!(type_name(&v1), "counter");
    assert_eq!(format!("{v1}"), "counter(hits:10)");
    assert_eq!(v1, v1_ptr);
    assert_eq!(v1, v1_eq);
    assert_ne!(v1, v_diff);
    assert_eq!(
        v1.methods(),
        vec!["get".to_string(), "inc".to_string(), "cwd".to_string()]
    );

    // JSON projection: Some(Dict) vs None fallback
    let projected = value_to_json(&v1);
    let roundtripped = json_to_value(&projected).expect("valid projected JSON");
    match roundtripped {
        Value::Dict(ref map) => {
            assert_eq!(map.get("label"), Some(&Value::String("hits".to_string())));
            assert_eq!(map.get("count"), Some(&Value::Int(10)));
        }
        other => panic!("expected Dict, got: {other:?}"),
    }
    assert_eq!(value_to_json(&v_opaque), "\"<external:counter>\"");

    // Live VM method dispatch receiving `&mut CoreRuntime`
    let (prog, _w) =
        ParserV3::compile("fn(c) [type_of(c), c.inc(5i), c.get(), c.cwd(), methods_of(c)] end")
            .expect("compile closure");
    let (closure_val, _) =
        execute_program(Arc::new(prog), &mut core).expect("evaluate closure literal");
    let res = execute_value(closure_val, vec![v1.clone()], &mut core)
        .expect("VM invocation on Value::External");

    let canon_dir = dir.path().canonicalize().expect("canon dir");
    let expected_items = vec![
        Value::String("counter".to_string()),
        Value::Int(15),
        Value::Int(15),
        Value::String(canon_dir.display().to_string()),
        Value::List(
            vec![
                Value::String("get".to_string()),
                Value::String("inc".to_string()),
                Value::String("cwd".to_string()),
            ]
            .into(),
        ),
    ];
    assert_eq!(res, Value::List(expected_items.into()));

    // Unknown method returns clean MoraError::Runtime through VM dispatch
    let (bad_prog, _w) = ParserV3::compile("fn(c) c.nope() end").expect("compile bad call");
    let (bad_fn, _) = execute_program(Arc::new(bad_prog), &mut core).expect("eval closure");
    let err = execute_value(bad_fn, vec![v1], &mut core).expect_err("unknown method must fail");
    assert!(matches!(err, MoraError::Runtime(_)), "got: {err:?}");
}

// ─── 6. W2.6 Exec Reactor + Virtual CWD + Cancellation Closure ───────────────────

#[test]
fn exec_parallel_inherits_virtual_cwd_without_mutating_process_cwd() {
    let dir = TempDir::new("w2_6_exec_cwd");
    let sandbox = dir.join("sandbox");
    let sub = sandbox.join("sub");
    std::fs::create_dir_all(&sub).expect("create sandbox/sub");
    let expected_sub = sub.canonicalize().expect("canonicalize sandbox/sub");
    let host_cwd_before = std::env::current_dir().expect("host cwd before");

    let mut core = CoreRuntime::default()
        .with_fs_root(&sandbox)
        .expect("confine");

    let pwd_cmd = if cfg!(windows) { "cd" } else { "pwd" };
    let script = format!("file.chdir(\"sub\")\nexec.parallel([\"{pwd_cmd}\"])");
    let val =
        eval_with_core(&script, &mut core).expect("exec.parallel must succeed in virtual_cwd");

    let Value::List(items) = val else {
        panic!("expected list from exec.parallel, got: {val:?}");
    };
    assert_eq!(items.len(), 1);
    let Value::Dict(ref entry) = items[0] else {
        panic!("expected dict item from exec.parallel, got: {:?}", items[0]);
    };
    let Some(Value::String(stdout)) = entry.get("stdout") else {
        panic!("expected stdout string in exec.parallel item, got: {entry:?}");
    };
    let actual_cwd = std::path::Path::new(stdout.trim())
        .canonicalize()
        .expect("canonicalize reported child cwd");
    assert_eq!(actual_cwd, expected_sub);
    assert_eq!(
        std::env::current_dir().expect("host cwd after"),
        host_cwd_before,
        "process-global std::env::current_dir must remain untouched"
    );
}

#[test]
fn exec_parallel_honors_task_context_cancellation_before_and_during_execution() {
    let dir = TempDir::new("w2_6_exec_cancel");

    // 1. Pre-cancelled TaskContext must abort before spawning subprocesses.
    let pre_ctx = TaskContext::root();
    pre_ctx.cancel("pre-cancelled exec");
    let mut pre_core = CoreRuntime::default()
        .with_fs_root(dir.path())
        .expect("confine pre")
        .with_task_context(pre_ctx);

    let fast_cmd = "echo hi";
    let pre_err = eval_with_core(&format!("exec.parallel([\"{fast_cmd}\"])"), &mut pre_core)
        .expect_err("pre-cancelled TaskContext must abort exec.parallel");
    assert_eq!(pre_err.kind_name(), "cancelled");
    assert!(matches!(pre_err, MoraError::Cancelled(_)));

    // 2. Mid-flight cancellation must terminate running child process group promptly.
    let mid_ctx = TaskContext::root();
    let cancel_handle = mid_ctx.clone();
    let mut mid_core = CoreRuntime::default()
        .with_fs_root(dir.path())
        .expect("confine mid")
        .with_task_context(mid_ctx);

    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        cancel_handle.cancel("mid-flight exec abort");
    });

    let sleep_cmd = if cfg!(windows) {
        "ping -n 10 127.0.0.1 >nul"
    } else {
        "sleep 8"
    };
    let started = std::time::Instant::now();
    let mid_err = eval_with_core(&format!("exec.parallel([\"{sleep_cmd}\"])"), &mut mid_core)
        .expect_err("mid-flight cancelled TaskContext must abort exec.parallel");
    let elapsed = started.elapsed();
    canceller.join().expect("join canceller thread");

    assert_eq!(mid_err.kind_name(), "cancelled");
    assert!(matches!(mid_err, MoraError::Cancelled(_)));
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "mid-flight exec cancellation took too long: {elapsed:?}"
    );
}
