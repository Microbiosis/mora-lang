//! v0.91: stats.* — 统计 builtin（APL/StreamIt 启发）
//!
//! 所有函数接受 list<number>，结果为 Float（除 min/max 可保留原元素类型）。
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
            let bins = args
                .get(1)
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n as usize),
                    Value::Float(n) => Some(*n as usize),
                    _ => None,
                })
                .ok_or_else(|| "stats.histogram requires (list, bins)".to_string())?;
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

fn min_f(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max_f(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// 中位数（先排序，再取中点）
fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// 分位数（线性插值）
fn quantile(xs: &[f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
        return Value::List(Vec::new());
    }
    let min = min_f(xs);
    let max = max_f(xs);
    if min == max {
        // 全部相等：单 bin
        let mut d = std::collections::HashMap::new();
        d.insert("lo".to_string(), Value::Float(min));
        d.insert("hi".to_string(), Value::Float(max));
        d.insert("count".to_string(), Value::Float(xs.len() as f64));
        return Value::List(vec![Value::Dict(d)]);
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
    Value::List(result)
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
        let r = call_stats_method("mean", &[Value::List(Vec::new())]).unwrap();
        assert!(matches!(r, Value::Float(0.0)));
    }
}
