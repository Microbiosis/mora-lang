//! Wave 3 — Stage 3C comprehensive end-to-end verification suite:
//! - Nominal `struct` & generic `enum` definitions, `@tag(...)` compile-time metadata & JSON
//!   projection (`omitempty`, aliases, `-` skip), `schema()` / `tags()` / `fields()` / `from_dict()`,
//!   and typed `match` destructuring.
//! - Source-level `trait` (supertraits + default methods), `impl Trait for Target` / inherent `impl`,
//!   built-in type trait impls, and static `dyn Trait` coercion checking.
//! - Python-style default parameters & named keyword arguments on `task`, closures, `impl` methods,
//!   struct constructors, and `Tensor::*` constructors.
//! - Multi-dimensional zero-copy `TensorSlice` (`t[0..2, :, -1]`, `t[..., None]`, `t[1:5:2]`) with
//!   static shape & step verification.
//! - Reverse-mode automatic differentiation (`grad`, `value_and_grad`, `argnums`, `no_grad ... end`),
//!   finite-difference gradient verification on neural layers / activations / normalizations / slices,
//!   and static rejection of in-place `tensor_mutate` inside differentiated functions.

use std::collections::HashMap;
use std::sync::Arc;

use mora::flow::json::JsonValue as JsonVal;
use mora::lsp::providers::completion_v3;
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

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

#[test]
fn stage3c_nominal_structs_tags_defaults_reflection_and_patterns() {
    let src = r#"
struct Config
  host: string = "localhost" @tag(json="server_host")
  port: int = 8080 @tag(json="port,omitempty")
  secret: string = "tok" @tag(json="-")
end

let c1 = Config()
let c2 = Config(port=0, host="0.0.0.0")
let c3 = Config::from_dict(json.parse("{\"host\":\"127.0.0.1\",\"port\":9000}"))

let matched = match c3 with
  Config { host: h, port: p, .. } -> "host=" + h + ":" + str(p)
end

[c1.host, str(c1.port), c2.json(), str(Config::fields().len()), matched, c1.tags().get("host").get("json")]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("localhost".to_string()));
    assert_eq!(v[1], Value::String("8080".to_string()));
    assert_eq!(
        v[2],
        Value::String("{\"server_host\":\"0.0.0.0\"}".to_string())
    );
    assert_eq!(v[3], Value::String("3".to_string()));
    assert_eq!(v[4], Value::String("host=127.0.0.1:9000".to_string()));
    assert_eq!(v[5], Value::String("server_host".to_string()));

    // Static checks on struct errors
    assert_typecheck_fails(
        "struct Pt\n  x: int\n  y: int\nend\nlet p = Pt(1)\n",
        "missing required field",
    );
    assert_typecheck_fails(
        "struct Pt\n  x: int\nend\nlet p = Pt(z=1)\n",
        "unknown field",
    );
    assert_typecheck_fails("struct Pt\n  x: int = \"bad\"\nend\n", "default value");
    assert_typecheck_fails(
        "struct Pt\n  x: int @tag(json=\"a,bogus\")\nend\n",
        "option",
    );
}

#[test]
fn stage3c_nominal_generic_enums_and_pattern_matching() {
    let src = r#"
enum Maybe<T>
  Just(T)
  Nothing
end

task unwrap_or(m: Maybe<int>, fallback: int): int
  match m with
    Maybe::Just(v) -> v + 1
    Maybe::Nothing -> fallback
  end
end

let a = Maybe::Just(41)
let b = Maybe::Nothing
[unwrap_or(a, 0), unwrap_or(b, 7), Maybe::variants().len()]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(vec![Value::Int(42), Value::Int(7), Value::Int(2)].into())
    );

    assert_typecheck_fails(
        "enum E\n  A(int)\nend\nlet x = E::A(1, 2)\n",
        "expected 1 arguments, got 2",
    );
    assert_typecheck_fails(
        "enum E\n  A(int)\nend\nlet x = E::A(1)\nmatch x with\n  E::A(a, b) -> a\nend\n",
        "expected 1 arguments, got 2",
    );
}

