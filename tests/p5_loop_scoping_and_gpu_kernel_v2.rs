//! P5-1B Integration & Live Hardware Verification Suite:
//! - GPU `kernel` v2: Workgroup `shared` memory (`shared tile: [f32; N]`) + `sync_threads()` barriers
//! - Full 2D / 3D launch geometry (`[gx, gy, gz]`, `[bx, by, bz]`) & `*_y` / `*_z` thread intrinsics
//! - Low-precision & integer buffers (`buf<f16>`, `buf<bf16>`, `buf<u8>`, `buf<i32>`)
//! - First-class bitwise operators (`&`, `|`, `^`, `<<`, `>>`) in both Mora & GPU kernels
//! - Live RTX CUDA PTX 7.0 + Vulkan SPIR-V 1.0 (atomic subword stores) + MSL + CPU workgroup barrier simulator
//! - `MORABC02` AOT roundtrip, `mora fmt` idempotence, and static HM rejections.

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

fn as_f64_list(val: &Value) -> Vec<f64> {
    let Value::List(items) = val else {
        panic!("expected list, got {val}");
    };
    items
        .iter()
        .map(|v| match v {
            Value::Float(f) => *f,
            Value::Int(n) => *n as f64,
            Value::Scalar(s) => s.as_f64(),
            other => panic!("expected numeric element, got {other}"),
        })
        .collect()
}

