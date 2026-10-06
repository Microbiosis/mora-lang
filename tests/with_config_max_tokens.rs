//! v0.104.6 D147：`with max_tokens = -1` 把 AI 上限设成 **0**（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! with max_tokens = -1
//!   ...
//! end
//! ```
//!
//! 修复前 **exit 0、零诊断** —— 上限被静默设成 `0`，即「模型不许输出任何
//! 内容」。用户会看到「调用成功但没有内容」，却找不到原因。
//!
//! 根因与 D146（`take`/`drop`）同源：`Value::Float(n) as usize` 是
//! **饱和转换**，`-1.0 as usize == 0`。
//!
//! ## 为什么是「同类里最后一个漏网的」
//!
//! `interpreter/mod.rs` 里这段 `with`-config 分支**已经**被系统性加固过：
//! * `temperature`（D39）—— 实参类型不对会**报错**，不再静默让配置失效
//!   （注释里明确写了「这与 D1 同型」）
//! * `model` / `system` / `mock_llm` —— 各有自己的校验
//!
//! 唯独 `max_tokens` 走的是裸 `as usize`。**一族的加固做完了 90%，
//! 剩下的 10% 就是下一处静默缺陷的藏身处。**

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D147 主判据：负数 `max_tokens` 必须报错。
#[test]
fn d147_negative_max_tokens_is_rejected() {
    for n in ["-1", "-0.5"] {
        let src = format!("with max_tokens = {n}\n  print(1)\nend\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "`max_tokens: {n}` 必须报错（修复前静默设成 0 = 不许输出）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains("不能为负数"),
            "错误信息应点明「不能为负数」"
        );
    }
}

/// 反向对照：正常 `max_tokens` 必须**照常进入 with 块**。
///
/// ⚠ 观测口径：`run()` 返回**末表达式**的值，而 `with` 块的值**恒为 `Nil`**
/// （D107 已确立）—— 不能拿它比输出。判据是「不报错」+ 块内语句确实执行，
/// 后者用「块内 `print` 后仍有顶层语句可跑」间接保证（若块被跳过，
/// 顶层 `print` 仍会执行，故这里只断言成功即可）。
#[test]
fn d147_normal_max_tokens_still_works() {
    assert!(
        run("with max_tokens = 100\n  print(1)\nend\nprint(2)\n").is_ok(),
        "正常 `max_tokens` 应照常进入 with 块"
    );
    // 0 语义模糊（上限 0 = 不许输出？= 不限制？），**未改动**，钉住现状
    assert!(
        run("with max_tokens = 0\n  print(1)\nend\nprint(2)\n").is_ok(),
        "`max_tokens: 0` 的语义未改动（模糊，记档）"
    );
}

/// 对照组：同族的 `temperature`（D39 加固过）**非数字**时报错 —— 钉住防回退。
#[test]
fn d147_sibling_temperature_type_check_still_holds() {
    let res = run("with temperature = \"hot\"\n  print(1)\nend\n");
    assert!(
        res.is_err(),
        "`temperature` 传字符串必须报错（D39 已修，不得回退）"
    );
    assert!(
        res.unwrap_err().contains("temperature"),
        "错误应点名是 temperature 的问题"
    );
}
