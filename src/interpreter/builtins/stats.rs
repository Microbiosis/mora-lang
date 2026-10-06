//! v0.91: stats.* — 统计 builtin（APL/StreamIt 启发）
//!
//! 所有函数接受 list`<number>`，结果为 Float（除 min/max 可保留原元素类型）。
//! 设计原则：复用 broadcast 后的 list，避免重新实现数值提取。

use crate::value::Value;

/// stats.* builtin 入口分发
pub fn call_stats_method(method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "sum" => {
            let xs = expect_number_list(args.first(), "stats.sum")?;
            Ok(sum_value(&xs))
        }
        "mean" => {
            let xs = expect_number_list(args.first(), "stats.mean")?;
            Ok(Value::Float(mean(&xs)))
        }
        "median" => {
            let xs = expect_number_list(args.first(), "stats.median")?;
            Ok(Value::Float(median(&xs)))
        }
        "var" => {
            let xs = expect_number_list(args.first(), "stats.var")?;
            Ok(Value::Float(variance(&xs, false)))
        }
        "stddev" => {
            let xs = expect_number_list(args.first(), "stats.stddev")?;
            Ok(Value::Float(variance(&xs, false).sqrt()))
        }
        "min" => {
            let xs = expect_number_list(args.first(), "stats.min")?;
            Ok(Value::Float(min_f(&xs)))
        }
        "max" => {
            let xs = expect_number_list(args.first(), "stats.max")?;
            Ok(Value::Float(max_f(&xs)))
        }
        "quantile" => {
            let xs = expect_number_list(args.first(), "stats.quantile")?;
            let q = args
                .get(1)
                .and_then(as_f64)
                .ok_or_else(|| "stats.quantile requires (list, q)".to_string())?;
            if !(0.0..=1.0).contains(&q) {
                return Err("stats.quantile: q must be in [0, 1]".to_string());
            }
            Ok(Value::Float(quantile(&xs, q)))
        }
        "histogram" => {
            let xs = expect_number_list(args.first(), "stats.histogram")?;
            // v0.104.6 D284：走 D246 的收口 `value_as_usize`（负数 / NaN / ±inf
            // 一律 `None`），并加上界守卫。
            //
            // 修前是 `Value::Int(n) => Some(*n as usize)`，两处都出事：
            //   ① `Value::Int(-1) as usize` —— **回绕成 1.8e19** ⇒ 下面
            //      `vec![0usize; bins]` 直接 **capacity overflow panic**；
            //      而 `json.parse("-1")` 就会产出 `Value::Int(-1)` ⇒
            //      两行普通代码让进程崩掉（实测）。
            //   ② 负数 Float 饱和成 **0** ⇒ `bins == 0` 分支**静默返回空列表**。
            //
            // ⚠ 上界是**判断题**：不封顶的话 `json.parse("100000000000")`
            // 会以**完全相同的方式**崩 —— 只修回绕等于没修。
            // 取 `HISTOGRAM_MAX_BINS`（1e6 bin ≈ 8 MB）远超任何合理用法，
            // 目的是把「进程 panic」变成「一条可操作的错误」。
            // 数值是判断题，若认为过严/过松请改这一个常量。
            const HISTOGRAM_MAX_BINS: usize = 1_000_000;
            //
            // v0.104.6 D329：补上**第三道**守卫 —— `bins` 必须是**整数**。
            //
            // D284 的两道（负数 / 上界）都没覆盖小数，于是：
            //   stats.histogram([1,2,3,4], 2.5) → exit 0，**2 个 bin**
            //   stats.histogram([1,2,3,4], 0.5) → exit 0，**空列表**
            // 两条都是**静默**的，且 `0.5` 那条静默得最彻底：`value_as_usize`
            // 向零截断得 `bins = 0`，而 `histogram()` 开头就有
            // `if bins == 0 … { return Value::List(Vec::new()) }` ——
            // 用户要 0.5 个分箱，拿到「0 个分箱」，与**显式**传 0 完全同形，
            // 于是 D284 判据 `d284_explicit_zero_bins_still_returns_empty_list`
            // 守住的「显式 0 返回空列表」这条**合法**语义，被小数**顺带**继承。
            //
            // 为什么不是「D246 收口该改成拒绝非整数」：它**已经**用判据显式
            // 钉住截断是设计 —— `tests/value_extraction_saturation.rs:54`
            //   assert_eq!(value_as_usize(&Value::Float(2.9)), Some(2), "应向零取整");
            // 那个收口有 6 个调用点（`crush_json` / `exec.parallel` / `max_steps` /
            // `backoff_ms` / `sandbox.cpu_cores` / `timeout_ms`），对它们而言
            // 向零截断是合理的**通用**约定（1.5 ms 超时没有意义，取 1 ms 就好）。
            // ⇒ 「bins 必须是整数」是 **`histogram` 自己的**契约，不是收口的。
            // 两处各自为政，正是本条只改这里的原因。
            //
            // 判定为缺陷而非设计，依据有二（缺一不可）：
            //   ① `docs/mora-spec.md:981` 签名写的是 **`list, int`** —— 小数
            //      本就不该进；而 `typeck/dispatch.rs:680` 只登记**元数**不校验
            //      参数类型，故收口是**唯一**能拦的地方；
            //   ② 本函数**自己的错误消息**写着 "bins must be an integer" ——
            //      消息承诺 integer、行为接受 2.5，**自相矛盾**。
            // 数值精度上 `2.0` 这类「浮点写的整数」仍应放行（`fract() == 0.0`），
            // 否则 dict 字面量给 `Float`（D98）会让 `histogram(xs, 2.0)` 崩。
            let bins = match args.get(1) {
                Some(v) => crate::flow::value_as_f64(v)
                    .filter(|n| {
                        n.is_finite() && n.fract() == 0.0 && *n >= 0.0 && *n <= HISTOGRAM_MAX_BINS as f64
                    })
                    .map(|n| n as usize),
                None => None,
            }
            .ok_or_else(|| {
                format!(
                    "stats.histogram: bins must be an integer in 0..={HISTOGRAM_MAX_BINS}, got {:?}",
                    args.get(1)
                )
            })?;
            Ok(histogram(&xs, bins))
        }
        "corr" => {
            let a = expect_number_list(args.first(), "stats.corr")?;
            let b = expect_number_list(args.get(1), "stats.corr")?;
            if a.len() != b.len() {
                return Err("stats.corr: lists must have equal length".to_string());
            }
            Ok(Value::Float(pearson_correlation(&a, &b)))
        }
        "cov" => {
            let a = expect_number_list(args.first(), "stats.cov")?;
            let b = expect_number_list(args.get(1), "stats.cov")?;
            if a.len() != b.len() {
                return Err("stats.cov: lists must have equal length".to_string());
            }
            // v0.104.6 D143：空列表此前得 `NaN`（`0.0 / 0`），与**同文件**
            // `mean` / `variance` / `median` / `min_f` / `max_f` / `pearson_correlation`
            // 一律「空列表返回 `0.0`」的显式约定**不一致** —— 那个约定就写在
            // `mean` 上方的注释里（v0.104.6 引入），`cov` 是同族里**唯一**漏掉守卫的。
            // 实测 `stats.cov([], [])` → `nan`、exit 0、零诊断。
            if a.is_empty() {
                return Ok(Value::Float(0.0));
            }
            let m = mean(&a);
            let n = mean(&b);
            let cov: f64 = a.iter().zip(b.iter()).map(|(x, y)| (x - m) * (y - n)).sum();
            Ok(Value::Float(cov / a.len() as f64))
        }
        other => Err(format!("stats.{}: unknown method", other)),
    }
}