#[test]
fn p5_1b_tiled_shared_memory_gemm_2d_cpu_cuda_vulkan() {
    // 10x14 @ 14x6 tiled GEMM with 4x4 workgroup tiles, shared memory arrays,
    // and two `sync_threads()` barriers per K-tile iteration.
    let gemm_kernel_src = r#"
kernel tiled_gemm_4x4(
  a: buf<f32>,
  b: buf<f32>,
  c: buf<f32>,
  m: i32,
  n: i32,
  k: i32
)
  shared tile_a: [f32; 16]
  shared tile_b: [f32; 16]

  let tx = thread_idx()
  let ty = thread_idx_y()
  let col = global_id()
  let row = global_id_y()

  let acc: f32 = 0.0
  let num_tiles = (k + 3) / 4
  let t: i32 = 0
  while t < num_tiles
    let a_col = t * 4 + tx
    if row < m and a_col < k then
      tile_a[ty * 4 + tx] = a[row * k + a_col]
    else
      tile_a[ty * 4 + tx] = f32(0)
    end
    let b_row = t * 4 + ty
    if b_row < k and col < n then
      tile_b[ty * 4 + tx] = b[b_row * n + col]
    else
      tile_b[ty * 4 + tx] = f32(0)
    end
    sync_threads()

    let i: i32 = 0
    while i < 4
      acc = acc + tile_a[ty * 4 + i] * tile_b[i * 4 + tx]
      i = i + 1
    end
    sync_threads()
    t = t + 1
  end

  if row < m and col < n then
    c[row * n + col] = acc
  end
end
"#;

    // 1. Inspect generated PTX, SPIR-V, and MSL artifacts.
    let inspect_src = format!(
        r#"{gemm_kernel_src}
struct InspectOut
  ptx: string
  msl: string
  spv_len: int
  spv_magic: int
end
let ptx = tiled_gemm_4x4.emit_ptx()
let msl = tiled_gemm_4x4.emit_msl()
let spv = tiled_gemm_4x4.emit_spirv_words([4, 4])
InspectOut(ptx, msl, len(spv), spv[0])
"#
    );
    let Value::StructInstance { fields: info, .. } =
        eval_source(&inspect_src).expect("inspect tiled_gemm_4x4")
    else {
        panic!("expected InspectOut struct");
    };
    let Value::String(ptx) = &info["ptx"] else {
        panic!("expected PTX string");
    };
    let Value::String(msl) = &info["msl"] else {
        panic!("expected MSL string");
    };
    assert!(
        ptx.contains(".shared .align 4 .f32 _sh_0[16];"),
        "PTX:\n{ptx}"
    );
    assert!(
        ptx.contains(".shared .align 4 .f32 _sh_1[16];"),
        "PTX:\n{ptx}"
    );
    assert!(ptx.contains("bar.sync 0;"), "PTX:\n{ptx}");
    assert!(ptx.contains("%ctaid.y"), "PTX:\n{ptx}");
    assert!(ptx.contains("%tid.y"), "PTX:\n{ptx}");
    assert!(msl.contains("threadgroup float _sh_0[16];"), "MSL:\n{msl}");
    assert!(msl.contains("threadgroup float _sh_1[16];"), "MSL:\n{msl}");
    assert!(
        msl.contains("threadgroup_barrier(mem_flags::mem_threadgroup);"),
        "MSL:\n{msl}"
    );
    assert!(msl.contains("int(_gid.y)"), "MSL:\n{msl}");
    assert_eq!(
        info["spv_magic"],
        Value::Int(0x0723_0203),
        "SPIR-V magic mismatch"
    );

    // 2. Execute 2D tiled shared-memory GEMM on CPU + every live GPU (CUDA, Vulkan)!
    let devices = available_devices();
    println!("P5-1B tiled GEMM live devices: {devices:?}");
    let mut cpu_ref: Option<Vec<f64>> = None;
    for dev in &devices {
        let run_src = format!(
            r#"{gemm_kernel_src}
let m = 10
let k = 14
let n = 6
let a_vals: list<list<float>> = []
let r = 0
while r < m
  let row: list<float> = []
  let j = 0
  while j < k
    row = row.push(((r * 3 + j * 5) % 11 - 5) * 0.25)
    j = j + 1
  end
  a_vals = a_vals.push(row)
  r = r + 1
end

let b_vals: list<list<float>> = []
let br = 0
while br < k
  let row: list<float> = []
  let bc = 0
  while bc < n
    row = row.push(((br * 7 + bc * 2) % 9 - 4) * 0.5)
    bc = bc + 1
  end
  b_vals = b_vals.push(row)
  br = br + 1
end

let a_cpu = Tensor::from_list(a_vals, "f32")
let b_cpu = Tensor::from_list(b_vals, "f32")
let expected = (a_cpu @ b_cpu).reshape([m * n]).to_list()

let a_dev = a_cpu.reshape([m * k]).to("{dev}")
let b_dev = b_cpu.reshape([k * n]).to("{dev}")
let c_dev = Tensor::zeros([m * n], "f32", "{dev}")

-- Non-multiple-of-4 dimensions (10x14 @ 14x6) test out-of-bounds guards + shared tile zero-padding!
-- grid_x = ceil(6/4) = 2, grid_y = ceil(10/4) = 3, block = [4, 4]
tiled_gemm_4x4.launch([2, 3], [4, 4], a_dev, b_dev, c_dev, m, n, k)
let actual = c_dev.to("cpu").to_list()
[expected, actual]
"#
        );
        let Value::List(res) = eval_source(&run_src)
            .unwrap_or_else(|e| panic!("2D tiled GEMM failed on device '{dev}': {e}"))
        else {
            panic!("expected [expected, actual]");
        };
        let expected = as_f64_list(&res[0]);
        let actual = as_f64_list(&res[1]);
        assert_eq!(expected.len(), actual.len(), "device {dev} length mismatch");
        for (idx, (e, a)) in expected.iter().zip(actual.iter()).enumerate() {
            assert!(
                (e - a).abs() < 1e-4,
                "device {dev} mismatch at idx {idx}: expected {e}, got {a}"
            );
        }
        if dev == "cpu" {
            cpu_ref = Some(actual);
        } else if let Some(ref ref_vals) = cpu_ref {
            for (idx, (c, g)) in ref_vals.iter().zip(actual.iter()).enumerate() {
                assert!(
                    (c - g).abs() < 1e-4,
                    "device {dev} vs cpu mismatch at idx {idx}: cpu={c}, gpu={g}"
                );
            }
        }
    }
}

