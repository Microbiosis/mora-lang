//! v0.104.6 D331 —— `math.abs` / `sign` / `floor` / `ceil` / `round` / `trunc`
//! 与三个浮点谓词**拒绝 BigInt**，而同模块另外 20 个函数**接受**（已修）
//!
//! ## 实测（修前）—— 模块内的**分裂**，分界线与「用哪个提取函数」逐个吻合
//!
//! ```text
//! math.sqrt(4n)      → 2.0                    ← 走 expect_number，接受
//! math.log(1n)       → 0.0                    ← 同上
//! math.pow(2n,10n)   → 1024.0                 ← 同上
//! math.sin(0n)       → 0.0                    ← 同上
//!
//! math.abs(-5n)      → math: numeric argument required   ← 走 unary_preserve，拒绝
//! math.floor(2.7n)   → 同上                             ← 同上
//! math.is_finite(1n) → math.is_finite requires a numeric argument  ← 手写 match，拒绝
//! ```
//!
//! 全量统计：**20 个接受 / 9 个拒绝 / 0 个相反**。
//!
//! ## 为什么是缺陷（而不是「BigInt 本来就不该进 math」）
//!
//! 三条**互相独立**的证据：
//!
//! ① **`docs/mora-spec.md:966-967` 的签名是 `number -> number`** ——
//!    `number` 涵盖 BigInt，且同一行写明「保留 Int 类型」，
//!    恰恰表明这个函数族是**按输入类型分流**的，没有排除 BigInt。
//! ② **`math.rs` 自己的 `expect_number`（本文件第 116 行）就认 BigInt**，
//!    且 CHANGELOG v0.150 那条**把它当成同族做对的样板**明写：
//!    「**必选**数值实参走 `math.rs::expect_number`，认 `Int`/`Float`/`BigInt`
//!    —— 实测 `math.pow(2, 10)=1024` 均正常」。
//! ③ **分界线与实现选择逐个吻合**：`expect_number` 的 20 个全接受，
//!    `unary_preserve` 的 6 个 + 手写 match 的 3 个全拒绝。
//!    若这是语义取舍，分界应当按**语义**分（三角 / 对数 / 取整），
//!    实际却按**调用了哪个函数**分 —— 那是**实现层的漏写**。
//!
//! ## 关键：BigInt 分支**不能**经 f64
//!
//! 若图省事写成 `unary_float` 再转回，会静默丢精度 —— `f64` 只有 53 位尾数：
//!
//! ```mora
//! let big = 99999999999999999999999999999999999999999n   -- 41 位
//! math.sqrt(big)   → 316227766016837943296.0             -- 只剩 15 位有效数字
//! ```
//!
//! 故 `abs` / `sign` / `floor` / `ceil` / `round` / `trunc` 的 BigInt 分支
//! **直接在整数域上算**（`BigIntOp` 枚举），不经任何浮点中转。实测
//! 41 位 BigInt 经 `floor` / `abs` / `ceil` / `round` / `trunc`
//! **逐位精确保持**，`sign` 正确给 `-1n`。
//!
//! 附带收益：`i64::MIN.abs()` 在 Rust 里会溢出，BigInt 无此问题 ——
//! 这正是「能接 BigInt 就该接」的实际价值，而不只是消除报错。
//!
//! ## 不回归：BigInt 谓词的语义是**恒定**的，不是「照搬 Int 的 false」
//!
//! `is_nan` / `is_inf` 对 BigInt **恒为 `false`**、`is_finite` **恒为 `true`** ——
//! 任意精度整数不存在 NaN / inf / 溢出。这与 `Int` 分支恰好一致，
//! 但理由不同：Int 是「因为 i64 有限」，BigInt 是「因为整数**总是**有限」。

use mora::interpreter::builtins::math::call_math_method;
use mora::value::Value;

/// 造一个 BigInt 字面量
fn bi(s: &str) -> Value {
    Value::BigInt(s.parse().expect("BigInt 字面量"))
}

fn call(method: &str, args: Vec<Value>) -> Result<Value, String> {
    call_math_method(method, &args)
}

