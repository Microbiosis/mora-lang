//! P6-1 Perceus / FBIP integration suite:
//! - Last-use ownership transfer (`MoveLocal` / `MoveGlobal`) turning functional `Dict` / `List`
//!   builder chains and loop accumulators into O(1) in-place reuse, while live aliases stay intact.
//! - `std/mcp` `McpServer` builder chains reuse their tool / resource / prompt dicts in place.
//! - CPU 64B host tensor in-place reuse guarded by uniqueness, view, and autograd-tape predicates.
//! - Live GPU VRAM in-place reuse when a CUDA / Vulkan device is present.
//! - Multi-shot delimited continuations stay replay-safe (moves degrade to copies under a barrier).
//! - Interprocedural borrow inference (`ControlFunction::param_borrows`) and `MORABC02` `.morac`
//!   AOT parity.

use std::process::Command;
use std::sync::Arc;

use mora::mir::control::{
    ControlInst, OptLevel, lower_program, lower_program_with_dyn_globals, optimize_program,
};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn run_at(src: &str, level: OptLevel) -> Value {
    let (mut prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "type errors for:\n{src}\ngot: {errs:?}");
    optimize_program(&mut prog, level);
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn run(src: &str) -> Value {
    run_at(src, OptLevel::None)
}

fn items(v: Value) -> Vec<Value> {
    match v {
        Value::List(xs) => xs.to_vec(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

fn floats(v: &Value) -> Vec<f64> {
    match v {
        Value::List(xs) => xs
            .iter()
            .map(|x| match x {
                Value::Float(f) => *f,
                Value::Int(n) => *n as f64,
                Value::Scalar(s) => s.as_f64(),
                other => panic!("expected numeric, got {other:?}"),
            })
            .collect(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn stat(v: &Value, key: &str) -> i64 {
    match v {
        Value::Dict(d) => int(d.get(key).unwrap_or_else(|| panic!("missing stat {key}"))),
        other => panic!("expected stats dict, got {other:?}"),
    }
}

fn assert_close(got: &[f64], want: &[f64], ctx: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "{ctx}: length mismatch {got:?} vs {want:?}"
    );
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-3, "{ctx}: got {got:?}, want {want:?}");
    }
}

#[test]
fn p6_1_dict_inplace_reuse_preserves_aliases_across_opt_levels() {
    let src = r#"
task build(n: int): dict<string, int>
  let d: dict<string, int> = {}
  let i = 0
  while i < n
    assign d = d.set("k" + str(i), i)
    assign i = i + 1
  end
  return d
end

task snapshot_then_grow(d: dict<string, int>): list<any>
  let snap = d
  let grown = d.set("extra", 99).remove("k0")
  let out: list<any> = []
  return out.push(snap.len()).push(snap.get("extra")).push(snap.get("k0")).push(grown.len()).push(grown.get("extra")).push(grown.get("k0"))
end

fbip_reset_stats()
let big = build(200)
let empty: dict<string, int> = {}
let chain = empty.set("a", 1).set("b", 2).set("c", 3).remove("b")
let aliased = snapshot_then_grow(build(4))
let st = fbip_stats()
let out: list<any> = []
out.push(big.len()).push(big.get("k199")).push(chain.len()).push(chain.get("b")).push(aliased).push(st)
"#;
    for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
        let v = items(run_at(src, level));
        assert_eq!(int(&v[0]), 200);
        assert_eq!(int(&v[1]), 199);
        assert_eq!(int(&v[2]), 2);
        assert_eq!(v[3], Value::Nil);
        let a = items(v[4].clone());
        // The live alias `snap` must observe none of the later mutations.
        assert_eq!(int(&a[0]), 4, "alias length mutated at {level:?}");
        assert_eq!(a[1], Value::Nil, "alias saw inserted key at {level:?}");
        assert_eq!(int(&a[2]), 0, "alias lost removed key at {level:?}");
        assert_eq!(int(&a[3]), 4);
        assert_eq!(int(&a[4]), 99);
        assert_eq!(a[5], Value::Nil);
        let reuses = stat(&v[5], "dict_inplace_reuses");
        assert!(
            reuses >= 200,
            "expected >= 200 in-place dict reuses at {level:?}, got {reuses}"
        );
    }
}

#[test]
fn p6_1_list_push_and_set_reuse_spine_with_live_snapshots_across_chunks() {
    let src = r#"
task build(n: int): list<any>
  let xs: list<int> = []
  let snaps: list<list<int>> = []
  let i = 0
  while i < n
    if i == 31 or i == 32 or i == 64 then
      assign snaps = snaps.push(xs)
    end
    assign xs = xs.push(i)
    assign i = i + 1
  end
  assign xs = xs.set(0, 1000).set(n - 1, 2000)
  let s0 = snaps.get(0)
  let s1 = snaps.get(1)
  let s2 = snaps.get(2)
  let out: list<any> = []
  return out.push(xs.len()).push(xs.get(0)).push(xs.get(1)).push(xs.get(n - 1)).push(s0.len()).push(s1.len()).push(s2.len()).push(s2.get(0)).push(s2.get(63))
end

fbip_reset_stats()
let res = build(500)
res.push(fbip_stats())
"#;
    let v = items(run(src));
    assert_eq!(int(&v[0]), 500);
    assert_eq!(int(&v[1]), 1000);
    assert_eq!(int(&v[2]), 1);
    assert_eq!(int(&v[3]), 2000);
    assert_eq!(int(&v[4]), 31, "snapshot at chunk boundary 31 mutated");
    assert_eq!(int(&v[5]), 32, "snapshot at chunk boundary 32 mutated");
    assert_eq!(int(&v[6]), 64, "snapshot at chunk boundary 64 mutated");
    assert_eq!(int(&v[7]), 0, "snapshot element rewritten by later set()");
    assert_eq!(int(&v[8]), 63);
    let reuses = stat(&v[9], "list_inplace_reuses");
    assert!(
        reuses >= 450,
        "expected >= 450 in-place list reuses, got {reuses}"
    );
}

#[test]
fn p6_1_std_mcp_server_builder_chain_reuses_tool_dict_in_place() {
    let src = r#"
import "std/mcp"

task make_server(n: int): McpServer
  let srv = server("fbip-demo", "1.0.0")
  let schema: dict<string, any> = {}
  let i = 0
  while i < n
    assign srv = srv.tool("t" + str(i), "tool", schema, fn(args: dict<string, any>) i end)
    assign i = i + 1
  end
  return srv
end

fbip_reset_stats()
let srv = make_server(64)
let st = fbip_stats()
let listed = json.parse(srv.handle_jsonrpc("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"))
let out: list<any> = []
out.push(srv.tools.len()).push(listed.get("result").get("tools").len()).push(st)
"#;
    let v = items(run(src));
    assert_eq!(int(&v[0]), 64);
    assert_eq!(int(&v[1]), 64);
    let reuses = stat(&v[2], "dict_inplace_reuses");
    assert!(
        reuses >= 60,
        "McpServer.tool() chain must reuse the tools dict in place, got {reuses}"
    );
}

#[test]
fn p6_1_cpu_tensor_inplace_reuse_and_safety_guards() {
    let src = r#"
task residual(x: Tensor<f64, [4], cpu>, b: Tensor<f64, [4], cpu>): list<any>
  let p0 = x.storage_id()
  let y = ((x + b) * 2.0 - 1.0).silu().relu()
  let out: list<any> = []
  return out.push(p0 == y.storage_id()).push(y.to_list())
end

task self_add(x: Tensor<f64, [3], cpu>): list<float>
  return (x + x).to_list()
end

task view_guard(m: Tensor<f64, [2, 2], cpu>): list<any>
  let v = m.transpose()
  let y = m + 10.0
  return [v.to_list(), y.to_list()]
end

task loss(w: Tensor<f64, [2], cpu>): float
  return ((w + 1.0) * 2.0).sum()
end

fbip_reset_stats()
let r = residual(tensor([1.0, -2.0, 0.5, 3.0]), tensor([0.5, 0.5, 0.5, 0.5]))
let host_after_chain = fbip_stats().get("tensor_host_reuses")
let sa = self_add(tensor([1.0, 2.0, 3.0]))
let vg = view_guard(tensor([[1.0, 2.0], [3.0, 4.0]]))
let w0 = tensor([1.0, 2.0])
let res = value_and_grad(loss)(w0)
let out: list<any> = []
out.push(r[0]).push(r[1]).push(host_after_chain).push(sa).push(vg[0]).push(vg[1]).push(res[0]).push(res[1].to_list()).push(w0.to_list())
"#;
    let v = items(run(src));
    assert_eq!(
        v[0],
        Value::Bool(true),
        "residual chain must keep its buffer"
    );
    let silu = |z: f64| z / (1.0 + (-z).exp());
    let want: Vec<f64> = [1.0, -2.0, 0.5, 3.0]
        .iter()
        .map(|x| silu((x + 0.5) * 2.0 - 1.0).max(0.0))
        .collect();
    assert_close(&floats(&v[1]), &want, "residual chain values");
    assert!(int(&v[2]) >= 5, "expected >= 5 host reuses, got {:?}", v[2]);
    assert_close(&floats(&v[3]), &[2.0, 4.0, 6.0], "x + x aliasing");
    let view = items(v[4].clone());
    assert_close(&floats(&view[0]), &[1.0, 3.0], "transposed view row 0");
    assert_close(&floats(&view[1]), &[2.0, 4.0], "transposed view row 1");
    let y = items(v[5].clone());
    assert_close(&floats(&y[0]), &[11.0, 12.0], "m + 10 row 0");
    match &v[6] {
        // ((w + 1.0) * 2.0).sum() over w = [1.0, 2.0] is 4.0 + 6.0 = 10.0.
        Value::Float(f) => assert!((f - 10.0).abs() < 1e-9, "loss value {f}"),
        other => panic!("expected float loss, got {other:?}"),
    }
    assert_close(&floats(&v[7]), &[2.0, 2.0], "autograd gradient");
    assert_close(
        &floats(&v[8]),
        &[1.0, 2.0],
        "primal tensor must not be mutated",
    );
}

/// Probe whether `dev` can actually run a native kernel in this environment.
///
/// A device may be *listed* by `Tensor::devices()` (a software Vulkan ICD, a half-installed
/// driver) while its native pipeline cannot compile or launch anything. Probing first keeps
/// the live FBIP assertions honest: an unusable backend is an **environment fact**, not a
/// product defect. The probe is deliberately tiny and separate from the assertions below —
/// if the backend works but the FBIP chain is wrong, the test still fails loudly.
fn backend_usable(dev: &str) -> bool {
    let src = format!(
        "let t = tensor([1.0, 2.0]).to_device(\"{dev}\")\nprint((t + 1.0).to_device(\"cpu\").to_list())\n"
    );
    let (mut prog, witnesses) = ParserV3::compile(&src).expect("设备探针必须能编译");
    let errors = check_program_witnesses(&witnesses);
    assert!(errors.is_empty(), "设备探针类型错误 {errors:?}");
    optimize_program(&mut prog, OptLevel::None);
    let mut core = CoreRuntime::default();
    match execute_program(Arc::new(prog), &mut core) {
        Ok(_) => true,
        Err(e) => {
            eprintln!(
                "p6_1 GPU VRAM reuse: {dev} backend unusable in this environment ({e}), skipping live check"
            );
            false
        }
    }
}

/// 逐个验证已发现的后端，驱动不可用时由探针输出原因。
fn usable_gpu_devices() -> impl Iterator<Item = String> {
    items(run("Tensor::devices()"))
        .into_iter()
        .filter_map(|device| match device {
            Value::String(name)
                if ["cuda", "vulkan", "metal"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                    && backend_usable(&name) =>
            {
                Some(name)
            }
            _ => None,
        })
}

#[test]
fn p6_1_gpu_vram_tensor_inplace_reuse_when_hardware_present() {
    let Some(dev) = usable_gpu_devices().find(|dev| !dev.starts_with("metal")) else {
        return;
    };
    let src = format!(
        r#"
task chain(x: Tensor<f64, [8], any>): list<any>
  let p0 = x.storage_id()
  let y = ((x + 1.0) * 2.0).relu()
  let out: list<any> = []
  return out.push(p0 == y.storage_id()).push(y.is_vram_resident()).push(y.to_device("cpu").to_list())
end

fbip_reset_stats()
let x = tensor([-3.0, -1.0, 0.0, 0.5, 1.0, 2.0, 3.0, 4.0]).to_device("{dev}")
let res = chain(x)
res.push(fbip_stats().get("tensor_vram_reuses"))
"#
    );
    let v = items(run(&src));
    assert_eq!(v[0], Value::Bool(true), "{dev}: VRAM buffer identity lost");
    assert_eq!(v[1], Value::Bool(true), "{dev}: result left VRAM");
    let want: Vec<f64> = [-3.0, -1.0, 0.0, 0.5, 1.0, 2.0, 3.0, 4.0]
        .iter()
        .map(|x: &f64| ((x + 1.0) * 2.0).max(0.0))
        .collect();
    assert_close(&floats(&v[2]), &want, "GPU chain values");
    assert!(
        int(&v[3]) >= 3,
        "{dev}: expected >= 3 VRAM reuses, got {:?}",
        v[3]
    );
}

#[test]
fn p6_1_multishot_continuations_stay_replay_safe() {
    // Mora requires the handler clause to have the SAME type as the handle body (the clause's
    // value becomes the value of the whole `handle` expression), so a multi-shot handler
    // combines its two `resume` results with `+` rather than collecting them in a list.
    //
    // The body appends the performed value to a list and returns the appended element, which
    // makes the two replays distinguishable: `resume(10) * 1000 + resume(20)` is 10020 only
    // when the second replay observes a pristine list. An in-place `push` that leaked across
    // replays would make the second `resume` observe the first one's append and yield 10010,
    // and a destructive move that was NOT downgraded to a copy would empty the captured slot
    // entirely — so this assertion pins down both halves of the barrier invariant.
    let src = r#"
fbip_reset_stats()
let out = handle Eff {
  let xs: list<int> = []
  let xs1 = xs.push(1)
  let k_val = perform Eff()
  let xs2 = xs1.push(k_val)
  xs2.get(1)
} {
  resume(10) * 1000 + resume(20)
}
let res: list<any> = []
res.push(out).push(fbip_stats())
"#;
    let v = items(run(src));
    assert_eq!(
        int(&v[0]),
        10020,
        "multi-shot replay must not observe another replay's mutation"
    );
    assert!(
        stat(&v[1], "continuation_copy_fallbacks") > 0,
        "moves under an active handler barrier must degrade to copies"
    );
}

#[test]
fn p6_1_borrow_inference_marks_read_only_params() {
    let src = r#"
task inspect_len(d: dict<string, int>): int
  return d.len()
end

task grow(d: dict<string, int>): dict<string, int>
  return d.set("z", 9)
end

let base = {"a": 1}
[inspect_len(base), grow(base).len()]
"#;
    let (mut prog, _) = ParserV3::compile(src).expect("compile borrow inference program");
    optimize_program(&mut prog, OptLevel::None);
    let borrows_of = |name: &str| -> Vec<bool> {
        prog.functions
            .iter()
            .find(|f| f.name.map(|s| prog.symbols.resolve(s)) == Some(name))
            .unwrap_or_else(|| panic!("function {name} not found"))
            .param_borrows
            .clone()
    };
    assert_eq!(borrows_of("inspect_len"), vec![true]);
    assert_eq!(borrows_of("grow"), vec![false]);
    let v = items(run(src));
    assert_eq!(int(&v[0]), 1);
    assert_eq!(int(&v[1]), 2);
}

#[test]
fn p6_1_dead_main_global_transfers_ownership_out_of_its_slot() {
    // A `<main>`-scope `let x = …` keeps its value in a global slot. Once `x` is dead the
    // Perceus pass transfers the ownership out of that slot, so the callee sees a uniquely
    // owned buffer and can reuse it in place instead of cloning it.
    let src = r#"
task consume(x: Tensor<f64, [4], cpu>): list<any>
  let p0 = x.storage_id()
  let y = x + 1.0
  let out: list<any> = []
  return out.push(p0 == y.storage_id()).push(y.to_list())
end

fbip_reset_stats()
let x = tensor([1.0, 2.0, 3.0, 4.0])
let r = consume(x)
r.push(fbip_stats())
"#;
    let v = items(run(src));
    assert_eq!(
        v[0],
        Value::Bool(true),
        "a dead <main> global must transfer its ownership out of its slot"
    );
    assert_close(
        &floats(&v[1]),
        &[2.0, 3.0, 4.0, 5.0],
        "consumed tensor values",
    );
    assert!(
        stat(&v[2], "tensor_host_reuses") >= 1,
        "expected >= 1 host reuse, got {:?}",
        v[2]
    );
}

#[test]
fn p6_1_live_main_global_keeps_its_value_and_is_not_reused_in_place() {
    // `x` is read again after the arithmetic, so it is still live: its buffer must NOT be
    // reused in place, and `x` must still hold its original value afterwards.
    let src = r#"
fbip_reset_stats()
let x = tensor([1.0, 2.0])
let y = x + 1.0
let out: list<any> = []
out.push(x.to_list()).push(y.to_list()).push(fbip_stats().get("tensor_host_reuses"))
"#;
    let v = items(run(src));
    assert_close(
        &floats(&v[0]),
        &[1.0, 2.0],
        "a still-live global must keep its original value",
    );
    assert_close(&floats(&v[1]), &[2.0, 3.0], "derived tensor values");
    assert_eq!(
        int(&v[2]),
        0,
        "a still-live global must not be reused in place"
    );
}

#[test]
fn p6_1_repl_chunk_keeps_main_globals_in_their_slots() {
    // The REPL captures every global of a chunk into the session environment, so a chunk's
    // `<main>`-private globals must keep their value — the Perceus pass may not transfer their
    // ownership out of their slots. A plain script's globals die with `<main>`, so there the
    // ownership transfer is allowed and is what makes in-place reuse possible at all.
    let src = r#"
let x = tensor([1.0, 2.0])
let y = x + 1.0
"#;
    let has_move_global = |prog: &mora::mir::control::ControlProgram| -> bool {
        prog.func(prog.entry)
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(i, ControlInst::MoveGlobal(..)))
    };

    let (_, witnesses) = ParserV3::compile(src).expect("compile REPL chunk");
    let chunk = lower_program_with_dyn_globals(&witnesses, &[]).expect("lower REPL chunk");
    assert!(
        chunk.preserve_globals_at_exit,
        "REPL chunks capture their globals after execution"
    );
    assert!(
        !has_move_global(&chunk),
        "a chunk whose globals are captured must not transfer global ownership out of its slot"
    );

    let (_, witnesses) = ParserV3::compile(src).expect("compile script");
    let mut script = lower_program(&witnesses).expect("lower script");
    assert!(!script.preserve_globals_at_exit);
    optimize_program(&mut script, OptLevel::None);
    assert!(
        has_move_global(&script),
        "a script's dead <main> global must transfer its ownership out of its slot"
    );
}

#[test]
fn p6_1_morac_aot_bytecode_preserves_fbip_reuse() {
    let src = r#"
task build(n: int): list<int>
  let xs: list<int> = []
  let i = 0
  while i < n
    assign xs = xs.push(i)
    assign i = i + 1
  end
  return xs
end

fbip_reset_stats()
let xs = build(300)
let st = fbip_stats()
print(str(xs.len()) + ":" + str(st.get("list_inplace_reuses") >= 290))
"#;
    let tmp = TempDir::new("p6_1_fbip_morac");
    let src_path = tmp.path().join("fbip.mora");
    let bc_path = tmp.path().join("fbip.morac");
    std::fs::write(&src_path, src).expect("write fbip source");
    let build = Command::new(env!("CARGO_BIN_EXE_mora"))
        .args(["build", "--emit-bc", "-o"])
        .arg(&bc_path)
        .arg(&src_path)
        .output()
        .expect("run mora build --emit-bc");
    assert!(
        build.status.success(),
        "mora build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    for target in [&src_path, &bc_path] {
        let out = Command::new(env!("CARGO_BIN_EXE_mora"))
            .arg(target)
            .output()
            .expect("run mora");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("300:true"),
            "{}: expected `300:true`, got stdout={stdout} stderr={}",
            target.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// P6-1 master plan: `Router.get(...)` must become O(n).
///
/// An immutable builder chain that clones the whole route spine on every registration is
/// O(n^2). The chain only reuses the spine when the *mutated* field is read **last**: while
/// `self` is still live a field load has to clone, which leaves the list shared
/// (`Rc::strong_count > 1`) and blocks the in-place reuse.
#[test]
fn p6_1_std_http_router_builder_chain_reuses_route_spine_in_place() {
    let src = r#"
import "std/net/http"

task build(n: int): any
  let r = router()
  let i = 0
  while i < n
    assign r = r.get("/p" + str(i), fn(req: any) 1 end)
    assign i = i + 1
  end
  return r
end

fbip_reset_stats()
let rr = build(64)
let st = fbip_stats()
let out: list<any> = []
out.push(rr.routes.len()).push(st)
"#;
    let v = items(run(src));
    assert_eq!(int(&v[0]), 64);
    let reuses = stat(&v[1], "list_inplace_reuses");
    assert!(
        reuses >= 60,
        "Router.get() chain must reuse the routes spine in place, got {reuses}"
    );
}

/// P6-1 master plan: in-place reuse must **never** mutate a shared RadixAttention CoW KV page.
///
/// Mora enforces this **structurally**, not by a runtime check: the KV allocator owns raw byte
/// pages, `write_kv` copies through a `&Tensor` borrow, and `read_kv` materializes fresh
/// tensors — so a `Tensor` storage can never alias a KV page. This test turns that
/// structural argument into an executable invariant: a full FBIP reuse chain must leave the
/// KV pages bit-identical.
#[test]
fn p6_1_fbip_tensor_reuse_never_corrupts_shared_kv_cache_pages() {
    let src = r#"
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cpu")
kv.alloc_seq(1, 4)
kv.append_tokens(1, 3)
let k0 = tensor([[1.5, -2.25], [3.0, 0.5]], dtype="f32", device="cpu")
let v0 = tensor([[0.25, 0.5], [0.75, 1.0]], dtype="f32", device="cpu")
kv.write_kv(1, 0, k0, v0)
let before = kv.read_kv(1)

fbip_reset_stats()
let t = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f32", device="cpu")
let r = ((t * 2.0f32) + 1.0f32).relu()
let after = kv.read_kv(1)
let k_err = (after[0] - before[0]).abs().sum()
let v_err = (after[1] - before[1]).abs().sum()
let out: list<any> = []
out.push(k_err < 1e-6f32).push(v_err < 1e-6f32).push(fbip_stats())
"#;
    let v = items(run(src));
    assert_eq!(v[0], Value::Bool(true), "K page corrupted by FBIP reuse");
    assert_eq!(v[1], Value::Bool(true), "V page corrupted by FBIP reuse");
    let reuses = stat(&v[2], "tensor_host_reuses");
    assert!(
        reuses >= 1,
        "the FBIP chain must actually reuse host buffers, got {reuses}"
    );
}

/// P6-1 master plan: "zero aliasing corruption across **branches / closures**".
///
/// A live alias must keep observing the value it was bound to, even when the original
/// binding is later mutated in place (branch) or captured by a closure. When the alias is
/// live the mutation must fall back to a copy — `dict_inplace_reuses` staying at 0 for these
/// two tasks is the signal that no in-place write happened.
#[test]
fn p6_1_fbip_reuse_keeps_branch_and_closure_aliases_intact() {
    let src = r#"
task branch_alias(flag: bool): list<any>
  let d: dict<string, int> = {"a": 1}
  let alias = d
  if flag then
    assign d = d.set("b", 2)
  else
    assign d = d.set("c", 3)
  end
  let out: list<any> = []
  return out.push(alias.len()).push(alias.get("b")).push(alias.get("c")).push(d.len())
end

task closure_alias(): list<any>
  let xs: list<int> = [1, 2, 3]
  let f = fn(x: int) x + xs.len() end
  let grown = xs.push(4)
  let out: list<any> = []
  return out.push(f(0)).push(grown.len()).push(xs.len())
end

fbip_reset_stats()
let b1 = branch_alias(true)
let b2 = branch_alias(false)
let c1 = closure_alias()
let st = fbip_stats()
let out: list<any> = []
out.push(b1).push(b2).push(c1).push(st)
"#;
    let v = items(run(src));
    // The live alias must observe none of the branch-local mutations.
    let t = items(v[0].clone());
    assert_eq!(int(&t[0]), 1, "branch alias length mutated");
    assert_eq!(t[1], Value::Nil, "branch alias saw the b key");
    assert_eq!(t[2], Value::Nil, "branch alias saw the c key");
    assert_eq!(int(&t[3]), 2);
    let f = items(v[1].clone());
    assert_eq!(int(&f[0]), 1, "else-branch alias length mutated");
    assert_eq!(f[1], Value::Nil, "else-branch alias saw the b key");
    assert_eq!(f[2], Value::Nil, "else-branch alias saw the c key");
    assert_eq!(int(&f[3]), 2);
    // The closure captured the list as of its creation, so it keeps seeing 3 elements.
    let c = items(v[2].clone());
    assert_eq!(
        int(&c[0]),
        3,
        "closure must observe the captured list length"
    );
    assert_eq!(int(&c[1]), 4, "grown list length");
    assert_eq!(int(&c[2]), 3, "original list must not be grown in place");
    assert_eq!(
        stat(&v[3], "dict_inplace_reuses"),
        0,
        "a live alias must force the copy path — no in-place dict write may happen"
    );
}

/// P6-1: the in-place reuse of a struct field must NOT depend on the *source order* of the
/// projections.
///
/// A consumed struct used to be loaded once per projection, so only the last load could
/// become an ownership transfer — the mutated field therefore only got in-place reuse when
/// it happened to be projected **last**, i.e. the reuse behaviour was decided by the order
/// the fields were written in. The compiler now coalesces the sibling loads and extracts
/// every projected field destructively, so both orders must behave identically.
#[test]
fn p6_1_struct_field_reuse_is_independent_of_projection_order() {
    let src = r#"
export struct Holder
  data: dict<string, int>
  tag: dict<string, int>
end

-- The mutated field is projected FIRST: `self` is still live when `self.data` is read.
task mutate_data_first(self: Holder, i: int): Holder
  return Holder(data = self.data.set("k" + str(i), i), tag = self.tag)
end

-- The mutated field is projected LAST.
task mutate_data_last(self: Holder, i: int): Holder
  let tg = self.tag
  let dt = self.data
  return Holder(data = dt.set("k" + str(i), i), tag = tg)
end

task build(n: int, variant: string): any
  let h = Holder(data = {}, tag = {"t": 0})
  let i = 0
  while i < n
    if variant == "first" then
      assign h = mutate_data_first(h, i)
    else
      assign h = mutate_data_last(h, i)
    end
    assign i = i + 1
  end
  return h
end

fbip_reset_stats()
let a = build(64, "first")
let sa = fbip_stats()
fbip_reset_stats()
let b = build(64, "last")
let sb = fbip_stats()
let out: list<any> = []
out.push(a.data.len()).push(b.data.len()).push(sa).push(sb)
"#;
    let v = items(run(src));
    assert_eq!(int(&v[0]), 64);
    assert_eq!(int(&v[1]), 64);
    let first = stat(&v[2], "dict_inplace_reuses");
    let last = stat(&v[3], "dict_inplace_reuses");
    assert!(
        first >= 60,
        "projecting the mutated field first must still reuse the buffer in place, got {first}"
    );
    assert_eq!(
        first, last,
        "the projection order must not change the number of in-place reuses"
    );
}

#[test]
fn p6_1_struct_projection_with_zero_arg_impl_method_and_continuation_is_sound() {
    let src = r#"
struct Boxed
  data: dict<string, int>
  tag: dict<string, int>
end

impl Boxed
  task summary(self): int
    return self.data.len()
  end
end

task field_then_method(b: Boxed): list<int>
  let d2 = b.data.set("extra", 99)
  let orig_n = b.summary()
  return [d2.len(), orig_n]
end

fbip_reset_stats()
let mix = field_then_method(Boxed(data = {"a": 1}, tag = {"t": 0}))

let replay_sum = handle Eff {
  let b = Boxed(data = {"a": 1}, tag = {"t": 2})
  let k = perform Eff()
  let b2 = Boxed(data = b.data.set("v" + str(k), k), tag = b.tag)
  b2.data.len() * 100 + b2.tag.len()
} {
  resume(10) * 1000 + resume(20)
}

-- Top-level `<main>`-private global struct projection order parity (`data` projected first vs last).
fbip_reset_stats()
let g1 = Boxed(data = {"a": 1}, tag = {"t": 0})
let g1_next = Boxed(data = g1.data.set("b", 2), tag = g1.tag)
let g_first_reuses = fbip_stats().get("dict_inplace_reuses")

fbip_reset_stats()
let g2 = Boxed(data = {"a": 1}, tag = {"t": 0})
let g2_next = Boxed(data = g2.tag, tag = g2.data.set("b", 2))
let g_last_reuses = fbip_stats().get("dict_inplace_reuses")

let out: list<any> = []
out.push(mix).push(replay_sum).push(g1_next.data.len()).push(g2_next.tag.len()).push(g_first_reuses).push(g_last_reuses)
"#;
    let v = items(run(src));
    let mix = items(v[0].clone());
    assert_eq!(int(&mix[0]), 2, "derived dict must contain inserted key");
    assert_eq!(
        int(&mix[1]),
        1,
        "zero-arg impl method must receive intact unmutated `self`"
    );
    assert_eq!(
        int(&v[1]),
        201201,
        "multi-shot continuation replay across struct field projections must stay isolated"
    );
    assert_eq!(int(&v[2]), 2);
    assert_eq!(int(&v[3]), 2);
    assert_eq!(
        int(&v[4]),
        1,
        "<main> global struct projecting mutated field first must reuse in place"
    );
    assert_eq!(
        int(&v[4]),
        int(&v[5]),
        "<main> global struct field reuse must be independent of projection order"
    );
}

#[test]
fn p6_1_borrowed_params_elide_caller_clones_across_inspections_and_reductions() {
    let src = r#"
struct Holder
  data: dict<string, int>
  tag: dict<string, int>
end

task check_tensor_stats(t: Tensor<f64, [4], cpu>): bool
  return t.sum() > 0.0 and t.max() < 1000.0
end

task inspect_holder(h: Holder): int
  let tv = h.tag.get("t")
  return h.data.len() + int(tv != nil)
end

task transitive_inspect(h: Holder): int
  return inspect_holder(h)
end

task leak_field(h: Holder): dict<string, int>
  return h.data
end

task peer_check(borrowed: dict<string, int>, mutated: dict<string, int>): list<int>
  return [borrowed.len(), mutated.set("second", 3).len()]
end

task loop_driver(n: int): list<any>
  let h = Holder(data = {}, tag = {"t": 1})
  let t = tensor([1.0, 2.0, 3.0, 4.0])
  let acc = 0
  let i = 0
  while i < n
    if check_tensor_stats(t) then
      assign acc = acc + transitive_inspect(h)
    end
    assign h = Holder(data = h.data.set("k" + str(i), i), tag = h.tag)
    assign t = t + 1.0
    assign i = i + 1
  end
  let d_single = {"only": 1}
  let hazard = peer_check(d_single, d_single.set("second", 2))
  let leaked = leak_field(h)
  let out: list<any> = []
  return out.push(acc).push(leaked.len()).push(t.to_list()).push(hazard)
end

fbip_reset_stats()
let res = loop_driver(50)
res.push(fbip_stats())
"#;
    let (mut prog, witnesses) = ParserV3::compile(src).expect("compile borrowed params test");
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "type errors: {errs:?}");
    optimize_program(&mut prog, OptLevel::Basic);
    let borrows_of = |name: &str| -> Vec<bool> {
        prog.functions
            .iter()
            .find(|f| f.name.map(|s| prog.symbols.resolve(s)) == Some(name))
            .unwrap_or_else(|| panic!("function {name} not found"))
            .param_borrows
            .clone()
    };
    assert_eq!(borrows_of("check_tensor_stats"), vec![true]);
    assert_eq!(borrows_of("inspect_holder"), vec![true]);
    assert_eq!(borrows_of("transitive_inspect"), vec![true]);
    assert_eq!(borrows_of("leak_field"), vec![false]);
    assert_eq!(borrows_of("peer_check"), vec![true, false]);

    let v = items(run_at(src, OptLevel::Basic));
    // sum_{i=0..49} (i + 1) = 1275
    assert_eq!(int(&v[0]), 1275);
    assert_eq!(int(&v[1]), 50);
    assert_close(&floats(&v[2]), &[51.0, 52.0, 53.0, 54.0], "loop tensor");
    let haz = items(v[3].clone());
    assert_eq!(
        int(&haz[0]),
        1,
        "peer_check(d, d.set(...)) must not dangle or observe mutated d in arg 0"
    );
    assert_eq!(int(&haz[1]), 2);
    let st = &v[4];
    let elisions = stat(st, "borrowed_call_elisions");
    assert!(
        elisions >= 100,
        "expected >= 100 zero-clone borrowed call elisions, got {elisions}"
    );
    assert!(stat(st, "dict_inplace_reuses") >= 48);
    assert!(stat(st, "tensor_host_reuses") >= 50);
}

#[test]
fn p6_1_math_module_and_tensor_log_sin_cos_reuse_host_and_vram_in_place() {
    let src = r#"
task math_mod_chain(x: Tensor<f64, [4], cpu>): list<any>
  let p0 = x.storage_id()
  let y = math.cos(math.sin(math.log(math.exp(x))))
  let out: list<any> = []
  return out.push(p0 == y.storage_id()).push(y.to_list())
end

task method_chain(x: Tensor<f64, [4], cpu>): list<any>
  let p0 = x.storage_id()
  let y = x.exp().log().sin().cos()
  let out: list<any> = []
  return out.push(p0 == y.storage_id()).push(y.to_list())
end

task grad_target(w: Tensor<f64, [2], cpu>): float
  return math.sin(w.log()).sum()
end

fbip_reset_stats()
let m1 = math_mod_chain(tensor([0.2, 0.5, 1.0, 1.5]))
let r1 = fbip_stats().get("tensor_host_reuses")
let m2 = method_chain(tensor([0.2, 0.5, 1.0, 1.5]))
let r2 = fbip_stats().get("tensor_host_reuses")
let g = value_and_grad(grad_target)(tensor([1.0, 2.0]))
let out: list<any> = []
out.push(m1[0]).push(m1[1]).push(r1).push(m2[0]).push(m2[1]).push(r2).push(g[1].to_list())
"#;
    let v = items(run(src));
    assert_eq!(
        v[0],
        Value::Bool(true),
        "math.* tensor chain must reuse host buffer"
    );
    assert_eq!(int(&v[2]), 4, "expected 4 host reuses from math.* chain");
    assert_eq!(
        v[3],
        Value::Bool(true),
        "Tensor.exp/log/sin/cos chain must reuse host buffer"
    );
    assert_eq!(int(&v[5]), 8, "expected 8 cumulative host reuses");
    let want: Vec<f64> = [0.2_f64, 0.5, 1.0, 1.5]
        .iter()
        .map(|x| x.exp().ln().sin().cos())
        .collect();
    assert_close(&floats(&v[1]), &want, "math.* values");
    assert_close(&floats(&v[4]), &want, "method values");
    // d/dw sin(ln(w)) = cos(ln(w)) / w
    let want_grad = [1.0_f64.ln().cos() / 1.0, 2.0_f64.ln().cos() / 2.0];
    assert_close(&floats(&v[6]), &want_grad, "autograd through log+sin");
}

#[test]
fn p6_1_over_64_operands_transfer_ownership_and_nested_handlers_keep_barrier_count_exact() {
    let mut elems = Vec::with_capacity(70);
    for i in 0..69 {
        elems.push(format!("{i}"));
    }
    elems.push("hi.len()".to_string());
    let big_list = elems.join(", ");
    let src = format!(
        r#"
task big_arity(): int
  let hi = {{"k": 1}}
  let xs = [{big_list}]
  return xs.len()
end

let nested = handle Outer {{
  let a = handle Inner {{
    let v = perform Inner()
    v + (perform Outer())
  }} {{
    resume(10) + resume(20)
  }}
  a
}} {{
  resume(1) + resume(2)
}}

let caught = try_call(fn()
  handle Boom {{
    let bad = 1 / 0
    bad
  }} {{
    resume(0)
  }}
end)

[big_arity(), nested, int(caught.is_err())]
"#
    );
    // 可增长的最后使用位图也覆盖第 65 个及以后的操作数。
    let mut big_reg_elems = String::new();
    let mut big_lets = String::new();
    for i in 0..68 {
        big_lets.push_str(&format!("  let r{i} = {i} + 1\n"));
        if i > 0 {
            big_reg_elems.push_str(", ");
        }
        big_reg_elems.push_str(&format!("r{i}"));
    }
    let ir_probe = format!(
        "task many(): list<int>\n{big_lets}  return [{big_reg_elems}]\nend\nmany().len()\n"
    );
    let (mut prog, _) = ParserV3::compile(&ir_probe).expect("compile >64-input ListLit");
    optimize_program(&mut prog, OptLevel::None);
    let many_fn = prog
        .functions
        .iter()
        .find(|f| f.name.map(|s| prog.symbols.resolve(s)) == Some("many"))
        .expect("find many()");
    let mask = many_fn
        .blocks
        .iter()
        .find_map(|block| {
            block
                .insts
                .iter()
                .position(
                    |inst| matches!(inst, ControlInst::ListLit(_, items) if items.len() == 68),
                )
                .map(|pc| &block.inst_kill_masks[pc])
        })
        .expect("68 个输入的列表指令");
    assert!((64..68).all(|bit| mask.test(bit)));

    let v = items(run(&src));
    assert_eq!(int(&v[0]), 70);
    // Deep multi-shot handlers:
    // `perform Outer()` inside `resume(10)` captures `K_outer = fn(x) -> (10 + x) + resume(20)`.
    // - Outer `resume(1)` re-installs deep `Outer` barrier: `10 + 1 = 11`, then `11 + resume(20)`
    //   performs `Outer()` capturing `fn(y) -> 11 + (20 + y)` => `(11 + 21) + (11 + 22) = 65`.
    // - Outer `resume(2)` re-installs deep `Outer` barrier: `10 + 2 = 12`, then `12 + resume(20)`
    //   performs `Outer()` capturing `fn(y) -> 12 + (20 + y)` => `(12 + 21) + (12 + 22) = 67`.
    // Total = 65 + 67 = 132, with `debug_assert_eq!` verifying `active_handler_barriers` at every step.
    assert_eq!(int(&v[1]), 132);
    assert_eq!(int(&v[2]), 1);
}

#[test]
fn p6_1_borrowed_and_owned_receivers_observe_the_same_nominal_methods() {
    let src = r#"
enum Verdict
  Ok
  Err
end
impl Verdict
  task is_ok(self): int
    return 41
  end
end
task borrowed_scalar(t: any): int
  return t.len()
end
let scalar = Tensor::ones([], "f64")
let custom = Verdict::Err
let own = Verdict::Ok.is_ok()
[scalar.len(), borrowed_scalar(scalar), Tensor::zeros([], "f64").len(), custom.is_ok(), own, int(Ok(1).is_ok())]
"#;
    for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
        assert_eq!(
            items(run_at(src, level)),
            vec![0, 0, 0, 41, 41, 1]
                .into_iter()
                .map(Value::Int)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn p6_1_owned_pattern_and_concatenation_preserve_live_values() {
    let src = r#"
enum Packet
  Data(dict<string, int>)
  Empty
end
task update(p: Packet): dict<string,int>
  return match p with
    Packet::Empty -> {}
    Packet::Data(d) -> d.set("two", 2)
  end
end
fbip_reset_stats()
let extended = update(Packet::Data({"one": 1}))
let moves = fbip_stats().get("dict_inplace_reuses")
let source = {"one": 1}
let snapshot = Packet::Data(source)
let again = update(snapshot)
let prefix = [10, 20]
let suffix = prefix + [30]
let msg = "原"
let text = msg + "地" + str(8)
let out: list<any> = []
out.push(extended.len()).push(moves).push(again.len()).push(source.len()).push(suffix).push(prefix).push(text).push(msg).push([1,2] + [3,4])
"#;
    let v = items(run(src));
    assert_eq!(
        (&v[0], &v[1], &v[2], &v[3]),
        (
            &Value::Int(2),
            &Value::Int(1),
            &Value::Int(2),
            &Value::Int(1)
        )
    );
    assert_eq!(floats(&v[4]), [10., 20., 30.]);
    assert_eq!(floats(&v[5]), [10., 20.]);
    assert_eq!(v[6], Value::String("原地8".into()));
    assert_eq!(v[7], Value::String("原".into()));
    assert_eq!(floats(&v[8]), [4., 6.]);
    let repeated = run(r#"
task read_twice(d: dict<string,int>): list<int>
  return match d with
    {a: first, a: second, ..} -> [first, second]
    _ -> []
  end
end
read_twice({"a": 7})
"#);
    assert_eq!(floats(&repeated), [7., 7.]);
    let counts = items(run(r#"
task append(left: list<int>): list<int>
  return left + [31,32,33]
end
fbip_reset_stats()
let result = append([1,2])
let hits = fbip_stats().get("list_inplace_reuses")
let out: list<any> = []
out.push(result).push(hits)
"#));
    assert_eq!(floats(&counts[0]), [1., 2., 31., 32., 33.]);
    assert!(int(&counts[1]) >= 1);
    let mut left = String::with_capacity(64);
    left.push_str("mora");
    let original = left.as_ptr();
    let Value::String(joined) = mora::flow::eval_binary(
        Value::String(left),
        &mora::common::BinaryOp::Add,
        Value::String("语言".into()),
    )
    .unwrap() else {
        panic!("字符串拼接结果")
    };
    assert_eq!(original, joined.as_ptr());
}

#[test]
fn p6_1_norm_storage_and_live_tapes() {
    let src = r#"
task norms(x: any): list<any>
  let id = x.storage_id()
  let y = x.rms_norm(0.00001)
  let mid = y.storage_id()
  let z = y.layer_norm(0.00001)
  let out: list<any> = []
  return out.push(id == mid).push(mid == z.storage_id()).push(z.to_device("cpu").to_list()).push(z.requires_grad())
end
fbip_reset_stats()
let r = norms(tensor([1.0,2.0,3.0], requires_grad = true))
let hits = fbip_stats().get("tensor_host_reuses")
let loss = fn(x: Tensor<f64,[3],cpu>)
  let y = x * 2.0
  return (y.rms_norm() * tensor([1.0,2.0,-3.0])).sum()
end
let x = tensor([1.0,2.0,3.0])
let direction = tensor([1.0,0.0,0.0])
let g = grad(loss)(x)
let tangent = jvp(loss, x, direction)[1]
let graph = trace_dag(loss, x).run(x)
let checked = no_grad
  norms(tensor([1.0,2.0,3.0], requires_grad = true))
end
let out: list<any> = []
out.push(r).push(hits).push(g.to_list()).push(float(tangent.item())).push(float(graph.item())).push(float(loss(x))).push(checked)
"#;
    let v = items(run(src));
    let y = items(v[0].clone());
    assert_eq!(
        (&y[0], &y[1], &y[3]),
        (&Value::Bool(true), &Value::Bool(true), &Value::Bool(true))
    );
    assert_eq!(int(&v[1]), 2);
    assert_close(&floats(&y[2]), &[-1.2247, 0., 1.2247], "按最后一维归一化");
    let Value::Float(j) = v[3] else {
        panic!("方向导数")
    };
    assert!((j - floats(&v[2])[0]).abs() < 1e-8 && j.abs() > 0.1);
    assert_eq!(v[4], v[5]);
    assert_eq!(v[6], v[0]);
}

#[test]
fn p6_1_real_adamw_step_and_no_grad_transformer() {
    let src = r#"
import "std/nn/layers"
import "std/nn/optim"
let cfg = ModelConfig(vocab_size = 12, d_model = 8, n_layers = 1, n_heads = 2, n_kv_heads = 1, d_ff = 16, dtype = "f32", device = "cpu")
let model = causal_lm_init(cfg, 0.15)
let ids = [1,2,3]
let targets = [2,3,4]
let params = model.parameters()
let loss = fn(ps: list<any>)
  return cross_entropy_loss(model.with_parameters(ps).forward(ids), targets)
end
let vg = value_and_grad(loss)(params)
let opt = adamw_new(params, 0.01)
fbip_reset_stats()
let update = opt.step(params, vg[1])
let step_hits = fbip_stats().get("tensor_host_reuses")
let trained = model.with_parameters(update.params)
let reference = trained.forward(ids)
fbip_reset_stats()
let inference = no_grad
  trained.forward(ids)
end
let forward_hits = fbip_stats().get("tensor_host_reuses")
assert(float((reference - inference).abs().max()) < 0.00001)
assert(float((model.forward(ids) - inference).abs().max()) > 0.00001)
assert(float(loss(params)) == float(vg[0]))
[step_hits, forward_hits, update.state.step_count]
"#;
    let v = items(run(src));
    assert!(int(&v[0]) > 0 && int(&v[1]) > 0);
    assert_eq!(int(&v[2]), 1);
}

#[test]
fn p6_1_gpu_row_norm_preserves_vram_and_row_boundaries() {
    for dev in usable_gpu_devices() {
        for dtype in ["f32", "f64", "f16", "bf16"] {
            let src = format!(
                r#"
task norm(x: any): list<any>
  let id = x.storage_id()
  let y = x.rms_norm(0.00001)
  let id2 = y.storage_id()
  let z = y.layer_norm(0.00001)
  let out: list<any> = []
  return out.push(id == id2 and id2 == z.storage_id()).push(z.to_device("cpu").to_list())
end
let x = tensor([[1.0,2.0,3.0], [6.0,4.0,2.0], [2.0,2.0,2.0]])
fbip_reset_stats()
let target = norm(x.to_dtype("{dtype}").to_device("{dev}"))
let hits = fbip_stats().get("tensor_vram_reuses")
let out: list<any> = []
out.push(target).push(norm(x.to_dtype("{dtype}"))).push(hits)
"#
            );
            let v = items(run(&src));
            let target = items(v[0].clone());
            let cpu = items(v[1].clone());
            assert_eq!(target[0], Value::Bool(true));
            assert_eq!(int(&v[2]), 2);
            for (a, b) in items(target[1].clone()).iter().zip(items(cpu[1].clone())) {
                for (actual, expected) in floats(a).iter().zip(floats(&b)) {
                    let tolerance = if dtype == "bf16" { 0.015 } else { 0.0015 };
                    assert!(
                        (actual - expected).abs() < tolerance,
                        "{dev} {dtype}: {actual} vs {expected}"
                    );
                }
            }
        }
    }
}

#[test]
fn p6_1_gpu_binary_reads_logical_coordinates_of_views() {
    for dev in usable_gpu_devices() {
        let src = format!(
            r#"
let a = tensor([[10.0,20.0],[30.0,40.0]]).to_device("{dev}")
let b = tensor([[1.0,2.0],[3.0,4.0]]).to_device("{dev}").transpose()
(a + b).to_device("cpu").to_list()
"#
        );
        let rows = items(run(&src));
        assert_eq!(floats(&rows[0]), [11., 23.]);
        assert_eq!(floats(&rows[1]), [32., 44.]);
    }
}
#[test]
fn p6_1_gpu_row_norm_is_layout_and_overflow_invariant() {
    for dev in usable_gpu_devices() {
        for dtype in ["f32", "f64"] {
            let src = format!(
                r#"
let x = tensor([[65536.0, 65536.0078125], [65536.0078125, 65536.0]]).to_dtype("{dtype}")
let dev = x.to_device("{dev}")
let narrow = x.layer_norm(0.00001).to_list()
let gpu_rows = dev.layer_norm(0.00001).to_device("cpu").to_list()
let view_rows = dev.transpose().layer_norm(0.00001).to_device("cpu").to_list()
let cpu_view = x.transpose().layer_norm(0.00001).to_list()
let small = tensor([[1e20, -1e20]]).to_dtype("{dtype}")
let gpu_small = small.to_device("{dev}").rms_norm(0.00001).to_device("cpu").to_list()
let cpu_small = small.rms_norm(0.00001).to_list()
let out: list<any> = []
out.push(narrow).push(gpu_rows).push(view_rows).push(cpu_view).push(gpu_small).push(cpu_small)
"#
            );
            let v = items(run(&src));
            let narrow = items(v[0].clone());
            let gpu = items(v[1].clone());
            let view = items(v[2].clone());
            let cpu_view = items(v[3].clone());
            let gpu_small = items(v[4].clone());
            let cpu_small = items(v[5].clone());
            let tolerance: f64 = if dtype == "f64" { 1e-9 } else { 1e-4 };
            for (a, b) in gpu.iter().zip(narrow.iter()) {
                assert_close(
                    &floats(a),
                    &floats(b),
                    &format!("{dev} {dtype} contiguous gpu vs cpu"),
                );
            }
            for (a, b) in view.iter().zip(cpu_view.iter()) {
                assert_close(
                    &floats(a),
                    &floats(b),
                    &format!("{dev} {dtype} transposed view"),
                );
            }
            for (a, b) in narrow.iter().zip(cpu_view.iter()) {
                let mut left = floats(a);
                let mut right = floats(b);
                left.sort_by(f64::total_cmp);
                right.sort_by(f64::total_cmp);
                assert_close(
                    &left,
                    &right,
                    &format!("{dev} {dtype} layout-invariant norm"),
                );
            }
            assert_close(
                &floats(&gpu_small[0]),
                &floats(&cpu_small[0]),
                &format!("{dev} {dtype} rms_norm overflow parity"),
            );
            for got in floats(&gpu_small[0]) {
                assert!(
                    (got.abs() - 1.0).abs() < tolerance.max(1e-4),
                    "{dev} {dtype}: rms_norm([1e20, -1e20]) = {got} is not finite/unit"
                );
            }
        }
    }
}

#[test]
fn p6_1_gpu_unary_reuse_holds_for_every_supported_dtype() {
    for dev in usable_gpu_devices() {
        for dtype in ["f32", "f64", "f16", "bf16"] {
            let src = format!(
                r#"
let x = tensor([[1.0, -2.0], [3.0, -4.0]]).to_dtype("{dtype}").to_device("{dev}")
let id = x.storage_id()
fbip_reset_stats()
let y = x.relu()
let out: list<any> = []
out.push(y.storage_id() == id).push(fbip_stats().get("tensor_vram_reuses")).push(fbip_stats().get("tensor_host_reuses")).push(y.to_device("cpu").to_list())
"#
            );
            let v = items(run(&src));
            assert_eq!(
                v[0],
                Value::Bool(true),
                "{dev} {dtype}: relu allocated a new storage instead of reusing"
            );
            assert_eq!(
                int(&v[1]),
                1,
                "{dev} {dtype}: tensor_vram_reuses should be 1, host_reuses={}",
                int(&v[2])
            );
            let rows = items(v[3].clone());
            assert_close(
                &floats(&rows[0]),
                &[1.0, 0.0],
                &format!("{dev} {dtype} relu"),
            );
            assert_close(
                &floats(&rows[1]),
                &[3.0, 0.0],
                &format!("{dev} {dtype} relu"),
            );
        }
    }
}

#[test]
fn p6_1_custom_impl_method_borrow_summary_matches_runtime_dispatch() {
    // The built-in name tables (is_ok, len, ...) describe VM intrinsics. A user impl that
    // reuses one of those names must be summarized from its own param_borrows, never from the
    // name table: here Verdict::is_ok(self) returns self, so a wrapper returning x.is_ok()
    // must not borrow x (the value escapes through the wrapper's return).
    let src = r#"
enum Verdict
  Ok
  Err
end

impl Verdict
  task is_ok(self): Verdict
    return self
  end

  task code(self): int
    return 41
  end
end

task wrap(x: Verdict): Verdict
  return x.is_ok()
end

task read_only(x: Verdict): int
  return x.code()
end

let wrapped = match wrap(Verdict::Ok) with
  Verdict::Ok -> 1
  Verdict::Err -> 0
end
let out: list<any> = []
out.push(wrapped).push(read_only(Verdict::Err))
"#;
    let (mut prog, witnesses) =
        ParserV3::compile(src).expect("compile custom impl summary program");
    assert!(check_program_witnesses(&witnesses).is_empty());
    optimize_program(&mut prog, OptLevel::None);
    let borrows_of = |pred: &dyn Fn(&str) -> bool| -> Vec<bool> {
        prog.functions
            .iter()
            .find(|f| f.name.map(|s| prog.symbols.resolve(s)).is_some_and(pred))
            .unwrap_or_else(|| panic!("function not found"))
            .param_borrows
            .clone()
    };
    assert_eq!(
        borrows_of(&|n| n.ends_with("::is_ok")),
        vec![false],
        "an impl method that returns self must escape its receiver"
    );
    assert_eq!(
        borrows_of(&|n| n.ends_with("::code")),
        vec![true],
        "a read-only impl method may borrow its receiver"
    );
    assert_eq!(
        borrows_of(&|n| n == "wrap"),
        vec![false],
        "the wrapper returns the impl method's value, so its argument escapes"
    );
    assert_eq!(
        borrows_of(&|n| n == "read_only"),
        vec![true],
        "a wrapper over a read-only impl method may borrow its argument"
    );
    let v = items(run(src));
    assert_eq!(int(&v[0]), 1);
    assert_eq!(int(&v[1]), 41);
}

#[test]
fn p6_1_borrowed_calls_do_not_clone_the_outer_arc() {
    // P6-1 passes a borrowed parameter as a slot address into the caller's frame. The debug-only
    // `regslot_heap_clones` trace counts outer `Arc<Value>` refcount bumps on the executing
    // thread: a borrowing call loop must add none, while `borrowed_call_elisions` proves the
    // borrow path (not a copy) was actually taken.
    let src = r#"
task read_len(d: dict<string, int>): int
  return d.len()
end

let base = {"a": 1, "b": 2}
fbip_reset_stats()
let i = 0
let total = 0
while i < 9
  assign total = total + read_len(base)
  assign i = i + 1
end
let stats = fbip_stats()
let out: list<any> = []
out.push(total).push(stats.get("regslot_heap_clones")).push(stats.get("borrowed_call_elisions"))
"#;
    let v = items(run(src));
    assert_eq!(int(&v[0]), 18);
    assert_eq!(
        int(&v[1]),
        0,
        "borrowed calls must not clone the caller's outer Arc<Value>"
    );
    assert!(
        int(&v[2]) >= 9,
        "borrow path must be taken at least once per call, got {}",
        int(&v[2])
    );
}
