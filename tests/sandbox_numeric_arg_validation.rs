//! v0.104.6 D283：`sandbox.*` 的用户数值参数**负数被静默换算**成合法 id / 0 核 0 内存（已修）
//!
//! ## 实测（修前）
//!
//! ```mora
//! sandbox.revoke(-1)
//! -- → Runtime error (MIR): sandbox.revoke: capability token 0 not found (revoked?)
//! ```
//!
//! `-1` 被解析成 `Value::Float(-1.0)`，`as u64` **饱和成 0**，
//! 于是去查 token 0 —— 错误信息把排查方向引到 **token 0**，
//! 而真正的问题（传了负数）被完全掩盖。
//!
//! 同一形状还有：`Value::Int(-1) as u64` 会**回绕成 `u64::MAX`**（1.8e19）。
//!
//! ## 影响面（本文件 4 处，此前全是裸 `as`）
//!
//! | 参数 | 目标类型 | 负数的修前结果 |
//! |---|---|---|
//! | `check_call(token_id, …)` | `u64` | 饱和成 **0** / 回绕成 `u64::MAX` |
//! | `revoke(token_id)` | `u64` | 同上 |
//! | `containerize(cpu_cores)` | `u32` | `Some(0)` —— **0 核** |
//! | `containerize(memory_mb)` | `u64` | `Some(0)` —— **0 内存** |
//!
//! 后两者比 `None`（不限）**更危险**：`Some(0)` 是一个「看起来成功」的
//! 限额。而它们的邻居参数（`mounts` / `network` / `image`）已在 D152
//! 修过同族的「传错类型 ⇒ 静默走默认」，**唯独这两个数值参数漏了**。
//!
//! ## 修法：改走 D246 立的收口
//!
//! [`crate::flow::value_as_usize`] 的文档本来就写着「负数返回 `None`
//! （而不是让 `as usize` 饱和成 0）—— 调用方据此报错或取显式默认值，
//! **不替它猜**」。本文件原先没用它。
//!
//! `cpu_cores` 另加 `u32` 上界校验 —— 直接 `as u32` 会**静默截断**
//! （如 `4294967297` → 1）。

use mora::interpreter::Interpreter;
use mora::value::Value;

fn call(method: &str, args: &[Value]) -> Result<Value, String> {
    let interp = Interpreter::new();
    interp.call_sandbox_method(method, args)
}

/// **主断言**：负数 `token_id` 必须被**明确拒绝**，而不是被换算成别的 id。
///
/// 修前 `sandbox.revoke(-1)` 报的是「capability token 0 not found」——
/// 错误信息指向 token 0，本条则要求它直说「负数」。
#[test]
fn d283_negative_token_id_is_rejected_not_coerced() {
    for v in [Value::Float(-1.0), Value::Int(-1)] {
        let err = call("revoke", std::slice::from_ref(&v)).expect_err("负数 token_id 应被拒绝");
        assert!(
            err.contains("non-negative"),
            "错误应直指「非负数」，而不是伪报一个 token id。实际：{err}（传入 {v:?}）"
        );
        // 修前的症状：提到 token 0
        assert!(
            !err.contains("token 0"),
            "错误不得把负数伪报成 token 0（那是 `Float(-1.0) as u64` 饱和的结果）。实际：{err}"
        );
    }
}

/// `check_call` 的 `token_id` 同上（另一处裸 `as`）。
#[test]
fn d283_check_call_rejects_negative_token_id() {
    let err = call(
        "check_call",
        &[Value::Float(-1.0), Value::String("file.read".into())],
    )
    .expect_err("负数 token_id 应被拒绝");
    assert!(
        err.contains("non-negative"),
        "check_call 的 token_id 也应校验非负。实际：{err}"
    );
}

/// 负数 `cpu_cores` 不得变成 `Some(0)`（0 核比「不限」更危险）。
#[test]
fn d283_negative_cpu_cores_is_rejected() {
    let args = vec![
        Value::String("docker".into()),
        Value::List(mora::value::list::List::from_vec(vec![])),
        Value::String("isolated".into()),
        Value::Float(-1.0),
    ];
    let err = call("containerize", &args).expect_err("负数 cpu_cores 应被拒绝");
    assert!(
        err.contains("cpu_cores") && err.contains("non-negative"),
        "错误应点名 cpu_cores 且直指非负。实际：{err}"
    );
}

/// 负数 `memory_mb` 同理（不得变成 0 内存）。
#[test]
fn d283_negative_memory_mb_is_rejected() {
    let args = vec![
        Value::String("docker".into()),
        Value::List(mora::value::list::List::from_vec(vec![])),
        Value::String("isolated".into()),
        Value::Float(1.0),
        Value::Float(-5.0),
    ];
    let err = call("containerize", &args).expect_err("负数 memory_mb 应被拒绝");
    assert!(
        err.contains("memory_mb") && err.contains("non-negative"),
        "错误应点名 memory_mb 且直指非负。实际：{err}"
    );
}

/// **对照组**：合法但未知的非负 `token_id` 仍走**原路径**（未被新校验拦下）。
///
/// 这是本次修复的行为边界：只拒绝「本来就该拒绝」的，不动其余一切。
#[test]
fn d283_valid_token_id_still_reaches_the_original_path() {
    let err = call("revoke", &[Value::Float(0.0)]).expect_err("token 0 确实不存在");
    assert!(
        err.contains("token 0 not found"),
        "合法的非负 token 仍应走到 CapabilityStore 的 TokenNotFound 路径。实际：{err}"
    );
    assert!(
        !err.contains("non-negative"),
        "合法的非负 token 不该触发新校验。实际：{err}"
    );
}

/// `u32` 上界：`as u32` 会把 `4294967297` **静默截断成 1**。
#[test]
fn d283_cpu_cores_rejects_values_above_u32_range() {
    let args = vec![
        Value::String("docker".into()),
        Value::List(mora::value::list::List::from_vec(vec![])),
        Value::String("isolated".into()),
        Value::Float(4294967297.0),
    ];
    let err = call("containerize", &args).expect_err("超 u32 上界应被拒绝而非截断");
    assert!(
        err.contains("cpu_cores"),
        "错误应点名 cpu_cores。实际：{err}"
    );
}
