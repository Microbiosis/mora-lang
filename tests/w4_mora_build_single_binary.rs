//! Wave 4 Step 4 verification: `mora build` single-binary compilation.
//! - Compiles a `.mora` app (with transitive local `.mora` imports AND `std/net/http` + `std/mcp`)
//!   into a standalone executable via `mora build <entry.mora> -o <out_bin>`.
//! - Removes the original `.mora` source directory and executes the standalone binary directly
//!   from an empty directory to prove self-contained single-binary execution.
//! - Verifies `mora build` rejects type errors up front without emitting a broken binary.

use std::process::Command;

use mora::test_support::TempDir;

#[test]
fn w4_mora_build_produces_self_contained_standalone_executable() {
    let mora_bin = env!("CARGO_BIN_EXE_mora");
    let src_dir = TempDir::new("w4_build_src");
    let out_dir = TempDir::new("w4_build_out");
    let run_dir = TempDir::new("w4_build_empty_cwd");

    let helper_path = src_dir.join("math_helper.mora");
    std::fs::write(
        &helper_path,
        "export task square(x: int): int\n  return x * x\nend\n",
    )
    .expect("write helper");

    let entry_path = src_dir.join("app.mora");
    std::fs::write(
        &entry_path,
        r#"
import "math_helper.mora"
import "std/net/http"
import "std/mcp"

task main()
  let srv = server("bundled-mcp", "1.2.3")
  let payload: dict<string, any> = {}
  assign payload = payload.set("sq", square(7)).set("srv", srv.name)
  let resp = json_response(200, payload)
  print(resp.body)
end
"#,
    )
    .expect("write entry");

    let exe_name = if cfg!(windows) {
        "standalone_app.exe"
    } else {
        "standalone_app"
    };
    let out_bin = out_dir.join(exe_name);

    let build_out = Command::new(mora_bin)
        .current_dir(src_dir.path())
        .args(["build", "app.mora", "-o"])
        .arg(&out_bin)
        .output()
        .expect("run mora build");
    assert!(
        build_out.status.success(),
        "mora build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );
    assert!(out_bin.exists(), "built executable must exist");

    // Delete source directory files so we prove the binary has zero source-file dependencies.
    let _ = std::fs::remove_file(&entry_path);
    let _ = std::fs::remove_file(&helper_path);

    let run_out = Command::new(&out_bin)
        .current_dir(run_dir.path())
        .output()
        .expect("execute standalone mora binary");
    assert!(
        run_out.status.success(),
        "standalone binary exited with failure:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        stdout.contains("49") && stdout.contains("bundled-mcp"),
        "expected bundled JSON output containing sq=49 and srv=bundled-mcp, got stdout={stdout:?}"
    );
    assert!(
        !stderr.contains("AI-native statically-typed language"),
        "standalone binary must not emit dev CLI banner, got stderr={stderr:?}"
    );
}

#[test]
fn w4_mora_build_rejects_type_errors_and_emits_no_binary() {
    let mora_bin = env!("CARGO_BIN_EXE_mora");
    let dir = TempDir::new("w4_build_bad");
    let bad_src = dir.join("bad.mora");
    let bad_out = dir.join(if cfg!(windows) { "bad.exe" } else { "bad" });
    std::fs::write(&bad_src, "let x: int = \"not_an_int\"\n").expect("write bad");

    let res = Command::new(mora_bin)
        .args(["build"])
        .arg(&bad_src)
        .arg("-o")
        .arg(&bad_out)
        .output()
        .expect("run mora build on invalid program");
    assert!(
        !res.status.success(),
        "mora build must fail on static type error"
    );
    assert!(
        !bad_out.exists(),
        "mora build must not emit executable when typecheck fails"
    );
}