#[test]
fn stage3c_source_traits_supertraits_default_methods_and_dyn_dispatch() {
    let src = r#"
trait Named
  task label(self): string
end

trait Describable: Named
  task describe(self): string
    return "item:" + self.label()
  end
end

struct Layer
  name: string
  width: int
end

impl Layer
  task new_square(n: int): Layer
    return Layer(name="sq", width=n)
  end
end

impl Named for Layer
  task label(self): string
    return self.name
  end
end

impl Describable for Layer
end

impl Named for int
  task label(self): string
    return str(self)
  end
end

let l = Layer::new_square(64)
let d: dyn Describable = l
let n = (42).label()
let c = (7 as dyn Named).label()
[d.describe(), n, c, str(l.width)]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::String("item:sq".to_string()),
                Value::String("42".to_string()),
                Value::String("7".to_string()),
                Value::String("64".to_string()),
            ]
            .into()
        )
    );

    // Unimplemented trait coercion must be statically rejected.
    assert_typecheck_fails(
        "trait Foo\n  task bar(self): int\nend\nlet x: dyn Foo = 1\n",
        "does not implement trait",
    );
    assert_typecheck_fails(
        "trait Foo\n  task bar(self): int\nend\nlet x = 1 as dyn Foo\n",
        "does not implement trait",
    );
    // Missing required method in impl must be statically rejected.
    assert_typecheck_fails(
        "trait Foo\n  task bar(self): int\nend\nstruct S\n  x: int\nend\nimpl Foo for S\nend\n",
        "missing method",
    );
    // Cyclic supertrait must be statically rejected.
    assert_typecheck_fails("trait A: B\nend\ntrait B: A\nend\n", "cyclic");
}

#[test]
fn stage3c_default_params_and_named_keyword_arguments() {
    let src = r#"
task linear_combo(a: int, b: int = 10, scale: int = 2): int
  return (a + b) * scale
end

let f = fn(x: int, bias: int = 5) x + bias end

[linear_combo(3), linear_combo(3, scale=4), linear_combo(scale=3, b=2, a=1), f(10), f(bias=20, x=5)]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::Int(26),
                Value::Int(52),
                Value::Int(9),
                Value::Int(15),
                Value::Int(25),
            ]
            .into()
        )
    );

    let kw_err =
        ParserV3::compile("task g(a: int, b: int = 1): int\n  return a + b\nend\ng(1, bogus=2)\n")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| panic!("expected compile/typeck rejection for unknown keyword"));
    assert!(
        kw_err.contains("unknown keyword argument"),
        "unexpected error: {kw_err}"
    );
    let dup_err =
        ParserV3::compile("task g(a: int, b: int = 1): int\n  return a + b\nend\ng(1, a=2)\n")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| panic!("expected compile/typeck rejection for duplicate keyword"));
    assert!(
        dup_err.contains("duplicate") || dup_err.contains("multiple"),
        "unexpected error: {dup_err}"
    );
    let miss_err =
        ParserV3::compile("task g(a: int, b: int = 1): int\n  return a + b\nend\ng(b=2)\n")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| {
                panic!("expected compile/typeck rejection for missing required param")
            });
    assert!(
        miss_err.contains("missing required"),
        "unexpected error: {miss_err}"
    );
    assert_typecheck_fails(
        "task g(a: int = \"bad\"): int\n  return a\nend\n",
        "mismatch",
    );
}

#[test]
fn stage3c_multidim_zero_copy_tensor_slicing_and_static_checks() {
    let src = r#"
let t = Tensor::arange(0, 12, dtype="f64").reshape([3, 4])
let s1 = t[1..3, 1:4:2]
let s2 = t[..., None]
let s3 = t[-1, ::-1]
let s4 = Tensor::arange(0, 5, dtype="f64")[::-1]
[str(s1.shape()), str(s2.shape()), str(s3.shape()), str(s1.to_list()), str(s3.to_list()), str(s4.to_list())]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("[2, 2]".to_string()));
    assert_eq!(v[1], Value::String("[3, 4, 1]".to_string()));
    assert_eq!(v[2], Value::String("[4]".to_string()));
    assert_eq!(v[3], Value::String("[[5.0, 7.0], [9.0, 11.0]]".to_string()));
    assert_eq!(v[4], Value::String("[11.0, 10.0, 9.0, 8.0]".to_string()));
    assert_eq!(v[5], Value::String("[4.0, 3.0, 2.0, 1.0, 0.0]".to_string()));

    // Static shape preservation through TensorSlice
    assert_typecheck_ok(
        "let t = Tensor::zeros([3, 4])\nlet v: Tensor<f64, [2, 2], cpu> = t[0..2, 1..3]\n",
    );
    assert_typecheck_fails("let t = Tensor::zeros([3, 4])\nlet v = t[::0]\n", "step");
    assert_typecheck_fails(
        "let t = Tensor::zeros([3, 4])\nlet v = t[0, 0, 0]\n",
        "slice axes",
    );
    assert_typecheck_fails(
        "let t = Tensor::zeros([3, 4])\nlet v = t[..., ...]\n",
        "ellipsis",
    );
}

