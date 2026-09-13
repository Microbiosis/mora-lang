//! v0.99: ambient random — PRNG 纯值 + ambient effect 操作分发。
//!
//! v0.91 版本是进程级全局状态机：`static Mutex<Xoshiro256>` +
//! `AtomicBool` 惰性初始化标志 + 时间种子 —— 副作用对类型系统完全不可见
//! （typeck 视 `random.*` 为纯 `Any`），测试共享全局序列互相干扰，
//! 并发靠一把进程级互斥锁防御。
//!
//! v0.99 数据流化（用数据流代替状态机）：
//! - **状态是纯值**：[`Xoshiro256`] 由 [`crate::runtime::core::CoreRuntime`]
//!   单属主持有（与 `gensym_counter` 同一 v0.95 模式），`&mut self` 线性
//!   穿线 —— 无锁、无 static、无初始化标志。Clone 按值复制：Pregel worker
//!   各自独立推进序列，并发从「共享一把锁」变成「值拷贝即隔离」。
//! - **副作用是 ambient effect**：`random.<method>(...)` 调用对应
//!   [`crate::mir::effect::ambient`] 里的标签，运行时由根状态兜底应答，
//!   用户 `handle random_seed { ... }` 可按动态作用域覆写；
//!   类型层把调用记入效果行（每操作一个 ambient 签名，签名预置于
//!   typeck 的 `effect_signatures`）。
//!
//! 算法保持 v0.91 语义不变：xoshiro256**（Vigna 2018）+ splitmix64 种子
//! 派生。零外部依赖；密码学不可用但足够 AI/统计工作。

use crate::value::Value;

// ── xoshiro256** 状态（纯值）──

#[derive(Debug, Clone, Copy)]
pub(crate) struct Xoshiro256 {
    s: [u64; 4],
}

impl Xoshiro256 {
    pub(crate) fn from_seed(seed: u64) -> Self {
        let mut sm = SplitMix64(seed);
        Self {
            s: [sm.next(), sm.next(), sm.next(), sm.next()],
        }
    }

    /// 运行时构造的默认种子：系统时间纳秒。每个 CoreRuntime 一份 ——
    /// 「进程级唯一」降级为「运行时实例级唯一」，跨实例隔离更彻底。
    pub(crate) fn from_time() -> Self {
        Self::from_seed(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234_5678_9abc_def0),
        )
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

