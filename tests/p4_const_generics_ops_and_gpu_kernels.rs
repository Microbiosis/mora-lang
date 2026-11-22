//! P4-1 Integration & Live Hardware Verification Suite:
//! - P4-1A: Compile-time Const Generics (`struct Linear<const IN: int, const OUT: int>`, Tensor shape propagation, runtime `int` access in `impl`, static HM mismatch rejections)
//! - P4-1A: Trait-driven Operator Overloading (`Add`/`Sub`/`Mul`/`Div`/`Mod`/`Pow`/`MatMul`/`Neg`/`Eq` on stackful VM frames + static HM errors)
//! - P4-1B: First-class Source-Level GPU `kernel` blocks (typed IR, PTX 7.0 + SPIR-V 1.0 + MSL + CPU emitters, `MORABC02` round-trip, live RTX CUDA & Vulkan 0-H2D/0-D2H VRAM chaining, `f64` PTX, `while`/`for` loop reduction, static DType/Device checks, and `mora fmt` formatting).

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

#[test]
fn p4_1a_const_generics_tensor_shape_and_runtime_values() {
    let ok_src = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
  bias: Tensor<f32, [OUT], cpu>
end

impl<const IN: int, const OUT: int> Linear<IN, OUT>
  task forward<B>(self, x: Tensor<f32, [B, IN], cpu>): Tensor<f32, [B, OUT], cpu>
    return (x @ self.weight) + self.bias
  end

  task dims(self): list<int>
    return [IN, OUT]
  end
end

let w = Tensor::from_list([[1.0, 0.0, 2.0], [0.0, 1.0, -1.0], [2.0, 0.0, 1.0], [0.0, 2.0, 0.5]], "f32")
let b = Tensor::from_list([0.5, -0.5, 1.0], "f32")
let layer: Linear<4, 3> = Linear<4, 3>(w, b)
let inferred = Linear(w, b)
let x = Tensor::from_list([[1.0, 2.0, 3.0, 4.0], [0.0, 1.0, 0.0, 2.0]], "f32")
let y1: Tensor<f32, [2, 3], cpu> = layer.forward(x)
let y2: Tensor<f32, [2, 3], cpu> = inferred.forward(x)
let d1 = layer.dims()
let d2 = inferred.dims()
assert_eq(d1[0], 4)
assert_eq(d1[1], 3)
assert_eq(d2[0], 4)
assert_eq(d2[1], 3)
[y1.to_list(), y2.to_list(), d1]
"#;

    let val = eval_source(ok_src).expect("const generic Linear<4, 3> should check and run");
    let Value::List(items) = val else {
        panic!("expected list, got {val}");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0], items[1],
        "explicit and shape-inferred Linear must match"
    );
    // Row 0: [1,2,3,4] @ w + [0.5,-0.5,1.0] = [1+6+0.5, 2+8-0.5, 2-2+3+2+1] = [7.5, 9.5, 6.0]
    let Value::List(rows) = &items[0] else {
        panic!("expected 2D list output");
    };
    let Value::List(r0) = &rows[0] else {
        panic!("expected row 0");
    };
    let as_f64 = |v: &Value| match v {
        Value::Float(f) => *f,
        Value::Scalar(s) => s.as_f64(),
        other => panic!("expected float, got {other}"),
    };
    assert!((as_f64(&r0[0]) - 7.5).abs() < 1e-4);
    assert!((as_f64(&r0[1]) - 9.5).abs() < 1e-4);
    assert!((as_f64(&r0[2]) - 6.0).abs() < 1e-4);

    // 1. Static compile-time rejection: explicit const arg mismatch against field tensor shape.
    let bad_explicit = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
  bias: Tensor<f32, [OUT], cpu>
