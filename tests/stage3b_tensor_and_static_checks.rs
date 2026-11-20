//! Wave 3 — Stage 3B comprehensive end-to-end verification suite:
//! - Track 3B-1: exact hardware `DType` codecs (`f16`, `bf16`, `fp8`/`fp8_e4m3`, `fp8_e5m2`,
//!   `i4`..`i64`, `u8`..`u64`, `bool`), non-erased runtime `Value::Scalar(ScalarValue)`,
//!   64-byte aligned compact packed `AlignedByteBuffer`, zero-copy strided views,
//!   `Tensor::randn`/`rand`, neural kernels, and `linalg.*`/`math.*`/`stats.*` tensor interop.
//! - Track 3B-2: all 5 AI-native static checks in the unified HM solver (`DType`, `Shape`,
//!   `Device`, scoped + source-aliased + inter-procedural `Resource`, and `Effect`),
//!   spanned `MoraError` diagnostics, and LSP hover/completion parity.

use std::collections::HashMap;
use std::sync::Arc;

use mora::common::{
    DType, Device, ScalarValue, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits,
    f64_to_f16_bits, f64_to_fp8_e4m3_bits, f64_to_fp8_e5m2_bits, fp8_e4m3_bits_to_f64,
    fp8_e5m2_bits_to_f64, pack_i4_nibble, unpack_i4_nibble,
};
use mora::error::MoraError;
use mora::lsp::json::Value as JsonVal;
use mora::lsp::providers::{completion_v3, hover_v3};
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::{check_program_witnesses, infer_program_types};
use mora::value::Value;
use mora::value::tensor::{AlignedByteBuffer, Tensor};
use proptest::prelude::*;

fn typeck_errors(src: &str) -> Result<Vec<mora::typeck::TypeError>, String> {
    let (_prog, witnesses) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    Ok(check_program_witnesses(&witnesses))
}

fn assert_typecheck_ok(src: &str) {
    let errs = typeck_errors(src).unwrap_or_else(|e| panic!("failed to compile:\n{src}\nerr: {e}"));
    assert!(
        errs.is_empty(),
        "expected no type errors for:\n{src}\ngot: {errs:?}"
    );
}

fn assert_typecheck_fails(src: &str, needle: &str) {
    let errs = typeck_errors(src).unwrap_or_else(|e| panic!("failed to compile:\n{src}\nerr: {e}"));
    assert!(
        !errs.is_empty(),
        "expected type error containing {needle:?} for:\n{src}\nbut typeck succeeded"
    );
    let combined = errs
        .iter()
        .map(mora::typeck::format_error)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.to_lowercase().contains(&needle.to_lowercase()),
        "expected diagnostic mentioning {needle:?} for:\n{src}\ngot:\n{combined}"
    );
}

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn compile_check_and_expect_runtime_err(src: &str) -> MoraError {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "expected runtime failure (not type error) for:\n{src}\ngot: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    execute_program(Arc::new(prog), &mut core)
        .expect_err(&format!("expected runtime error for:\n{src}"))
}

// ============================================================================
// Track 3B-1: Hardware DTypes, Exact Bit Codecs, Non-Erased ScalarValue
// ============================================================================

