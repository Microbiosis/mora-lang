//! v0.104.6 D285：`exec.parallel` 的 `max_concurrent` / `timeout_ms`
//! **负数被静默换算**，且同一句 `-1` 因数值类型不同行为天差地别（已修）
//!
//! ## 缺陷：错误消息**声称**的约束，代码并未强制
//!
//! 修前那一行的错误消息写着 `max_concurrent must be a non-negative number`，
//! 但那个 `_ =>` 分支**只挡了非数值类型**（`Value::Nil` 之类），
//! **根本没有非负检查**：
//!
//! | 参数 | `Float(-1.0)` | `Int(-1)` |
//! |---|---|---|
//! | `max_concurrent` | 饱和成 0 → `.max(1)` → **1** | **回绕**成 `usize::MAX` → **并发上限形同虚设** |
//! | `timeout_ms` | 饱和成 0 → `Duration::ZERO` ⇒ **立刻杀进程** | **回绕**成 `u64::MAX` ≈ 5.8 亿年 ⇒ **永不超时** |
//!
//! ## 实测（真实 CLI，`ping -n 4` 需要约 3 秒）
//!
//! | 源码 | 修前实测 | 修后 |
//! |---|---|---|
//! | `exec.parallel(cmds, 1, -1.0)` | **280ms** 返回（`Duration::ZERO` 把命令秒杀，输出是 taskkill 的 `SUCCESS`） | 明确报错 |
//! | `exec.parallel(cmds, 1, json.parse("-1"))` | **3106ms** 返回（超时**完全失效**，命令跑满全程） | 明确报错 |
//!
//! ⇒ 同一句源码里的 `-1`，一边「立刻杀」一边「永不超时」；exit 都是 0、零诊断。
//!
//! ## 修法：改走 D246 的收口 `value_as_usize`
//!
//! **不动的部分**：`max_concurrent = 0` 仍钳到 1（既有行为）；
//! `max_concurrent` / `timeout_ms` 显式传 `nil` 与**缺参**同义。

use mora::interpreter::Interpreter;
use mora::value::Value;

fn exec(args: &[Value]) -> Result<Value, String> {
    let interp = Interpreter::new();
    interp.call_exec_method("parallel", args)
}

fn cmds() -> Value {
    Value::List(mora::value::list::List::from_vec(vec![Value::String(
        "echo x".into(),
    )]))
}

/// **主断言**：负数 `max_concurrent` 必须被拒绝（两种数值类型都是）。
///
/// 修前二者都**静默通过**：Float 得 1、Int 得 `usize::MAX`。
#[test]
fn d285_negative_max_concurrent_is_rejected() {
    for v in [Value::Float(-1.0), Value::Int(-1)] {
        let err = exec(&[cmds(), v.clone()]).expect_err("负数 max_concurrent 应被拒绝");
        assert!(
            err.contains("max_concurrent") && err.contains("non-negative"),
            "错误应点名 max_concurrent 且说明非负。实际：{err}（传入 {v:?}）"
        );
    }
}

/// **主断言**：负数 `timeout_ms` 必须被拒绝（两种数值类型都是）。
///
/// 修前 Float 得「立刻杀」、Int 得「永不超时」—— 同一句 `-1`，两种行为。
#[test]
fn d285_negative_timeout_ms_is_rejected() {
    for v in [Value::Float(-1.0), Value::Int(-1)] {
        let err = exec(&[cmds(), Value::Int(1), v.clone()]).expect_err("负数 timeout_ms 应被拒绝");
        assert!(
            err.contains("timeout_ms") && err.contains("non-negative"),
            "错误应点名 timeout_ms 且说明非负。实际：{err}（传入 {v:?}）"
        );
    }
}

/// **对照组 1**：正常的 `max_concurrent` 仍照常执行。
#[test]
fn d285_normal_max_concurrent_still_runs() {
    let v = exec(&[cmds(), Value::Int(1)]).expect("正常 max_concurrent 应成功");
    assert!(
        matches!(v, Value::List(ref l) if !l.is_empty()),
        "应正常返回命令结果列表，实际：{v:?}"
    );
}

/// **对照组 2**：`max_concurrent = 0` 的**既有行为**（钳到 1）必须保留。
///
/// 修前是 `(*n as usize).max(1)`，即 0 → 1。本条刻意**不改**它 ——
/// 要拒绝的只是「负数被静默换算」，不是「0 的钳位」。
#[test]
fn d285_zero_max_concurrent_still_clamps_to_one() {
    let v = exec(&[cmds(), Value::Int(0)]).expect("max_concurrent=0 仍应成功（既有行为）");
    assert!(
        matches!(v, Value::List(ref l) if !l.is_empty()),
        "max_concurrent=0 的既有行为是「当作 1 执行」，不该被本条改掉"
    );
}

/// **对照组 3**：显式 `nil` 与**缺参**同义（`max_concurrent` 走全部并发）。
#[test]
fn d285_explicit_nil_acts_like_omitted_arg() {
    let v = exec(&[cmds(), Value::Nil]).expect("nil 应等同缺参");
    assert!(
        matches!(v, Value::List(ref l) if !l.is_empty()),
        "nil 应照常执行。实际：{v:?}"
    );
    // 缺参同样应成功
    exec(&[cmds()]).expect("缺 max_concurrent 应成功");
}

/// **对照组 4**：正的 `timeout_ms` 仍照常执行（本条不能把超时功能弄坏）。
#[test]
fn d285_normal_timeout_still_runs() {
    let v = exec(&[cmds(), Value::Int(1), Value::Float(30_000.0)]).expect("正常 timeout_ms 应成功");
    assert!(
        matches!(v, Value::List(ref l) if !l.is_empty()),
        "应正常返回命令结果列表，实际：{v:?}"
    );
}