end
let w = Tensor::zeros([4, 3], "f32")
let b = Tensor::zeros([3], "f32")
let layer = Linear<4, 5>(w, b)
layer
"#;
    assert!(
        check_source(bad_explicit).is_err(),
        "Linear<4, 5> with [4, 3] weight must fail static HM check"
    );

    // 2. Static compile-time rejection: shape-inferred const conflict between weight and bias.
    let bad_inferred = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
  bias: Tensor<f32, [OUT], cpu>
end
let w = Tensor::zeros([4, 3], "f32")
let b = Tensor::zeros([7], "f32")
let layer = Linear(w, b)
layer
"#;
    assert!(
        check_source(bad_inferred).is_err(),
        "Linear(w:[4,3], b:[7]) must fail static HM check"
    );

    // 3. Static compile-time rejection: annotation const mismatch (`Linear<4, 5>` vs `Linear<4, 3>`).
    let bad_anno = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
  bias: Tensor<f32, [OUT], cpu>
end
let w = Tensor::zeros([4, 3], "f32")
let b = Tensor::zeros([3], "f32")
let layer: Linear<4, 5> = Linear<4, 3>(w, b)
layer
"#;
    assert!(
        check_source(bad_anno).is_err(),
        "Linear<4, 3> assigned to Linear<4, 5> must fail static HM check"
    );

    // 4. Static compile-time rejection: method call tensor shape mismatch (`[2, 9]` vs `[B, 4]`).
    let bad_forward = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
  bias: Tensor<f32, [OUT], cpu>
end
impl<const IN: int, const OUT: int> Linear<IN, OUT>
  task forward<B>(self, x: Tensor<f32, [B, IN], cpu>): Tensor<f32, [B, OUT], cpu>
    return (x @ self.weight) + self.bias
  end
end
let w = Tensor::zeros([4, 3], "f32")
let b = Tensor::zeros([3], "f32")
let layer = Linear<4, 3>(w, b)
let bad_x = Tensor::zeros([2, 9], "f32")
layer.forward(bad_x)
"#;
    assert!(
        check_source(bad_forward).is_err(),
        "layer.forward([2, 9]) on Linear<4, 3> must fail static HM check"
    );

    // 5. Static compile-time rejection: non-integer argument in const generic slot.
    let bad_const_kind = r#"
struct Linear<const IN: int, const OUT: int>
  weight: Tensor<f32, [IN, OUT], cpu>
end
let w = Tensor::zeros([4, 3], "f32")
let layer = Linear<string, 3>(w)
layer
"#;
    assert!(
        check_source(bad_const_kind).is_err(),
        "non-integer type in const generic slot must fail static HM check"
    );

    // 6. Enum const generics: explicit unit variant, explicit payload variant, shape-inferred
    // payload variant, and runtime reading of `N` inside `impl<const N: int> Packet<N>`.
    let enum_ok = r#"
enum Packet<const N: int>
  Empty
  Data(Tensor<f32, [N], cpu>)
end

impl<const N: int> Packet<N>
  task width(self): int
    return N
  end
end

let t4 = Tensor::from_list([1.0, 2.0, 3.0, 4.0], "f32")
let p_empty: Packet<6> = Packet<6>::Empty
let p_explicit: Packet<4> = Packet<4>::Data(t4)
let p_inferred = Packet::Data(t4)
assert_eq(p_empty.width(), 6)
assert_eq(p_explicit.width(), 4)
assert_eq(p_inferred.width(), 4)
[p_empty.width(), p_explicit.width(), p_inferred.width()]
"#;
    let eval = eval_source(enum_ok).expect("const generic enum Packet<N> should check and run");
    assert_eq!(
        eval,
        Value::List(vec![Value::Int(6), Value::Int(4), Value::Int(4)].into())
    );

    // 7. Static compile-time rejection on enum const generic shape mismatch (`Packet<8>::Data([4])`).
    let bad_enum = r#"
enum Packet<const N: int>
  Empty
  Data(Tensor<f32, [N], cpu>)
