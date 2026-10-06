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
            Ok(Value::Float(dot(&a, &b)?))
        }
        "cross" => {
            let a = expect_f64_vec(args.first(), "linalg.cross")?;
            let b = expect_f64_vec(args.get(1), "linalg.cross")?;
            Ok(Value::List(
                cross(&a, &b)?.into_iter().map(Value::Float).collect(),
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
            matmul(&a, &b)
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
    let m: Vec<Vec<f64>> = match v {
        Value::List(rows) => {
            let mut out: Vec<Vec<f64>> = Vec::with_capacity(rows.len());
            for row in rows.iter() {
                let xs = match row {
                    Value::List(xs) => xs,
                    _ => return Err(format!("{}: matrix rows must be lists", ctx)),
                };
                let mut r: Vec<f64> = Vec::with_capacity(xs.len());
                for x in xs.iter() {
                    r.push(as_f64(x).ok_or_else(|| format!("{}: non-numeric", ctx))?);
                }
                out.push(r);
            }
            out
        }
        _ => return Err(format!("{}: matrix argument required", ctx)),
    };

    // v0.104.6 D330：**参差矩阵**（各行宽度不一致）必须报错。
    //
    // D142 在这一层加了「A 行数 == B 行数」的守卫，但**只**看
    // `a[0].len()` 与 `b[0].len()`（即**第一行**的宽度）——
    // 它假设矩阵是**规整**的，而 `list<list<number>>` 这个类型
    // **不表达**「每行等宽」：`[[1,2],[3]]` 的类型与 `[[1,2],[3,4]]`
    // **完全相同**。于是一条本该在输入层拦住的错误，一路穿到算术层。
    //
    // 实测（修前，**五个** panic，exit 101）：
    // ```text
    // linalg.transpose([[1,2],[3]])        → linalg.rs:186 index out of bounds: len 1, index 1
    // linalg.transpose([[1],[2,3]])        → **静默丢数据** → [[1.0, 2.0]]（第 2 行的第 2 个元素消失）
    // linalg.transpose([[1,2,3],[4,5]])    → linalg.rs:186 index out of bounds: len 2, index 2
    // linalg.matmul([[1,2],[3]],[[1],[2]]) → linalg.rs:163 index out of bounds: len 1, index 1
    // linalg.matmul([[1,2,3],[4,5]],[[1],[2],[3]]) → linalg.rs:163 index out of bounds
    // ```
    // 其中两条**不崩反而更糟**——`transpose` 按 `a[0].len()` 建结果，
    // `matmul` 按 `n = a[0].len()` 取 `a[i][k]`，于是**多出来的行元素
    // 被静默丢弃**，调用方拿到一个**行数对、列数错**的「合法」矩阵：
    // `matmul([[1,2],[3,4,5]],[[1,0],[0,1]])` → `[[1.0,2.0],[3.0,4.0]]`（第 3 列没了）。
    //
    // 为什么是**缺陷**而非「宽松接受」：
    // ① `docs/mora-spec.md:993-994` 的签名是 `list<list>` → `list<list>`，
    //    返回类型承诺**一个矩阵**，而参差输入的「结果」根本不是调用方
    //    写下的那个矩阵 —— **静默改变数据形状**（与 D325 `reshape`
    //    丢数据是同一类，D325 已定为缺陷）；
    // ② panic 直接杀进程（exit 101，无 `MoraError`、无诊断），
    //    而本项目所有其它维度问题（D142）都已走干净报错。
    //
    // 放在 `expect_f64_matrix` 而非 `matmul` / `transpose` 各自里：
    // 这是**两个函数共用的唯一入口**，守卫加一次即可同时覆盖，
    // 且 `ctx` 已带调用点名，错误能直接告诉用户是哪个函数。
    //
    // 措辞用「维度不匹配」与 D142 保持一致（同一族的同一类问题），
    // 并给出**期望宽度、实际宽度、第几行**三个可操作信息。
    if let Some(width) = m.first().map(Vec::len) {
        for (i, row) in m.iter().enumerate().skip(1) {
            if row.len() != width {
                return Err(format!(
                    "{}: 维度不匹配 —— 矩阵各行宽度必须一致（第 1 行 {width} 列，第 {} 行 {} 列）",
                    ctx,
                    i + 1,
                    row.len()
                ));
            }
        }
    }
    Ok(m)
}

/// 点积（内积）：sum(a[i] * b[i])
/// 点积
///
/// v0.104.6 D142：维度不匹配此前**静默返回 `NaN`**（注释声称「上层
/// `expect_f64_vec` 已校验」—— 实测它**只校验是不是 List + 元素是数值**，
/// 不看维度）。于是 `linalg.dot([1,2],[1,2,3])` 得 `nan`、**exit 0、零诊断**。
/// 维度不同的点积在数学上无意义，`NaN` 会顺着后续算术静默污染整个表达式。
/// 现改为明确报错。
fn dot(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!(
            "linalg.dot: 向量维度不匹配（{} 维 vs {} 维）",
            a.len(),
            b.len()
        ));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}

/// 3D 叉积（仅支持 3D 向量）
///
/// v0.104.6 D142：同 `dot` —— 非 3D 输入此前静默返回 `[NaN, NaN, NaN]`。
fn cross(a: &[f64], b: &[f64]) -> Result<Vec<f64>, String> {
    if a.len() != 3 || b.len() != 3 {
        return Err(format!(
            "linalg.cross: 只支持 3D 向量，得到 {} 维与 {} 维",
            a.len(),
            b.len()
        ));
    }
    Ok(vec![
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ])
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
///
/// v0.104.6 D142：维度不匹配此前**静默返回空列表**，注释只写「维度不匹配」
/// 却既不报错也不提示。实测 `linalg.matmul([[1,2]], [[1],[2],[3]])` 得 `[]`、
/// **exit 0、零诊断** —— 调用方无法区分「结果为空矩阵」与「参数写错了」。
/// 现改为明确报错。
fn matmul(a: &[Vec<f64>], b: &[Vec<f64>]) -> Result<Value, String> {
    if a.is_empty() || b.is_empty() {
        return Ok(Value::List(Vec::new().into()));
    }
    let m = a.len();
    let n = a[0].len();
    let p = b[0].len();
    if b.len() != n {
        return Err(format!(
            "linalg.matmul: 维度不匹配 —— A 是 {m}×{n}，B 有 {} 行（应为 {n} 行）",
            b.len()
        ));
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
    Ok(Value::List(
        result
            .into_iter()
            .map(|row| Value::List(row.into_iter().map(Value::Float).collect()))
            .collect(),
    ))
}

/// 矩阵转置
fn transpose(a: &[Vec<f64>]) -> Value {
    if a.is_empty() {
        return Value::List(Vec::new().into());
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
        let a = Value::List(vec![Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)].into());
        let b = Value::List(vec![Value::Float(4.0), Value::Float(5.0), Value::Float(6.0)].into());
        let r = call_linalg_method("dot", &[a, b]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 32.0).abs() < 1e-9));
    }

    #[test]
    fn cross_3d() {
        let x = Value::List(vec![Value::Float(1.0), Value::Float(0.0), Value::Float(0.0)].into());
        let y = Value::List(vec![Value::Float(0.0), Value::Float(1.0), Value::Float(0.0)].into());
        let r = call_linalg_method("cross", &[x, y]).unwrap();
        if let Value::List(v) = &r
            && let [Value::Float(a), Value::Float(b), Value::Float(c)] = &v.to_vec()[..]
        {
            assert!((*a - 0.0).abs() < 1e-9);
            assert!((*b - 0.0).abs() < 1e-9);
            assert!((*c - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn norm_l2() {
        let v = Value::List(vec![Value::Float(3.0), Value::Float(4.0)].into());
        let r = call_linalg_method("norm", &[v]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 5.0).abs() < 1e-9));
    }

    #[test]
    fn matmul_2x2() {
        let a = Value::List(
            vec![
                Value::List(vec![Value::Float(1.0), Value::Float(2.0)].into()),
                Value::List(vec![Value::Float(3.0), Value::Float(4.0)].into()),
            ]
            .into(),
        );
        let b = Value::List(
            vec![
                Value::List(vec![Value::Float(5.0), Value::Float(6.0)].into()),
                Value::List(vec![Value::Float(7.0), Value::Float(8.0)].into()),
            ]
            .into(),
        );
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
        let a = Value::List(
            vec![
                Value::List(vec![Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)].into()),
                Value::List(vec![Value::Float(4.0), Value::Float(5.0), Value::Float(6.0)].into()),
            ]
            .into(),
        );
        let r = call_linalg_method("transpose", &[a]).unwrap();
        if let Value::List(rows) = r {
            assert_eq!(rows.len(), 3); // 2x3 → 3x2
            assert_eq!(rows.len(), 3);
        } else {
            panic!("expected list");
        }
    }
}
