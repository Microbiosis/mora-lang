//! Wave 4 P3-B verification: Toolchain CLI (`mora fmt`, `mora test`), first-class `assert` /
//! `assert_eq` across HM/VM/MORABC02, and runnable `examples/*.mora` showcase verification.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use mora::parser_v3::ParserV3;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;

fn mora_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mora")
}

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static HM type error for:\n{src}\nbut typeck succeeded"
    );
}

#[test]
fn p3_mora_fmt_cli_check_inplace_stdout_stdin_and_idempotence() {
    let td = TempDir::new("p3_mora_fmt");
    let sub = td.path().join("pkg");
    std::fs::create_dir_all(&sub).expect("create subdir");
    let target = sub.join("sample.mora");

    let messy = "\
-- header comment
struct Config
name:string @tag(desc=\"cfg\")
limit:int=10
end

trait Named
task label(self):string
task greet(self):string
return \"hi:\" + self.label()
end
end

let xs:list<int> = [1,2,-3]
if xs.len()>0 then
print(xs[0]+(-1))  -- inline note
else
print(0)
end
";
    std::fs::write(&target, messy).expect("write messy.mora");

    // 1. `--check` detects unformatted file and exits 1
    let chk1 = Command::new(mora_bin())
        .args(["fmt", "--check"])
        .arg(td.path())
        .output()
        .expect("run mora fmt --check");
    assert_eq!(
        chk1.status.code(),
        Some(1),
        "expected exit 1 for unformatted file"
    );
    let chk1_out = String::from_utf8_lossy(&chk1.stdout);
    assert!(
        chk1_out.contains("sample.mora"),
        "expected sample.mora in --check output, got: {chk1_out}"
    );

    // 2. `--stdout` prints formatted text without modifying the file or printing banner
    let out_stdout = Command::new(mora_bin())
        .args(["fmt", "--stdout"])
        .arg(&target)
        .output()
        .expect("run mora fmt --stdout");
    assert!(out_stdout.status.success());
    assert!(
        out_stdout.stderr.is_empty(),
        "mora fmt --stdout must not emit banner on stderr: {}",
        String::from_utf8_lossy(&out_stdout.stderr)
    );
    let formatted_stdout = String::from_utf8_lossy(&out_stdout.stdout).into_owned();
    assert_eq!(
        std::fs::read_to_string(&target).expect("read original"),
        messy,
        "--stdout must not overwrite file on disk"
    );
    assert!(formatted_stdout.contains("name: string @tag(desc = \"cfg\")"));
    assert!(formatted_stdout.contains("let xs: list<int> = [1, 2, -3]"));
    assert!(formatted_stdout.contains("  print(xs[0] + (-1)) -- inline note"));
    assert!(formatted_stdout.contains("\nelse\n  print(0)\nend\n"));
    assert!(formatted_stdout.contains("  task label(self): string\n  task greet(self): string\n    return \"hi:\" + self.label()\n  end\nend\n"));

    // 3. Stdin mode (`mora fmt -`) matches `--stdout` byte-for-byte
    let mut child = Command::new(mora_bin())
        .args(["fmt", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mora fmt -");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(messy.as_bytes())
        .expect("write stdin");
    let stdin_out = child.wait_with_output().expect("wait mora fmt -");
    assert!(stdin_out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&stdin_out.stdout),
        formatted_stdout,
        "stdin formatting must match --stdout exactly"
    );

    // 4. In-place `mora fmt <dir>` rewrites cleanly and becomes strictly idempotent
    let run_fmt = Command::new(mora_bin())
        .arg("fmt")
        .arg(td.path())
        .output()
        .expect("run mora fmt");
    assert!(run_fmt.status.success());

    let chk2 = Command::new(mora_bin())
        .args(["fmt", "--check"])
        .arg(td.path())
        .output()
        .expect("run second mora fmt --check");
    assert!(
        chk2.status.success(),
        "expected exit 0 after formatting; stderr={}",
        String::from_utf8_lossy(&chk2.stderr)
    );

    // 5. Formatted script still executes with identical semantics
    let exec = Command::new(mora_bin())
        .arg(&target)
        .output()
        .expect("run formatted sample.mora");
    assert!(
        exec.status.success(),
        "formatted script failed to run: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&exec.stdout).trim(), "0");

    // 6. Verify formatter idempotence + parse preservation across embedded stdlib sources
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "std/testing.mora",
        "std/net/http.mora",
        "std/mcp.mora",
        "std/nn/safetensors.mora",
        "std/nn/gguf.mora",
    ] {
        let std_path = repo_root.join(rel);
        let pass1 = Command::new(mora_bin())
            .args(["fmt", "--stdout"])
            .arg(&std_path)
            .output()
            .expect("format stdlib pass 1");
        assert!(pass1.status.success(), "mora fmt failed on {rel}");
        let s1 = String::from_utf8_lossy(&pass1.stdout).into_owned();
        let mut child2 = Command::new(mora_bin())
            .args(["fmt", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("format stdlib pass 2");
        child2
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(s1.as_bytes())
            .expect("write pass 1");
        let pass2 = child2.wait_with_output().expect("wait pass 2");
        assert!(pass2.status.success());
        let s2 = String::from_utf8_lossy(&pass2.stdout);
        assert_eq!(s1, s2, "mora fmt is not idempotent on {rel}");
        ParserV3::compile(&s1).unwrap_or_else(|e| panic!("formatted {rel} failed to compile: {e}"));
    }
}

#[test]
fn p3_mora_test_cli_discovery_concurrency_filter_and_failure_reporting() {
    let td = TempDir::new("p3_mora_test");
    let sub = td.path().join("nested");
    std::fs::create_dir_all(&sub).expect("create nested dir");

    // 1. Multi-task test file (`*_test.mora`) with non-test helper task
    std::fs::write(
        td.path().join("math_test.mora"),
        r#"
let shared_base = 10

task helper_double(x: int): int
  return x * 2
end

task test_add(): nil
  assign shared_base = shared_base + 5
  assert_eq(shared_base, 15, "isolated top-level state")
end

task test_mul(): nil
  assert_eq(helper_double(21), 42)
  assert(shared_base == 10, "concurrent test must not see other test mutations")
end

task test_tensor_assert(): nil
  let a = tensor([1.0, 2.0, 3.0], dtype="f32")
  let b = a * 2.0f32
  assert_eq(b.shape(), [3])
  assert((b.sum() - 12.0f32).abs() < 1e-5f32, "tensor sum mismatch")
end
"#,
    )
    .expect("write math_test.mora");

    // 2. Top-level assertion test file (`test_*.mora`)
    std::fs::write(
        sub.join("test_top_level.mora"),
        r#"
let xs = [1, 2, 3, 4]
assert_eq(xs.len(), 4)
assert(xs[3] == 4, "last element should be 4")
"#,
    )
    .expect("write test_top_level.mora");

    // 3. Non-test helper file (must be ignored by directory discovery!)
    std::fs::write(
        td.path().join("non_test_helper.mora"),
        "assert(false, \"non-test files must not be executed by directory discovery\")\n",
    )
    .expect("write non_test_helper.mora");

    // Run all discovered tests concurrently via TaskScheduler (`--jobs 4`)
    let run_all = Command::new(mora_bin())
        .args(["test", "--jobs", "4"])
        .arg(td.path())
        .output()
        .expect("run mora test --jobs 4");
    let stdout_all = String::from_utf8_lossy(&run_all.stdout);
    let stderr_all = String::from_utf8_lossy(&run_all.stderr);
    assert!(
        run_all.status.success(),
        "mora test failed:\nstdout:\n{stdout_all}\nstderr:\n{stderr_all}"
    );
    assert!(
        stdout_all.contains("test result: ok. 4 passed; 0 failed; 0 filtered out"),
        "unexpected test summary:\n{stdout_all}"
    );
    assert!(stdout_all.contains("::test_add ... ok"));
    assert!(stdout_all.contains("::test_mul ... ok"));
    assert!(stdout_all.contains("::test_tensor_assert ... ok"));
    assert!(stdout_all.contains("test_top_level.mora::<top-level> ... ok"));

    // Run filtered subset (`--filter test_mul --jobs 1`)
    let run_flt = Command::new(mora_bin())
        .args(["test", "--filter", "test_mul", "--jobs=1"])
        .arg(td.path())
        .output()
        .expect("run mora test --filter");
    let stdout_flt = String::from_utf8_lossy(&run_flt.stdout);
    assert!(
        run_flt.status.success(),
        "filtered test failed:\n{stdout_flt}"
    );
    assert!(
        stdout_flt.contains("test result: ok. 1 passed; 0 failed; 3 filtered out"),
        "unexpected filtered summary:\n{stdout_flt}"
    );

    // Add failing runtime assertion test + static HM type error test
    std::fs::write(
        td.path().join("failing_test.mora"),
        r#"
task test_ok(): nil
  assert(true)
end

task test_boom(): nil
  assert_eq(2 + 2, 5, "math broke")
end
"#,
    )
    .expect("write failing_test.mora");

    std::fs::write(
        td.path().join("type_err_test.mora"),
        "assert_eq(1, \"not-an-int\")\n",
    )
    .expect("write type_err_test.mora");

    let run_fail = Command::new(mora_bin())
        .arg("test")
        .arg(td.path())
        .output()
        .expect("run mora test with failures");
    let stdout_fail = String::from_utf8_lossy(&run_fail.stdout);
    assert_eq!(
        run_fail.status.code(),
        Some(1),
        "expected exit code 1 when tests fail:\n{stdout_fail}"
    );
    assert!(stdout_fail.contains("::test_ok ... ok"), "{stdout_fail}");
    assert!(
        stdout_fail.contains("::test_boom ... FAILED"),
        "{stdout_fail}"
    );
    assert!(stdout_fail.contains("math broke"), "{stdout_fail}");
    assert!(
        stdout_fail.contains("type_err_test.mora ... FAILED"),
        "{stdout_fail}"
    );
    assert!(
        stdout_fail.contains("test result: FAILED. 5 passed; 2 failed; 0 filtered out"),
        "unexpected failure summary:\n{stdout_fail}"
    );
}

#[test]
fn p3_assert_and_assert_eq_static_hm_and_aot_bytecode_parity() {
    // Static HM catches invalid argument types at compile time
    assert_typecheck_fails("assert(123)\n");
    assert_typecheck_fails("assert(true, 42)\n");
    assert_typecheck_fails("assert_eq(1, \"1\")\n");
    assert_typecheck_fails("assert_eq(1, 2, 99)\n");

    // MORABC02 bytecode codec round-trip for BuiltinFnId::Assert (43) & BuiltinFnId::AssertEq (44)
    let td = TempDir::new("p3_assert_bc");
    let src_path = td.path().join("check_assert.mora");
    let bc_path = td.path().join("check_assert.morac");
    std::fs::write(
        &src_path,
        r#"
assert(3 > 2, "three > two")
assert_eq("mora", "mora")
assert_eq([1, 2], [1, 2], "list equality")
print("bc_assert_ok")
"#,
    )
    .expect("write check_assert.mora");

    let build = Command::new(mora_bin())
        .args(["build", "--emit-bc"])
        .arg(&src_path)
        .arg("-o")
        .arg(&bc_path)
        .output()
        .expect("mora build --emit-bc");
    assert!(
        build.status.success(),
        "mora build --emit-bc failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run_bc = Command::new(mora_bin())
        .arg(&bc_path)
        .output()
        .expect("run check_assert.morac");
    assert!(
        run_bc.status.success(),
        "running .morac failed: {}",
        String::from_utf8_lossy(&run_bc.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_bc.stdout).contains("bc_assert_ok"),
        "missing expected output from .morac"
    );
}

#[test]
fn p3_all_examples_typecheck_and_run_cleanly() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = repo_root.join("examples");
    let mut examples: Vec<_> = std::fs::read_dir(&examples_dir)
        .expect("read examples/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mora"))
        .collect();
    examples.sort();
    assert!(
        examples.len() >= 4,
        "expected >= 4 runnable examples in examples/, found {}",
        examples.len()
    );

    for path in &examples {
        let name = path.file_name().unwrap().to_string_lossy();
        let chk = Command::new(mora_bin())
            .args(["--check"])
            .arg(path)
            .output()
            .expect("run mora --check on example");
        assert!(
            chk.status.success(),
            "mora --check failed on example {name}:\nstderr: {}",
            String::from_utf8_lossy(&chk.stderr)
        );

        let run = Command::new(mora_bin())
            .arg(path)
            .output()
            .expect("run example with mora");
        assert!(
            run.status.success(),
            "example {name} failed to run:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(
            !stdout.trim().is_empty(),
            "example {name} produced empty stdout"
        );
        if name == "ai_compute_dag_and_autograd.mora" {
            assert!(stdout.contains("FusedRmsNormLinear"));
            assert!(stdout.contains("FusedFlashAttention"));
            assert!(stdout.contains("Reclaimed free pages:"));
        } else if name == "ml_weights_safetensors_gguf.mora" {
            assert!(stdout.contains("f32/f16/bf16 bit-exact PASS"));
            assert!(stdout.contains("GGUF v3 bytes:"));
        } else if name == "cloud_http_https_mcp.mora" {
            assert!(stdout.contains("prompt_len=10,budget=64"));
            assert!(stdout.contains("mora-cloud-ready"));
        }
    }

    // Verify all 3 new showcase examples pass `mora fmt --check` cleanly
    let fmt_chk = Command::new(mora_bin())
        .args(["fmt", "--check"])
        .arg(examples_dir.join("ai_compute_dag_and_autograd.mora"))
        .arg(examples_dir.join("ml_weights_safetensors_gguf.mora"))
        .arg(examples_dir.join("cloud_http_https_mcp.mora"))
        .output()
        .expect("run mora fmt --check on showcase examples");
    assert!(
        fmt_chk.status.success(),
        "new examples should be cleanly formatted:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&fmt_chk.stdout),
        String::from_utf8_lossy(&fmt_chk.stderr)
    );
}
