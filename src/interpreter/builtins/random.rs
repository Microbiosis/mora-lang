//! v0.91: random.* — PRNG builtin
//!
//! 实现：xoshiro256**（Vigna 2018）+ splitmix64 种子派生。
//! 零外部依赖；密码学不可用但足够 AI/统计工作。
//! 全局状态用 Mutex 包装，保证 Send/Sync。

use crate::value::Value;
use std::sync::Mutex;

// ── xoshiro256** 状态 ──

struct Xoshiro256 {
    s: [u64; 4],
}

impl Xoshiro256 {
    fn from_seed(seed: u64) -> Self {
        let mut sm = SplitMix64(seed);
        Self {
            s: [sm.next(), sm.next(), sm.next(), sm.next()],
        }
    }

    /// xoshiro256** 一步：返回 64-bit 随机数
    fn next_u64(&mut self) -> u64 {
        let result = (self.s[1].wrapping_mul(5)).rotate_left(7).wrapping_mul(9);
        let t = self.s[1].wrapping_shl(17);
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// 返回 [0, 1) 范围的 f64
    fn next_f64(&mut self) -> f64 {
        // 取 53 位精度
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// 返回 [min, max) 范围的 i64（均匀）
    fn next_i64_in(&mut self, min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        let range = (max - min) as u64;
        min + (self.next_u64() % range) as i64
    }
}

/// splitmix64：种子派生器（Vigna 2018 推荐）
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

// ── 全局状态 ──

static STATE: Mutex<Xoshiro256> = Mutex::new(Xoshiro256 {
    s: [0x0; 4],
});

fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut Xoshiro256) -> R,
{
    let mut guard = STATE.lock().expect("random: poisoned mutex");
    f(&mut guard)
}

#[allow(static_mut_refs)]
fn ensure_initialized(state: &mut Xoshiro256) {
    // 首次访问时用系统时间初始化
    use std::sync::atomic::{AtomicBool, Ordering};
    static INITED: AtomicBool = AtomicBool::new(false);
    if !INITED.load(Ordering::Relaxed) {
        *state = Xoshiro256::from_seed(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234_5678_9abc_def0),
        );
        INITED.store(true, Ordering::Relaxed);
    }
}

// ── random.* 入口 ──

pub fn call_random_method(method: &str, args: &[Value]) -> Result<Value, String> {
    with_state(|rng| {
        ensure_initialized(rng);
        match method {
            "random" => Ok(Value::Float(rng.next_f64())),
            "rand_int" => {
                let min = args
                    .first()
                    .and_then(|v| match v {
                        Value::Int(n) => Some(*n),
                        Value::Float(n) => Some(*n as i64),
                        _ => None,
                    })
                    .ok_or_else(|| "random.rand_int requires (min, max)".to_string())?;
                let max = args
                    .get(1)
                    .and_then(|v| match v {
                        Value::Int(n) => Some(*n),
                        Value::Float(n) => Some(*n as i64),
                        _ => None,
                    })
                    .ok_or_else(|| "random.rand_int requires (min, max)".to_string())?;
                Ok(Value::Float(rng.next_i64_in(min, max) as f64))
            }
            "rand_float" => {
                let min = args
                    .first()
                    .and_then(as_f64)
                    .ok_or_else(|| "random.rand_float requires (min, max)".to_string())?;
                let max = args
                    .get(1)
                    .and_then(as_f64)
                    .ok_or_else(|| "random.rand_float requires (min, max)".to_string())?;
                Ok(Value::Float(min + (max - min) * rng.next_f64()))
            }
            "rand_choice" => {
                let items = match args.first() {
                    Some(Value::List(xs)) => xs,
                    _ => return Err("random.rand_choice requires a list".to_string()),
                };
                if items.is_empty() {
                    return Err("random.rand_choice: empty list".to_string());
                }
                let idx = rng.next_i64_in(0, items.len() as i64) as usize;
                Ok(items[idx].clone())
            }
            "seed" => {
                let n = args
                    .first()
                    .and_then(|v| match v {
                        Value::Int(n) => Some(*n as u64),
                        Value::Float(n) => Some(*n as u64),
                        _ => None,
                    })
                    .ok_or_else(|| "random.seed requires a numeric argument".to_string())?;
                *rng = Xoshiro256::from_seed(n);
                Ok(Value::Nil)
            }
            "shuffle" => {
                let items = match args.first() {
                    Some(Value::List(xs)) => xs.clone(),
                    _ => return Err("random.shuffle requires a list".to_string()),
                };
                // Fisher-Yates
                let n = items.len();
                let mut result = items;
                for i in (1..n).rev() {
                    let j = rng.next_i64_in(0, (i + 1) as i64) as usize;
                    result.swap(i, j);
                }
                Ok(Value::List(result))
            }
            other => Err(format!("random.{}: unknown method", other)),
        }
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_in_unit_interval() {
        for _ in 0..100 {
            let r = call_random_method("random", &[]).unwrap();
            if let Value::Float(x) = r {
                assert!((0.0..1.0).contains(&x), "got {}", x);
            } else {
                panic!("expected float");
            }
        }
    }

    #[test]
    fn seed_determinism() {
        call_random_method("seed", &[Value::Int(42)]).unwrap();
        let a = call_random_method("random", &[]).unwrap();
        call_random_method("seed", &[Value::Int(42)]).unwrap();
        let b = call_random_method("random", &[]).unwrap();
        assert_eq!(format!("{}", a), format!("{}", b));
    }

    #[test]
    fn rand_int_in_range() {
        call_random_method("seed", &[Value::Int(1)]).unwrap();
        for _ in 0..50 {
            let r = call_random_method("rand_int", &[Value::Int(5), Value::Int(10)]).unwrap();
            if let Value::Float(n) = r {
                let n = n as i64;
                assert!((5..10).contains(&n));
            }
        }
    }

    #[test]
    fn shuffle_preserves_elements() {
        call_random_method("seed", &[Value::Int(7)]).unwrap();
        let input = Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]);
        let r = call_random_method("shuffle", &[input]).unwrap();
        if let Value::List(out) = r {
            assert_eq!(out.len(), 5);
            // 元素集合不变
            let mut orig = vec![1, 2, 3, 4, 5];
            let mut got: Vec<i64> = out
                .iter()
                .filter_map(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .collect();
            orig.sort();
            got.sort();
            assert_eq!(orig, got);
        } else {
            panic!("expected list");
        }
    }
}