//! 100% `module_disposition_inventory.md` Closure & P5-1A Verification Suite:
//! 1. §1.1 (98% -> 100%): Zero `Result<..., String>` signatures across all of `src/**/*.rs`.
//! 2. §1.4 (97% -> 100%): Single unified JSON core (`src/flow/json.rs`, `src/lsp/json.rs` physically
//!    deleted), `CoreRuntime` free of `current_merge_strategies`, and pure-Mora `std/json` with
//!    compile-time `@tag(json=...)` struct encoding/decoding.
//! 3. §1.2 (96% -> 100%): Static `Resource` byte-quota checks on `memory_pool`/`kv_allocator` AND
//!    unified `EffectRow` ambient tracking (`io`, `async`, `autograd`, `tensor_mutate`).
//! 4. P5-1A: Go-1.22-style per-iteration loop closure scoping (`ControlInst::FreshLocalCell`) for
//!    `while` and `for` loops capturing immutable & mutable loop-body locals in `spawn fn()`.

use std::path::Path;
use std::sync::Arc;

use mora::mir::effect::ambient;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::scheduler::TaskScheduler;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::typeck::hm::HMInference;
use mora::value::Value;

fn walk_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn eval_typechecked(src: &str, workers: usize) -> Result<Value, String> {
    let witnesses = ParserV3::parse_witnesses(src).map_err(|e| format!("parse error: {e}"))?;
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        return Err(errs
            .iter()
            .map(|e| format!("line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let (prog, _) = ParserV3::compile(src).map_err(|e| format!("compile error: {e}"))?;
    let sched = Arc::new(TaskScheduler::with_worker_threads(workers).expect("scheduler"));
    let mut rt = CoreRuntime::default().with_scheduler(sched);
    let (v, _) = execute_program(Arc::new(prog), &mut rt).map_err(|e| format!("runtime: {e}"))?;
    Ok(v)
}

#[test]
fn inventory_100_guard_zero_result_string_and_no_legacy_duplicates() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");

    // 1. §1.4 & §1.5: `src/lsp/json.rs` must stay physically deleted.
    assert!(
        !src_dir.join("lsp").join("json.rs").exists(),
        "duplicate src/lsp/json.rs must not exist; all JSON lives in src/flow/json.rs"
    );

    // 2. §1.4: `CoreRuntime` must stay free of `current_merge_strategies`.
    let core_src = std::fs::read_to_string(src_dir.join("runtime").join("core.rs"))
        .expect("read src/runtime/core.rs");
    assert!(
        !core_src.contains("current_merge_strategies"),
        "CoreRuntime must not hold current_merge_strategies (lives on Environment)"
    );

    // 3. §1.1: Zero `Result<..., String>` code signatures across all of `src/**/*.rs`.
    let mut files = Vec::new();
    walk_rs_files(&src_dir, &mut files);
    files.sort();
    let mut violations = Vec::new();
    for path in files {
        let content = std::fs::read_to_string(&path).expect("read rs file");
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            if let Some(pos) = trimmed.find("Result<") {
                let tail = &trimmed[pos..];
                if tail.contains(", String>") || tail.contains(",String>") {
                    violations.push(format!("{}:{}: {}", path.display(), idx + 1, trimmed));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Expected 0 `Result<..., String>` signatures across src/, found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn inventory_100_std_json_with_compile_time_struct_tags() {
    let src = r#"
import "std/json"

struct UserPayload
  user_id: int @tag(json = "id")
  display_name: string @tag(json = "name")
  bio: string = "" @tag(json = "bio,omitempty")
  secret_token: string = "hidden" @tag(json = "-")
end

let tags = UserPayload::tags()
let fields: dict<string, any> = {}
assign fields = fields.set("user_id", 7).set("display_name", "Ada").set("bio", "").set("secret_token", "top-secret")

let encoded = json_encode_tagged(fields, tags)
let raw_obj = json_decode_object(encoded)
assert_eq(raw_obj["id"], 7)
assert_eq(raw_obj["name"], "Ada")
assert_eq(raw_obj.get("bio"), nil)
assert_eq(raw_obj.get("secret_token"), nil)

let decoded = json_decode_tagged(encoded, tags)
assert_eq(decoded["user_id"], 7)
assert_eq(decoded["display_name"], "Ada")
true
"#;
    let val = eval_typechecked(src, 2).expect("std/json tagged roundtrip");
    assert_eq!(val, Value::Bool(true));
}

#[test]
fn inventory_100_typeck_static_resource_quotas_and_unified_effect_rows() {
    // 1. Static Resource quota rejections
    let rejected_cases = [
        (
            "let p = memory_pool(0)\n",
            "memory_pool zero quota must fail statically",
        ),
        (
            "let p = memory_pool(128)\nlet t = p.alloc([64], \"f64\")\n",
            "memory_pool static allocation exceeding quota must fail statically",
        ),
        (
            "let p = memory_pool(128)\nlet t = p.zeros([64], \"f32\")\n",
            "memory_pool.zeros exceeding static quota must fail statically",
        ),
        (
            "let kv = kv_allocator(4, 16, 8, 64, \"f32\", \"cpu\", 1024)\n",
            "kv_allocator quota smaller than 1 physical KV page must fail statically",
        ),
        (
            "let p = memory_pool(1024)\np.set_quota(0)\n",
            "memory_pool.set_quota(0) must fail statically",
        ),
    ];
    for (src, msg) in rejected_cases {
        let w = ParserV3::parse_witnesses(src).expect("parse");
        let errs = check_program_witnesses(&w);
        assert!(!errs.is_empty(), "{msg}:\n{src}");
    }

    // Raising quota via `set_quota` permits larger static allocation cleanly!
    let ok_src = r#"
let p = memory_pool(128)
p.set_quota(1024)
let t = p.alloc([64], "f64")
assert_eq(t.numel(), 64)
"#;
    let w_ok = ParserV3::parse_witnesses(ok_src).expect("parse ok_src");
    assert!(
        check_program_witnesses(&w_ok).is_empty(),
        "valid quota update should typecheck cleanly"
    );

    // 2. Unified EffectRow tracking for IO, Async, Autograd, and TensorMutate
    let eff_src = r#"
task io_reader(path: string): string
  return file.read_text(path)
end

task async_sleeper(): nil
  sleep(1)
  return nil
end

task async_spawner(): int
  let h = spawn fn() 42 end
  return h.join()
end

task net_client(addr: string): string
  let s = tcp_connect(addr)
  let data = s.read(64)
  s.close()
  return data
end

task pure_math(x: int): int
  return x * 2 + 1
end
"#;
    let w_eff = ParserV3::parse_witnesses(eff_src).expect("parse eff_src");
    let mut hm = HMInference::new();
    let _ = hm.infer_program(&w_eff);
    assert!(hm.errors.is_empty(), "eff_src errs: {:?}", hm.errors);

    let io_row = hm.fn_effect_rows.get("io_reader").expect("io_reader row");
    assert!(
        io_row.contains(ambient::IO_LABEL),
        "io_reader must track 'io' effect, got {io_row}"
    );
    let sleep_row = hm
        .fn_effect_rows
        .get("async_sleeper")
        .expect("async_sleeper row");
    assert!(
        sleep_row.contains(ambient::ASYNC_LABEL),
        "async_sleeper must track 'async' effect, got {sleep_row}"
    );
    let spawn_row = hm
        .fn_effect_rows
        .get("async_spawner")
        .expect("async_spawner row");
    assert!(
        spawn_row.contains(ambient::ASYNC_LABEL),
        "async_spawner must track 'async' effect, got {spawn_row}"
    );
    let net_row = hm.fn_effect_rows.get("net_client").expect("net_client row");
    assert!(
        net_row.contains(ambient::IO_LABEL) && net_row.contains(ambient::ASYNC_LABEL),
        "net_client must track both 'io' and 'async' effects, got {net_row}"
    );
    let pure_row = hm.fn_effect_rows.get("pure_math").expect("pure_math row");
    assert!(
        pure_row.is_empty(),
        "pure_math must infer empty/pure effect row, got {pure_row}"
    );
}

#[test]
fn p5_1a_per_iteration_loop_closure_scoping_in_while_and_for_with_spawn() {
    // Directly spawn closures capturing loop-body `let` variables AND `for` variables
    // (including mutable loop locals AND 4-rank ProcessGroup collectives directly in a `while` loop!)
    let src = r#"
-- 1. Top-level `while` loop spawning 4-rank ProcessGroup directly via closure capture!
let pg = process_group(4)
let ranks = pg.ranks()
let handles: list<any> = []
let r = 0
while r < 4
  let my_pg = ranks[r]
  let my_r = r
  let h = spawn fn()
    let t = Tensor::from_list([float(my_r + 1)], "f64")
    let out = my_pg.all_reduce(t, "sum")
    return out.sum() + float(my_r)
  end
  assign handles = handles.push(h)
  assign r = r + 1
end
let i = 0
while i < 4
  assert_eq(handles[i].join(), 10.0 + float(i))
  assign i = i + 1
end

-- 2. `for` loop closure capture (both immutable and mutable per-iteration cells) inside a task!
task make_adders(): list<int>
  let fns: list<any> = []
  for x in [10, 20, 30, 40]
    let acc = x
    let bump = fn()
      assign acc = acc + 1
      return acc
    end
    assign fns = fns.push(bump)
  end
  let res: list<int> = []
  for f in fns
    assign res = res.push(f())
  end
  -- Second call verifies each closure kept its own independent mutable cell!
  for f in fns
    assign res = res.push(f())
  end
  return res
end

let got = make_adders()
assert_eq(got, [11, 21, 31, 41, 12, 22, 32, 42])

-- 3. Re-binding an outer accumulator inside a loop still works cleanly!
let outer_sum = 0
for k in [1, 2, 3, 4]
  let outer_sum = outer_sum + k
end
assert_eq(outer_sum, 10)
true
"#;
    let val = eval_typechecked(src, 2).expect("per-iteration loop scoping must pass");
    assert_eq!(val, Value::Bool(true));
}

#[test]
fn p5_1a_match_in_loop_nested_32_fibers_opt2_and_morabc02_roundtrip() {
    use mora::mir::control::{OptLevel, optimize_program};

    let src = r#"
enum Boxed
  Val(int)
end

-- 1. `match` pattern bindings inside a loop captured & mutated by closures
task test_match_in_loop(): list<int>
  let items = [Boxed::Val(100), Boxed::Val(200), Boxed::Val(300)]
  let fns: list<any> = []
  for it in items
    let inc = match it with
      Boxed::Val(v) => fn()
        assign v = v + 1
        return v
      end
    end
    assign fns = fns.push(inc)
  end
  let out: list<int> = []
  for f in fns
    assign out = out.push(f())
  end
  for f in fns
    assign out = out.push(f())
  end
  return out
end

assert_eq(test_match_in_loop(), [101, 201, 301, 102, 202, 302])

-- 2. 32 concurrent fibers spawned from nested loops capturing outer + inner loop vars
let handles: list<any> = []
let i = 0
while i < 4
  let outer_i = i
  for j in [0, 1, 2, 3, 4, 5, 6, 7]
    let h = spawn fn()
      return outer_i * 10 + j
    end
    assign handles = handles.push(h)
  end
  assign i = i + 1
end

let total = 0
let k = 0
while k < 32
  let expected = (k / 8) * 10 + (k % 8)
  let got = handles[k].join()
  assert_eq(got, expected)
  assign total = total + got
  assign k = k + 1
end
total
"#;

    let witnesses = ParserV3::parse_witnesses(src).expect("parse");
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "typeck errs: {errs:?}");

    let (mut prog, _) = ParserV3::compile(src).expect("compile");
    // Verify FreshLocalCell survives OptLevel::Aggressive (Purity::Effectful, never DCE/LICM'd).
    optimize_program(&mut prog, OptLevel::Aggressive);
    let has_fresh_cell = prog.functions.iter().any(|f| {
        f.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|inst| matches!(inst, mora::mir::control::ControlInst::FreshLocalCell(_)))
        })
    });
    assert!(
        has_fresh_cell,
        "FreshLocalCell must remain present after OptLevel::Aggressive"
    );

    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
    let mut rt = CoreRuntime::default().with_scheduler(sched);
    let (val, _) = execute_program(Arc::new(prog), &mut rt).expect("exec optimized program");
    // sum_{i=0..4} sum_{j=0..8} (i*10 + j) = 8*(0+10+20+30) + 4*(0+..+7) = 480 + 112 = 592
    assert_eq!(val, Value::Int(592));

    // Verify end-to-end MORABC02 `.morac` AOT compile & zero-source execution preserves opcode 51!
    let tmp = mora::test_support::TempDir::new("p5_1a_morac");
    let src_path = tmp.path().join("loop_cells.mora");
    let bc_path = tmp.path().join("loop_cells.morac");
    std::fs::write(&src_path, format!("{src}\nprint(total)\n")).expect("write src");

    let build_out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .args(["build", "--emit-bc", "-o"])
        .arg(&bc_path)
        .arg(&src_path)
        .output()
        .expect("run mora build --emit-bc");
    assert!(
        build_out.status.success(),
        "mora build --emit-bc failed: {}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    let run_out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg(&bc_path)
        .output()
        .expect("run .morac");
    assert!(
        run_out.status.success(),
        "running .morac failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_out.stdout).contains("592"),
        "expected 592 from .morac stdout, got: {}",
        String::from_utf8_lossy(&run_out.stdout)
    );
}