#[test]
fn stage3c_reverse_mode_autograd_grad_value_and_grad_and_no_grad() {
    // 1. Scalar differentiation + multi-arg value_and_grad
    let scalar_src = r#"
task poly(x: float, y: float): float
  return (x ** 2.0) * y + 3.0 * y
end

let vg = value_and_grad(poly, argnums=[0, 1])
let res = vg(2.0, 5.0)
let val = res[0]
let grads = res[1]

let detached = grad(fn(x: float)
  let c = no_grad
    x * 10.0
  end
  return x * 2.0 + c
end)(3.0)

[val, grads[0], grads[1], detached]
"#;
    let scalar_out = compile_check_and_run(scalar_src);
    let Value::List(items) = scalar_out else {
        panic!("expected list, got {scalar_out:?}");
    };
    let sv = items.to_vec();
    assert_eq!(sv[0], Value::Float(35.0)); // 4*5 + 15
    assert_eq!(sv[1], Value::Float(20.0)); // d/dx = 2*x*y = 20
    assert_eq!(sv[2], Value::Float(7.0)); // d/dy = x^2 + 3 = 7
    assert_eq!(sv[3], Value::Float(2.0)); // no_grad detached c

    // 2. Neural layer (matmul @, bias, activation, slice, sum) verified against finite differences
    let nn_src = r#"
struct Linear
  w: Tensor<f64, [2, 2], cpu>
  b: Tensor<f64, [1, 2], cpu>
end

impl Linear
  task forward(self, x: Tensor<f64, [1, 2], cpu>): Tensor<f64, [1, 2], cpu>
    return (x @ self.w + self.b).silu()
  end
end

let layer = Linear(w=tensor([[1.0, -0.5], [0.25, 0.75]]), b=tensor([[0.1, -0.2]]))
let x = tensor([[1.5, -2.0]])

task loss_fn(w: Tensor<f64, [2, 2], cpu>, b: Tensor<f64, [1, 2], cpu>): float
  let m = Linear(w=w, b=b)
  let h = m.forward(x)
  let sliced = h[0, :]
  return (sliced * sliced).sum()
end

let res = value_and_grad(loss_fn, argnums=[0, 1])(layer.w, layer.b)
let l0 = res[0]
let gw = res[1][0]
let gb = res[1][1]

let eps = 1e-6
let w_p = tensor([[1.0 + eps, -0.5], [0.25, 0.75]])
let w_m = tensor([[1.0 - eps, -0.5], [0.25, 0.75]])
let fd_w00 = (loss_fn(w_p, layer.b) - loss_fn(w_m, layer.b)) / (2.0 * eps)

let b_p = tensor([[0.1 + eps, -0.2]])
let b_m = tensor([[0.1 - eps, -0.2]])
let fd_b00 = (loss_fn(layer.w, b_p) - loss_fn(layer.w, b_m)) / (2.0 * eps)

[l0, gw[0][0], fd_w00, gb[0][0], fd_b00]
"#;
    let nn_out = compile_check_and_run(nn_src);
    let Value::List(nn_items) = nn_out else {
        panic!("expected list, got {nn_out:?}");
    };
    let nv = nn_items.to_vec();
    let gw00 = match nv[1] {
        Value::Float(f) => f,
        _ => panic!("expected float gw[0][0], got {:?}", nv[1]),
    };
    let fd_w00 = match nv[2] {
        Value::Float(f) => f,
        _ => panic!("expected float fd_w00, got {:?}", nv[2]),
    };
    let gb00 = match nv[3] {
        Value::Float(f) => f,
        _ => panic!("expected float gb[0][0], got {:?}", nv[3]),
    };
    let fd_b00 = match nv[4] {
        Value::Float(f) => f,
        _ => panic!("expected float fd_b00, got {:?}", nv[4]),
    };
    assert!(
        (gw00 - fd_w00).abs() < 1e-5,
        "autograd dL/dw[0,0]={gw00} vs finite-diff={fd_w00}"
    );
    assert!(
        (gb00 - fd_b00).abs() < 1e-5,
        "autograd dL/db[0,0]={gb00} vs finite-diff={fd_b00}"
    );

    // 3. Static safety checks for grad / value_and_grad
    assert_typecheck_fails(
        "task bad(x: Tensor<f64, [2], cpu>): float\n  x.fill_(1.0)\n  return x.sum()\nend\ngrad(bad)\n",
        "tensor_mutate",
    );
    assert_typecheck_fails(
        "task bad(x: int): float\n  return 1.0\nend\ngrad(bad)\n",
        "floating-point",
    );
    assert_typecheck_fails(
        "task bad(x: Tensor<f64, [2], cpu>): Tensor<f64, [2], cpu>\n  return x\nend\ngrad(bad)\n",
        "scalar",
    );
}

#[test]
fn stage3c_lsp_completion_exposes_new_keywords() {
    let mut docs = HashMap::new();
    let uri = "file:///stage3c.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let x = 1\n".to_string(),
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
    for expected in ["trait", "impl", "no_grad", "grad", "value_and_grad"] {
        assert!(
            labels.contains(&expected),
            "LSP completion missing Stage 3C entry {expected:?}"
        );
    }
}
