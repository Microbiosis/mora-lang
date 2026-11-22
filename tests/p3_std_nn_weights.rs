//! Wave 4 / P3 ML Standard Library verification:
//! - Compact binary Tensor primitives (`to_bytes`, `to_utf8`, `Tensor::from_bytes`,
//!   `Tensor::from_utf8`, `Tensor::cat`) + `file.{read_binary,read_binary_range,write_binary}`
//! - Pure-Mora `std/nn/safetensors` (Hugging Face Safetensors spec + DoS/offset security checks)
//! - Pure-Mora `std/nn/gguf` (GGUF v3 binary spec, 13 KV metadata types, Q8_0/Q4_0 dequantization)

use std::collections::HashMap;
use std::sync::Arc;

use mora::flow::json::JsonValue as JsonVal;
use mora::lsp::providers::completion_v3;
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

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

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static type error for:\n{src}\nbut typeck succeeded"
    );
}

#[test]
fn p3_binary_tensor_and_file_primitives_static_types_and_lsp() {
    let td = TempDir::new("p3_bin_prim");
    let bin_path = td.path().join("sample.bin");
    let path_lit = bin_path.to_string_lossy().replace('\\', "/");

    let src = format!(
        r#"
let utf = Tensor::from_utf8("Mora-AI-权重-2026")
let back_str: string = utf.to_utf8()

let a = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f32")
let b = tensor([[5.0, 6.0], [7.0, 8.0]], dtype="f32")
let c0 = Tensor::cat([a, b], 0)
let c1 = Tensor::cat([a, b], -1)

let raw = a.to_bytes()
let a2 = Tensor::from_bytes(raw, [2, 2], "f32")
let diff_a = (a - a2).abs().max()

-- Transposed strided view serializes in logical row-major order
let at = a.transpose()
let at_back = Tensor::from_bytes(at.to_bytes(), [2, 2], "f32")
let diff_at = (at - at_back).abs().max()

-- Binary range file I/O
let p = "{path_lit}"
let payload = Tensor::cat([Tensor::from_utf8("HDR:"), a.to_bytes(), Tensor::from_utf8(":END")], 0)
file.write_binary(p, payload)
let full = file.read_binary(p)
let head_str = file.read_binary_range(p, 0, 4).to_utf8()
let mid_t = Tensor::from_bytes(file.read_binary_range(p, 4, 16), [2, 2], "f32")
let tail_str = file.read_binary_range(p, 20, 4).to_utf8()
let diff_mid = (a - mid_t).abs().max()

[back_str, str(c0.shape()), str(c1.shape()), str(diff_a < 1e-6f32), str(diff_at < 1e-6f32), str(full.numel()), head_str, tail_str, str(diff_mid < 1e-6f32)]
"#
    );

    let out = compile_check_and_run(&src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("Mora-AI-权重-2026".to_string()));
    assert_eq!(v[1], Value::String("[4, 2]".to_string()));
    assert_eq!(v[2], Value::String("[2, 4]".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
    assert_eq!(v[4], Value::String("true".to_string()));
    assert_eq!(v[5], Value::String("24".to_string()));
    assert_eq!(v[6], Value::String("HDR:".to_string()));
    assert_eq!(v[7], Value::String(":END".to_string()));
    assert_eq!(v[8], Value::String("true".to_string()));

    // Static HM rejections for misused binary primitives
    assert_typecheck_fails(
        r#"
let t = tensor([1.0, 2.0], dtype="f64")
let bad = t.to_utf8()
"#,
    );
    assert_typecheck_fails(
        r#"
let t = tensor([1.0, 2.0], dtype="f64")
let bad = Tensor::from_bytes(t, [2], "f32")
"#,
    );
    assert_typecheck_fails(
        r#"
file.write_binary("out.bin", "not-a-u8-tensor")
"#,
    );
    assert_typecheck_fails(
        r#"
let t = tensor([1.0, 2.0], dtype="f64")
file.write_binary("out.bin", t)
"#,
    );

    // LSP completion includes the new Tensor binary constructors
    let mut docs = HashMap::new();
    let uri = "file:///p3_weights.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let b = Tensor::from_utf8(\"hi\")\n".to_string(),
            version: 1,
            diagnostics: Vec::new(),
        },
    );
    let comp = completion_v3(
        &docs,
        &JsonVal::Object(
            [(
                "textDocument".to_string(),
                JsonVal::Object(
                    [("uri".to_string(), JsonVal::String_(uri.to_string()))]
                        .into_iter()
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        ),
    );
    let labels: Vec<&str> = comp
        .as_array()
        .expect("completion array")
        .iter()
        .filter_map(|item| item.get("label").and_then(|v| v.as_str()))
        .collect();
    assert!(labels.contains(&"Tensor::from_bytes"));
    assert!(labels.contains(&"Tensor::from_utf8"));
    assert!(labels.contains(&"Tensor::cat"));
}

#[test]
fn p3_std_nn_safetensors_all_dtypes_lazy_load_and_security_guards() {
    let td = TempDir::new("p3_safetensors");
    let ckpt_path = td.path().join("model.safetensors");
    let path_lit = ckpt_path.to_string_lossy().replace('\\', "/");

    let src = format!(
        r#"
import "std/nn/safetensors"

let t_f64 = tensor([[1.25, -2.5], [3.75, 0.0]], dtype="f64")
let t_f32 = tensor([[0.5, -1.5, 2.0], [3.0, 4.5, -0.25]], dtype="f32")
let t_f16 = tensor([1.0, -0.5, 2.5, 0.0], dtype="f16")
let t_bf16 = tensor([1.5, -3.0, 0.25, 8.0], dtype="bf16")
let t_fp8e4 = tensor([1.0, -2.0, 0.5, 4.0], dtype="fp8_e4m3")
let t_fp8e5 = tensor([1.0, -2.0, 0.5, 4.0], dtype="fp8_e5m2")
let t_i64 = tensor([10, -20, 3000000000], dtype="i64")
let t_i32 = tensor([-100, 0, 200], dtype="i32")
let t_i16 = tensor([-12, 34], dtype="i16")
let t_i8 = tensor([-120, 120], dtype="i8")
let t_u64 = tensor([0, 42], dtype="u64")
let t_u32 = tensor([1, 65536], dtype="u32")
let t_u16 = tensor([2, 1024], dtype="u16")
let t_u8 = tensor([0, 127, 255], dtype="u8")
let t_bool = tensor([true, false, true], dtype="bool")
let t_trans = t_f32.transpose()

let weights: dict<string, any> = {{}}
assign weights = weights.set("w.f64", t_f64)
assign weights = weights.set("w.f32", t_f32)
assign weights = weights.set("w.f16", t_f16)
assign weights = weights.set("w.bf16", t_bf16)
assign weights = weights.set("w.fp8e4", t_fp8e4)
assign weights = weights.set("w.fp8e5", t_fp8e5)
assign weights = weights.set("w.i64", t_i64)
assign weights = weights.set("w.i32", t_i32)
assign weights = weights.set("w.i16", t_i16)
assign weights = weights.set("w.i8", t_i8)
assign weights = weights.set("w.u64", t_u64)
assign weights = weights.set("w.u32", t_u32)
assign weights = weights.set("w.u16", t_u16)
assign weights = weights.set("w.u8", t_u8)
assign weights = weights.set("w.bool", t_bool)
assign weights = weights.set("w.trans", t_trans)
let meta: dict<string, string> = {{"format": "mora", "model": "llama-mini"}}

let p = "{path_lit}"
let save_res = save(p, weights, meta)
let saved_bytes = match save_res with
  SafetensorsResult::Ok(n) -> n
  SafetensorsResult::Err(e) -> -1
end

-- 1. Lazy header-only inspect
let insp_res = inspect(p)
let insp_ok = match insp_res with
  SafetensorsResult::Ok(h) -> (h.data_offset % 8 == 0) and (h.metadata.get("format") == "mora") and (h.tensors.len() == 16)
  SafetensorsResult::Err(_) -> false
end

-- 2. Lazy single-tensor load onto cuda:0 (or cpu)
let lazy_res = load_tensor(p, "w.trans", "cuda:0")
let lazy_ok = match lazy_res with
  SafetensorsResult::Ok(t) -> t.device() == "cuda:0" and t.shape() == [3, 2] and (t.to_device("cpu").to_bytes() == t_trans.to_bytes())
  SafetensorsResult::Err(_) -> false
end

-- 3. Full checkpoint load: bit-exact byte equality across all 16 tensors
task verify_all_st(sf: SafetensorsFile, weights: dict<string, any>): bool
  let ok = sf.metadata.get("model") == "llama-mini"
  for k in weights.keys()
    let orig = weights.get(str(k))
    let got = sf.tensors.get(str(k))
    if got == nil or orig.dtype() != got.dtype() or orig.shape() != got.shape() or orig.to_bytes() != got.to_bytes() then
      assign ok = false
    end
  end
  return ok
end

let load_res = load(p)
let all_exact = match load_res with
  SafetensorsResult::Ok(sf) -> verify_all_st(sf, weights)
  SafetensorsResult::Err(_) -> false
end

-- 4. Missing tensor cleanly returns Err
let miss_err = match load_tensor(p, "no_such_layer") with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("not found")
end

-- 5. Security guard: HeaderTooLarge (> 100MB)
let huge_hdr = Tensor::cat([Tensor::full([1], 100000001, dtype="i64").to_bytes(), Tensor::from_utf8("{{}}")], 0)
let huge_err = match inspect_bytes(huge_hdr) with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("HeaderTooLarge")
end

-- 6. Security guard: overlapping data_offsets rejected
let overlap_json = Tensor::from_utf8("{{\"a\":{{\"dtype\":\"U8\",\"shape\":[4],\"data_offsets\":[0,4]}},\"b\":{{\"dtype\":\"U8\",\"shape\":[4],\"data_offsets\":[2,6]}}}}")
let overlap_buf = Tensor::cat([Tensor::full([1], overlap_json.numel(), dtype="i64").to_bytes(), overlap_json, Tensor::zeros([6], dtype="u8")], 0)
let overlap_err = match decode(overlap_buf) with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("overlapping")
end

-- 7. Security guard: unmapped hole/gap in data_offsets rejected
let hole_json = Tensor::from_utf8("{{\"a\":{{\"dtype\":\"U8\",\"shape\":[2],\"data_offsets\":[0,2]}},\"b\":{{\"dtype\":\"U8\",\"shape\":[2],\"data_offsets\":[4,6]}}}}")
let hole_buf = Tensor::cat([Tensor::full([1], hole_json.numel(), dtype="i64").to_bytes(), hole_json, Tensor::zeros([6], dtype="u8")], 0)
let hole_err = match decode(hole_buf) with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("gap/hole")
end

-- 8. Security guard: trailing extra unreferenced bytes rejected
let extra_json = Tensor::from_utf8("{{\"a\":{{\"dtype\":\"U8\",\"shape\":[2],\"data_offsets\":[0,2]}}}}")
let extra_buf = Tensor::cat([Tensor::full([1], extra_json.numel(), dtype="i64").to_bytes(), extra_json, Tensor::zeros([5], dtype="u8")], 0)
let extra_err = match decode(extra_buf) with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("size mismatch")
end

-- 9. Security guard: header not starting with '{{'
let bad_hdr_raw = Tensor::from_utf8("[]")
let bad_hdr_buf = Tensor::cat([Tensor::full([1], 2, dtype="i64").to_bytes(), bad_hdr_raw], 0)
let bad_hdr_err = match inspect_bytes(bad_hdr_buf) with
  SafetensorsResult::Ok(_) -> false
  SafetensorsResult::Err(msg) -> msg.contains("0x7B")
end

[str(saved_bytes > 0), str(insp_ok), str(lazy_ok), str(all_exact), str(miss_err), str(huge_err), str(overlap_err), str(hole_err), str(extra_err), str(bad_hdr_err)]
"#
    );

    let out = compile_check_and_run(&src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    for (idx, item) in v.iter().enumerate() {
        assert_eq!(
            *item,
            Value::String("true".to_string()),
            "safetensors assertion #{idx} failed: {v:?}"
        );
    }

    // Wire-format independent Rust verification of the written `.safetensors` file
    let raw = std::fs::read(&ckpt_path).expect("read saved safetensors");
    assert!(raw.len() > 16);
    let hdr_len = u64::from_le_bytes(raw[0..8].try_into().unwrap()) as usize;
    assert_eq!((8 + hdr_len) % 8, 0, "header must be 8-byte aligned");
    assert_eq!(raw[8], b'{', "JSON header must start with '{{'");
    let hdr_str = std::str::from_utf8(&raw[8..8 + hdr_len]).expect("valid UTF-8 header");
    assert!(hdr_str.contains("\"F8_E4M3\""));
    assert!(hdr_str.contains("\"F8_E5M2\""));
    assert!(hdr_str.contains("\"BF16\""));
    assert!(hdr_str.contains("\"__metadata__\""));
}

/// Build an independent raw GGUF v3 binary file containing both Q8_0 and Q4_0 quantized tensors
/// directly in Rust to prove wire-level interoperability with llama.cpp / GGML specs.
fn build_quantized_gguf_v3_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let push_str = |buf: &mut Vec<u8>, s: &str| {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    };

    // Header: magic="GGUF", version=3, tensor_count=2, kv_count=1
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&2u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());

    // KV 0: "general.architecture" = "mora-quant" (STRING = 8)
    push_str(&mut out, "general.architecture");
    out.extend_from_slice(&8u32.to_le_bytes());
    push_str(&mut out, "mora-quant");

    // Tensor 0: "blk.0.q8", dims=[32, 1] (GGML inner-first -> Mora shape [1, 32]), type=Q8_0 (8), offset=0
    push_str(&mut out, "blk.0.q8");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&32u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    // Tensor 1: "blk.0.q4", dims=[32] (Mora shape [32]), type=Q4_0 (2), offset=64 (32-byte aligned after 34B)
    push_str(&mut out, "blk.0.q4");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&32u64.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&64u64.to_le_bytes());

    // Pad header to 32-byte alignment
    while out.len() % 32 != 0 {
        out.push(0);
    }

    // Q8_0 block (34 bytes): scale = 0.5 (f16 = 0x3800), quants[i] = (i as i8) - 16
    out.extend_from_slice(&0x3800u16.to_le_bytes());
    for i in 0..32i8 {
        out.push((i - 16) as u8);
    }
    // Pad from 34 -> 64 bytes
    out.resize(out.len() + 30, 0);

    // Q4_0 block (18 bytes): scale = 0.25 (f16 = 0x3400)
    // For j in 0..16: low nibble = 9 (val = +1 * 0.25 = +0.25), high nibble = 6 (val = -2 * 0.25 = -0.50)
    out.extend_from_slice(&0x3400u16.to_le_bytes());
    for _ in 0..16 {
        let byte = (6u8 << 4) | 9u8;
        out.push(byte);
    }

    out
}

#[test]
fn p3_std_nn_gguf_v3_metadata_roundtrip_lazy_load_and_quantized_decoding() {
    let td = TempDir::new("p3_gguf");
    let ckpt_path = td.path().join("mini_llm.gguf");
    let quant_path = td.path().join("quant.gguf");
    std::fs::write(&quant_path, build_quantized_gguf_v3_bytes()).expect("write quant gguf");

    let ckpt_lit = ckpt_path.to_string_lossy().replace('\\', "/");
    let quant_lit = quant_path.to_string_lossy().replace('\\', "/");

    let src = format!(
        r#"
import "std/nn/gguf"

-- 1. Build a rich GGUF v3 checkpoint covering all 13 metadata KV types + 8 hardware dtypes + 64B alignment
let meta: dict<string, any> = {{}}
assign meta = meta.set("m.u8", meta_u8(200))
assign meta = meta.set("m.i8", meta_i8(-42))
assign meta = meta.set("m.u16", meta_u16(40000))
assign meta = meta.set("m.i16", meta_i16(-12345))
assign meta = meta.set("m.u32", meta_u32(70000))
assign meta = meta.set("m.i32", meta_i32(-99999))
assign meta = meta.set("m.u64", meta_u64(123456789012))
assign meta = meta.set("m.i64", meta_i64(-123456789012))
assign meta = meta.set("m.f32", meta_f32(1.5))
assign meta = meta.set("m.f64", meta_f64(3.141592653589793))
assign meta = meta.set("m.bool", meta_bool(true))
assign meta = meta.set("m.str", meta_string("mora-gguf-v3"))
assign meta = meta.set("m.arr_str", meta_array(GGUF_TYPE_STRING, ["tok0", "tok1", "tok2"]))
assign meta = meta.set("m.arr_i32", meta_array(GGUF_TYPE_INT32, [10, -20, 30]))

let w_f32 = tensor([[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]], dtype="f32")
let w_f16 = tensor([[0.5, -1.5], [2.25, 0.0]], dtype="f16")
let w_bf16 = tensor([1.0, -2.0, 4.0, 0.5], dtype="bf16")
let w_f64 = tensor([1.125, -2.25], dtype="f64")
let w_i8 = tensor([-10, 0, 10], dtype="i8")
let w_i16 = tensor([-300, 300], dtype="i16")
let w_i32 = tensor([-100000, 100000], dtype="i32")
let w_i64 = tensor([-9000000000, 9000000000], dtype="i64")

let weights: dict<string, any> = {{}}
assign weights = weights.set("blk.0.f32", w_f32)
assign weights = weights.set("blk.0.f16", w_f16)
assign weights = weights.set("blk.0.bf16", w_bf16)
assign weights = weights.set("blk.0.f64", w_f64)
assign weights = weights.set("blk.0.i8", w_i8)
assign weights = weights.set("blk.0.i16", w_i16)
assign weights = weights.set("blk.0.i32", w_i32)
assign weights = weights.set("blk.0.i64", w_i64)

let p = "{ckpt_lit}"
let save_ok = match save(p, weights, meta, 64) with
  GgufResult::Ok(n) -> n > 0
  GgufResult::Err(_) -> false
end

-- 2. Lazy inspect header from disk (verifies alignment=64, version=3, all 13 KV types + general.alignment)
task check_gguf_header(h: GgufHeader): bool
  return h.version == 3 and h.alignment == 64 and (h.data_offset % 64 == 0) and h.tensor_count == 8 and h.kv_count == 15 and h.metadata.get("m.u8") == 200 and h.metadata.get("m.i8") == -42 and h.metadata.get("m.u16") == 40000 and h.metadata.get("m.i16") == -12345 and h.metadata.get("m.u32") == 70000 and h.metadata.get("m.i32") == -99999 and h.metadata.get("m.u64") == 123456789012 and h.metadata.get("m.i64") == -123456789012 and h.metadata.get("m.bool") == true and h.metadata.get("m.str") == "mora-gguf-v3" and h.metadata.get("m.arr_str") == ["tok0", "tok1", "tok2"] and h.metadata.get("m.arr_i32") == [10, -20, 30]
end

let insp_ok = match inspect(p) with
  GgufResult::Ok(h) -> check_gguf_header(h)
  GgufResult::Err(_) -> false
end

-- 3. Full decode: bit-exact equality & 3D shape preservation (GGML reversed dims round-trip!)
task verify_all_gguf(gf: GgufFile, weights: dict<string, any>): bool
  let ok = true
  for k in weights.keys()
    let orig = weights.get(str(k))
    let got = gf.tensors.get(str(k))
    if got == nil or orig.dtype() != got.dtype() or orig.shape() != got.shape() or orig.to_bytes() != got.to_bytes() then
      assign ok = false
    end
  end
  return ok
end

let all_exact = match load(p) with
  GgufResult::Ok(gf) -> verify_all_gguf(gf, weights)
  GgufResult::Err(_) -> false
end

-- 4. Lazy single-tensor load
let lazy_ok = match load_tensor(p, "blk.0.f32") with
  GgufResult::Ok(t) -> t.shape() == [1, 2, 3] and t.to_bytes() == w_f32.to_bytes()
  GgufResult::Err(_) -> false
end

-- 5. Wire-level quantized GGUF v3 decoding (Q8_0 & Q4_0 generated externally by Rust)
task verify_quant_gguf(qf: GgufFile): bool
  let arch_ok = qf.metadata.get("general.architecture") == "mora-quant"
  let q8: any = qf.tensors.get("blk.0.q8")
  let q4: any = qf.tensors.get("blk.0.q4")
  let q8_ok = q8.shape() == [1, 32] and q8.dtype() == "f32" and float(q8[0, 0]) == -8.0 and float(q8[0, 16]) == 0.0 and float(q8[0, 31]) == 7.5
  let q4_ok = q4.shape() == [32] and q4.dtype() == "f32" and float(q4[0]) == 0.25 and float(q4[15]) == 0.25 and float(q4[16]) == -0.5 and float(q4[31]) == -0.5
  return arch_ok and q8_ok and q4_ok
end

let qp = "{quant_lit}"
let quant_ok = match load(qp) with
  GgufResult::Ok(qf) -> verify_quant_gguf(qf)
  GgufResult::Err(_) -> false
end

-- 6. Pure-Mora Q8_0 quantize -> dequantize accuracy check
task check_q8_dequant(raw_q8: any, orig_vec: any): bool
  return match dequantize_q8_0(raw_q8, [32]) with
    GgufResult::Ok(rec) -> (orig_vec - rec).abs().max() < 0.02f32
    GgufResult::Err(_) -> false
  end
end

let orig_vec = Tensor::arange(-16.0, 16.0, 1.0, dtype="f32") * 0.1f32
let q8_rt_ok = match quantize_q8_0(orig_vec) with
  GgufResult::Ok(raw_q8) -> check_q8_dequant(raw_q8, orig_vec)
  GgufResult::Err(_) -> false
end

-- 7. Reject bad magic & unsupported version
let bad_magic = match inspect_bytes(Tensor::from_utf8("NOPE0000000000000000000000")) with
  GgufResult::Ok(_) -> false
  GgufResult::Err(msg) -> msg.contains("magic")
end
let bad_ver_buf = Tensor::cat([Tensor::from_utf8("GGUF"), Tensor::full([1], 99, dtype="u32").to_bytes(), Tensor::zeros([16], dtype="u8")], 0)
let bad_ver = match inspect_bytes(bad_ver_buf) with
  GgufResult::Ok(_) -> false
  GgufResult::Err(msg) -> msg.contains("version")
end

[str(save_ok), str(insp_ok), str(all_exact), str(lazy_ok), str(quant_ok), str(q8_rt_ok), str(bad_magic), str(bad_ver)]
"#
    );

    let out = compile_check_and_run(&src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    for (idx, item) in v.iter().enumerate() {
        assert_eq!(
            *item,
            Value::String("true".to_string()),
            "gguf assertion #{idx} failed: {v:?}"
        );
    }

    // Wire-format independent Rust verification of saved GGUF v3 file
    let raw = std::fs::read(&ckpt_path).expect("read saved gguf");
    assert_eq!(&raw[0..4], b"GGUF");
    assert_eq!(u32::from_le_bytes(raw[4..8].try_into().unwrap()), 3);
    assert_eq!(u64::from_le_bytes(raw[8..16].try_into().unwrap()), 8);
    assert_eq!(u64::from_le_bytes(raw[16..24].try_into().unwrap()), 15);
}
