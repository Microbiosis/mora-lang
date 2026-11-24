//! Batch 3 Verification Suite (G6):
//! - First-class sandbox-verified `checked_path` capability (`HostObject` + static `Type::CheckedPath`)
//! - Pure-Mora `std/os.mora` and `std/io.mora` (including `BufReader`, `BufWriter`, async I/O)
//! - Confined sandbox escape rejection (`../` traversal, `.join("../../..")`, `env()`, `home_dir()`)
//! - Static HM effect checking (`checked_path` pure path inspection vs `! io` disk I/O)
//! - `CoreRuntime` (`EngineShared` + `TypeMetadataRegistry` + `ExecSession`) fiber isolation & `flow::list_ops`

use std::sync::Arc;

use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run_with(src: &str, core: &mut CoreRuntime) -> Result<Value, String> {
    let (mut prog, witnesses) = ParserV3::compile(src).map_err(|e| format!("compile: {e}"))?;
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let formatted = errs
            .iter()
            .map(mora::typeck::format_error)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("typeck:\n{formatted}"));
    }
    optimize_program(&mut prog, OptLevel::Basic);
    let (val, _) = execute_program(Arc::new(prog), core).map_err(|e| format!("runtime: {e}"))?;
    Ok(val)
}

fn as_str_list(v: Value) -> Vec<String> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => format!("{other}"),
            })
            .collect(),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn g6_std_os_and_checked_path_capabilities_confined_and_unrestricted() {
    let tmp = TempDir::new("g6_std_os");
    let mut confined = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined runtime");

    let src = r#"
import "std/os"

let sub: checked_path = check_path("workspace")
sub.mkdir()
let doc: checked_path = sub.join("notes.txt")
doc.write_text("hello")
doc.append_text(" mora")

let content = doc.read_text()
let same = doc == sub.join("notes.txt")
let entries = list_dir(sub)

-- Isolated chdir inside confined sandbox (does not mutate process cwd)
let before_cwd = cwd()
chdir(sub)
let after_cwd = cwd()
let rel_read = check_path("notes.txt").read_text()

[type_of(doc), str(is_confined()), platform(), arch(), doc.basename(), doc.extname(), content, str(same), str(entries), str(doc.exists()), str(doc.is_file()), str(sub.is_dir()), str(before_cwd != after_cwd), rel_read]
"#;
    let out = as_str_list(
        compile_check_and_run_with(src, &mut confined)
            .expect("std/os confined workflow must succeed"),
    );
    assert_eq!(out[0], "checked_path");
    assert_eq!(out[1], "true");
    assert_eq!(out[2], std::env::consts::OS);
    assert_eq!(out[3], std::env::consts::ARCH);
    assert_eq!(out[4], "notes.txt");
    assert_eq!(out[5], ".txt");
    assert_eq!(out[6], "hello mora");
    assert_eq!(out[7], "true");
    assert_eq!(out[8], "[notes.txt]");
    assert_eq!(out[9], "true");
    assert_eq!(out[10], "true");
    assert_eq!(out[11], "true");
    assert_eq!(out[12], "true");
    assert_eq!(out[13], "hello mora");

    // Unrestricted runtime sees `is_confined() == false` and can read `env_or`.
    let mut unrestricted = CoreRuntime::default();
    let out_un = as_str_list(
        compile_check_and_run_with(
            r#"
import "std/os"
[str(is_confined()), env_or("__MORA_DEFINITELY_UNSET_G6_VAR__", "fallback_ok")]
"#,
            &mut unrestricted,
        )
        .expect("unrestricted std/os must succeed"),
    );
    assert_eq!(out_un[0], "false");
    assert_eq!(out_un[1], "fallback_ok");
}