#[test]
fn track3b1_non_erased_low_bitwidth_scalars_and_exact_arithmetic() {
    // 1. Hardware quantization fidelity in scalar arithmetic:
    // In IEEE 754 binary16 (10-bit mantissa), 1.0 + 2^-12 = 1.0 + 0.000244140625 rounds back to 1.0!
    let f16_round =
        compile_check_and_run("let a: f16 = 1.0f16\nlet b: f16 = 0.000244140625f16\na + b\n");
    assert_eq!(
        f16_round,
        Value::Scalar(ScalarValue::F16(f64_to_f16_bits(1.0)))
    );

    // FP8 E4M3 saturates finite values at 448.0; FP8 E5M2 represents 1024.0 cleanly.
    let e4m3_sat = compile_check_and_run("let x: fp8 = 400.0fp8\nlet y: fp8 = 200.0fp8\nx + y\n");
    assert_eq!(
        e4m3_sat,
        Value::Scalar(ScalarValue::FP8E4M3(f64_to_fp8_e4m3_bits(448.0)))
    );
    let e5m2_large = compile_check_and_run("let x: fp8_e5m2 = 1024.0fp8_e5m2\nx + x\n");
    assert_eq!(
        e5m2_large,
        Value::Scalar(ScalarValue::FP8E5M2(f64_to_fp8_e5m2_bits(2048.0)))
    );

    // Runtime type_of preserves exact hardware scalar dtype names
    assert_eq!(
        compile_check_and_run("type_of(1.0f16)\n"),
        Value::String("f16".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(1.0bf16)\n"),
        Value::String("bf16".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(1.0fp8)\n"),
        Value::String("fp8".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(1.0fp8_e5m2)\n"),
        Value::String("fp8_e5m2".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(-8i4)\n"),
        Value::String("i4".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(-128i8)\n"),
        Value::String("i8".into())
    );
    assert_eq!(
        compile_check_and_run("type_of(255u8)\n"),
        Value::String("u8".into())
    );

    // Signed unary negation works cleanly on variables of signed scalar types
    assert_eq!(
        compile_check_and_run("let x: f16 = 2.5f16\n-x\n"),
        Value::Scalar(ScalarValue::F16(f64_to_f16_bits(-2.5)))
    );
    assert_eq!(
        compile_check_and_run("let n: i8 = 42i8\n-n\n"),
        Value::Scalar(ScalarValue::I8(-42))
    );

    // Explicit scalar casting via .to_dtype(...)
    assert_eq!(
        compile_check_and_run("let a: f16 = 1.5f16\nlet b: bf16 = a.to_dtype(\"bf16\")\nb\n"),
        Value::Scalar(ScalarValue::BF16(f64_to_bf16_bits(1.5)))
    );

    // Exact checked overflow on low-bitwidth integer scalar arithmetic (no silent i64 widening!)
    let err_i8 =
        compile_check_and_expect_runtime_err("let a: i8 = 100i8\nlet b: i8 = 50i8\na + b\n");
    assert!(err_i8.to_string().contains("overflow"), "got: {err_i8}");

    let err_i4 = compile_check_and_expect_runtime_err("let a: i4 = 7i4\nlet b: i4 = 1i4\na + b\n");
    assert!(err_i4.to_string().contains("overflow"), "got: {err_i4}");

    let err_u8 = compile_check_and_expect_runtime_err("let a: u8 = 0u8\nlet b: u8 = 1u8\na - b\n");
    assert!(err_u8.to_string().contains("overflow"), "got: {err_u8}");

    // Lexical / parse-time range rejection for out-of-bounds literals
    for bad in [
        "8i4\n", "-9i4\n", "128i8\n", "-129i8\n", "-1u8\n", "256u8\n",
    ] {
        assert!(
            ParserV3::compile(bad).is_err(),
            "out-of-range literal `{bad}` must fail at parse/lex time"
        );
    }
}

// ============================================================================
// Track 3B-1: Compact 64-Byte Aligned TensorStorage, Views, Constructors & Kernels
// ============================================================================

#[test]
fn track3b1_compact_64byte_aligned_storage_and_nbytes() {
    let cases = [
        (DType::F64, 1024, 8192),
        (DType::F32, 1024, 4096),
        (DType::F16, 1024, 2048),
        (DType::BF16, 1024, 2048),
        (DType::FP8E4M3, 1024, 1024),
        (DType::FP8E5M2, 1024, 1024),
        (DType::I8, 1024, 1024),
        (DType::U8, 1024, 1024),
        (DType::Bool, 1024, 1024),
        (DType::I4, 1024, 512), // 4-bit nibble-packed: 2 elements per byte!
        (DType::I4, 5, 3),      // odd length ceil(5 / 2) = 3 bytes
    ];
    for (dt, len, expected_bytes) in cases {
        let t = Tensor::zeros(vec![len], dt, Device::Cpu, false).expect("alloc tensor");
        assert_eq!(t.nbytes(), expected_bytes, "nbytes mismatch for {dt}");
        assert_eq!(
            t.storage.byte_len(),
            expected_bytes,
            "physical storage byte_len mismatch for {dt}"
        );
        let addr = t.storage.aligned_byte_ptr() as usize;
        assert_eq!(
            addr % 64,
            0,
            "TensorStorage pointer 0x{addr:x} for {dt} is not 64-byte aligned"
        );
    }

    // Verify Mora-level .nbytes() / .byte_len() matches
    assert_eq!(
        compile_check_and_run("Tensor::zeros([1024], \"i4\").nbytes()\n"),
        Value::Int(512)
    );
    assert_eq!(
        compile_check_and_run("Tensor::zeros([1024], \"f16\").nbytes()\n"),
        Value::Int(2048)
    );
    assert_eq!(
        compile_check_and_run("Tensor::zeros([1024], \"fp8_e5m2\").byte_len()\n"),
        Value::Int(1024)
    );
    assert_eq!(
        compile_check_and_run("tensor([true, false, true]).nbytes()\n"),
        Value::Int(3)
    );
}

#[test]
fn track3b1_zero_copy_views_auto_dtype_constructors_and_kernels() {
    // Auto-inferred DType from typed literal lists & exact element return types
    assert_eq!(
        compile_check_and_run("tensor([1.0f16, 2.5f16]).dtype()\n"),
        Value::String("f16".into())
    );
    let val = compile_check_and_run(
        "let t = tensor([1.0f16, 2.5f16])\n\
         [t[0], t.sum()]\n",
    );
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Scalar(ScalarValue::F16(f64_to_f16_bits(1.0))),
                Value::Scalar(ScalarValue::F16(f64_to_f16_bits(3.5))),
            ]
            .into()
        )
    );

    // Zero-copy strided views share underlying Arc<TensorStorage>
    let base = Tensor::from_raw(
        vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        vec![2, 3],
        DType::F16,
        Device::Cpu,
        false,
    )
    .expect("base");
    let tr = base.transpose(None, None).expect("transpose");
    assert!(
        Arc::ptr_eq(&base.storage, &tr.storage),
        "transpose must be zero-copy"
    );
    assert!(!tr.is_contiguous());
    assert_eq!(tr.shape, vec![3, 2]);
    assert_eq!(tr.get_at(&[1, 0]), 2.0);
    assert_eq!(tr.get_at(&[0, 1]), 4.0);

    let rs = base.reshape(vec![3, 2]).expect("reshape");
    assert!(
        Arc::ptr_eq(&base.storage, &rs.storage),
        "contiguous reshape must be zero-copy"
    );
    let Value::Tensor(row0) = base.index_axis0(0).expect("row0") else {
        panic!("expected 1D tensor view");
    };
    assert!(
        Arc::ptr_eq(&base.storage, &row0.storage),
        "axis-0 ND index must be zero-copy"
    );
    let owned = tr.contiguous().expect("contiguous");
    assert!(
        !Arc::ptr_eq(&base.storage, &owned.storage),
        "contiguous() materializes owning buffer"
    );
    assert!(owned.is_contiguous());

    // Deterministic Tensor::randn & Tensor::rand via random.seed(...)
    let rand_eq = compile_check_and_run(
        "random.seed(42)\n\
         let a = Tensor::randn([2, 3], \"f32\")\n\
         random.seed(42)\n\
         let b = Tensor::randn([2, 3], \"f32\")\n\
         a == b\n",
    );
    assert_eq!(rand_eq, Value::Bool(true));

    // Broadcasting, batched ND @, unary negation on tensor, activations & normalizations
    let bcast = compile_check_and_run(
        "let a = tensor([[1.0], [2.0]])\n\
         let b = tensor([[10.0, 20.0, 30.0]])\n\
         (a + b).to_list()\n",
    );
    assert_eq!(
        bcast.to_string(),
        "[[11.0, 21.0, 31.0], [12.0, 22.0, 32.0]]"
    );

    let batched_mm = compile_check_and_run(
        "let a = Tensor::ones([2, 2, 3], \"f32\")\n\
         let b = Tensor::ones([1, 3, 2], \"f32\")\n\
         let c = a @ b\n\
         c.shape()\n",
    );
    assert_eq!(batched_mm.to_string(), "[2, 2, 2]");
    let batched_elem = compile_check_and_run(
        "let a = Tensor::ones([2, 2, 3], \"f32\")\n\
         let b = Tensor::ones([1, 3, 2], \"f32\")\n\
         (a @ b)[0][0][0]\n",
    );
    assert_eq!(batched_elem, Value::Scalar(ScalarValue::F32(3.0)));

    let softmax_sum = compile_check_and_run(
        "let x = tensor([1.0, 2.0, 3.0])\n\
         let s = x.softmax()\n\
         s.sum()\n",
    );
    if let Value::Float(v) = softmax_sum {
        assert!((v - 1.0).abs() < 1e-9, "softmax sum should be 1.0, got {v}");
    } else {
        panic!("expected Float softmax sum, got {softmax_sum:?}");
    }

    let neg_t = compile_check_and_run(
        "let x = tensor([1.0f16, -2.0f16])\n\
         (-x).to_list()\n",
    );
    assert_eq!(neg_t.to_string(), "[-1.0, 2.0]");

    // RMSNorm & LayerNorm + activations execute cleanly
    let norm_shape = compile_check_and_run(
        "let x = tensor([[1.0, 2.0, 3.0, 4.0]])\n\
         x.relu().gelu().silu().tanh().sigmoid().rms_norm().layer_norm().shape()\n",
    );
    assert_eq!(norm_shape.to_string(), "[1, 4]");
}

#[test]
fn track3b1_linalg_math_stats_native_tensor_interop() {
    let dot = compile_check_and_run(
        "let a = tensor([1.0f16, 2.0f16, 3.0f16])\n\
         let b = tensor([4.0f16, 5.0f16, 6.0f16])\n\
         linalg.dot(a, b)\n",
    );
    assert_eq!(dot, Value::Scalar(ScalarValue::F16(f64_to_f16_bits(32.0))));

    let mm_shape = compile_check_and_run(
        "let a = Tensor::ones([2, 3], \"f32\")\n\
         let b = Tensor::ones([3, 4], \"f32\")\n\
         let c: Tensor<f32, [2, 4], cpu> = linalg.matmul(a, b)\n\
         let ct: Tensor<f32, [4, 2], cpu> = linalg.transpose(c)\n\
         ct.shape()\n",
    );
    assert_eq!(mm_shape.to_string(), "[4, 2]");

    let cross_val = compile_check_and_run(
        "let x = tensor([1.0f32, 0.0f32, 0.0f32])\n\
         let y = tensor([0.0f32, 1.0f32, 0.0f32])\n\
         let z: Tensor<f32, [3], cpu> = linalg.cross(x, y)\n\
         z.to_list()\n",
    );
    assert_eq!(cross_val.to_string(), "[0.0, 0.0, 1.0]");

    let math_t = compile_check_and_run(
        "let x = tensor([0.0f32, 4.0f32])\n\
         let y: Tensor<f32, [2], cpu> = math.sqrt(x)\n\
         y.to_list()\n",
    );
    assert_eq!(math_t.to_string(), "[0.0, 2.0]");

    let stats_mean = compile_check_and_run(
        "let x = tensor([2.0f32, 4.0f32, 6.0f32])\n\
         stats.mean(x)\n",
    );
    assert_eq!(stats_mean, Value::Float(4.0));
}

// ============================================================================
// Track 3B-2: All 5 AI-Native Static Checks in HM
// ============================================================================

#[test]
fn track3b2_check1_dtype_strictness_and_polymorphism() {
    // Reject mixed precision scalars & tensors without explicit cast
    assert_typecheck_fails("let x = 1.0f16 + 1.0bf16\n", "DType");
    assert_typecheck_fails("let x = 1.0f16 + 1.0\n", "DType");
    assert_typecheck_fails("let x = 1.0fp8 + 1.0fp8_e5m2\n", "DType");
    assert_typecheck_fails("let x: float = 1.0f16\n", "f16");
    assert_typecheck_fails(
        "let a = Tensor::zeros([2, 2], \"f16\")\nlet b = Tensor::zeros([2, 2], \"bf16\")\na + b\n",
        "DType",
    );
    assert_typecheck_fails("let a = Tensor::zeros([2, 2], \"f16\")\na * 2.0\n", "DType");
    assert_typecheck_fails(
        "let a = Tensor::zeros([2, 3], \"f16\")\nlet b = Tensor::zeros([3, 4], \"f32\")\na @ b\n",
        "DType",
    );
    // Reject arithmetic / softmax / randn on incompatible dtypes
    assert_typecheck_fails(
        "let m = tensor([true, false])\nm + m\n",
        "numeric Tensor DType",
    );
    assert_typecheck_fails(
        "let x = Tensor::zeros([2, 2], \"i32\")\nx.softmax()\n",
        "floating-point",
    );
    assert_typecheck_fails("Tensor::randn([2, 2], \"i8\")\n", "floating-point");
    assert_typecheck_fails("let u: u8 = 5u8\n-u\n", "signed numeric");

    // Polymorphic DType/Shape/Device function instantiates cleanly across dtypes
    let res_f16 = compile_check_and_run(
        "task scale<D, S, Dev>(x: Tensor<D, S, Dev>, k: D): Tensor<D, S, Dev>\n\
           return x * k\n\
         end\n\
         let a = scale(tensor([1.0f16, 2.0f16]), 3.0f16)\n\
         a[1]\n",
    );
    assert_eq!(
        res_f16,
        Value::Scalar(ScalarValue::F16(f64_to_f16_bits(6.0)))
    );
    let res_f32 = compile_check_and_run(
        "task scale<D, S, Dev>(x: Tensor<D, S, Dev>, k: D): Tensor<D, S, Dev>\n\
           return x * k\n\
         end\n\
         let b = scale(tensor([1.0f32, 2.0f32]), 4.0f32)\n\
         b[1]\n",
    );
    assert_eq!(res_f32, Value::Scalar(ScalarValue::F32(8.0)));
    // Mismatched rigid scalar type parameter inside polymorphic function is statically rejected
    assert_typecheck_fails(
        "task bad_scale<D, E, S, Dev>(x: Tensor<D, S, Dev>, k: E): Tensor<D, S, Dev>\n\
           return x * k\n\
         end\n",
        "DType",
    );
}

#[test]
fn track3b2_check2_static_shape_and_dependent_dimension_polymorphism() {
    // Dependent symbolic dimensions across multiple call sites with distinct shapes
    assert_typecheck_ok(
        "task matmul_2d(a: Tensor<f16, [M, K]>, b: Tensor<f16, [K, N]>): Tensor<f16, [M, N]>\n\
           return a @ b\n\
         end\n\
         let c1: Tensor<f16, [2, 4]> = matmul_2d(Tensor::ones([2, 3], \"f16\"), Tensor::ones([3, 4], \"f16\"))\n\
         let c2: Tensor<f16, [5, 7]> = matmul_2d(Tensor::ones([5, 6], \"f16\"), Tensor::ones([6, 7], \"f16\"))\n\
         let bcast: Tensor<f16, [2, 3]> = Tensor::ones([2, 1], \"f16\") + Tensor::ones([1, 3], \"f16\")\n\
         let wild: Tensor<f16, [_, 4]> = c1\n\
         let rs: Tensor<f16, [4, 2]> = c1.reshape([4, 2])\n\
         let tr: Tensor<f16, [2, 4]> = rs.transpose()\n",
    );

    // Reject inner dimension mismatch on `@`
    assert_typecheck_fails(
        "let a = Tensor::ones([2, 3], \"f16\")\nlet b = Tensor::ones([4, 2], \"f16\")\na @ b\n",
        "contraction",
    );
    // Reject symbolic mismatch when calling dependent matmul_2d
    assert_typecheck_fails(
        "task matmul_2d(a: Tensor<f16, [M, K]>, b: Tensor<f16, [K, N]>): Tensor<f16, [M, N]>\n\
           return a @ b\n\
         end\n\
         matmul_2d(Tensor::ones([2, 3], \"f16\"), Tensor::ones([5, 4], \"f16\"))\n",
        "3",
    );
    // Reject non-broadcastable shapes
    assert_typecheck_fails(
        "Tensor::ones([2, 3], \"f16\") + Tensor::ones([2, 4], \"f16\")\n",
        "broadcast",
    );
    // Reject element-count mismatch on reshape
    assert_typecheck_fails("Tensor::ones([2, 3], \"f16\").reshape([4, 2])\n", "reshape");
    // Reject 1D transpose & non-scalar .item()
    assert_typecheck_fails("Tensor::ones([4], \"f16\").transpose()\n", "rank >= 2");
    assert_typecheck_fails("Tensor::ones([2, 2], \"f16\").item()\n", "1-element");
    // Reject 2D tensors passed to linalg.dot
    assert_typecheck_fails(
        "linalg.dot(Tensor::ones([2, 2], \"f16\"), Tensor::ones([2, 2], \"f16\"))\n",
        "1D",
    );
}

#[test]
fn track3b2_check3_static_device_placement() {
    // Reject cross-device binary & matmul operations without explicit .to(...)
    assert_typecheck_fails(
        "let a = Tensor::ones([2, 2], \"f16\", \"cpu\")\n\
         let b = Tensor::ones([2, 2], \"f16\", \"cuda:0\")\n\
         a + b\n",
        "same device",
    );
    assert_typecheck_fails(
        "let a = Tensor::ones([2, 3], \"f16\", \"cpu\")\n\
         let b = Tensor::ones([3, 2], \"f16\", \"metal:0\")\n\
         a @ b\n",
        "same device",
    );
    // Explicit device transfer succeeds and propagates target device statically
    assert_typecheck_ok(
        "let a = Tensor::ones([2, 3], \"f16\", \"cpu\")\n\
         let b = Tensor::ones([3, 2], \"f16\", \"cuda:0\")\n\
         let c: Tensor<f16, [2, 2], cuda:0> = a.to(\"cuda:0\") @ b\n",
    );
    // Explicit runtime check also rejects mismatched devices on dynamic paths
    let err = compile_check_and_expect_runtime_err(
        "let a = Tensor::ones([2, 2], \"f16\", \"cpu\")\n\
         let dev = \"cuda:0\"\n\
         let b = a.to(dev)\n\
         a + b\n",
    );
    assert!(err.to_string().contains("Device mismatch"), "got: {err}");
}

#[test]
fn track3b2_check4_and_check5_resource_scoping_aliasing_interproc_and_effects() {
    // 1. Direct in-place mutation on requires_grad=true tensor rejected statically
    assert_typecheck_fails(
        "let w = Tensor::zeros([2, 2], \"f16\", \"cpu\", true)\nw.fill_(0.0f16)\n",
        "gradient-tracked",
    );
    assert_typecheck_fails(
        "let w = Tensor::zeros([2, 2], \"f16\").with_grad()\nw.add_(1.0f16)\n",
        "gradient-tracked",
    );

    // 2. In-place mutation on strided view rejected statically; allowed after .contiguous()
    assert_typecheck_fails(
        "let x = Tensor::zeros([2, 3], \"f16\")\nlet v = x.transpose()\nv.fill_(1.0f16)\n",
        "aliased or strided",
    );
    assert_typecheck_ok(
        "let x = Tensor::zeros([2, 3], \"f16\")\nlet v = x.transpose().contiguous()\nv.fill_(1.0f16)\n",
    );

    // 3. Source aliasing propagation: creating a view `v = x.transpose()` also marks `x` as aliased!
    assert_typecheck_fails(
        "let x = Tensor::zeros([2, 3], \"f16\")\nlet v = x.transpose()\nx.fill_(1.0f16)\n",
        "aliased or strided",
    );
    // Re-acquiring an owning contiguous buffer clears aliasing cleanly
    assert_typecheck_ok(
        "let x = Tensor::zeros([2, 3], \"f16\")\nlet v = x.transpose()\nassign x = x.contiguous()\nx.fill_(1.0f16)\n",
    );

    // 4. Lexical scope isolation: grad/view variable `x` inside `inner` must NOT pollute outer `x`!
    assert_typecheck_ok(
        "let x = Tensor::zeros([2, 2], \"f16\")\n\
         task inner()\n\
           let x = Tensor::zeros([2, 2], \"f16\", \"cpu\", true)\n\
           return x.sum()\n\
         end\n\
         x.fill_(2.0f16)\n",
    );

    // 5. Inter-procedural resource check: passing grad or view tensor into mutating function rejected at call site!
    assert_typecheck_fails(
        "task zero_out(buf: Tensor<f16, [2, 2]>)\n\
           buf.fill_(0.0f16)\n\
         end\n\
         let w = Tensor::ones([2, 2], \"f16\", \"cpu\", true)\n\
         zero_out(w)\n",
        "mutated in-place",
    );
    assert_typecheck_fails(
        "task zero_out(buf: Tensor<f16, [2, 2]>)\n\
           buf.fill_(0.0f16)\n\
         end\n\
         let x = Tensor::ones([2, 2], \"f16\")\n\
         let v = x.transpose()\n\
         zero_out(v)\n",
        "mutated in-place",
    );
    assert_typecheck_ok(
        "task zero_out(buf: Tensor<f16, [2, 2]>)\n\
           buf.fill_(0.0f16)\n\
         end\n\
         let own = Tensor::ones([2, 2], \"f16\")\n\
         zero_out(own)\n",
    );

    // 6. Check 5 (Effect): in-place mutation records `tensor_mutate` and `Tensor::randn` records `random_gauss`
    let src = "task mutate_and_sample()\n\
                 let t = Tensor::randn([2, 2], \"f16\")\n\
                 t.fill_(1.0f16)\n\
                 return t\n\
               end\n";
    let (_prog, witnesses) = ParserV3::compile(src).expect("compile");
    let info = infer_program_types(&witnesses);
    let fn_ty = info
        .env_types
        .get("mutate_and_sample")
        .expect("mutate_and_sample in env");
    let ty_str = fn_ty.name();
    assert!(
        ty_str.contains("tensor_mutate"),
        "expected tensor_mutate in effect row, got: {ty_str}"
    );
    assert!(
        ty_str.contains("random_random"),
        "expected random_random in effect row, got: {ty_str}"
    );
}

// ============================================================================
// Wave 2 Tail Cleanup Verification: Spanned MoraError + LSP Stage 3B Support
// ============================================================================

#[test]
fn spanned_mora_error_and_lsp_tensor_support() {
    let errs = typeck_errors("let x: float = 1.0f16\n").expect("compile");
    assert_eq!(errs.len(), 1);
    let mora_err = MoraError::from_type_errors(&errs);
    assert_eq!(mora_err.kind_name(), "typeck");
    let sp = mora_err.span().expect("type error must preserve Span");
    assert_eq!(sp.line, 1);

    // LSP completion & hover surface Stage 3B Tensor and low-bitwidth types
    let uri = "file:///stage3b.mora";
    let text = "let t = Tensor::zeros([2, 3], \"f16\", \"cpu\")\n";
    let mut docs = HashMap::new();
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: text.to_string(),
            version: 1,
            diagnostics: Vec::new(),
        },
    );
    let mut td = std::collections::BTreeMap::new();
    td.insert("uri".to_string(), JsonVal::String_(uri.to_string()));
    let mut params = std::collections::BTreeMap::new();
    params.insert("textDocument".to_string(), JsonVal::Object(td.clone()));

    let comp = completion_v3(&docs, &JsonVal::Object(params.clone()));
    let labels: Vec<String> = comp
        .as_array()
        .expect("completion array")
        .iter()
        .filter_map(|item| {
            item.get("label")
                .and_then(|l| l.as_str())
                .map(str::to_string)
        })
        .collect();
    for expected in [
        "tensor",
        "Tensor",
        "Tensor::randn",
        "f16",
        "bf16",
        "fp8",
        "fp8_e5m2",
        "i4",
    ] {
        assert!(
            labels.iter().any(|l| l == expected),
            "LSP completion missing `{expected}`"
        );
    }

    let mut pos = std::collections::BTreeMap::new();
    pos.insert("line".to_string(), JsonVal::Number(0.0));
    pos.insert("character".to_string(), JsonVal::Number(4.0));
    params.insert("position".to_string(), JsonVal::Object(pos));
    let hov = hover_v3(&docs, &JsonVal::Object(params)).expect("hover result");
    let hov_text = hov.to_string();
    assert!(
        hov_text.contains("Tensor<f16, [2, 3], cpu>"),
        "expected LSP hover to display Tensor<f16, [2, 3], cpu>, got: {hov_text}"
    );
}