end
let t4 = Tensor::zeros([4], "f32")
let p = Packet<8>::Data(t4)
p
"#;
    assert!(
        check_source(bad_enum).is_err(),
        "Packet<8>::Data([4]) must fail static HM check"
    );
}

#[test]
fn p4_1a_trait_operator_overloading_full_suite_and_static_errors() {
    let ok_src = r#"
struct Vec2
  x: float
  y: float
end

impl Add for Vec2
  task add(self, rhs: Vec2): Vec2
    return Vec2(self.x + rhs.x, self.y + rhs.y)
  end
end

impl Sub for Vec2
  task sub(self, rhs: Vec2): Vec2
    return Vec2(self.x - rhs.x, self.y - rhs.y)
  end
end

impl Mul for Vec2
  task mul(self, rhs: float): Vec2
    return Vec2(self.x * rhs, self.y * rhs)
  end
end

impl Div for Vec2
  task div(self, rhs: float): Vec2
    return Vec2(self.x / rhs, self.y / rhs)
  end
end

impl Mod for Vec2
  task rem(self, rhs: float): Vec2
    return Vec2(self.x % rhs, self.y % rhs)
  end
end

impl Pow for Vec2
  task pow(self, rhs: float): Vec2
    return Vec2(self.x ** rhs, self.y ** rhs)
  end
end

impl MatMul for Vec2
  task matmul(self, rhs: Vec2): float
    return self.x * rhs.x + self.y * rhs.y
  end
end

impl Neg for Vec2
  task neg(self): Vec2
    return Vec2(-self.x, -self.y)
  end
end

impl Eq for Vec2
  task eq(self, rhs: Vec2): bool
    let dx = self.x - rhs.x
    let dy = self.y - rhs.y
    return (dx * dx + dy * dy) < 0.0001
  end
end

struct Plain
  v: int
end

let a = Vec2(3.0, 4.0)
let b = Vec2(1.0, 2.0)
let sum: Vec2 = a + b
let diff: Vec2 = a - b
let scaled: Vec2 = a * 2.0
let halved: Vec2 = scaled / 2.0
let modded: Vec2 = Vec2(7.0, 9.0) % 4.0
let sq: Vec2 = b ** 2.0
let dot: float = a @ b
let neg_a: Vec2 = -a

assert_eq(sum.x, 4.0)
assert_eq(sum.y, 6.0)
assert_eq(diff.x, 2.0)
assert_eq(diff.y, 2.0)
assert_eq(halved.x, 3.0)
assert_eq(modded.x, 3.0)
assert_eq(modded.y, 1.0)
assert_eq(sq.x, 1.0)
assert_eq(sq.y, 4.0)
assert_eq(dot, 11.0)
assert_eq(neg_a.x, -3.0)
assert_eq(neg_a.y, -4.0)
assert(a == Vec2(3.00001, 4.0))
assert(a != b)
assert(Plain(5) == Plain(5))
assert(Plain(5) != Plain(6))
dot
"#;

    let res = eval_source(ok_src).expect("all 9 overloaded operators should check and run");
    assert_eq!(res, Value::Float(11.0));

    // Static HM error: wrong RHS type for overloaded operator (`Vec2 + "bad"`).
    let bad_rhs = r#"
struct Vec2
  x: float
  y: float
end
impl Add for Vec2
  task add(self, rhs: Vec2): Vec2
    return Vec2(self.x + rhs.x, self.y + rhs.y)
  end
end
let a = Vec2(1.0, 2.0)
a + "oops"
"#;
    assert!(
        check_source(bad_rhs).is_err(),
        "Vec2 + string must fail static HM check"
    );

    // Static HM error: unimplemented operator on nominal struct.
    let missing_op = r#"
struct Boxed
  v: int
end
let a = Boxed(1)
let b = Boxed(2)
a + b
"#;
    assert!(
        check_source(missing_op).is_err(),
        "Boxed + Boxed without impl Add must fail static HM check"
    );
}