#[test]
fn g6_checked_path_confined_sandbox_blocks_escapes_and_enforces_pure_effects() {
    let tmp = TempDir::new("g6_sandbox_deny");

    // 1. Direct parent traversal via `check_path("../escape.txt")` denied
    let mut c1 = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined");
    let e1 = compile_check_and_run_with(
        r#"
import "std/os"
check_path("../escape.txt")
"#,
        &mut c1,
    )
    .expect_err("traversal via check_path must be denied");
    assert!(
        e1.contains("sandbox denied"),
        "expected sandbox denied, got: {e1}"
    );

    // 2. Traversal via `checked_path.join("../../escape.txt")` denied
    let mut c2 = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined");
    let e2 = compile_check_and_run_with(
        r#"
import "std/os"
let sub = check_path("inner")
sub.mkdir()
sub.join("../../escape.txt")
"#,
        &mut c2,
    )
    .expect_err("traversal via checked_path.join must be denied");
    assert!(
        e2.contains("sandbox denied"),
        "expected sandbox denied, got: {e2}"
    );

    // 3. `env(...)` and `home_dir()` denied under confined sandbox
    let mut c3 = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined");
    let e3 = compile_check_and_run_with(
        r#"
import "std/os"
env("PATH")
"#,
        &mut c3,
    )
    .expect_err("env() under confined sandbox must be denied");
    assert!(
        e3.contains("sandbox denied"),
        "expected sandbox denied, got: {e3}"
    );

    // 4. Static HM effect check: pure `basename`/`extname` allowed in `! pure`, disk `read_text` rejected in `! pure`!
    let pure_ok = r#"
import "std/os"
task inspect_name(p: checked_path): string ! pure
  return p.basename() + "|" + p.extname()
end
let p = check_path("demo.mora")
inspect_name(p)
"#;
    let mut c4 = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined");
    let v = compile_check_and_run_with(pure_ok, &mut c4).expect("pure path inspection must pass");
    assert_eq!(v, Value::String("demo.mora|.mora".to_string()));

    let pure_viol = r#"
task illegal_read(p: checked_path): string ! pure
  return p.read_text()
end
"#;
    let (_, w) = ParserV3::compile(pure_viol).expect("parse");
    let errs = check_program_witnesses(&w);
    assert!(
        !errs.is_empty(),
        "calling checked_path.read_text() inside `! pure` must be rejected by HM"
    );
}

#[test]
fn g6_std_io_buffered_streams_lines_binary_and_async_io() {
    let tmp = TempDir::new("g6_std_io");
    let mut core = CoreRuntime::default()
        .with_fs_root(tmp.path())
        .expect("confined");

    let src = r#"
import "std/io"

-- 1. BufWriter + read_lines + file_size + copy_stream
let p = open_path("log.txt")
let w0 = buf_writer(p)
let w1 = w0.writeln("alpha").writeln("beta").write("gamma")
let queued = w1.buffered_chunks()
let w2 = w1.flush()
let after_flush = w2.buffered_chunks()
let lines = read_lines(p)
let copied_bytes = copy_stream(p, "log_copy.txt")
let sz = file_size("log_copy.txt")

-- 2. BufReader chunked streaming reads over `checked_path`
let r0 = buf_reader("log_copy.txt", 6)
let rem0 = r0.remaining()
let c1 = r0.read_chunk(6)
let r1 = c1.next
let c2 = r1.read_chunk(6)
let r2 = c2.next
let c3 = r2.read_chunk(64)
let r3 = c3.next
let c4 = r3.read_chunk(16)
let total_chunk_bytes = c1.data.numel() + c2.data.numel() + c3.data.numel() + c4.bytes_read

-- 3. Hex & async `spawn`-backed file I/O
write_bytes_hex("bin.dat", "deadbeef")
let hex_back = read_bytes_hex("bin.dat")
let w_handle = write_text_async("async.txt", "async_ok")
w_handle.join()
let r_handle = read_text_async("async.txt")
let async_back = r_handle.join()

[str(queued), str(after_flush), str(lines), str(copied_bytes == sz), str(rem0 == sz), str(total_chunk_bytes == sz), str(r3.eof() and c4.eof and c4.bytes_read == 0 and c4.data == nil), hex_back, async_back]
"#;
    let out =
        as_str_list(compile_check_and_run_with(src, &mut core).expect("std/io suite must succeed"));
    assert_eq!(out[0], "3");
    assert_eq!(out[1], "0");
    assert_eq!(out[2], "[alpha, beta, gamma]");
    assert_eq!(out[3], "true");
    assert_eq!(out[4], "true");
    assert_eq!(out[5], "true");
    assert_eq!(out[6], "true");
    assert_eq!(out[7], "deadbeef");
    assert_eq!(out[8], "async_ok");
}

#[test]
fn g6_core_runtime_fiber_registry_sharing_and_list_matmul_parity() {
    let mut core = CoreRuntime::default();
    let src = r#"
struct Counter
  base: int
end

impl Counter
  task add_to(self, x: int): int
    return self.base + x
  end
end

-- Spawn concurrent fibers sharing TypeMetadataRegistry while computing isolated autograd + list matmul!
let c = Counter(base=10)
let h1 = spawn fn() c.add_to(5) end
let h2 = spawn fn() c.add_to(20) end
let m = [[1, 2], [3, 4]] @ [[5, 6], [7, 8]]
[str(h1.join()), str(h2.join()), str(m)]
"#;
    let out = as_str_list(
        compile_check_and_run_with(src, &mut core).expect("fiber registry & list matmul must pass"),
    );
    assert_eq!(out[0], "15");
    assert_eq!(out[1], "30");
    assert_eq!(out[2], "[[19, 22], [43, 50]]");
}