/// **主断言**：取整一族的六个函数必须接受 BigInt，且**保留 BigInt 类型**。
///
/// 类型保留是本条的核心 —— 走 `unary_float` 中转也能让断言「不报错」，
/// 但会把 `5n` 变成 `5.0`（降级）。故每条都断言 `matches!(.., Value::BigInt(_))`。
#[test]
fn d331_rounding_family_accepts_bigint_and_preserves_type() {
    for (method, arg, want) in [
        ("abs", "-5", "5"),
        ("sign", "-3", "-1"),
        ("sign", "3", "1"),
        ("sign", "0", "0"),
        ("floor", "2", "2"),
        ("ceil", "2", "2"),
        ("round", "2", "2"),
        ("trunc", "2", "2"),
    ] {
        let got = call(method, vec![bi(arg)])
            .unwrap_or_else(|e| panic!("math.{method}({arg}n) 不应报错: {e}"));
        match got {
            Value::BigInt(b) => assert_eq!(
                b.to_string(),
                want,
                "math.{method}({arg}n) 应得 {want}n; 实得 {b}n（**BigInt 类型必须保留**，\
                 降级成 Float 说明走了浮点中转，会丢精度）"
            ),
            other => panic!("math.{method}({arg}n) 应保留 BigInt 类型; 实得 {other:?}"),
        }
    }
}

/// **精度断言**：41 位 BigInt 经取整族必须**逐位精确**保持。
///
/// 这是「不经 f64」的**直接证据**。若实现改成
/// `unary_float(args, …).map(|f| bi(f.to_string()))`，
/// 这条会立刻红 —— 因为 `f64` 只剩 15 位有效数字。
#[test]
fn d331_bigint_precision_survives_rounding() {
    const BIG: &str = "99999999999999999999999999999999999999999"; // 41 位
    for method in ["floor", "ceil", "round", "trunc"] {
        let got =
            call(method, vec![bi(BIG)]).unwrap_or_else(|e| panic!("math.{method} 不应报错: {e}"));
        let s = match &got {
            Value::BigInt(b) => b.to_string(),
            other => panic!("math.{method}(BIG) 应得 BigInt; 实得 {other:?}"),
        };
        assert_eq!(
            s, BIG,
            "math.{method}(41位 BigInt) 必须**逐位精确**保持; 实得 {s}\
             （若变短，说明经过 f64 中转丢了精度）"
        );
    }
    let got = call("abs", vec![bi(&format!("-{BIG}"))]).expect("abs 不应报错");
    match got {
        Value::BigInt(b) => assert_eq!(
            b.to_string(),
            BIG,
            "math.abs(负 41 位 BigInt) 应精确给出正数; 实得 {b}"
        ),
        other => panic!("math.abs 应保留 BigInt; 实得 {other:?}"),
    }
    let got = call("sign", vec![bi(&format!("-{BIG}"))]).expect("sign 不应报错");
    match got {
        Value::BigInt(b) => assert_eq!(b.to_string(), "-1", "math.sign(负 BigInt) 应为 -1n"),
        other => panic!("math.sign 应保留 BigInt; 实得 {other:?}"),
    }
}

/// 三个浮点谓词必须接受 BigInt，且**恒定**语义正确。
///
/// 这三条尤其重要：它们的实参在 D331 之前被 `_ =>` 分支吞掉，
/// 而 `_ =>` 同时也承担「类型真的不对」的情况（`math.is_finite("x")`）。
/// 拆开之后，两种错误的诊断**不再混淆**。
#[test]
fn d331_float_predicates_accept_bigint() {
    for (method, want) in [("is_nan", false), ("is_inf", false), ("is_finite", true)] {
        let got = call(method, vec![bi("1")])
            .unwrap_or_else(|e| panic!("math.{method}(1n) 不应报错 —— BigInt 是 number: {e}"));
        assert_eq!(
            got,
            Value::Bool(want),
            "math.{method}(1n) 应为 {want}（任意精度整数无 NaN / inf / 溢出）"
        );
    }
    // 超大 BigInt 同样成立 —— 它不会「溢出成 inf」
    for (method, want) in [("is_nan", false), ("is_inf", false), ("is_finite", true)] {
        let got = call(method, vec![bi("123456789012345678901234567890")])
            .unwrap_or_else(|e| panic!("math.{method}(超大 BigInt) 不应报错: {e}"));
        assert_eq!(
            got,
            Value::Bool(want),
            "math.{method}(超大 BigInt) 应为 {want}"
        );
    }
}