#[test]
fn p5_1b_low_precision_and_quantized_q4_bitwise_kernel_cpu_cuda_vulkan() {
    // Real LLM quantized dequantize + bit-pack kernel using buf<u8>, buf<f16>, buf<bf16>, buf<i32>, buf<f32>
    // AND subword writes into f16, bf16, u8, i32 buffers (testing odd-length atomic subword stores on Vulkan!).
    let q4_kernel_src = r#"
kernel q4_dequant_and_pack(
  packed_q4: buf<u8>,
  scales_f16: buf<f16>,
  bias_bf16: buf<bf16>,
  out_f32: buf<f32>,
  out_f16: buf<f16>,
  out_bf16: buf<bf16>,
  out_u8: buf<u8>,
  out_i32: buf<i32>,
  n_bytes: i32
)
  let tid = global_id()
  if tid < n_bytes then
    let byte_val = packed_q4[tid]
    let lo_q = byte_val & 15
    let hi_q = (byte_val >> 4) & 15
    let re_packed = ((hi_q << 4) | lo_q) ^ 85
    let sc = scales_f16[tid]
    let bs = bias_bf16[tid]
    let deq = (f32(lo_q - 8) + f32(hi_q - 8) * f32(0.5)) * sc + bs
    out_f32[tid] = deq
    out_f16[tid] = deq
    out_bf16[tid] = deq
    out_u8[tid] = re_packed
    out_i32[tid] = (hi_q << 8) | lo_q
  end
end
"#;

    let devices = available_devices();
    let mut cpu_baseline: Option<Vec<Vec<f64>>> = None;

    for dev in &devices {
        // Odd length 13 exercises partial trailing 16-bit and 8-bit lanes on Vulkan atomic subword stores!
        let run_src = format!(
            r#"{q4_kernel_src}
let n = 13
let packed = Tensor::from_list([18, 52, 86, 120, 154, 188, 222, 240, 15, 165, 90, 255, 0], "u8").to("{dev}")
let scales = Tensor::from_list([0.25, 0.5, 1.0, 0.75, 1.25, 0.5, 2.0, 0.125, 1.5, 0.25, 0.5, 1.0, 0.5], "f16").to("{dev}")
let bias = Tensor::from_list([0.5, -0.5, 1.0, 0.0, 0.25, -1.0, 2.0, 0.5, -0.25, 1.0, 0.0, -0.5, 3.0], "bf16").to("{dev}")
let out_f32 = Tensor::zeros([n], "f32", "{dev}")
let out_f16 = Tensor::zeros([n], "f16", "{dev}")
let out_bf16 = Tensor::zeros([n], "bf16", "{dev}")
let out_u8 = Tensor::zeros([n], "u8", "{dev}")
let out_i32 = Tensor::zeros([n], "i32", "{dev}")

q4_dequant_and_pack.launch(1, 16, packed, scales, bias, out_f32, out_f16, out_bf16, out_u8, out_i32, n)
struct Q4Out
  f32s: list<any>
  f16s: list<any>
  bf16s: list<any>
  u8s: list<any>
  i32s: list<any>
end
Q4Out(out_f32.to("cpu").to_list(), out_f16.to("cpu").to_list(), out_bf16.to("cpu").to_list(), out_u8.to("cpu").to_list(), out_i32.to("cpu").to_list())
"#
        );
        let Value::StructInstance {
            fields: outputs, ..
        } = eval_source(&run_src)
            .unwrap_or_else(|e| panic!("Q4 low-prec kernel failed on device '{dev}': {e}"))
        else {
            panic!("expected Q4Out struct");
        };
        let f32s = as_f64_list(&outputs["f32s"]);
        let f16s = as_f64_list(&outputs["f16s"]);
        let bf16s = as_f64_list(&outputs["bf16s"]);
        let u8s = as_f64_list(&outputs["u8s"]);
        let i32s = as_f64_list(&outputs["i32s"]);

        let raw_bytes: [i32; 13] = [18, 52, 86, 120, 154, 188, 222, 240, 15, 165, 90, 255, 0];
        let raw_scales: [f64; 13] = [
            0.25, 0.5, 1.0, 0.75, 1.25, 0.5, 2.0, 0.125, 1.5, 0.25, 0.5, 1.0, 0.5,
        ];
        let raw_bias: [f64; 13] = [
            0.5, -0.5, 1.0, 0.0, 0.25, -1.0, 2.0, 0.5, -0.25, 1.0, 0.0, -0.5, 3.0,
        ];
        for i in 0..13 {
            let b = raw_bytes[i];
            let lo = b & 15;
            let hi = (b >> 4) & 15;
            let exp_f32 = ((lo - 8) as f64 + (hi - 8) as f64 * 0.5) * raw_scales[i] + raw_bias[i];
            let exp_u8 = (b ^ 85) as f64;
            let exp_i32 = ((hi << 8) | lo) as f64;
            assert!(
                (f32s[i] - exp_f32).abs() < 1e-3,
                "device {dev} out_f32[{i}]: expected {exp_f32}, got {}",
                f32s[i]
            );
            assert!(
                (f16s[i] - exp_f32).abs() < 2e-2,
                "device {dev} out_f16[{i}]: expected ~{exp_f32}, got {}",
                f16s[i]
            );
            assert!(
                (bf16s[i] - exp_f32).abs() < 8e-2,
                "device {dev} out_bf16[{i}]: expected ~{exp_f32}, got {}",
                bf16s[i]
            );
            assert_eq!(
                u8s[i], exp_u8,
                "device {dev} out_u8[{i}] mismatch: expected {exp_u8}, got {}",
                u8s[i]
            );
            assert_eq!(
                i32s[i], exp_i32,
                "device {dev} out_i32[{i}] mismatch: expected {exp_i32}, got {}",
                i32s[i]
            );
        }

        let pack = vec![f32s, f16s, bf16s, u8s, i32s];
        if dev == "cpu" {
            cpu_baseline = Some(pack);
        } else if let Some(ref base) = cpu_baseline {
            for b_idx in 0..5 {
                for i in 0..13 {
                    assert!(
                        (base[b_idx][i] - pack[b_idx][i]).abs() < 1e-3,
                        "device {dev} vs cpu mismatch buf #{b_idx}[{i}]: cpu={}, gpu={}",
                        base[b_idx][i],
                        pack[b_idx][i]
                    );
                }
            }
        }
    }
}

