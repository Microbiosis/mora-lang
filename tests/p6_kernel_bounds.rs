//! P6-3B: Omega-backed GPU `kernel` shared-memory static bounds verification & guard elision.
//!
//! Verifies:
//! 1. Compile-time HM rejection (`UnsatisfiedDimConstraint`) for provably negative / out-of-bounds
//!    shared-memory indices (constants, symbolic offsets, `else`-branch negation, post-loop exits,
//!    and static `launch` / `emit_spirv_words` block-geometry violations).
//! 2. Exact guard elision in CUDA PTX, Vulkan SPIR-V, and Metal MSL when `omega` proves
//!    `0 <= idx < len` (guarded `if`, `bitand`, `%`, `min/max` clamp, and launch-specialized
//!    `local_size` / `block`), vs safe predicated/branchless guard emission when indeterminate.
//! 3. Live execution safety on CPU + CUDA + Vulkan for both elided fast paths and guarded OOB paths.

use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::{CoreRuntime, vm};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn typecheck_source(src: &str) -> Result<(), String> {
    let (_, witnesses) = ParserV3::compile(src).map_err(|e| e.to_string())?;
    let errs = check_program_witnesses(&witnesses);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn eval_source(src: &str) -> Result<Value, String> {
    let (prog, witnesses) = ParserV3::compile(src).map_err(|e| e.to_string())?;
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        return Err(errs
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let mut core = CoreRuntime::default();
    let (val, _) = vm::execute_program(Arc::new(prog), &mut core).map_err(|e| e.to_string())?;
    Ok(val)
}

fn available_devices() -> Vec<String> {
    let mut out = vec!["cpu".to_string()];
    let mut probe = |dev: &str| {
        let src = format!(
            r#"
let t = Tensor::zeros([4], "f32", "{dev}")
t.device()
"#
        );
        if let Ok(Value::String(d)) = eval_source(&src)
            && d == dev
        {
            out.push(dev.to_string());
        }
    };
    probe("cuda:0");
    probe("vulkan:0");
    out
}

#[test]
fn p6_3b_kernel_shared_bounds_static_rejections() {
    // 1. Constant upper out-of-bounds: tile[16] on len 16
    let err1 = typecheck_source(
        r#"
kernel oob_const(out: buf<f32>) shared tile: [f32; 16]
    tile[16] = f32(1.0)
    out[0] = tile[0]
end
"#,
    )
    .expect_err("tile[16] on [f32; 16] must fail at compile time");
    assert!(
        err1.contains("provably out-of-bounds") && err1.contains("0 <= 16 < 16"),
        "unexpected diagnostic: {err1}"
    );

    // 2. Constant negative index: tile[-1]
    let err2 = typecheck_source(
        r#"
kernel oob_neg(out: buf<f32>) shared tile: [f32; 16]
    tile[-1] = f32(1.0)
    out[0] = tile[0]
end
"#,
    )
    .expect_err("tile[-1] must fail at compile time");
    assert!(
        err2.contains("provably negative"),
        "unexpected diagnostic: {err2}"
    );

    // 3. Symbolic thread_idx() + 16 >= 16 (since tid >= 0)
    let err3 = typecheck_source(
        r#"
kernel oob_tid_shift(out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    tile[tid + 16] = f32(1.0)
    out[0] = tile[0]
end
"#,
    )
    .expect_err("tile[tid + 16] must fail at compile time");
    assert!(
        err3.contains("provably out-of-bounds"),
        "unexpected diagnostic: {err3}"
    );

    // 4. `else` branch negation: `if tid < 16` implies `tid >= 16` in `else`
    let err4 = typecheck_source(
        r#"
kernel oob_else_branch(out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid < 16
        tile[tid] = f32(1.0)
    else
        tile[tid] = f32(2.0)
    end
    out[0] = f32(0.0)
end
"#,
    )
    .expect_err("else branch tile[tid] where tid >= 16 must fail at compile time");
    assert!(
        err4.contains("provably out-of-bounds"),
        "unexpected diagnostic: {err4}"
    );

    // 5. Post-loop exit invariant: after `let i = 0; while i < 4 { i = i + 1 }`, `i >= 4`
    let err5 = typecheck_source(
        r#"
kernel oob_post_loop(out: buf<f32>) shared tile: [f32; 4]
    let i = 0
    while i < 4
        tile[i] = f32(1.0)
        i = i + 1
    end
    out[0] = tile[i]
end
"#,
    )
    .expect_err("post-loop tile[i] with i >= 4 on len 4 must fail at compile time");
    assert!(
        err5.contains("provably out-of-bounds"),
        "unexpected diagnostic: {err5}"
    );

    // 6. Launch-time static `block` violation: `tile[block_dim() - 1]` with `block = 32` on len 16
    let err6 = typecheck_source(
        r#"
kernel launch_check(out: buf<f32>) shared tile: [f32; 16]
    tile[block_dim() - 1] = f32(1.0)
    out[0] = tile[0]
end
let out = Tensor::zeros([16], "f32")
launch_check.launch(1, 32, out)
"#,
    )
    .expect_err("launch with block=32 makes block_dim()-1 == 31 >= 16");
    assert!(
        err6.contains("launch block [32, 1, 1]") && err6.contains("provably out-of-bounds"),
        "unexpected diagnostic: {err6}"
    );

    // Same kernel launched with block = 16 MUST succeed!
    typecheck_source(
        r#"
kernel launch_check_ok(out: buf<f32>) shared tile: [f32; 16]
    tile[block_dim() - 1] = f32(1.0)
    out[0] = tile[0]
end
let out = Tensor::zeros([16], "f32")
launch_check_ok.launch(1, 16, out)
"#,
    )
    .expect("launch with block=16 makes block_dim()-1 == 15 < 16");

    // 7. Static `emit_spirv_words(32)` violation on same kernel
    let err7 = typecheck_source(
        r#"
kernel spv_check(out: buf<f32>) shared tile: [f32; 16]
    tile[block_dim() - 1] = f32(1.0)
    out[0] = tile[0]
end
let _w = spv_check.emit_spirv_words(32)
"#,
    )
    .expect_err("emit_spirv_words(32) makes block_dim()-1 == 31 >= 16");
    assert!(
        err7.contains("launch block [32, 1, 1]"),
        "unexpected diagnostic: {err7}"
    );

    // 8. Wrapping `i32` condition MUST NOT prune live branch as dead code:
    //    at tid=0, `0 + 2147483647 + 1` wraps to `-2147483648 < 4` (true!), so `tile[tid + 300]`
    //    is reachable and MUST be rejected at compile time (never elided as dead code!).
    let err8 = typecheck_source(
        r#"
kernel wrap_cond_oob(out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid + 2147483647 + 1 < 4
        tile[tid + 300] = f32(1.0)
    end
    out[0] = tile[0]
end
"#,
    )
    .expect_err("wrapping condition is reachable at tid=0 and tile[tid + 300] is OOB");
    assert!(
        err8.contains("provably out-of-bounds"),
        "unexpected diagnostic: {err8}"
    );
}

#[test]
fn p6_3b_wrapping_i32_guard_preservation_and_launch_caps() {
    // 1. Wrapping `i32` condition with indeterminate target index (`tile[tid + x_off]` or `tile[tid]`)
    //    MUST preserve safe runtime guards in PTX & MSL (never elide as dead branch!).
    // 2. Wrapping `while` induction step (`i < 2147483647; i = i + 2`) MUST NOT assume `0 <= i`
    //    and MUST emit safe runtime guards on `tile[i]` inside `if i < 16`.
    let val = eval_source(
        r#"
kernel wrap_cond_guarded(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid + 2147483647 + 1 < 4
        tile[tid] = x[tid]
        out[tid] = tile[tid]
    end
end

kernel wrap_loop_guarded(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let i = 0
    while i < 2147483647
        if i < 16
            tile[i] = x[0]
        end
        i = i + 2
    end
    out[0] = tile[0]
end

struct WrapInspect
    ptx_cond: string
    msl_cond: string
    ptx_loop: string
    msl_loop: string
end
WrapInspect(wrap_cond_guarded.emit_ptx(), wrap_cond_guarded.emit_msl(), wrap_loop_guarded.emit_ptx(), wrap_loop_guarded.emit_msl())
"#,
    )
    .expect("wrapping kernels with runtime guards must compile cleanly");

    let Value::StructInstance { fields, .. } = val else {
        panic!("expected WrapInspect struct, got {val:?}");
    };
    let as_str = |k: &str| match &fields[k] {
        Value::String(s) => s.as_str(),
        other => panic!("expected string for {k}, got {other:?}"),
    };
    let ptx_cond = as_str("ptx_cond");
    let msl_cond = as_str("msl_cond");
    let ptx_loop = as_str("ptx_loop");
    let msl_loop = as_str("msl_loop");

    assert!(
        ptx_cond.contains("and.pred") && ptx_cond.contains("st.shared.f32 ["),
        "wrapping-condition PTX MUST keep runtime bounds guard:\n{ptx_cond}"
    );
    assert!(
        msl_cond.contains("if (_sh_i >= 0 && _sh_i < 16)") && msl_cond.contains("? _sh_0["),
        "wrapping-condition MSL MUST keep runtime bounds guard:\n{msl_cond}"
    );
    assert!(
        ptx_loop.contains("and.pred") && ptx_loop.contains("st.shared.f32 ["),
        "wrapping-step while loop PTX MUST keep runtime bounds guard on tile[i]:\n{ptx_loop}"
    );
    assert!(
        msl_loop.contains("if (_sh_i >= 0 && _sh_i < 16)"),
        "wrapping-step while loop MSL MUST keep runtime bounds guard on tile[i]:\n{msl_loop}"
    );

    // 3. Runtime `block > 65535` launch cap enforced so `0 <= tid < block_dim <= 65535` stays sound!
    let cap_err = eval_source(
        r#"
kernel cap_probe(out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid < 16
        tile[tid] = f32(1.0)
        out[tid] = tile[tid]
    end
end
let out = Tensor::zeros([16], "f32", "cpu")
cap_probe.launch(1, 70000, out)
"#,
    )
    .expect_err("block > 65535 must be rejected at runtime");
    assert!(
        cap_err.contains("65535"),
        "expected block cap diagnostic, got: {cap_err}"
    );
}

#[test]
fn p6_3b_kernel_shared_guard_elision_and_safe_guards_ptx_msl_spirv() {
    let val = eval_source(
        r#"
kernel guarded_elided(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid < 16
        tile[tid] = x[tid]
        sync_threads()
        out[tid] = tile[tid]
    end
end

kernel unguarded_needs_guard(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    tile[tid] = x[tid]
    sync_threads()
    out[tid] = tile[tid]
end

kernel idiom_elided(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    tile[tid & 15] = x[tid]
    sync_threads()
    let a = tile[tid % 16]
    let b = tile[min(max(tid, 0), 15)]
    out[tid] = a + b
end

kernel nested_shared_mix(x: buf<f32>, out: buf<f32>)
    shared idx_sh: [i32; 16]
    shared data_sh: [f32; 16]
    let tid = thread_idx()
    if tid < 16
        idx_sh[tid] = tid
        data_sh[tid] = x[tid]
        sync_threads()
        data_sh[idx_sh[tid]] = data_sh[tid]
        out[tid] = data_sh[idx_sh[tid]]
    end
end

struct CodegenInspect
    ptx_elided: string
    msl_elided: string
    ptx_guarded: string
    msl_guarded: string
    ptx_idiom: string
    msl_idiom: string
    ptx_nested: string
    msl_nested: string
    spv_len_16: int
    spv_len_32: int
    spv_magic_16: int
    spv_magic_32: int
    spv_nested_magic: int
end

let spv_16 = unguarded_needs_guard.emit_spirv_words(16)
let spv_32 = unguarded_needs_guard.emit_spirv_words(32)
let spv_nested = nested_shared_mix.emit_spirv_words(16)
CodegenInspect(guarded_elided.emit_ptx(), guarded_elided.emit_msl(), unguarded_needs_guard.emit_ptx(), unguarded_needs_guard.emit_msl(), idiom_elided.emit_ptx(), idiom_elided.emit_msl(), nested_shared_mix.emit_ptx(), nested_shared_mix.emit_msl(), len(spv_16), len(spv_32), spv_16[0], spv_32[0], spv_nested[0])
"#,
    )
    .expect("codegen inspection program must pass HM and run");

    let Value::StructInstance { fields, .. } = val else {
        panic!("expected CodegenInspect struct, got {val:?}");
    };
    let as_str = |key: &str| -> &str {
        match &fields[key] {
            Value::String(s) => s.as_str(),
            other => panic!("expected string for {key}, got {other:?}"),
        }
    };
    let as_int = |key: &str| -> i64 {
        match &fields[key] {
            Value::Int(n) => *n,
            other => panic!("expected int for {key}, got {other:?}"),
        }
    };

    let ptx_elided = as_str("ptx_elided");
    let msl_elided = as_str("msl_elided");
    let ptx_guarded = as_str("ptx_guarded");
    let msl_guarded = as_str("msl_guarded");
    let ptx_idiom = as_str("ptx_idiom");
    let msl_idiom = as_str("msl_idiom");
    let ptx_nested = as_str("ptx_nested");
    let msl_nested = as_str("msl_nested");
    let spv_len_16 = as_int("spv_len_16");
    let spv_len_32 = as_int("spv_len_32");
    let spv_magic_16 = as_int("spv_magic_16");
    let spv_magic_32 = as_int("spv_magic_32");
    let spv_nested_magic = as_int("spv_nested_magic");

    // 1. Guarded kernel (`if tid < 16`): PTX & MSL elide ALL shared bounds checks!
    assert!(
        ptx_elided.contains("    st.shared.f32 [") && ptx_elided.contains("    ld.shared.f32 "),
        "elided PTX must emit raw unpredicated shared ops:\n{ptx_elided}"
    );
    assert!(
        !ptx_elided.contains("and.pred"),
        "elided PTX must NOT emit shared bounds `and.pred`:\n{ptx_elided}"
    );
    assert!(
        !msl_elided.contains("_sh_i >= 0") && !msl_elided.contains("? _sh_0["),
        "elided MSL must NOT emit runtime shared bounds guards:\n{msl_elided}"
    );

    // 2. Unguarded kernel (`tile[tid]` without block_dim bound): PTX & MSL emit safe guards!
    assert!(
        ptx_guarded.contains("and.pred")
            && ptx_guarded.contains("st.shared.f32 [")
            && ptx_guarded.contains("ld.shared.f32 ")
            && ptx_guarded.contains("mov.f32 ")
            && ptx_guarded.contains("0f00000000"),
        "unguarded PTX must emit predicated store/load guards:\n{ptx_guarded}"
    );
    assert!(
        msl_guarded.contains("if (_sh_i >= 0 && _sh_i < 16)")
            && msl_guarded.contains("? _sh_0[")
            && msl_guarded.contains(": 0.0f)"),
        "unguarded MSL must emit safe store & load bounds guards:\n{msl_guarded}"
    );

    // 3. Bitmask / modulo / clamp idioms (`& 15`, `% 16`, `min(max(tid, 0), 15)`): 100% elided!
    assert!(
        !ptx_idiom.contains("and.pred")
            && ptx_idiom.contains("    st.shared.f32 [")
            && ptx_idiom.contains("    ld.shared.f32 "),
        "idiom PTX must elide all 3 shared bounds guards:\n{ptx_idiom}"
    );
    assert!(
        !msl_idiom.contains("_sh_i >= 0") && !msl_idiom.contains("? _sh_0["),
        "idiom MSL must elide all 3 shared bounds guards:\n{msl_idiom}"
    );

    // 4. SPIR-V specialization by `local_size`: 16 elides guards (fewer words) vs 32 emits guards!
    assert_eq!(spv_magic_16, 0x0723_0203);
    assert_eq!(spv_magic_32, 0x0723_0203);
    assert_eq!(spv_nested_magic, 0x0723_0203);
    assert!(
        spv_len_16 < spv_len_32,
        "SPIR-V with local_size=16 (proven in-bounds, {spv_len_16} words) must be strictly smaller than local_size=32 (guarded, {spv_len_32} words)"
    );

    // 5. Nested shared access (`data_sh[idx_sh[tid]]`): inner `idx_sh[tid]` is elided (proven < 16),
    //    while outer `data_sh[dynamic]` MUST receive a safe runtime guard!
    assert!(
        msl_nested.contains("_sh_0[tid] = tid;")
            && msl_nested.contains("if (_sh_i >= 0 && _sh_i < 16) { _sh_1[_sh_i] = _sh_1[tid]; }")
            && msl_nested
                .contains("((_sh_0[tid]) >= 0 && (_sh_0[tid]) < 16 ? _sh_1[(_sh_0[tid])] : 0.0f)"),
        "nested MSL must elide inner `idx_sh[tid]` while guarding outer `data_sh[idx_sh[tid]]`:\n{msl_nested}"
    );
    assert!(
        ptx_nested.contains("    st.shared.s32 [")
            && ptx_nested.contains("    ld.shared.s32 ")
            && ptx_nested.contains("and.pred"),
        "nested PTX must elide inner `idx_sh[tid]` while predicating outer `data_sh[idx_sh[tid]]`:\n{ptx_nested}"
    );
}

fn val_as_f64(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        Value::Scalar(s) => s.as_f64(),
        Value::Int(i) => *i as f64,
        other => panic!("expected numeric value, got {other:?}"),
    }
}

#[test]
fn p6_3b_kernel_live_execution_elided_and_guarded_safety() {
    let devices = available_devices();
    println!("P6-3B live devices: {devices:?}");

    // 1. Run proven-elided shared-memory reverse kernel across CPU + every live GPU backend.
    for dev in &devices {
        let src = format!(
            r#"
kernel rev_tile(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    if tid < 16
        tile[tid] = x[tid]
        sync_threads()
        out[tid] = tile[15 - tid]
    end
end

let vals: list<float> = []
let i = 0
while i < 16
    vals = vals.push(float(i))
    i = i + 1
end
let x = Tensor::from_list(vals, "f32").to("{dev}")
let out = Tensor::zeros([16], "f32", "{dev}")
rev_tile.launch(1, 16, x, out)
out.to("cpu").to_list()
"#
        );
        let Value::List(host) =
            eval_source(&src).unwrap_or_else(|e| panic!("rev_tile failed on '{dev}': {e}"))
        else {
            panic!("expected list from to_list()");
        };
        assert_eq!(host.len(), 16);
        for (idx, v) in host.iter().enumerate() {
            let f = val_as_f64(v);
            let expected = (15 - idx) as f64;
            assert!(
                (f - expected).abs() < 1e-4,
                "dev={dev} idx={idx}: got {f}, expected {expected}"
            );
        }
    }

    // 2. Run unguarded kernel across CPU + every live GPU backend with block=32 > shared len 16:
    //    threads 0..15 must read/write exact values without corruption, and threads 16..31 must
    //    safely skip OOB shared store and read 0.0f32 from guarded shared load (4-backend parity!).
    for dev in &devices {
        let src = format!(
            r#"
kernel guarded_oob_probe(x: buf<f32>, out: buf<f32>) shared tile: [f32; 16]
    let tid = thread_idx()
    tile[tid] = x[tid]
    sync_threads()
    out[tid] = tile[tid]
end

let vals: list<float> = []
let i = 0
while i < 32
    vals = vals.push(float(i + 1))
    i = i + 1
end
let x = Tensor::from_list(vals, "f32").to("{dev}")
let out = Tensor::zeros([32], "f32", "{dev}")
guarded_oob_probe.launch(1, 32, x, out)
out.to("cpu").to_list()
"#
        );
        let Value::List(host) = eval_source(&src)
            .unwrap_or_else(|e| panic!("guarded_oob_probe failed on '{dev}': {e}"))
        else {
            panic!("expected list from to_list()");
        };
        assert_eq!(host.len(), 32);
        for (idx, v) in host.iter().enumerate() {
            let f = val_as_f64(v);
            let expected = if idx < 16 { (idx + 1) as f64 } else { 0.0 };
            assert!(
                (f - expected).abs() < 1e-4,
                "dev={dev} idx={idx}: got {f}, expected {expected}"
            );
        }
    }
}