#[test]
fn p4_1b_source_level_gpu_kernel_cpu_codegen_and_morabc02() {
    let src = r#"
kernel saxpy(a: f32, x: buf<f32>, y: buf<f32>, out: buf<f32>, n: i32)
  let idx = global_id()
  if idx < n then
    out[idx] = a * x[idx] + y[idx]
  end
end

kernel row_sum(mat: buf<f32>, out: buf<f32>, rows: i32, cols: i32)
  let r = global_id()
  if r < rows then
    let acc: f32 = 0.0
    let c: i32 = 0
    while c < cols
      acc = acc + mat[r * cols + c]
      c = c + 1
    end
    out[r] = acc
  end
end

kernel act_chain(x: buf<f32>, out: buf<f32>, n: i32)
  let idx = global_id()
  if idx < n then
    let v = max(x[idx], 0.0)
    out[idx] = tanh(sqrt(v + 1.0)) + exp(-v)
  end
end

let ptx = saxpy.emit_ptx()
let spv = saxpy.emit_spirv_words(64)
let msl = saxpy.emit_msl()

let x = Tensor::from_list([1.0, 2.0, 3.0, 4.0], "f32")
let y = Tensor::from_list([10.0, 20.0, 30.0, 40.0], "f32")
let out = Tensor::zeros([4], "f32")
saxpy.launch(1, 64, 2.5f32, x, y, out, 4)

let mat = Tensor::from_list([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], "f32")
let rsum = Tensor::zeros([2], "f32")
row_sum.launch(1, 32, mat, rsum, 2, 3)

let act_in = Tensor::from_list([-2.0, 0.0, 3.0], "f32")
let act_out = Tensor::zeros([3], "f32")
act_chain.launch(1, 32, act_in, act_out, 3)

struct KernelTestOut
  ptx: string
  spv: list<int>
  msl: string
  saxpy: list<any>
  rsum: list<any>
  act: list<any>
end

KernelTestOut(ptx, spv, msl, out.to_list(), rsum.to_list(), act_out.to_list())
"#;

    check_source(src).expect("kernel program should pass static HM check");
    let val = eval_source(src).expect("execute kernel program");
    let Value::StructInstance {
        fields: res_map, ..
    } = val
    else {
        panic!("expected struct result");
    };

    // Also verify MORABC02 `.morac` AOT bytecode round-trip via `mora build --bytecode` + `mora run`!
    let tmp = TempDir::new("p4_1b_morac");
    let script_path = tmp.path().join("kernel_test.mora");
    let morac_path = tmp.path().join("kernel_test.morac");
    let morac_src = r#"
kernel scale_add(a: f32, x: buf<f32>, out: buf<f32>, n: i32)
  let i = global_id()
  if i < n then
    out[i] = a * x[i] + 1.0
  end
end
let x = Tensor::from_list([2.0, 4.0], "f32")
let out = Tensor::zeros([2], "f32")
scale_add.launch(1, 32, 3.0f32, x, out, 2)
print(out.to_list())
"#;
    std::fs::write(&script_path, morac_src).expect("write morac script");
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
    let run_out = Command::new(mora_bin)
        .args([morac_path.to_str().expect("utf8 path")])
        .output()
        .expect("run compiled .morac");
    assert!(
        run_out.status.success(),
        "mora run .morac failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    let run_stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(
        run_stdout.contains("7") && run_stdout.contains("13"),
        "expected [7, 13] from .morac kernel execution, got: {run_stdout}"
    );

    // 1. Validate PTX ISA 7.0 pure ASCII output
    let Value::String(ptx) = &res_map["ptx"] else {
        panic!("expected ptx string");
    };
    assert!(ptx.is_ascii(), "PTX must be strict pure ASCII for ptxas");
    assert!(ptx.contains(".version 7.0"));
    assert!(ptx.contains(".target sm_50"));
    assert!(ptx.contains(".visible .entry saxpy("));

    // 2. Validate SPIR-V 1.0 binary word stream structure
    let Value::List(spv_vals) = &res_map["spv"] else {
        panic!("expected spirv words list");
    };
    let words: Vec<u32> = spv_vals
        .iter()
        .map(|v| match v {
            Value::Int(n) => *n as u32,
            other => panic!("expected int word, got {other}"),
        })
        .collect();
    assert_eq!(words[0], 0x0723_0203, "SPIR-V magic");
    assert_eq!(words[1], 0x0001_0000, "SPIR-V 1.0");
    let mut pc = 5usize;
    while pc < words.len() {
        let wc = (words[pc] >> 16) as usize;
        assert!(wc >= 1);
        pc += wc;
    }
    assert_eq!(pc, words.len(), "valid SPIR-V instruction boundaries");

    // 3. Validate Metal MSL output
    let Value::String(msl) = &res_map["msl"] else {
        panic!("expected msl string");
    };
    assert!(msl.contains("#include <metal_stdlib>"));
    assert!(msl.contains("kernel void saxpy("));

    // 4. Validate exact CPU numerical outputs
    let to_f64s = |v: &Value| -> Vec<f64> {
        let Value::List(xs) = v else {
            panic!("expected list, got {v}");
        };
        xs.iter()
            .map(|x| match x {
                Value::Float(f) => *f,
                Value::Scalar(s) => s.as_f64(),
                other => panic!("expected float, got {other}"),
            })
            .collect()
    };
    let saxpy_out = to_f64s(&res_map["saxpy"]);
    assert_eq!(saxpy_out, vec![12.5, 25.0, 37.5, 50.0]);
    let rsum_out = to_f64s(&res_map["rsum"]);
    assert_eq!(rsum_out, vec![6.0, 15.0]);
    let act_out = to_f64s(&res_map["act"]);
    for (&got, &inp) in act_out.iter().zip([-2.0_f64, 0.0, 3.0].iter()) {
        let v = inp.max(0.0);
        let expected = (v + 1.0).sqrt().tanh() + (-v).exp();
        assert!((got - expected).abs() < 1e-4, "got {got} vs {expected}");
    }

    // 5. Static HM checks on kernel launches: DType mismatch, cross-device mismatch, wrong arity.
    let bad_dtype = r#"
kernel k(x: buf<f32>, n: i32)
  let i = global_id()
  if i < n then
    x[i] = x[i] + 1.0
  end
end
let t = Tensor::zeros([4], "f64")
k.launch(1, 4, t, 4)
"#;
    assert!(
        check_source(bad_dtype).is_err(),
        "passing Tensor<f64> to buf<f32> must fail static HM check"
    );

    let bad_cross_dev = r#"
kernel copy_k(src: buf<f32>, dst: buf<f32>, n: i32)
  let i = global_id()
  if i < n then
    dst[i] = src[i]
  end
end
let a = Tensor::zeros([4], "f32")
let b = Tensor::zeros([4], "f32", "cuda:0")
copy_k.launch(1, 4, a, b, 4)
"#;
    assert!(
        check_source(bad_cross_dev).is_err(),
        "mixing cpu and cuda:0 buffers in one kernel.launch must fail static HM check"
    );

    let bad_body = r#"
kernel broken(x: buf<f32>)
  let i: i32 = x[0]
end
broken
"#;
    assert!(
        check_source(bad_body).is_err(),
        "assigning f32 load to i32 local inside kernel must fail static HM check"
    );
}

