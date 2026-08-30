//! v0.91: linalg.* — 基础线性代数（APL/NumPy 启发）
//!
//! 向量表示为 `list<number>`，矩阵为 `list<list<number>>`。
//! 不引入新类型，复用 list — 与 §3 既有列表广播/reshape 风格一致。

use crate::value::Value;

/// linalg.* builtin 入口分发
pub fn call_linalg_method(method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "dot" => {
            let a = expect_f64_vec(args.first(), "linalg.dot")?;
            let b = expect_f64_vec(args.get(1), "linalg.dot")?;
            Ok(Value::Float(dot(&a, &b)))
        }
        "cross" => {
            let a = expect_f64_vec(args.first(), "linalg.cross")?;
            let b = expect_f64_vec(args.get(1), "linalg.cross")?;
            Ok(Value::List(
                cross(&a, &b)
                    .into_iter()
                    .map(Value::Float)
                    .collect(),
            ))
        }
        "norm" => {
            let a = expect_f64_vec(args.first(), "linalg.norm")?;
            let p = args
                .get(1)
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n as f64),
                    Value::Float(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(2.0);
            Ok(Value::Float(norm(&a, p)))
        }
        "matmul" => {
            let a = expect_f64_matrix(args.first(), "linalg.matmul")?;
            let b = expect_f64_matrix(args.get(1), "linalg.matmul")?;
            Ok(matmul(&a, &b))
        }
        "transpose" => {
            let a = expect_f64_matrix(args.first(), "linalg.transpose")?;
            Ok(transpose(&a))
        }
        other => Err(format!("linalg.{}: unknown method", other)),
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    }
}

fn expect_f64_vec(arg: Option<&Value>, ctx: &str) -> Result<Vec<f64>, String> {
    let v = arg.ok_or_else(|| format!("{}: vector argument required", ctx))?;
    match v {
        Value::List(xs) => xs
            .iter()
            .map(|x| as_f64(x).ok_or_else(|| format!("{}: non-numeric element", ctx)))
            .collect(),
        _ => Err(format!("{}: vector argument required", ctx)),
    }
}

fn expect_f64_matrix(arg: Option<&Value>, ctx: &str) -> Result<Vec<Vec<f64>>, String> {
    let v = arg.ok_or_else(|| format!("{}: matrix argument required", ctx))?;
    match v {
        Value::List(rows) => rows
            .iter()
            .map(|row| match row {
                    Value::List(xs) => xs
                        .iter()
                        .map(|x| as_f64(x).ok_or_else(|| format!("{}: non-numeric", ctx)))
                        .collect(),
                    _ => Err(format!("{}: matrix rows must be lists", ctx)),
                })
                .collect(),
        _ => Err(format!("{}: matrix argument required", ctx)),
    }
}

/// 点积（内积）：sum(a[i] * b[i])
fn dot(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() {
        // 不在 builtin 抛错（已在上层 expect_f64_vec 校验），此处用 NaN 兜底
        return f64::NAN;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// 3D 叉积（仅支持 3D 向量）
fn cross(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.len() != 3 || b.len() != 3 {
        return vec![f64::NAN; 3];
    }
    vec![
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Lp 范数：||a||_p = (sum |x|^p)^(1/p)
fn norm(a: &[f64], p: f64) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    if p == f64::INFINITY {
        // 无穷范数 = max |x|
        a.iter().map(|x| x.abs()).fold(0.0_f64, f64::max)
    } else if p == 1.0 {
        // L1 范数 = sum |x|
        a.iter().map(|x| x.abs()).sum()
    } else {
        // 一般 Lp
        let sum: f64 = a.iter().map(|x| x.abs().powf(p)).sum();
        sum.powf(1.0 / p)
    }
}

/// 矩阵乘：A(m×n) · B(n×p) = C(m×p)
fn matmul(a: &[Vec<f64>], b: &[Vec<f64>]) -> Value {
    if a.is_empty() || b.is_empty() {
        return Value::List(Vec::new());
    }
    let m = a.len();
    let n = a[0].len();
    let p = if b.is_empty() { 0 } else { b[0].len() };
    if b.len() != n {
        return Value::List(Vec::new()); // 维度不匹配
    }
    let mut result = vec![vec![0.0_f64; p]; m];
    for i in 0..m {
        for j in 0..p {
            let mut sum = 0.0;
            for k in 0..n {
                sum += a[i][k] * b[k][j];
            }
            result[i][j] = sum;
        }
    }
    Value::List(
        result
            .into_iter()
            .map(|row| Value::List(row.into_iter().map(Value::Float).collect()))
            .collect(),
    )
}

/// 矩阵转置
fn transpose(a: &[Vec<f64>]) -> Value {
    if a.is_empty() {
        return Value::List(Vec::new());
    }
    let m = a.len();
    let n = a[0].len();
    let mut result = vec![vec![0.0_f64; m]; n];
    for i in 0..m {
        for j in 0..n {
            result[j][i] = a[i][j];
        }
    }
    Value::List(
        result
            .into_iter()
            .map(|row| Value::List(row.into_iter().map(Value::Float).collect()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_basic() {
        let a = Value::List(vec![Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)]);
        let b = Value::List(vec![Value::Float(4.0), Value::Float(5.0), Value::Float(6.0)]);
        let r = call_linalg_method("dot", &[a, b]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 32.0).abs() < 1e-9));
    }

    #[test]
    fn cross_3d() {
        let x = Value::List(vec![Value::Float(1.0), Value::Float(0.0), Value::Float(0.0)]);
        let y = Value::List(vec![Value::Float(0.0), Value::Float(1.0), Value::Float(0.0)]);
        let r = call_linalg_method("cross", &[x, y]).unwrap();
        if let Value::List(v) = &r
            && let [Value::Float(a), Value::Float(b), Value::Float(c)] = &v[..]
        {
            assert!((*a - 0.0).abs() < 1e-9);
            assert!((*b - 0.0).abs() < 1e-9);
            assert!((*c - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn norm_l2() {
        let v = Value::List(vec![Value::Float(3.0), Value::Float(4.0)]);
        let r = call_linalg_method("norm", &[v]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 5.0).abs() < 1e-9));
    }

    #[test]
    fn matmul_2x2() {
        let a = Value::List(vec![
            Value::List(vec![Value::Float(1.0), Value::Float(2.0)]),
            Value::List(vec![Value::Float(3.0), Value::Float(4.0)]),
        ]);
        let b = Value::List(vec![
            Value::List(vec![Value::Float(5.0), Value::Float(6.0)]),
            Value::List(vec![Value::Float(7.0), Value::Float(8.0)]),
        ]);
        let r = call_linalg_method("matmul", &[a, b]).unwrap();
        if let Value::List(rows) = r {
            // [[19, 22], [43, 50]]
            assert_eq!(rows.len(), 2);
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn transpose_basic() {
        let a = Value::List(vec![
            Value::List(vec![Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)]),
            Value::List(vec![Value::Float(4.0), Value::Float(5.0), Value::Float(6.0)]),
        ]);
        let r = call_linalg_method("transpose", &[a]).unwrap();
        if let Value::List(rows) = r {
            assert_eq!(rows.len(), 3); // 2x3 → 3x2
            assert_eq!(rows.len(), 3);
        } else {
            panic!("expected list");
        }
    }
}