// ── 内部数值提取与统计原语 ──

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    }
}

fn expect_number_list(arg: Option<&Value>, ctx: &str) -> Result<Vec<f64>, String> {
    let v = arg.ok_or_else(|| format!("{}: list argument required", ctx))?;
    let list = match v {
        Value::List(xs) => xs,
        _ => return Err(format!("{}: list argument required", ctx)),
    };
    list.iter()
        .map(|x| as_f64(x).ok_or_else(|| format!("{}: non-numeric element", ctx)))
        .collect()
}

fn sum_value(xs: &[f64]) -> Value {
    // sum 保留 Int 当全是 Int 且不溢出；这里简化为 Float
    // （精确 Int sum 留给 caller 用 reduce）
    Value::Float(xs.iter().sum())
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// v0.104.6：空列表返回 `0.0`，与同文件的 `mean` / `median` 完全一致。
///
/// 此前 `min_f` / `max_f` 用 `fold(INFINITY, min)` / `fold(NEG_INFINITY, max)`，
/// 没有任何空列表守卫 —— 空列表直接漏出 IEEE 哨兵 `inf` / `-inf`。而同文件的
/// `mean`（`if xs.is_empty() { return 0.0 }`）与 `median` **都有**守卫：
/// 同一个文件里一半守卫、一半不守卫，是漏写而非设计。
///
/// 语言作者已确认取 `0.0`（与 mean/median 统一），而不是返回 Nil 或报错。
///
/// 注：`sum([])` 仍是 `-0.0`（`f64::iter().sum()` 的空迭代器初值），未在本次
/// 决定范围内；`shape` 的参差嵌套仍只按第一个子列表递归。二者作为「spec 未
/// 规定的既有行为」钉在 `tests/list_methods.rs` 的 undefined quirks 组里。
fn min_f(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max_f(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// 中位数（先排序，再取中点）
///
/// v0.104.6 D242：排序用 `f64::total_cmp` 而非
/// `a.partial_cmp(b).unwrap_or(Ordering::Equal)`。
///
/// 后者遇 NaN 时 `partial_cmp` 返回 `None` → `Equal` ⇒ **NaN 与一切相等**，
/// 而它与别的数的实际大小关系又不一致 ⇒ 违反传递性。`sort_by` 在非全序
/// 比较器下**静默**产出依赖输入顺序的结果。
///
/// 实测（同一组 `[1, NaN, 3, 2]`，仅输入顺序不同）：
/// `median([1,NaN,3,2]) = 1.5` 而 `median([2,1,3,NaN]) = 2.5`
/// —— **中位数是统计值，顺序依赖意味着同组数据给出不同答案。**
///
/// `total_cmp` 是 IEEE 754 定义的**全序**（NaN 统一排到末尾）。
fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// 分位数（线性插值）
///
/// v0.104.6 D242：同 [`median`]，排序改用 `total_cmp`（NaN 破坏
/// `partial_cmp + Equal` 的全序性）。实测 `quantile([1,NaN,3,2], 0.75)`
/// 修前得 `2.25`、换输入顺序得 `nan` —— 同一组数据两个答案。
fn quantile(xs: &[f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = pos - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

/// 方差（population）。sample=true 时除以 n-1。
fn variance(xs: &[f64], sample: bool) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let sum: f64 = xs.iter().map(|x| (x - m).powi(2)).sum();
    let denom = if sample { xs.len() - 1 } else { xs.len() } as f64;
    sum / denom
}

/// 皮尔逊相关系数
fn pearson_correlation(a: &[f64], b: &[f64]) -> f64 {
    let ma = mean(a);
    let mb = mean(b);
    let cov: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - ma) * (y - mb))
        .sum();
    let va: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = b.iter().map(|x| (x - mb).powi(2)).sum();
    let denom = (va * vb).sqrt();
    if denom == 0.0 { 0.0 } else { cov / denom }
}

/// 直方图：返回 `[{lo, hi, count}, ...]` list of dict
fn histogram(xs: &[f64], bins: usize) -> Value {
    if bins == 0 || xs.is_empty() {
        return Value::List(Vec::new().into());
    }
    let min = min_f(xs);
    let max = max_f(xs);
    if min == max {
        // 全部相等：单 bin
        let mut d = std::collections::HashMap::new();
        d.insert("lo".to_string(), Value::Float(min));
        d.insert("hi".to_string(), Value::Float(max));
        d.insert("count".to_string(), Value::Float(xs.len() as f64));
        return Value::List(vec![Value::Dict(d)].into());
    }
    let width = (max - min) / bins as f64;
    let mut counts = vec![0usize; bins];
    for &x in xs {
        let mut idx = ((x - min) / width) as usize;
        if idx >= bins {
            idx = bins - 1;
        }
        counts[idx] += 1;
    }
    let mut result = Vec::with_capacity(bins);
    for (i, &c) in counts.iter().enumerate() {
        let mut d = std::collections::HashMap::new();
        d.insert("lo".to_string(), Value::Float(min + i as f64 * width));
        d.insert("hi".to_string(), Value::Float(min + (i + 1) as f64 * width));
        d.insert("count".to_string(), Value::Float(c as f64));
        result.push(Value::Dict(d));
    }
    Value::List(result.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(xs: &[f64]) -> Value {
        Value::List(xs.iter().map(|x| Value::Float(*x)).collect())
    }

    #[test]
    fn sum_basic() {
        let r = call_stats_method("sum", &[list(&[1.0, 2.0, 3.0, 4.0])]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 10.0).abs() < 1e-9));
    }

    #[test]
    fn mean_basic() {
        let r = call_stats_method("mean", &[list(&[2.0, 4.0, 6.0])]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 4.0).abs() < 1e-9));
    }

    #[test]
    fn median_odd() {
        let r = call_stats_method("median", &[list(&[1.0, 2.0, 3.0])]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 2.0).abs() < 1e-9));
    }

    #[test]
    fn median_even() {
        let r = call_stats_method("median", &[list(&[1.0, 2.0, 3.0, 4.0])]).unwrap();
        assert!(matches!(r, Value::Float(x) if (x - 2.5).abs() < 1e-9));
    }

    #[test]
    fn min_max() {
        assert!(matches!(
            call_stats_method("min", &[list(&[3.0, 1.0, 2.0])]).unwrap(),
            Value::Float(x) if (x - 1.0).abs() < 1e-9
        ));
        assert!(matches!(
            call_stats_method("max", &[list(&[3.0, 1.0, 2.0])]).unwrap(),
            Value::Float(x) if (x - 3.0).abs() < 1e-9
        ));
    }

    #[test]
    fn histogram_returns_bins() {
        let r =
            call_stats_method("histogram", &[list(&[1.0, 2.0, 3.0, 4.0]), Value::Int(2)]).unwrap();
        if let Value::List(bins) = r {
            assert_eq!(bins.len(), 2);
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn empty_list_mean_zero() {
        let r = call_stats_method("mean", &[Value::List(Vec::new().into())]).unwrap();
        assert!(matches!(r, Value::Float(0.0)));
    }
}