#[test]
fn p4_1b_live_gpu_cuda_and_vulkan_zero_copy_vram_kernels() {
    let devices_val = eval_source("Tensor::devices()").expect("Tensor::devices()");
    let Value::List(dev_list) = devices_val else {
        panic!("expected device list");
    };
    let devs: Vec<String> = dev_list
        .iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    for target in ["cuda:0", "vulkan:0"] {
        if !devs.iter().any(|d| d == target) {
            continue;
        }
        let src = format!(
            r#"
kernel saxpy_vram(a: f32, x: buf<f32>, y: buf<f32>, out: buf<f32>, n: i32)
  let idx = global_id()
  if idx < n then
    out[idx] = a * x[idx] + y[idx]
  end
end

kernel relu_tanh_vram(inp: buf<f32>, out: buf<f32>, n: i32)
  let idx = global_id()
  if idx < n then
    let v = max(inp[idx], 0.0)
    out[idx] = tanh(v * 0.1) + sqrt(v + 1.0)
  end
end

kernel row_sum_vram(mat: buf<f32>, out: buf<f32>, rows: i32, cols: i32)
  let r = global_id()
  if r < rows then
    let acc: f32 = 0.0
    for c in range(cols)
      acc = acc + mat[r * cols + c]
    end
    out[r] = acc
  end
end

let dev = "{target}"
let x = Tensor::from_list([-4.0, -1.0, 2.0, 5.0, 8.0, 11.0, 14.0, 17.0], "f32").to(dev)
let y = Tensor::from_list([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], "f32").to(dev)
let mid = Tensor::zeros([8], "f32", dev)
let out = Tensor::zeros([8], "f32", dev)
let mat = Tensor::from_list([[1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]], "f32").to(dev)
let rsum = Tensor::zeros([2], "f32", dev)

let before = Tensor::device_info(dev)
saxpy_vram.launch(1, 64, 2.0f32, x, y, mid, 8)
relu_tanh_vram.launch(1, 64, mid, out, 8)
row_sum_vram.launch(1, 32, mat, rsum, 2, 4)
let after_gpu = Tensor::device_info(dev)

struct GpuRunOut
  before: dict<string, string>
  after_gpu: dict<string, string>
  still_vram: bool
  host_out: list<any>
  host_rsum: list<any>
end

let still_vram = out.is_vram_resident()
let host_out = out.to_list()
let host_rsum = rsum.to_list()
GpuRunOut(before, after_gpu, still_vram, host_out, host_rsum)
"#
        );

        let val = eval_source(&src)
            .unwrap_or_else(|e| panic!("live GPU kernel test failed on {target}: {e}"));
        let Value::StructInstance {
            fields: res_map, ..
        } = val
        else {
            panic!("expected struct result on {target}");
        };
        let get_u64 = |dict: &Value, key: &str| -> u64 {
            let Value::Dict(m) = dict else {
                panic!("expected dict");
            };
            match m.get(key) {
                Some(Value::String(s)) => s.parse().unwrap_or(0),
                _ => 0,
            }
        };
        let h2d_before = get_u64(&res_map["before"], "vram_h2d_copies");
        let d2h_before = get_u64(&res_map["before"], "vram_d2h_copies");
        let hits_before = get_u64(&res_map["before"], "vram_hits");

        let h2d_after = get_u64(&res_map["after_gpu"], "vram_h2d_copies");
        let d2h_after = get_u64(&res_map["after_gpu"], "vram_d2h_copies");
        let hits_after = get_u64(&res_map["after_gpu"], "vram_hits");

        assert_eq!(
            h2d_after - h2d_before,
            0,
            "{target}: 3 chained GPU kernels must incur 0 H2D copies"
        );
        assert_eq!(
            d2h_after - d2h_before,
            0,
            "{target}: 3 chained GPU kernels must incur 0 D2H copies prior to readback"
        );
        assert!(
            hits_after - hits_before >= 3,
            "{target}: expected >= 3 live GPU VRAM kernel hits, got {}",
            hits_after - hits_before
        );
        assert_eq!(
            res_map["still_vram"],
            Value::Bool(true),
            "{target}: out must stay VRAM-resident"
        );

        let to_f64s = |v: &Value| -> Vec<f64> {
            let Value::List(xs) = v else {
                panic!("expected list");
            };
            xs.iter()
                .map(|x| match x {
                    Value::Float(f) => *f,
                    Value::Scalar(s) => s.as_f64(),
                    other => panic!("expected float, got {other}"),
                })
                .collect()
        };
        let got_out = to_f64s(&res_map["host_out"]);
        let xs = [-4.0_f64, -1.0, 2.0, 5.0, 8.0, 11.0, 14.0, 17.0];
        let ys = [1.0_f64, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        for i in 0..8 {
            let m = 2.0 * xs[i] + ys[i];
            let v = m.max(0.0);
            let exp_val = (v * 0.1).tanh() + (v + 1.0).sqrt();
            assert!(
                (got_out[i] - exp_val).abs() < 1e-3,
                "{target}[{i}]: got {} vs expected {exp_val}",
                got_out[i]
            );
        }
        let got_rsum = to_f64s(&res_map["host_rsum"]);
        assert!((got_rsum[0] - 10.0).abs() < 1e-4, "{target}: row 0 sum");
        assert!((got_rsum[1] - 26.0).abs() < 1e-4, "{target}: row 1 sum");
    }

    // Live CUDA f64 kernel test (exercises native PTX f64 + range-reduced exp/tanh on RTX GPU!)
    if devs.iter().any(|d| d == "cuda:0") {
        let f64_src = r#"
kernel exp_f64(x: buf<f64>, out: buf<f64>, n: i32)
  let i = global_id()
  if i < n then
    out[i] = exp(x[i]) + tanh(x[i])
  end
end

let x = Tensor::from_list([-1.5, 0.0, 0.5, 2.0], "f64").to("cuda:0")
let out = Tensor::zeros([4], "f64", "cuda:0")
exp_f64.launch(1, 32, x, out, 4)
out.to_list()
"#;
        let val = eval_source(f64_src).expect("live CUDA f64 kernel");
        let Value::List(xs) = val else {
            panic!("expected list");
        };
        for (i, &inp) in [-1.5_f64, 0.0, 0.5, 2.0].iter().enumerate() {
            let Value::Float(got) = xs[i] else {
                panic!("expected f64");
            };
            let exp_val = inp.exp() + inp.tanh();
            assert!(
                (got - exp_val).abs() < 1e-6,
                "cuda f64[{i}]: got {got} vs {exp_val}"
            );
        }
    }
}

#[test]
fn p4_1_mora_fmt_preserves_const_generics_ops_and_kernels() {
    let raw = r#"
struct Linear<const IN: int, const OUT: int>
weight: Tensor<f32, [IN, OUT], cpu>
end

kernel saxpy(a: f32, x: buf<f32>, out: buf<f32>, n: i32)
let idx = global_id()
if idx < n then
out[idx] = a * x[idx]
end
end
"#;
    let tmp = TempDir::new("p4_1_fmt");
    let file = tmp.path().join("p4_fmt.mora");
    std::fs::write(&file, raw).expect("write raw mora");
    let mora_bin = env!("CARGO_BIN_EXE_mora");

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
    assert!(
        formatted.contains("  weight: Tensor<f32, [IN, OUT], cpu>"),
        "expected indented struct field:\n{formatted}"
    );
    assert!(
        formatted.contains("  let idx = global_id()"),
        "expected indented kernel body:\n{formatted}"
    );
    assert!(
        formatted.contains("    out[idx] = a * x[idx]"),
        "expected doubly indented if body inside kernel:\n{formatted}"
    );

    let check_fmt = Command::new(mora_bin)
        .args(["fmt", "--check", file.to_str().expect("utf8 path")])
        .output()
        .expect("run mora fmt --check");
    assert!(
        check_fmt.status.success(),
        "mora fmt --check must be idempotent:\n{}",
        String::from_utf8_lossy(&check_fmt.stderr)
    );
    check_source(&formatted).expect("formatted P4-1 source must check cleanly");
}