/// **对照组 1**：`Int` / `Float` 两条既有路径**逐字不变**。
#[test]
fn d331_int_and_float_paths_unchanged() {
    for (method, arg, want) in [
        ("abs", Value::Float(-5.0), Value::Float(5.0)),
        ("abs", Value::Int(-5), Value::Int(5)),
        ("sign", Value::Float(-3.5), Value::Float(-1.0)),
        ("sign", Value::Float(0.0), Value::Float(0.0)),
        ("sign", Value::Float(-0.0), Value::Float(0.0)),
        ("floor", Value::Float(3.7), Value::Float(3.0)),
        ("floor", Value::Float(-3.5), Value::Float(-4.0)),
        ("ceil", Value::Float(-3.5), Value::Float(-3.0)),
        ("round", Value::Float(-3.5), Value::Float(-4.0)),
        ("round", Value::Float(2.5), Value::Float(3.0)),
        ("trunc", Value::Float(-3.5), Value::Float(-3.0)),
        // Int 路径是恒等（取整对整数无作用）
        ("floor", Value::Int(42), Value::Int(42)),
        ("ceil", Value::Int(42), Value::Int(42)),
        ("round", Value::Int(42), Value::Int(42)),
        ("trunc", Value::Int(42), Value::Int(42)),
    ] {
        let got =
            call(method, vec![arg.clone()]).unwrap_or_else(|e| panic!("math.{method} 报错: {e}"));
        assert_eq!(got, want, "math.{method}({arg:?}) 应得 {want:?}");
    }
}

/// **对照组 2**：**非数值**实参仍然干净报错，不得被新分支放宽。
///
/// 修 BigInt 的最大风险是「兜底写太宽，把 `"x"` / `true` 也放进来」。
/// 故这条专门钉死 `_ =>` 分支**只**对 `String` / `Bool` / `Nil` 生效。
#[test]
fn d331_non_numeric_still_rejected() {
    for method in ["abs", "sign", "floor", "ceil", "round", "trunc"] {
        for arg in [Value::String("5".into()), Value::Bool(true), Value::Nil] {
            let err = call(method, vec![arg.clone()])
                .expect_err(&format!("math.{method}({arg:?}) 必须报错"));
            assert!(
                err.contains("numeric argument required"),
                "math.{method}({arg:?}) 的错误应说明需要数值; 实得: {err}"
            );
        }
    }
    // ⚠ 三个谓词的措辞与 `unary_preserve` **不同**且**刻意不同**：
    // 谓词写 `math.is_nan requires a numeric argument`（**没有** `math:` 前缀，
    // 因为方法名已经拼在里面了），而 `unary_preserve` 写
    // `math: numeric argument required`（共用一个泛化消息，方法名不出现）。
    // 两条路径的措辞不一致是**既有事实**，本条钉住现状而不统一它们 ——
    // 统一措辞属独立的一次措辞清理，不该混进「修 BigInt 漏写」这条。
    for method in ["is_nan", "is_inf", "is_finite"] {
        let err = call(method, vec![Value::String("1".into())])
            .expect_err(&format!("math.{method}(\"1\") 必须报错"));
        assert!(
            err.contains("requires a numeric argument"),
            "math.{method}(\"1\") 的错误应说明需要数值; 实得: {err}"
        );
    }
}

/// **对照组 3**：走 `expect_number` 的 20 个函数**本来就接受 BigInt**，不得回归。
///
/// 这条是「本次修复范围」的正面声明 —— 提醒后来者：
/// D331 修的是**另外 9 个**，不是把 BigInt 引入 math（它早就在了）。
#[test]
fn d331_expect_number_family_still_accepts_bigint() {
    for (method, args) in [
        ("sqrt", vec!["4"]),
        ("cbrt", vec!["8"]),
        ("log2", vec!["8"]),
        ("log10", vec!["100"]),
        ("pow", vec!["2", "10"]),
        ("hypot", vec!["3", "4"]),
        ("atan2", vec!["0", "0"]),
        ("sin", vec!["0"]),
        ("exp", vec!["0"]),
        ("fract", vec!["2"]),
    ] {
        let vals: Vec<Value> = args.iter().map(|s| bi(s)).collect();
        call(method, vals).unwrap_or_else(|e| panic!("math.{method}(BigInt) 本就应接受: {e}"));
    }
}