#[test]
fn p5_1b_3d_launch_geometry_and_divergent_barrier_detection() {
    // Full 3D grid [2, 2, 2] x block [2, 2, 2] = 64 threads verifying all 15 coordinate built-ins.
    let k3d_src = r#"
kernel fill_3d(out: buf<i32>)
  let gx = global_id()
  let gy = global_id_y()
  let gz = global_id_z()
  let nx = grid_dim() * block_dim()
  let ny = grid_dim_y() * block_dim_y()
  let idx = (gz * ny + gy) * nx + gx
  let cz = (block_idx_z() * block_dim_z() + thread_idx_z()) * 100
  let cy = (block_idx_y() * block_dim_y() + thread_idx_y()) * 10
  let cx = block_idx() * block_dim() + thread_idx()
  out[idx] = cz + cy + cx + grid_dim_z() * 1000
end
"#;

    for dev in available_devices() {
        let src = format!(
            r#"{k3d_src}
let out = Tensor::zeros([64], "i32", "{dev}")
fill_3d.launch([2, 2, 2], [2, 2, 2], out)
out.to("cpu").to_list()
"#
        );
        let vals = as_f64_list(
            &eval_source(&src).unwrap_or_else(|e| panic!("3D launch failed on {dev}: {e}")),
        );
        assert_eq!(vals.len(), 64);
        for gz in 0..4 {
            for gy in 0..4 {
                for gx in 0..4 {
                    let idx = (gz * 4 + gy) * 4 + gx;
                    let expected = (2 * 1000 + gz * 100 + gy * 10 + gx) as f64;
                    assert_eq!(
                        vals[idx], expected,
                        "3D mismatch on {dev} at ({gx},{gy},{gz})"
                    );
                }
            }
        }
    }

    // Divergent `sync_threads()` inside a conditional branch MUST be rejected cleanly by CPU simulator!
    let divergent_src = r#"
kernel bad_barrier(out: buf<i32>)
  shared s: [i32; 4]
  let t = thread_idx()
  s[t] = t
  if t == 0 then
    sync_threads()
  end
  out[t] = s[t]
end
let out = Tensor::zeros([4], "i32")
bad_barrier.launch(1, 4, out)
"#;
    let err = eval_source(divergent_src)
        .expect_err("divergent sync_threads() must fail loudly on CPU simulator");
    assert!(
        err.contains("divergent sync_threads()"),
        "expected divergent barrier diagnostic, got: {err}"
    );
}