    /// ambient effect 操作分发：label 见
    /// [`crate::mir::effect::ambient::RANDOM_LABELS`]，args 为方法调用实参
    /// （不含方法名 —— 操作已由标签编码）。`&mut self` 线性推进状态。
    ///
    /// 错误消息与 v0.91 保持逐字一致（运行时防御消息；类型层签名是第一道门）。
    pub(crate) fn dispatch_op(&mut self, label: &str, args: &[Value]) -> Result<Value, String> {
        match label {
            "random_random" => Ok(Value::Float(self.next_f64())),
            "random_rand_int" => {
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
                Ok(Value::Float(self.next_i64_in(min, max) as f64))
            }
            "random_rand_float" => {
                let min = args
                    .first()
                    .and_then(as_f64)
                    .ok_or_else(|| "random.rand_float requires (min, max)".to_string())?;
                let max = args
                    .get(1)
                    .and_then(as_f64)
                    .ok_or_else(|| "random.rand_float requires (min, max)".to_string())?;
                Ok(Value::Float(min + (max - min) * self.next_f64()))
            }
            "random_rand_choice" => {
                let items = match args.first() {
                    Some(Value::List(xs)) => xs,
                    _ => return Err("random.rand_choice requires a list".to_string()),
                };
                if items.is_empty() {
                    return Err("random.rand_choice: empty list".to_string());
                }
                let idx = self.next_i64_in(0, items.len() as i64) as usize;
                Ok(items[idx].clone())
            }
            "random_seed" => {
                let n = args
                    .first()
                    .and_then(|v| match v {
                        Value::Int(n) => Some(*n as u64),
                        Value::Float(n) => Some(*n as u64),
                        _ => None,
                    })
                    .ok_or_else(|| "random.seed requires a numeric argument".to_string())?;
                *self = Xoshiro256::from_seed(n);
                Ok(Value::Nil)
            }
            "random_shuffle" => {
                let items = match args.first() {
                    Some(Value::List(xs)) => xs.clone(),
                    _ => return Err("random.shuffle requires a list".to_string()),
                };
                // Fisher-Yates
                let n = items.len();
                let mut result = items;
                for i in (1..n).rev() {
                    let j = self.next_i64_in(0, (i + 1) as i64) as usize;
                    result.swap(i, j);
                }
                Ok(Value::List(result))
            }
            other => Err(format!("random: unknown ambient effect label `{}`", other)),
        }
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

    // v0.99：测试不再共享任何全局状态 —— 每个 handler 独立种子，可并行。
    // （v0.91 版本的 seed_determinism / rand_int_in_range 等测试操作进程级
    // 全局 Mutex 状态，`cargo test` 多线程下互相推进序列，靠时序侥幸通过。）

    #[test]
    fn random_in_unit_interval() {
        let mut rng = Xoshiro256::from_seed(42);
        for _ in 0..100 {
            let r = rng.dispatch_op("random_random", &[]).unwrap();
            if let Value::Float(x) = r {
                assert!((0.0..1.0).contains(&x), "got {}", x);
            } else {
                panic!("expected float");
            }
        }
    }

    #[test]
    fn seed_determinism() {
        let mut a = Xoshiro256::from_seed(42);
        let x1 = a.dispatch_op("random_random", &[]).unwrap();
        let x2 = a.dispatch_op("random_random", &[]).unwrap();
        a.dispatch_op("random_seed", &[Value::Int(42)]).unwrap();
        let y1 = a.dispatch_op("random_random", &[]).unwrap();
        let y2 = a.dispatch_op("random_random", &[]).unwrap();
        assert_eq!(format!("{}", x1), format!("{}", y1));
        assert_eq!(format!("{}", x2), format!("{}", y2));
    }

    #[test]
    fn rand_int_in_range() {
        let mut rng = Xoshiro256::from_seed(1);
        for _ in 0..50 {
            let r = rng
                .dispatch_op("random_rand_int", &[Value::Int(5), Value::Int(10)])
                .unwrap();
            if let Value::Float(n) = r {
                let n = n as i64;
                assert!((5..10).contains(&n));
            }
        }
    }

    #[test]
    fn shuffle_preserves_elements() {
        let mut rng = Xoshiro256::from_seed(7);
        let input = Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]);
        let r = rng.dispatch_op("random_shuffle", &[input]).unwrap();
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

    #[test]
    fn instances_are_independent() {
        // v0.99 核心性质：实例按值隔离 —— 推进一个实例不影响另一个
        //（旧全局 Mutex 语义下两者共享同一序列，互相推进）。
        let mut a = Xoshiro256::from_seed(99);
        let mut b = Xoshiro256::from_seed(99);
        // b 先走两步，a 不动
        let _ = b.dispatch_op("random_random", &[]).unwrap();
        let _ = b.dispatch_op("random_random", &[]).unwrap();
        // a 的第一步仍等于种子 99 序列的第一步（对照全新实例）
        let a1 = a.dispatch_op("random_random", &[]).unwrap();
        let mut reference = Xoshiro256::from_seed(99);
        let r1 = reference.dispatch_op("random_random", &[]).unwrap();
        assert_eq!(format!("{}", a1), format!("{}", r1));
        // 同种子实例序列逐值一致
        let b_realigned_seed = b.dispatch_op("random_seed", &[Value::Int(99)]).unwrap();
        assert!(matches!(b_realigned_seed, Value::Nil));
        let b1 = b.dispatch_op("random_random", &[]).unwrap();
        assert_eq!(format!("{}", b1), format!("{}", r1));
    }

    #[test]
    fn labels_cover_all_methods() {
        use crate::mir::effect::ambient;
        // 方法 ↔ 标签一致：每个标签都能被 dispatch_op 分发（到达参数校验
        // 或正常返回，而不是 unknown label）。用户面 unknown-method 错误
        // 在 method_dispatch 层产生。
        let mut rng = Xoshiro256::from_seed(1);
        for label in ambient::RANDOM_LABELS {
            let outcome = rng.dispatch_op(label, &[]);
            if let Err(err) = outcome {
                assert!(!err.contains("unknown ambient"), "{}", err);
            }
        }
        // 未知标签有明确错误
        let err = rng.dispatch_op("random_nonsense", &[]).unwrap_err();
        assert!(err.contains("unknown ambient"));
    }
}
