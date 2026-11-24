//! Batch 4 Stress & Stability Verification Suite (G1):
//! 1. M:N cooperative scheduler stress (256 Rust futures + 200 concurrent Mora fibers with O(1) TCO)
//! 2. Bounded `Channel` backpressure (8 producers × 40 msgs / 4 consumers over cap=4) & fair 4-arm `select` + cancellation
//! 3. `KVPageAllocator` tight-quota priority preemption, CoW fork, watermark callbacks & physical `swap_out`/`swap_in` (CPU + GPU)
//! 4. Operator fusion (all 4 fused passes) + concurrent `DagPlanCache` / `8×f32` & `4×f64` AVX2 JIT + GPU VRAM slot pooling
//! 5. Reverse-mode tape autograd (`value_and_grad`/`no_grad`) + IR-level symbolic `dag.vjp`/`dag.grad`/`dag.jvp`/HVP numerical gradients
//! 6. Strict 1 MiB OS-thread stack `mora-lsp` full JSON-RPC 2.0 lifecycle stability test

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use mora::common::Device;
use mora::flow::escape_json_string;
use mora::flow::json::{JsonParser, JsonValue};
use mora::lsp::server::Server;
use mora::lsp::transport::{read_message, write_message};
use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::compute_dag::backend_for_device;
use mora::runtime::scheduler::{TaskContext, TaskScheduler};
use mora::runtime::vm::{execute_program, run_main_task};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Result<Value, String> {
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
    let mut core = CoreRuntime::default();
    let prog = Arc::new(prog);
    let (last, globals) =
        execute_program(Arc::clone(&prog), &mut core).map_err(|e| format!("runtime: {e}"))?;
    let main_res =
        run_main_task(prog, globals, &mut core).map_err(|e| format!("runtime(main): {e}"))?;
    if main_res != Value::Nil {
        Ok(main_res)
    } else {
        Ok(last)
    }
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

// ============================================================================
// 1. M:N Cooperative Scheduler Stress (Rust TaskScheduler + 200 Mora Fibers)
// ============================================================================

#[test]
fn g1_stress_mn_cooperative_scheduler_256_fibers_and_tail_calls() {
    // Part A: 256 concurrent async tasks fanned out across a 4-worker bounded TaskScheduler
    let sched = Arc::new(TaskScheduler::with_worker_threads(4).expect("4-worker scheduler"));
    let total_rust: i64 = sched.block_on(async {
        let mut group = sched.group::<i64>(None);
        for i in 1i64..=256 {
            group.spawn(move |_ctx| async move {
                if i % 32 == 0 {
                    mora::runtime::scheduler::sleep(Duration::from_millis(1)).await;
                }
                Ok(i * i)
            });
        }
        let vals = group.join_all().await.expect("all 256 tasks must succeed");
        assert_eq!(vals.len(), 256);
        vals.into_iter().sum()
    });
    // sum_{i=1}^{256} i^2 = 256 * 257 * 513 / 6 = 5,625,216
    assert_eq!(total_rust, 5_625_216);

    // Part B: 200 concurrent Mora VM fibers running tail-recursive accumulators & captured closures
    let src = r#"
task tail_sum(n: int, acc: int): int
  if n <= 0 then
    return acc
  end
  return tail_sum(n - 1, acc + n)
end

task main()
  let handles = []
  let k = 1
  while k <= 200
    let target = k
    let h = spawn fn() tail_sum(target, 0) end
    assign handles = handles.push(h)
    assign k = k + 1
  end

  let total = 0
  let i = 0
  while i < handles.len()
    assign total = total + handles[i].join()
    assign i = i + 1
  end
  [str(handles.len()), str(total)]
end
"#;
    let out = as_str_list(compile_check_and_run(src).expect("200-fiber M:N Mora stress must pass"));
    assert_eq!(out[0], "200");
    // sum_{k=1}^{200} k*(k+1)/2 = 200 * 201 * 202 / 6 = 1,353,400
    assert_eq!(out[1], "1353400");
}

// ============================================================================
// 2. Bounded Channel Backpressure & Fair Multi-Arm Select Stress
// ============================================================================

#[test]
fn g1_stress_channel_bounded_backpressure_and_fair_select() {
    // Part A: 8 producers × 40 messages (= 320 msgs) through a tight cap=4 channel -> 4 consumers
    let src_backpressure = r#"
task producer(ch: channel<int>, worker_id: int, count: int): int
  let i = 1
  while i <= count
    ch.send(worker_id * 1000 + i)
    assign i = i + 1
  end
  return count
end

task consumer(ch: channel<int>, items_to_read: int): int
  let acc = 0
  let n = 0
  while n < items_to_read
    assign acc = acc + ch.recv()
    assign n = n + 1
  end
  return acc
end

task main()
  let ch = channel(4)
  let prods = []
  let w = 1
  while w <= 8
    let wid = w
    assign prods = prods.push(spawn producer(ch, wid, 40))
    assign w = w + 1
  end

  let cons = []
  let c = 0
  while c < 4
    assign cons = cons.push(spawn consumer(ch, 80))
    assign c = c + 1
  end

  let produced = 0
  let pi = 0
  while pi < prods.len()
    assign produced = produced + prods[pi].join()
    assign pi = pi + 1
  end

  let consumed_sum = 0
  let ci = 0
  while ci < cons.len()
    assign consumed_sum = consumed_sum + cons[ci].join()
    assign ci = ci + 1
  end

  ch.close()
  [str(produced), str(consumed_sum), str(ch.is_closed())]
end
"#;
    let out = as_str_list(
        compile_check_and_run(src_backpressure)
            .expect("bounded channel backpressure stress must pass"),
    );
    assert_eq!(out[0], "320");
    // Expected sum: sum_{w=1..8} sum_{i=1..40} (w*1000 + i)
    //   = 40 * 1000 * (8*9/2) + 8 * (40*41/2)
    //   = 1,440,000 + 6,560 = 1,446,560
    assert_eq!(out[1], "1446560");
    assert_eq!(out[2], "true");

    // Part B: Fair 4-channel `select` under simultaneous contention + context cancellation
    let src_select = r#"
task feed(ch: channel<int>, tag: int, count: int): int
  let i = 0
  while i < count
    ch.send(tag)
    assign i = i + 1
  end
  return count
end

task main()
  let c0 = channel(2)
  let c1 = channel(2)
  let c2 = channel(2)
  let c3 = channel(2)

  let p0 = spawn feed(c0, 0, 30)
  let p1 = spawn feed(c1, 1, 30)
  let p2 = spawn feed(c2, 2, 30)
  let p3 = spawn feed(c3, 3, 30)

  let n0 = 0
  let n1 = 0
  let n2 = 0
  let n3 = 0
  let total = 0
  while total < 120
    let tag = select
      case v = c0.recv() -> v
      case v = c1.recv() -> v
      case v = c2.recv() -> v
      case v = c3.recv() -> v
    end
    if tag == 0 then assign n0 = n0 + 1 end
    if tag == 1 then assign n1 = n1 + 1 end
    if tag == 2 then assign n2 = n2 + 1 end
    if tag == 3 then assign n3 = n3 + 1 end
    assign total = total + 1
  end

  p0.join()
  p1.join()
  p2.join()
  p3.join()

  -- Context cancellation propagation check
  let root = context()
  let child = root.child()
  let cancelled_out = channel(1)
  let h_cancel = spawn do
    while !child.is_cancelled()
      sleep(2)
    end
    cancelled_out.send(1)
  end
  root.cancel()
  let cancel_ack = cancelled_out.recv()
  h_cancel.join()

  [str(n0), str(n1), str(n2), str(n3), str(total), str(cancel_ack), str(child.is_cancelled())]
end
"#;
    let out_sel = as_str_list(
        compile_check_and_run(src_select)
            .expect("4-arm fair select & context cancellation stress must pass"),
    );
    assert_eq!(out_sel[0], "30");
    assert_eq!(out_sel[1], "30");
    assert_eq!(out_sel[2], "30");
    assert_eq!(out_sel[3], "30");
    assert_eq!(out_sel[4], "120");
    assert_eq!(out_sel[5], "1");
    assert_eq!(out_sel[6], "true");

    // Part C: Hierarchical TaskContext deadline + bounded Rust channel cancellation under load
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
    sched.block_on(async {
        let (tx, _rx) = sched.channel::<i32>(1).expect("bounded(1)");
        tx.try_send(7).expect("fill slot");
        let root = TaskContext::root();
        let timed = root.with_timeout(Duration::from_millis(20));
        let err = tx
            .send_ctx(99, &timed)
            .await
            .expect_err("blocked send must abort on deadline");
        assert!(
            err.is_cancellation_or_timeout(),
            "expected timeout/cancel, got: {err}"
        );
    });
}

// ============================================================================
// 3. KV-Cache Tight-Quota Preemption, CoW Fork & Physical Swap Stress
// ============================================================================

#[test]
fn g1_stress_kv_cache_page_preemption_and_physical_swap_quota() {
    // Part A: CPU tight-quota multi-sequence preemption + CoW fork + physical swap roundtrips
    // 16 physical pages, 2 tokens/page, 2 heads, head_dim=2, f32 => 64 bytes/page.
    // Hard quota max_bytes = 640 (at most 10 active physical pages).
    let src_cpu = r#"
let kv: kv_allocator = kv_allocator(total_pages=16, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cpu", max_bytes=640)
let wm_hits = atom(0)
kv.on_watermark(0.5, fn(ev)
  swap(wm_hits, fn(c) c + 1 end)
end)

-- Allocate 5 active sequences (2 pages each = 10 pages = 640B = exact hard quota!)
let s = 1
while s <= 5
  let prio = s * 2
  kv.alloc_seq(s, prio)
  kv.append_tokens(s, 3)
  let base = float(s)
  let k0 = tensor([[base, base + 0.25], [base + 0.5, base + 0.75]], dtype="f32", device="cpu")
  let v0 = tensor([[base * 2.0, -base], [base * 0.5, base + 1.0]], dtype="f32", device="cpu")
  kv.write_kv(s, 0, k0, v0)
  assign s = s + 1
end

-- Snapshot seq 3 and seq 4 before swapping them out to host
let snap3 = kv.read_kv(3)
let snap4 = kv.read_kv(4)

-- Priority preemption of lowest-priority seq 1 (prio=2) frees 2 pages
let evicted_id = kv.evict_preempt(5)

-- Swap out seq 3 & seq 4 (frees 4 more pages) -> now 4 pages used (seq 2 + seq 5)
let rel3 = kv.swap_out(3)
let rel4 = kv.swap_out(4)

-- CoW fork high-priority seq 5 -> seq 50 (shares pages with 0 new allocs), then CoW write on seq 50
kv.fork_seq(5, 50)
let k50 = tensor([[99.0, 98.0], [97.0, 96.0]], dtype="f32", device="cpu")
let v50 = tensor([[11.0, 12.0], [13.0, 14.0]], dtype="f32", device="cpu")
kv.write_kv(50, 0, k50, v50)

-- Admit a burst of temporary sequences (6, 7) reusing the freed/swapped pages, then free the survivor
kv.alloc_seq(6, 1)
kv.append_tokens(6, 4)
kv.alloc_seq(7, 1)
kv.append_tokens(7, 4)
let evicted_burst = kv.evict_preempt(3)
if evicted_burst == 6 then
  kv.free_seq(7)
else
  kv.free_seq(6)
end

-- Swap seq 3 and seq 4 back in and verify bit-exact K/V preservation!
kv.swap_in(3)
kv.swap_in(4)
let restored3 = kv.read_kv(3)
let restored4 = kv.read_kv(4)
let err3 = (restored3[0] - snap3[0]).abs().sum() + (restored3[1] - snap3[1]).abs().sum()
let err4 = (restored4[0] - snap4[0]).abs().sum() + (restored4[1] - snap4[1]).abs().sum()

-- Verify CoW isolation: seq 5 token 0 must still start with 5.0 while seq 50 has 99.0
let kv5 = kv.read_kv(5)
let kv50 = kv.read_kv(50)
let cow_ok = (kv5[0][0][0][0] == 5.0f32) and (kv50[0][0][0][0] == 99.0f32)

-- Second round of swap cycles across seq 2, 3, 4, 5 to verify zero leaks across repeated swaps
kv.swap_out(2)
kv.swap_out(5)
kv.swap_in(2)
kv.swap_in(5)

-- Release all remaining sequences
kv.free_seq(2)
kv.free_seq(3)
kv.free_seq(4)
kv.free_seq(5)
kv.free_seq(50)

let st = kv.stats()
[str(evicted_id), str(rel3), str(rel4), str(evicted_burst > 0), str(err3 < 1e-6f32), str(err4 < 1e-6f32), str(cow_ok), str(st.get("used_pages")), str(st.get("bytes_used")), str(st.get("swapped_seqs")), str(st.get("swap_outs")), str(st.get("swap_ins")), str(deref(wm_hits))]
"#;
    let out = as_str_list(
        compile_check_and_run(src_cpu)
            .expect("KV cache CPU quota/preemption/swap stress must pass"),
    );
    assert_eq!(out[0], "1", "lowest-priority seq 1 must be preempted first");
    assert_eq!(out[1], "2");
    assert_eq!(out[2], "2");
    assert_eq!(out[3], "true", "burst low-priority seq must be preempted");
    assert_eq!(out[4], "true", "seq 3 K/V data corrupted across swap");
    assert_eq!(out[5], "true", "seq 4 K/V data corrupted across swap");
    assert_eq!(out[6], "true", "CoW fork isolation violated");
    assert_eq!(out[7], "0", "page leak detected after free_seq");
    assert_eq!(out[8], "0", "byte leak detected after free_seq");
    assert_eq!(out[9], "0", "swapped_seqs leak detected");
    assert_eq!(out[10], "4", "expected 4 swap_outs");
    assert_eq!(out[11], "4", "expected 4 swap_ins");
    let wm_hits: i64 = out[12].parse().expect("wm_hits");
    assert!(wm_hits >= 1, "expected watermark callback to fire");

    // Part B: Physical GPU VRAM swap stress on every live VRAM-resident backend (cuda:0 / vulkan:0)
    for dev_str in ["cuda:0", "vulkan:0"] {
        let dev = Device::parse(dev_str).expect("valid device");
        if !backend_for_device(&dev).supports_vram_residency() {
            continue;
        }
        let src_gpu = format!(
            r#"
let kv: kv_allocator = kv_allocator(total_pages=8, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="{dev_str}")
let round = 0
let max_err = 0.0f32
while round < 4
  let sid = round + 1
  kv.alloc_seq(sid, 5)
  kv.append_tokens(sid, 4)
  let base = float(sid) * 1.5
  let k0 = tensor([[base, -base], [0.5, 1.25]], dtype="f32", device="{dev_str}")
  let v0 = tensor([[2.0, base + 0.5], [-1.0, 3.0]], dtype="f32", device="{dev_str}")
  kv.write_kv(sid, 0, k0, v0)
  let before = kv.read_kv(sid)
  kv.swap_out(sid)
  kv.swap_in(sid)
  let after = kv.read_kv(sid)
  let e = (after[0] - before[0]).abs().sum() + (after[1] - before[1]).abs().sum()
  if e > max_err then assign max_err = e end
  kv.free_seq(sid)
  assign round = round + 1
end
let st = kv.stats()
[str(st.get("vram_backed")), str(st.get("vram_bytes_allocated")), str(st.get("used_pages")), str(st.get("swap_outs")), str(st.get("swap_ins")), str(max_err < 1e-5f32)]
"#
        );
        let gpu_out = as_str_list(
            compile_check_and_run(&src_gpu)
                .unwrap_or_else(|e| panic!("GPU KV swap stress on {dev_str} failed: {e}")),
        );
        assert_eq!(gpu_out[0], "1", "{dev_str}: expected VRAM-backed allocator");
        assert_eq!(
            gpu_out[1], "0",
            "{dev_str}: GPU VRAM leak after freeing all sequences"
        );
        assert_eq!(gpu_out[2], "0", "{dev_str}: page leak");
        assert_eq!(gpu_out[3], "4");
        assert_eq!(gpu_out[4], "4");
        assert_eq!(gpu_out[5], "true", "{dev_str}: K/V mismatch after GPU swap");
    }
}

// ============================================================================
// 4. Operator Fusion, Concurrent JIT (f32 & f64) & GPU VRAM Slot Reuse Stress
// ============================================================================

#[test]
fn g1_stress_operator_fusion_vram_slot_reuse_and_f32_f64_jit_concurrency() {
    // Part A: Concurrent multi-fiber execution across all 4 fused passes + f32 & f64 native JIT
    let src_cpu = r#"
let x32 = tensor([[0.5, -0.25, 1.0, 0.75, -0.5, 0.25, 1.5, -0.75, 0.5], [-0.5, 0.5, 0.25, -1.0, 1.25, -0.25, 0.75, 0.5, -0.25]], dtype="f32", device="cpu")
let y32 = tensor([[1.0, 0.5, 1.5, 0.25, 2.0, 0.5, 1.0, 0.5, 1.25], [0.5, 1.5, 0.25, 1.0, 0.5, 2.0, 0.5, 1.0, 0.75]], dtype="f32", device="cpu")
let x64 = tensor([[0.5, -0.25, 1.0, 0.75, -0.5, 0.25], [-0.5, 0.5, 0.25, -1.0, 1.25, -0.25]], dtype="f64", device="cpu")
let w64 = tensor([[0.2, -0.1, 0.3, 0.1], [-0.1, 0.25, -0.15, 0.2], [0.15, 0.05, 0.2, -0.25], [-0.2, 0.15, 0.1, 0.3], [0.1, -0.2, 0.25, -0.05], [0.05, 0.1, -0.2, 0.15]], dtype="f64", device="cpu")
let g64 = tensor([1.0, 0.9, 1.1, 0.95, 1.05, 1.0], dtype="f64", device="cpu")
let b64 = tensor([0.1, -0.1, 0.05, 0.2], dtype="f64", device="cpu")

let q64 = tensor([[[0.4, -0.2, 0.5, 0.1], [-0.1, 0.3, 0.2, -0.4], [0.2, 0.1, -0.3, 0.4]]], dtype="f64", device="cpu")
let k64 = tensor([[[0.2, 0.3, -0.1, 0.4], [-0.3, 0.1, 0.5, -0.2], [0.4, -0.2, 0.1, 0.3]]], dtype="f64", device="cpu")
let v64 = tensor([[[1.0, 0.5, -0.5, 0.25], [0.25, -0.75, 0.5, 1.0], [-0.5, 0.25, 1.0, -0.5]]], dtype="f64", device="cpu")

let d_ew32 = trace_dag(fn(a, b) ((a * b + a - 0.125f32).relu().sqrt() + (a * 0.5f32).abs()) * 1.25f32 end, x32, y32).compile()
let d_ew64 = trace_dag(fn(a) ((a * a + 0.5).sqrt() - a.abs() * 0.25).relu() + 1.0 end, x64).compile()
let d_rn64 = trace_dag(fn(a, g, w, b) ((a.rms_norm(1e-5) * g) @ w + b).silu() end, x64, g64, w64, b64).compile()
let d_la64 = trace_dag(fn(a, w, b) (a @ w + b).gelu() end, x64, w64, b64).compile()
let d_fa64 = trace_dag(fn(qq, kk, vv) ((qq @ kk.transpose(1, 2)) * 0.5).softmax() @ vv end, q64, k64, v64).compile()

-- Reference eager outputs
let ref_ew32 = ((x32 * y32 + x32 - 0.125f32).relu().sqrt() + (x32 * 0.5f32).abs()) * 1.25f32
let ref_ew64 = ((x64 * x64 + 0.5).sqrt() - x64.abs() * 0.25).relu() + 1.0
let ref_rn64 = ((x64.rms_norm(1e-5) * g64) @ w64 + b64).silu()
let ref_la64 = (x64 @ w64 + b64).gelu()
let ref_fa64 = ((q64 @ k64.transpose(1, 2)) * 0.5).softmax() @ v64

-- Spawn 16 concurrent workers stressing all 5 compiled fused DAGs in parallel
let workers = []
let i = 0
while i < 16
  let h = spawn fn()
    let e1 = (d_ew32.run(x32, y32) - ref_ew32).abs().sum()
    let e2 = (d_ew64.run(x64) - ref_ew64).abs().sum()
    let e3 = (d_rn64.run(x64, g64, w64, b64) - ref_rn64).abs().sum()
    let e4 = (d_la64.run(x64, w64, b64) - ref_la64).abs().sum()
    let e5 = (d_fa64.run(q64, k64, v64) - ref_fa64).abs().sum()
    return (e1 < 1e-4f32) and (e2 < 1e-10) and (e3 < 1e-10) and (e4 < 1e-10) and (e5 < 1e-10)
  end
  assign workers = workers.push(h)
  assign i = i + 1
end

let all_ok = true
let j = 0
while j < workers.len()
  if !workers[j].join() then
    assign all_ok = false
  end
  assign j = j + 1
end

let rep32 = d_ew32.cost_report()
let rep64 = d_ew64.cost_report()
[str(all_ok), str(d_rn64.is_fused()), str(d_la64.is_fused()), str(d_fa64.is_fused()), str(rep32.get("native_jit_isa")), str(rep32.get("native_jit_f32_dispatches")), str(rep64.get("native_jit_dispatches"))]
"#;
    let out = as_str_list(
        compile_check_and_run(src_cpu)
            .expect("concurrent operator fusion + f32/f64 JIT stress must pass"),
    );
    assert_eq!(
        out[0], "true",
        "concurrent fused DAG worker parity mismatch"
    );
    assert_eq!(out[1], "true", "RmsNormLinear should fuse");
    assert_eq!(out[2], "true", "LinearAct should fuse");
    assert_eq!(out[3], "true", "FlashAttention should fuse");
    if out[4] == "x86_64-avx2-jit" {
        let f32_d: i64 = out[5].parse().expect("f32 dispatches");
        let f64_d: i64 = out[6].parse().expect("f64 dispatches");
        assert!(f32_d >= 16, "expected >=16 f32 JIT dispatches, got {f32_d}");
        assert!(f64_d >= 16, "expected >=16 f64 JIT dispatches, got {f64_d}");
    }

    // Part B: Repeated GPU VRAM scratch slot pooling stress on live GPU backends
    for dev_str in ["cuda:0", "vulkan:0"] {
        let dev = Device::parse(dev_str).expect("valid device");
        if !backend_for_device(&dev).supports_vram_residency() {
            continue;
        }
        let src_gpu = format!(
            r#"
let x = tensor([[0.5, -0.25, 0.75, 0.25], [-0.5, 0.5, -0.25, 1.0]], dtype="f32", device="{dev_str}")
let w1 = tensor([[0.2, 0.1, -0.1, 0.3], [0.1, 0.2, 0.1, -0.1], [-0.1, 0.1, 0.2, 0.1], [0.2, -0.1, 0.1, 0.2]], dtype="f32", device="{dev_str}")
let w2 = tensor([[0.15, -0.1, 0.2, 0.1], [0.2, 0.15, -0.1, 0.1], [0.1, 0.2, 0.15, -0.1], [-0.1, 0.1, 0.2, 0.15]], dtype="f32", device="{dev_str}")
let w3 = tensor([[0.1, 0.2, -0.1, 0.15], [-0.1, 0.1, 0.2, 0.1], [0.15, -0.1, 0.1, 0.2], [0.2, 0.15, -0.1, 0.1]], dtype="f32", device="{dev_str}")
let w4 = tensor([[0.25, -0.15], [0.1, 0.2], [-0.1, 0.25], [0.2, 0.1]], dtype="f32", device="{dev_str}")

let dag = trace_dag(fn(a, m1, m2, m3, m4) (((((a @ m1).relu() @ m2).tanh() @ m3).silu()) @ m4) end, x, w1, w2, w3, w4).compile()
let ref_out = dag.run(x, w1, w2, w3, w4)
let max_diff = 0.0f32
let it = 0
while it < 12
  let cur = dag.run(x, w1, w2, w3, w4)
  let d = (cur - ref_out).abs().sum()
  if d > max_diff then assign max_diff = d end
  assign it = it + 1
end
let mp = dag.memory_plan()
[str(mp.get("total_intermediates")), str(mp.get("vram_peak_slots")), str(mp.get("vram_scratch_allocs")), str(mp.get("vram_buffer_reuses")), str(max_diff < 1e-5f32)]
"#
        );
        let g = as_str_list(
            compile_check_and_run(&src_gpu)
                .unwrap_or_else(|e| panic!("GPU VRAM slot reuse stress on {dev_str} failed: {e}")),
        );
        let inter: i64 = g[0].parse().expect("inter");
        let slots: i64 = g[1].parse().expect("slots");
        let allocs: i64 = g[2].parse().expect("allocs");
        let reuses: i64 = g[3].parse().expect("reuses");
        assert!(
            slots > 0 && slots < inter,
            "{dev_str}: slots={slots} inter={inter}"
        );
        assert!(
            allocs <= slots,
            "{dev_str}: allocs={allocs} > slots={slots}"
        );
        assert!(
            reuses >= 30,
            "{dev_str}: expected >=30 VRAM reuses, got {reuses}"
        );
        assert_eq!(
            g[4], "true",
            "{dev_str}: non-deterministic output across VRAM slot reuse"
        );
    }
}

// ============================================================================
// 5. Autograd & IR-Level Symbolic VJP/JVP/HVP Numerical Gradient Stress
// ============================================================================

#[test]
fn g1_stress_autograd_and_ir_dag_vjp_jvp_numerical_gradients() {
    let src = r#"
let x = tensor([[0.65, -0.35, 0.85, -0.2], [-0.45, 0.75, 0.15, 0.55]], dtype="f64", device="cpu")
let w1 = tensor([[0.4, -0.25, 0.3, -0.15], [0.15, 0.5, -0.35, 0.2], [-0.2, 0.35, 0.6, -0.1], [0.25, -0.15, 0.2, 0.45]], dtype="f64", device="cpu")
let b1 = tensor([0.1, -0.15, 0.2, -0.05], dtype="f64", device="cpu")
let w2 = tensor([[0.3, -0.4, 0.2, 0.1], [-0.2, 0.35, 0.45, -0.15], [0.25, 0.15, -0.3, 0.4], [-0.1, 0.2, 0.35, -0.25]], dtype="f64", device="cpu")

-- Deep multi-layer block combining RMSNorm, MatMul, Bias, GELU, SiLU, Softmax, Tanh & LayerNorm
task block_loss(a: Tensor<f64, [2, 4], cpu>, m1: Tensor<f64, [4, 4], cpu>, bi: Tensor<f64, [4], cpu>, m2: Tensor<f64, [4, 4], cpu>): float
  let h1 = (a.rms_norm(1e-5) @ m1 + bi).gelu()
  let h2 = (h1 @ m2).silu() + h1.tanh() * 0.25
  let p = h2.softmax()
  let norm = h2.layer_norm(1e-5)
  return (p * norm + h1 * 0.1).sum()
end

-- 1. Tape reverse-mode `value_and_grad` across all 4 tensor arguments
let vg = value_and_grad(block_loss, argnums=[0, 1, 2, 3])(x, w1, b1, w2)
let tape_gx = vg[1][0]
let tape_gw1 = vg[1][1]
let tape_gb1 = vg[1][2]
let tape_gw2 = vg[1][3]

-- 2. IR-level symbolic `dag.grad` (fused) across all 4 arguments
let fwd_dag = trace_dag(block_loss, x, w1, b1, w2)
let ir_gx = fwd_dag.grad(0).compile().run(x, w1, b1, w2)
let ir_gw1 = fwd_dag.grad(1).compile().run(x, w1, b1, w2)
let ir_gb1 = fwd_dag.grad(2).compile().run(x, w1, b1, w2)
let ir_gw2 = fwd_dag.grad(3).compile().run(x, w1, b1, w2)

let tape_vs_ir = (tape_gx - ir_gx).abs().sum() + (tape_gw1 - ir_gw1).abs().sum() + (tape_gb1 - ir_gb1).abs().sum() + (tape_gw2 - ir_gw2).abs().sum()

-- 3. Full central finite-difference numerical check on every element of x (8 elems) & b1 (4 elems)
let eps = 1e-6
let max_fd_err = 0.0
let r = 0
while r < 2
  let c = 0
  while c < 4
    let delta = tensor([[if r == 0 and c == 0 then eps else 0.0 end, if r == 0 and c == 1 then eps else 0.0 end, if r == 0 and c == 2 then eps else 0.0 end, if r == 0 and c == 3 then eps else 0.0 end], [if r == 1 and c == 0 then eps else 0.0 end, if r == 1 and c == 1 then eps else 0.0 end, if r == 1 and c == 2 then eps else 0.0 end, if r == 1 and c == 3 then eps else 0.0 end]], dtype="f64", device="cpu")
    let fd = (block_loss(x + delta, w1, b1, w2) - block_loss(x - delta, w1, b1, w2)) / (2.0 * eps)
    let g_val = ir_gx.to_list()[r][c]
    let diff = math.abs(g_val - fd)
    if diff > max_fd_err then assign max_fd_err = diff end
    assign c = c + 1
  end
  assign r = r + 1
end

let bi_idx = 0
while bi_idx < 4
  let db = tensor([if bi_idx == 0 then eps else 0.0 end, if bi_idx == 1 then eps else 0.0 end, if bi_idx == 2 then eps else 0.0 end, if bi_idx == 3 then eps else 0.0 end], dtype="f64", device="cpu")
  let fd_b = (block_loss(x, w1, b1 + db, w2) - block_loss(x, w1, b1 - db, w2)) / (2.0 * eps)
  let gb_val = ir_gb1.to_list()[bi_idx]
  let diff_b = math.abs(gb_val - fd_b)
  if diff_b > max_fd_err then assign max_fd_err = diff_b end
  assign bi_idx = bi_idx + 1
end

-- 4. Multi-direction JVP/VJP adjointness stress: dot(jvp(x, dx), dy) == dot(dx, vjp(x, dy)) over 6 directions
task vec_layer(a: Tensor<f64, [2, 4], cpu>, m1: Tensor<f64, [4, 4], cpu>, bi: Tensor<f64, [4], cpu>): Tensor<f64, [2, 4], cpu>
  return (a.rms_norm(1e-5) @ m1 + bi).silu() + a.sigmoid() * 0.5 + a.layer_norm(1e-5)
end
let vdag = trace_dag(vec_layer, x, w1, b1)
let jvp_fn = vdag.jvp(0).compile()
let vjp_fn = vdag.vjp(0).compile()
let max_adj_err = 0.0
let d = 1
while d <= 6
  let s = float(d) * 0.17
  let dx = tensor([[s, -s * 0.7, s * 0.4, -s * 0.2], [s * 0.3, -s * 0.5, s * 0.9, -s * 0.6]], dtype="f64", device="cpu")
  let dy = tensor([[-s * 0.4, s * 0.8, -s * 0.3, s * 0.5], [s * 0.6, -s * 0.2, -s * 0.7, s * 0.4]], dtype="f64", device="cpu")
  let lhs = (jvp_fn.run(x, w1, b1, dx) * dy).sum()
  let rhs = (dx * vjp_fn.run(x, w1, b1, dy)).sum()
  let aerr = math.abs(lhs - rhs)
  if aerr > max_adj_err then assign max_adj_err = aerr end
  assign d = d + 1
end

-- 5. 2nd-order symbolic HVP `grad(0).jvp(0)` checked against finite difference of `grad(0)`
let hvp_dag = fwd_dag.grad(0).jvp(0).compile()
let grad_dag = fwd_dag.grad(0).compile()
let v_dir = tensor([[0.3, -0.5, 0.2, 0.4], [-0.2, 0.6, -0.3, 0.1]], dtype="f64", device="cpu")
let hvp_exact = hvp_dag.run(x, w1, b1, w2, v_dir)
let h_eps = 1e-6
let hvp_fd = (grad_dag.run(x + v_dir * h_eps, w1, b1, w2) - grad_dag.run(x - v_dir * h_eps, w1, b1, w2)) / (2.0 * h_eps)
let hvp_err = (hvp_exact - hvp_fd).abs().sum()

[str(tape_vs_ir < 1e-9), str(max_fd_err < 1e-5), str(max_adj_err < 1e-10), str(hvp_err < 1e-4)]
"#;
    let out = as_str_list(
        compile_check_and_run(src)
            .expect("autograd & IR-level VJP/JVP/HVP numerical gradient stress must pass"),
    );
    assert_eq!(out[0], "true", "tape vs IR symbolic grad mismatch");
    assert_eq!(
        out[1], "true",
        "central finite-difference numerical gradient mismatch"
    );
    assert_eq!(out[2], "true", "VJP/JVP adjointness identity mismatch");
    assert_eq!(out[3], "true", "2nd-order HVP finite-difference mismatch");
}

// ============================================================================
// 6. Strict 1 MiB OS-Thread Stack `mora-lsp` Full Protocol Stability Test
// ============================================================================

fn build_deep_lsp_test_document() -> String {
    let mut doc = String::from(
        r#"-- Deep multi-construct Mora fixture for 1 MiB stack LSP stability verification
enum EvalResult<T>
  Ok(T)
  Err(string)
end

trait Scorable
  task score(self): float ! pure
end

struct LayerConfig<const D: int>
  name: string @tag(json = "name")
  scale: float = 1.0
  weight: Tensor<f64, [D, D], cpu>
end

impl<const D: int> Scorable for LayerConfig<D>
  task score(self): float ! pure
    return self.scale * 2.0
  end
end

task safe_divide(a: float, b: float): EvalResult<float> ! pure
  if b == 0.0 then
    return EvalResult::Err("div zero")
  end
  return EvalResult::Ok(a / b)
end

task compute_pipeline(x: float, y: float): EvalResult<float> ! pure
  defer 0
  let q = safe_divide(x, y)?
  let nested = if q > 10.0 then
    match int(q) with
      11 -> q + 1.0
      12 -> q + 2.0
      _ -> q * 0.5
    end
  else
    q + 3.0
  end
  return EvalResult::Ok(nested)
end

task deep_arith(seed: int): int ! pure
  let acc = seed
"#,
    );
    // Add 128-level nested parenthesized arithmetic expression & chained `if` branches to prove O(1) OS stack
    let mut expr = "acc".to_string();
    for i in 1..=128 {
        expr = format!("({expr} + {i})");
    }
    doc.push_str(&format!("  let deep_val = {expr}\n"));
    for i in 0..16 {
        doc.push_str(&format!(
            "  let step_{i} = if deep_val > {i} then deep_val - {i} else deep_val + {i} end\n"
        ));
    }
    doc.push_str("  return step_15\nend\n\nlet top_res = deep_arith(42)\n");
    doc
}

fn push_framed(buf: &mut Vec<u8>, body: &str) {
    write_message(buf, body).expect("frame LSP message");
}

#[test]
fn g1_stress_mora_lsp_1mb_os_thread_stack_stability() {
    const ONE_MIB: usize = 1024 * 1024;

    let handle = std::thread::Builder::new()
        .name("mora-lsp-1mb-stack".to_string())
        .stack_size(ONE_MIB)
        .spawn(|| {
            let rich_doc = build_deep_lsp_test_document();
            let rich_uri = "file:///stress/deep_model.mora";
            let err_uri = "file:///stress/bad_types.mora";
            let bad_doc = "let broken: int = \"not_an_int\"\n";

            let mut wire_in: Vec<u8> = Vec::new();
            // 1. initialize + initialized
            push_framed(
                &mut wire_in,
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
            );
            push_framed(
                &mut wire_in,
                r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            );

            // 2. didOpen rich clean document + bad type document
            push_framed(
                &mut wire_in,
                &format!(
                    r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{rich_uri}","languageId":"mora","version":1,"text":"{}"}}}}}}"#,
                    escape_json_string(&rich_doc)
                ),
            );
            push_framed(
                &mut wire_in,
                &format!(
                    r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{err_uri}","languageId":"mora","version":1,"text":"{}"}}}}}}"#,
                    escape_json_string(bad_doc)
                ),
            );

            // 3. Full LSP provider sweep on the deep document (ids 2..=11)
            let requests = [
                format!(r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":24,"character":6}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":3,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":25,"character":2}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":33,"character":12}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":5,"method":"textDocument/references","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":24,"character":6}},"context":{{"includeDeclaration":true}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":6,"method":"textDocument/documentSymbol","params":{{"textDocument":{{"uri":"{rich_uri}"}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":7,"method":"textDocument/formatting","params":{{"textDocument":{{"uri":"{rich_uri}"}},"options":{{"tabSize":2,"insertSpaces":true}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":8,"method":"textDocument/rangeFormatting","params":{{"textDocument":{{"uri":"{rich_uri}"}},"range":{{"start":{{"line":0,"character":0}},"end":{{"line":15,"character":0}}}},"options":{{"tabSize":2,"insertSpaces":true}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":9,"method":"textDocument/rename","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":24,"character":6}},"newName":"safe_div"}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":10,"method":"textDocument/semanticTokens/full","params":{{"textDocument":{{"uri":"{rich_uri}"}}}}}}"#),
                format!(r#"{{"jsonrpc":"2.0","id":11,"method":"textDocument/foldingRange","params":{{"textDocument":{{"uri":"{rich_uri}"}}}}}}"#),
            ];
            for req in &requests {
                push_framed(&mut wire_in, req);
            }

            // 4. Rapid burst of 10 incremental didChange edits + didSave + repeated hover/completion
            let mut cur_line = rich_doc.lines().count();
            for edit_idx in 0..10 {
                let version = edit_idx + 2;
                let extra = format!("let burst_{edit_idx}: int = deep_arith({edit_idx})\n");
                push_framed(
                    &mut wire_in,
                    &format!(
                        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{rich_uri}","version":{version}}},"contentChanges":[{{"range":{{"start":{{"line":{cur_line},"character":0}},"end":{{"line":{cur_line},"character":0}}}},"text":"{}"}}]}}}}"#,
                        escape_json_string(&extra)
                    ),
                );
                cur_line += 1;
            }
            push_framed(
                &mut wire_in,
                &format!(
                    r#"{{"jsonrpc":"2.0","method":"textDocument/didSave","params":{{"textDocument":{{"uri":"{rich_uri}"}}}}}}"#
                ),
            );
            push_framed(
                &mut wire_in,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":12,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{rich_uri}"}},"position":{{"line":{},"character":6}}}}}}"#,
                    cur_line - 1
                ),
            );

            // 5. Fix the bad_types document incrementally and confirm diagnostics clear to 0
            push_framed(
                &mut wire_in,
                &format!(
                    r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{err_uri}","version":2}},"contentChanges":[{{"text":"let fixed: int = 123\n"}}]}}}}"#
                ),
            );

            // 6. shutdown + exit
            push_framed(
                &mut wire_in,
                r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#,
            );
            push_framed(
                &mut wire_in,
                r#"{"jsonrpc":"2.0","method":"exit","params":null}"#,
            );

            // Run the REAL `mora::lsp::server::Server` loop inside this 1 MiB stack thread!
            let mut server = Server::new();
            let mut reader = Cursor::new(wire_in);
            let mut wire_out: Vec<u8> = Vec::new();
            server
                .run_with_io(&mut reader, &mut wire_out)
                .expect("mora-lsp Server::run_with_io must complete cleanly on 1 MiB stack");

            // Decode and validate all emitted JSON-RPC 2.0 responses & notifications
            let mut out_cursor = Cursor::new(wire_out);
            let mut seen_ids: Vec<i64> = Vec::new();
            let mut diag_counts_rich: Vec<usize> = Vec::new();
            let mut diag_counts_err: Vec<usize> = Vec::new();
            let mut saw_symbols = false;
            let mut saw_tokens = false;
            let mut saw_folds = false;

            while let Some(raw) = read_message(&mut out_cursor).expect("valid framed output") {
                let msg = JsonParser::new(&raw)
                    .parse_value()
                    .expect("valid JSON-RPC response");
                if let Some(id_num) = msg.get("id").and_then(|v| v.as_i64()) {
                    seen_ids.push(id_num);
                    assert!(
                        msg.get("error").is_none(),
                        "unexpected LSP JSON-RPC error for id={id_num}: {raw}"
                    );
                    if id_num == 6
                        && let Some(arr) = msg.get("result").and_then(|v| v.as_array())
                    {
                        saw_symbols = !arr.is_empty();
                    }
                    if id_num == 10
                        && let Some(data) = msg
                            .get("result")
                            .and_then(|v| v.get("data"))
                            .and_then(|v| v.as_array())
                    {
                        saw_tokens = !data.is_empty();
                    }
                    if id_num == 11
                        && let Some(arr) = msg.get("result").and_then(|v| v.as_array())
                    {
                        saw_folds = !arr.is_empty();
                    }
                } else if msg.get("method").and_then(JsonValue::as_str)
                    == Some("textDocument/publishDiagnostics")
                {
                    let params = msg.get("params").expect("diag params");
                    let uri = params
                        .get("uri")
                        .and_then(JsonValue::as_str)
                        .expect("diag uri");
                    let count = params
                        .get("diagnostics")
                        .and_then(|v| v.as_array())
                        .map_or(0, Vec::len);
                    if uri == rich_uri {
                        diag_counts_rich.push(count);
                    } else if uri == err_uri {
                        diag_counts_err.push(count);
                    }
                }
            }

            assert_eq!(
                seen_ids,
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 99],
                "every LSP request must receive an ordered response"
            );
            assert!(
                !diag_counts_rich.is_empty() && diag_counts_rich.iter().all(|&c| c == 0),
                "rich document diagnostics should be 0 across open/10 edits/save, got {diag_counts_rich:?}"
            );
            assert_eq!(
                diag_counts_err,
                vec![1, 0],
                "bad_types doc must report 1 diagnostic on open and 0 after fix"
            );
            assert!(saw_symbols, "documentSymbol must return non-empty symbols");
            assert!(saw_tokens, "semanticTokens/full must return non-empty data");
            assert!(saw_folds, "foldingRange must return non-empty folds");
        })
        .expect("spawn 1 MiB stack thread");

    handle
        .join()
        .expect("mora-lsp must NOT overflow a 1 MiB OS thread stack");
}