#[test]
fn p5_1b_morabc02_aot_roundtrip_and_formatting_and_static_checks() {
    let src = r#"
kernel shared_rev(inp: buf<f16>, out: buf<f16>)
  shared scratch: [f32; 8]
  let t = thread_idx()
  scratch[t] = inp[t]
  sync_threads()
  out[t] = scratch[7 - t]
end

let x = Tensor::from_list([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], "f16")
let y = Tensor::zeros([8], "f16")
shared_rev.launch(1, 8, x, y)
y.to_list()
"#;

    // 1. MORABC02 `.morac` AOT compile -> delete source -> run zero-source bytecode!
    check_source(src).expect("shared_rev should pass static HM");
    let tmp = TempDir::new("p5_1b_morac");
    let script_path = tmp.path().join("shared_rev.mora");
    let morac_path = tmp.path().join("shared_rev.morac");
    let morac_src = format!("{src}\nprint(y.to_list())\n");
    std::fs::write(&script_path, &morac_src).expect("write morac script");
    let mora_bin = env!("CARGO_BIN_EXE_mora");
    let build_out = Command::new(mora_bin)
        .args([
            "build",
            "--bytecode",
            script_path.to_str().expect("utf8 path"),
            "-o",
            morac_path.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("run mora build --bytecode");
    assert!(
        build_out.status.success(),
        "mora build --bytecode failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );
    std::fs::remove_file(&script_path).expect("remove source before zero-source .morac run");
    let run_out = Command::new(mora_bin)
        .arg(morac_path.to_str().expect("utf8 path"))
        .output()
        .expect("run .morac");
    assert!(
        run_out.status.success(),
        "running .morac failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(
        stdout.contains("8") && stdout.contains("1"),
        "unexpected .morac output: {stdout}"
    );
    let val = eval_source(src).expect("eval shared_rev");
    let out = as_f64_list(&val);
    assert_eq!(out, vec![8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);

    // 2. `mora fmt` idempotence for shared arrays, bitwise ops, and buf<f16>/buf<u8>.
    let raw_fmt = r#"kernel k_fmt(a: buf<u8>, b: buf<f16>)
shared tile: [f32; 16]
let t = thread_idx()
let lo = (a[t] & 15) | ((a[t] >> 4) << 4)
let x = lo ^ 3
tile[t] = f32(x)
sync_threads()
b[t] = tile[t]
end
"#;
    let file = tmp.path().join("k_fmt.mora");
    std::fs::write(&file, raw_fmt).expect("write raw mora");

    let fmt1 = Command::new(mora_bin)
        .args(["fmt", file.to_str().expect("utf8 path")])
        .output()
        .expect("run mora fmt");
    assert!(
        fmt1.status.success(),
        "mora fmt failed: {}",
        String::from_utf8_lossy(&fmt1.stderr)
    );
    let formatted = std::fs::read_to_string(&file).expect("read formatted");
    let check_fmt = Command::new(mora_bin)
        .args(["fmt", "--check", file.to_str().expect("utf8 path")])
        .output()
        .expect("run mora fmt --check");
    assert!(
        check_fmt.status.success(),
        "mora fmt --check must be idempotent:\n{formatted}\n{}",
        String::from_utf8_lossy(&check_fmt.stderr)
    );
    check_source(&formatted).expect("formatted kernel must still parse and typecheck cleanly");

    // 3. Static HM rejections.
    let bad_dtype = r#"
kernel takes_f16(x: buf<f16>)
  let i = global_id()
  x[i] = f32(1)
end
let wrong = Tensor::zeros([4], "f32")
takes_f16.launch(1, 4, wrong)
"#;
    assert!(
        check_source(bad_dtype).is_err(),
        "passing Tensor<f32> to buf<f16> must fail HM"
    );

    let bad_grid = r#"
kernel k1(x: buf<f32>)
  let i = global_id()
  x[i] = f32(1)
end
let buf = Tensor::zeros([4], "f32")
k1.launch("bad", 4, buf)
"#;
    assert!(
        check_source(bad_grid).is_err(),
        "string grid dimension must fail HM"
    );

    let bad_shared_len = r#"
kernel k_bad_sh(x: buf<f32>)
  shared s: [f32; 0]
  let i = global_id()
  x[i] = f32(1)
end
"#;
    assert!(
        ParserV3::parse_witnesses(bad_shared_len).is_err(),
        "zero-length shared array must fail parse"
    );
}

#[test]
fn p5_1b_bitwise_operators_and_nested_generics_coexist_cleanly() {
    let src = r#"
-- Nested generics (`list<list<int>>`) must NOT collide with `>>` shift operator!
let matrix: list<list<int>> = [[1, 2], [3, 4]]
let a = 240 & 15
let b = 240 | 15
let c = 170 ^ 85
let d = 1 << 6
let e = 128 >> 3
let f = -16 >> 2
-- Precedence: & binds tighter than ^, which binds tighter than |, shifts bind tighter than &
let prec = 1 | 5 ^ 7 & 6
let shift_prec = 1 + 2 << 2
[a, b, c, d, e, f, prec, shift_prec, matrix[1][1]]
"#;
    let val = eval_source(src).expect("bitwise ops and nested generics should pass");
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Int(0),
                Value::Int(255),
                Value::Int(255),
                Value::Int(64),
                Value::Int(16),
                Value::Int(-4),
                Value::Int(3),
                Value::Int(12),
                Value::Int(4),
            ]
            .into()
        )
    );
}