// ============================================================================
// Property-Based Verification (`proptest`): Hardware Codecs & Packed Buffers
// ============================================================================

proptest! {
    #[test]
    fn prop_f16_bits_roundtrip_idempotent(bits in any::<u16>()) {
        let decoded = f16_bits_to_f64(bits);
        let reencoded = f64_to_f16_bits(decoded);
        if decoded.is_nan() {
            prop_assert!(f16_bits_to_f64(reencoded).is_nan());
        } else {
            prop_assert_eq!(reencoded, bits);
        }
    }

    #[test]
    fn prop_bf16_bits_roundtrip_idempotent(bits in any::<u16>()) {
        let decoded = bf16_bits_to_f64(bits);
        let reencoded = f64_to_bf16_bits(decoded);
        if decoded.is_nan() {
            prop_assert!(bf16_bits_to_f64(reencoded).is_nan());
        } else {
            prop_assert_eq!(reencoded, bits);
        }
    }

    #[test]
    fn prop_fp8_e4m3_and_e5m2_roundtrip_idempotent(bits in any::<u8>()) {
        let d43 = fp8_e4m3_bits_to_f64(bits);
        let r43 = f64_to_fp8_e4m3_bits(d43);
        if d43.is_nan() {
            prop_assert!(fp8_e4m3_bits_to_f64(r43).is_nan());
        } else {
            prop_assert_eq!(r43, bits);
        }

        let d52 = fp8_e5m2_bits_to_f64(bits);
        let r52 = f64_to_fp8_e5m2_bits(d52);
        if d52.is_nan() {
            prop_assert!(fp8_e5m2_bits_to_f64(r52).is_nan());
        } else {
            prop_assert_eq!(r52, bits);
        }
    }

    #[test]
    fn prop_i4_nibble_packed_buffer_roundtrip(elems in proptest::collection::vec(-8i8..=7i8, 1..=129)) {
        let mut buf = AlignedByteBuffer::zeroed(elems.len(), DType::I4);
        prop_assert_eq!(buf.byte_len(), elems.len().div_ceil(2));
        prop_assert_eq!((buf.aligned_byte_ptr() as usize) % 64, 0);
        for (idx, &v) in elems.iter().enumerate() {
            prop_assert_eq!(unpack_i4_nibble(pack_i4_nibble(v)), v);
            buf.set_f64(idx, v as f64);
        }
        for (idx, &v) in elems.iter().enumerate() {
            prop_assert_eq!(buf.get_value(idx), Value::Scalar(ScalarValue::I4(v)));
        }
    }
}
