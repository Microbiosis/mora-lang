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
        "-- HELPER_SECRET_SOURCE_COMMENT_99173\nexport task square(x: int): int\n  return x * x\nend\n",
    )
    .expect("write helper");

    let entry_path = src_dir.join("app.mora");
    std::fs::write(
        &entry_path,
        r#"
-- ENTRY_SECRET_SOURCE_COMMENT_88421
import "math_helper.mora"
import "std/net/http"
import "std/mcp"

task main()
  let srv = server("bundled-mcp", "1.2.3")
  let df = grad(fn(x: float) x * x * 3.0 end)
  let payload: dict<string, any> = {}
  assign payload = payload.set("sq", square(7)).set("srv", srv.name).set("deriv", df(2.0))
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

    // Verify MORABC02TAIL footer & prove ZERO raw `.mora` source comments were embedded!
    let bin_bytes = std::fs::read(&out_bin).expect("read built binary");
    assert!(
        bin_bytes.ends_with(b"MORABC02TAIL"),
        "built executable must end with MORABC02TAIL AOT bytecode trailer"
    );
    assert!(
        !bin_bytes
            .windows(b"ENTRY_SECRET_SOURCE_COMMENT_88421".len())
            .any(|w| w == b"ENTRY_SECRET_SOURCE_COMMENT_88421"),
        "AOT MORABC02 binary must NOT embed raw entry source text"
    );
    assert!(
        !bin_bytes
            .windows(b"HELPER_SECRET_SOURCE_COMMENT_99173".len())
            .any(|w| w == b"HELPER_SECRET_SOURCE_COMMENT_99173"),
        "AOT MORABC02 binary must NOT embed raw imported module source text"
    );

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
        stdout.contains("49") && stdout.contains("bundled-mcp") && stdout.contains("12.0"),
        "expected bundled JSON output containing sq=49, srv=bundled-mcp, deriv=12.0, got stdout={stdout:?}"
    );
    assert!(
        !stderr.contains("AI-native statically-typed language"),
        "standalone binary must not emit dev CLI banner, got stderr={stderr:?}"
    );
}

#[test]
fn p2_mora_build_emit_bc_morac_zero_source_execution_and_checksum_guard() {
    let mora_bin = env!("CARGO_BIN_EXE_mora");
    let src_dir = TempDir::new("p2_morac_src");
    let out_dir = TempDir::new("p2_morac_out");
    let run_dir = TempDir::new("p2_morac_run");

    let mod_path = src_dir.join("calc.mora");
    std::fs::write(
        &mod_path,
        "-- CALC_PRIVATE_COMMENT_777\nexport task cube(n: int): int\n  return n * n * n\nend\n",
    )
    .expect("write calc");
    let entry_path = src_dir.join("main.mora");
    std::fs::write(
        &entry_path,
        "-- MAIN_PRIVATE_COMMENT_666\nimport \"calc.mora\"\ntask main()\n  print(\"cube=\", cube(4))\nend\n",
    )
    .expect("write main");

    let bc_path = out_dir.join("main.morac");
    let build = Command::new(mora_bin)
        .current_dir(src_dir.path())
        .args(["--opt=2", "build", "main.mora", "--emit-bc", "-o"])
        .arg(&bc_path)
        .output()
        .expect("run mora build --emit-bc");
    assert!(
        build.status.success(),
        "mora build --emit-bc failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let bc_bytes = std::fs::read(&bc_path).expect("read .morac");
    assert!(
        bc_bytes.starts_with(b"MORABC02"),
        ".morac must start with MORABC02 header"
    );
    assert!(
        !bc_bytes
            .windows(b"MAIN_PRIVATE_COMMENT_666".len())
            .any(|w| w == b"MAIN_PRIVATE_COMMENT_666"),
        ".morac must not contain raw entry source"
    );
    assert!(
        !bc_bytes
            .windows(b"CALC_PRIVATE_COMMENT_777".len())
            .any(|w| w == b"CALC_PRIVATE_COMMENT_777"),
        ".morac must not contain raw imported module source"
    );

    // Delete original source files before executing `.morac`!
    let _ = std::fs::remove_file(&entry_path);
    let _ = std::fs::remove_file(&mod_path);

    for args in [
        vec![bc_path.to_string_lossy().to_string()],
        vec!["run".to_string(), bc_path.to_string_lossy().to_string()],
    ] {
        let run_out = Command::new(mora_bin)
            .current_dir(run_dir.path())
            .args(&args)
            .output()
            .expect("execute .morac");
        assert!(
            run_out.status.success(),
            "executing .morac with {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run_out.stdout),
            String::from_utf8_lossy(&run_out.stderr)
        );
        let stdout = String::from_utf8_lossy(&run_out.stdout);
        assert!(
            stdout.contains("cube=") && stdout.contains("64"),
            "expected 'cube=' and '64' from .morac, got stdout={stdout:?}"
        );
    }

    // Corrupt a payload byte and verify checksum rejection with exit code 2
    let bad_bc = out_dir.join("corrupt.morac");
    let mut bad_bytes = bc_bytes;
    bad_bytes[12] ^= 0xAA;
    std::fs::write(&bad_bc, &bad_bytes).expect("write corrupt.morac");
    let bad_run = Command::new(mora_bin)
        .arg(&bad_bc)
        .output()
        .expect("run corrupt .morac");
    assert_eq!(
        bad_run.status.code(),
        Some(2),
        "corrupt .morac must fail with exit code 2"
    );
    assert!(
        String::from_utf8_lossy(&bad_run.stderr).contains("checksum mismatch"),
        "expected checksum mismatch diagnostic, got: {}",
        String::from_utf8_lossy(&bad_run.stderr)
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
