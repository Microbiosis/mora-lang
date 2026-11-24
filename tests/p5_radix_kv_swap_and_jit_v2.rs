//! P5-2 Integration & Live Hardware Verification Suite:
//! - P5-2A: `KVPageAllocator` Content-Addressed RadixAttention Prefix Cache (`cache_prefix`,
//!   `match_prefix`, `attach_prefix`), CoW isolation on cached pages, automatic LRU reclamation
//!   under pool/quota pressure, and live GPU VRAM prefix sharing + `swap_out`/`swap_in` integrity.
//! - P5-2B: Zero-Dep W^X Native JIT v2 — `8×f32` AVX2 `FusedElementwise` and `4×8`/`4×4` AVX2+FMA
//!   `FusedLinearAct` register-blocked microkernels + `MORA_DISABLE_NATIVE_JIT=1` parity switch.

use std::process::Command;
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn check_source(src: &str) -> Result<(), String> {
    let witnesses = ParserV3::parse_witnesses(src).map_err(|e| format!("parse error: {e}"))?;
    let errors = check_program_witnesses(&witnesses);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors
            .iter()
            .map(|e| format!("line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn eval_source(src: &str) -> Result<Value, String> {
    check_source(src)?;
    let (prog, _witnesses) = ParserV3::compile(src).map_err(|e| format!("compile error: {e}"))?;
    let mut rt = CoreRuntime::default();
    let (val, _globals) =
        execute_program(Arc::new(prog), &mut rt).map_err(|e| format!("runtime error: {e}"))?;
    Ok(val)
}

fn available_devices() -> Vec<String> {
    let mut devs = vec!["cpu".to_string()];
    if let Ok(Value::List(items)) = eval_source("Tensor::devices()") {
        for v in items {
            if let Value::String(s) = v
                && !devs.contains(&s)
            {
                devs.push(s);
            }
        }
    }
    devs
}

#[test]
fn p5_2a_radix_prefix_cache_multiturn_cow_and_lru_reclamation() {
    let src = r#"
let kv = kv_allocator(6, 2, 2, 4, "f32", "cpu")

-- Turn 1: prefill 4-token shared system prompt [10, 20, 30, 40] + 1 user token [99] (3 pages)
kv.alloc_seq(1)
kv.append_tokens(1, 5)
let p = 0
while p < 5
  let base = (p + 1) * 1.0
  let kt = Tensor::full([2, 4], base, "f32")
  let vt = Tensor::full([2, 4], base * 10.0, "f32")
  kv.write_kv(1, p, kt, vt)
  p = p + 1
end

-- Cache full 2-token blocks: [10, 20] (blk 0) and [30, 40] (blk 1); trailing token 99 is partial -> 2 blocks cached!
let cached_blocks = kv.cache_prefix(1, [10, 20, 30, 40, 99])
assert_eq(cached_blocks, 2)
assert_eq(kv.match_prefix([10, 20, 30, 40, 777]), 4)
assert_eq(kv.match_prefix([10, 20, 55, 66]), 2)
assert_eq(kv.match_prefix([99, 20, 30, 40]), 0)

let pages_seq1 = kv.seq_pages(1)

-- Free Turn 1: trailing page 2 is reclaimed immediately, while cached prefix pages 0..2 STAY WARM in radix cache!
let freed_after_turn1 = kv.free_seq(1)
assert_eq(freed_after_turn1, 1)
assert_eq(kv.used_pages(), 2)
assert_eq(kv.match_prefix([10, 20, 30, 40]), 4)

-- Turn 2: zero-copy attach 4-token shared prefix, append 2 new user tokens [50, 60]
kv.alloc_seq(2)
let matched_t2 = kv.attach_prefix(2, [10, 20, 30, 40, 50, 60])
assert_eq(matched_t2, 4)
assert_eq(kv.seq_tokens(2), 4)
-- Physical pages 0 and 1 are IDENTICAL to Turn 1's cached pages (zero copy!)
let pages_seq2_prefix = kv.seq_pages(2)
assert_eq(pages_seq2_prefix[0], pages_seq1[0])
assert_eq(pages_seq2_prefix[1], pages_seq1[1])
assert_eq(kv.used_pages(), 2)

-- Append uncached suffix tokens 4 & 5
kv.append_tokens(2, 2)
kv.write_kv(2, 4, Tensor::full([2, 4], 50.0, "f32"), Tensor::full([2, 4], 500.0, "f32"))
kv.write_kv(2, 5, Tensor::full([2, 4], 60.0, "f32"), Tensor::full([2, 4], 600.0, "f32"))

-- CoW safety test: mutating token 1 (in cached block 0) on seq 2 must split page via CoW
-- WITHOUT corrupting the immutable cached radix prefix!
kv.write_kv(2, 1, Tensor::full([2, 4], -999.0, "f32"), Tensor::full([2, 4], -999.0, "f32"))
let pages_seq2_after_cow = kv.seq_pages(2)
assert(pages_seq2_after_cow[0] != pages_seq1[0])

-- Turn 3 attaches the same prefix and MUST see original value 2.0 (NOT -999.0)!
kv.alloc_seq(3)
assert_eq(kv.attach_prefix(3, [10, 20, 30, 40]), 4)
let kv3 = kv.read_kv(3)
let k3 = kv3[0].to_list()
assert_eq(k3[1][0][0], 2.0f32)

-- Free active sequences 2 and 3; cached radix prefix remains warm (2 pages used, 4 free out of 6 total)
kv.free_seq(2)
kv.free_seq(3)
assert_eq(kv.used_pages(), 2)

-- Automatic LRU Radix Reclamation: allocate a new sequence needing ALL 6 pages (12 tokens).
-- Allocator MUST automatically evict the 2 inactive radix-cached pages instead of OOMing!
kv.alloc_seq(4)
kv.append_tokens(4, 12)
assert_eq(kv.used_pages(), 6)
assert_eq(kv.free_pages(), 0)
assert_eq(kv.match_prefix([10, 20, 30, 40]), 0)

let st = kv.stats()
assert_eq(st["prefix_cache_hits"], 2)
assert_eq(st["prefix_cached_pages"], 0)
assert_eq(st["prefix_evictions"], 2)
kv.free_seq(4)
st["prefix_cache_hits"]
"#;
    let val = eval_source(src).expect("RadixAttention prefix cache + CoW + LRU eviction");
    assert_eq!(val, Value::Int(2));
}

#[test]
fn p5_2a_live_gpu_vram_radix_prefix_and_swap_under_quota() {
    for dev in available_devices() {
        let src = format!(
            r#"
-- 4 physical pages x 2 tokens x 2 heads x 4 head_dim x f32 (each page = 2 * 2*2*4*4 = 128 bytes)
-- Hard quota = 384 bytes (3 pages max!)
let kv = kv_allocator(4, 2, 2, 4, "f32", "{dev}", 384)

kv.alloc_seq(10)
kv.append_tokens(10, 4)
kv.write_kv(10, 0, Tensor::full([2, 4], 1.5, "f32", "{dev}"), Tensor::full([2, 4], 10.5, "f32", "{dev}"))
kv.write_kv(10, 1, Tensor::full([2, 4], 2.5, "f32", "{dev}"), Tensor::full([2, 4], 20.5, "f32", "{dev}"))
kv.write_kv(10, 2, Tensor::full([2, 4], 3.5, "f32", "{dev}"), Tensor::full([2, 4], 30.5, "f32", "{dev}"))
kv.write_kv(10, 3, Tensor::full([2, 4], 4.5, "f32", "{dev}"), Tensor::full([2, 4], 40.5, "f32", "{dev}"))
assert_eq(kv.cache_prefix(10, [1, 2, 3, 4]), 2)

-- Swap seq 10 out to host RAM; physical pages remain pinned in radix cache while seq 10 is swapped!
kv.swap_out(10)
assert_eq(kv.is_swapped(10), true)

-- New seq 11 zero-copy attaches the warm GPU VRAM prefix (uses 0 extra pages!) + 1 new page (3/3 quota!)
kv.alloc_seq(11)
assert_eq(kv.attach_prefix(11, [1, 2, 3, 4, 5, 6]), 4)
kv.append_tokens(11, 2)
kv.write_kv(11, 4, Tensor::full([2, 4], 5.5, "f32", "{dev}"), Tensor::full([2, 4], 50.5, "f32", "{dev}"))
kv.write_kv(11, 5, Tensor::full([2, 4], 6.5, "f32", "{dev}"), Tensor::full([2, 4], 60.5, "f32", "{dev}"))
let r11 = kv.read_kv(11)
let k11 = r11[0].to("cpu").to_list()
let v11 = r11[1].to("cpu").to_list()
assert_eq(k11[0][0][0], 1.5f32)
assert_eq(k11[3][0][0], 4.5f32)
assert_eq(k11[5][0][0], 6.5f32)
assert_eq(v11[2][0][0], 30.5f32)

-- Free seq 11; now swap_in seq 10 needs 2 pages under 3-page quota -> automatically reclaims LRU radix pages!
kv.free_seq(11)
kv.swap_in(10)
assert_eq(kv.is_swapped(10), false)
let r10 = kv.read_kv(10)
let k10 = r10[0].to("cpu").to_list()
let v10 = r10[1].to("cpu").to_list()
assert_eq(k10[0][0][0], 1.5f32)
assert_eq(k10[3][0][0], 4.5f32)
assert_eq(v10[1][0][0], 20.5f32)
assert_eq(v10[3][0][0], 40.5f32)
kv.free_seq(10)
let st = kv.stats()
assert(st["prefix_cache_hits"] >= 1)
assert(st["swap_outs"] >= 1)
assert(st["swap_ins"] >= 1)
st["prefix_cache_hits"]
"#
        );
        let val = eval_source(&src)
            .unwrap_or_else(|e| panic!("GPU VRAM radix+swap test failed on '{dev}': {e}"));
        assert_eq!(val, Value::Int(1));
    }
}

#[test]
fn p5_2b_fused_linear_act_and_f32_jit_e2e_and_kill_switch_parity() {
    let script = r#"
let x_f32 = Tensor::from_list([[1.0, -2.0, 0.5, 3.0, -1.5, 2.0], [-0.5, 1.5, 2.0, -1.0, 0.25, 1.0], [2.0, 0.5, -1.5, 1.0, 3.0, -2.0], [-1.0, -1.0, 1.0, 2.0, -0.5, 0.75], [0.5, 2.5, -0.5, -2.0, 1.5, 1.25]], "f32")
let w_f32 = Tensor::from_list([[0.5, -0.25, 1.0, 0.0, -0.5, 0.75, 0.25, -1.0, 0.5, 0.125], [-0.5, 1.0, 0.25, -0.75, 0.5, 0.0, 1.0, 0.25, -0.5, 0.5], [1.0, 0.5, -0.5, 0.25, 1.0, -0.5, 0.0, 0.75, 0.25, -0.25], [0.25, -0.5, 0.75, 1.0, -0.25, 0.5, -0.5, 0.0, 1.0, 0.75], [-0.75, 0.25, 0.0, 0.5, 0.75, -1.0, 0.5, 0.25, -0.25, 1.0], [0.5, 0.75, -0.25, -0.5, 0.25, 1.0, -0.75, 0.5, 0.0, -0.5]], "f32")
let b_f32 = Tensor::from_list([0.1, -0.2, 0.3, -0.4, 0.5, -0.1, 0.2, -0.3, 0.4, -0.5], "f32")

let dag = trace_dag(fn(x, w, b)
  let h = ((x @ w) + b).relu()
  return (h * 0.5f32) + 1.0f32
end, [x_f32, w_f32, b_f32]).fuse()

let y = dag.run([x_f32, w_f32, b_f32])
let rep = dag.cost_report()
print("JIT_GEMM=" + str(rep["native_jit_gemm_dispatches"]))
print("JIT_F32=" + str(rep["native_jit_f32_dispatches"]))
print("OUT=" + str(y.to_list()))
"#;

    let tmp = TempDir::new("p5_2b_jit_parity");
    let file = tmp.path().join("jit_gemm_parity.mora");
    std::fs::write(&file, script).expect("write jit parity script");
    let mora_bin = env!("CARGO_BIN_EXE_mora");

    // 1. Run with W^X Native JIT enabled (default).
    let run_jit = Command::new(mora_bin)
        .arg(file.to_str().expect("utf8 path"))
        .env_remove("MORA_DISABLE_NATIVE_JIT")
        .output()
        .expect("run with native JIT");
    assert!(
        run_jit.status.success(),
        "JIT run failed:\n{}",
        String::from_utf8_lossy(&run_jit.stderr)
    );
    let out_jit = String::from_utf8_lossy(&run_jit.stdout);

    // 2. Run with `MORA_DISABLE_NATIVE_JIT=1` kill switch (pure simd_kernels reference).
    let run_ref = Command::new(mora_bin)
        .arg(file.to_str().expect("utf8 path"))
        .env("MORA_DISABLE_NATIVE_JIT", "1")
        .output()
        .expect("run with MORA_DISABLE_NATIVE_JIT=1");
    assert!(
        run_ref.status.success(),
        "fallback run failed:\n{}",
        String::from_utf8_lossy(&run_ref.stderr)
    );
    let out_ref = String::from_utf8_lossy(&run_ref.stdout);

    let extract_line = |text: &str, prefix: &str| -> String {
        text.lines()
            .find_map(|l| l.strip_prefix(prefix).map(str::to_string))
            .unwrap_or_else(|| panic!("missing '{prefix}' in:\n{text}"))
    };

    let jit_gemm: i64 = extract_line(&out_jit, "JIT_GEMM=").parse().unwrap();
    let ref_gemm: i64 = extract_line(&out_ref, "JIT_GEMM=").parse().unwrap();
    assert_eq!(ref_gemm, 0, "kill switch must disable GEMM JIT dispatches");

    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
        assert!(
            jit_gemm >= 1,
            "expected native_jit_gemm_dispatches >= 1 on AVX2+FMA CPU, got {jit_gemm}"
        );
    }

    let y_jit = extract_line(&out_jit, "OUT=");
    let y_ref = extract_line(&out_ref, "OUT=");
    assert_eq!(
        y_jit, y_ref,
        "W^X AVX2+FMA FusedLinearAct + 8xf32 JIT output must match simd_kernels fallback!\njit: {y_jit}\nref: {y_ref}"
    );
}
