//! v0.104.6 D277/D278：quasiquote 的 `,,splice` 标记在 witness 层**丢失**
//! ⇒ 9 层路径把它渲染成 `List([…])`（**已修**）
//!
//! ## 实测（修前）
//!
//! `tests/fixtures/e2e/quasiquote.mora`：
//!
//! | | 9 层路径（**默认**） | `MORA_9LAYER=0`（emit.rs 原路径） |
//! |---|---|---|
//! | quasiquote 展开的列表 | **`List([Float(1.0), Float(2.0), Float(3.0)])`** | `1, 2, 3` |
//!
//! **默认配置下用户拿到的就是错输出**，而差分判它「通过」（假阴性）。
//!
//! ## 根因：标记只进了 emit 侧的 `segments`，witness 里一个字节都没留
//!
//! `parser_v3::emit::emit_quasiquote_w` 里 `,,expr` 的处理：
//!
//! ```text
//! if is_splice { segments.push(UnquoteSplice(reg)) } else { segments.push(Unquote(reg)) }
//! witness_segments.push(w);        // ← 只有子表达式的 witness
//! ```
//!
//! ⇒ `segments`（带 `is_splice`）只喂给 **emit.rs 自己**那条路；
//! witness 里的 splice 段是**裸 `Variable("items")`**，与普通 unquote 段
//! **完全不可区分**（编译产物可直接印证）。
//!
//! ## 两处配套缺口，下游本就就绪
//!
//! | 位置 | 修前状态 |
//! |---|---|
//! | `parser_v3/emit.rs` | 标记**不产出** |
//! | `mir/witness_to_fcfg.rs` | 有消费约定（注释写着「UnquoteSplice 标记：`Literal(Boolean(true))`」）但**只跳过标记**、从不发射 `UnquoteSplice` |
//! | `mir/fcfg_lower.rs:498` | ✅ **已**正确处理 `UnquoteSplice` |
//! | `mir/ehir_to_core.rs:462` | ✅ **已**正确处理 `UnquoteSplice` |
//!
//! ⇒ 补上前两处即可点亮整条链，无需改动下游。
//!
//! ## 修后
//!
//! `` let items = [1,2,3] `` / `` let q = `,,items `` ⇒ 两条路径都是
//! **`1, 2, 3`**，普通 unquote 形态（`` `10+,x `` ⇒ `10+5`）**不受影响**。
//!
//! ## 本文件为何**只有一条测试**、且单独成文件
//!
//! 它要 `MORA_9LAYER=0` 切换编译路径，而**环境变量是进程全局的**：
//! ① 放在别的测试文件里会污染那里并行执行、同样要 spawn `mora.exe` 的测试；
//! ② 即便在本文件内，**多条**测试并行跑也会互相踩这个变量
//!    （本轮第一版就因此让对照判据假红）。
//! ⇒ 合并成一条串行断言 + 独立测试文件 = 互不干扰。

use std::process::Command;

const FIXTURE: &str = "tests/fixtures/e2e/quasiquote.mora";

/// 跑 `mora run` 并取 stdout（trim 尾部空白）。
fn run_with(env9: Option<&str>) -> String {
    // SAFETY: 本测试文件**只有这一条**测试在跑（见文件末说明），
    // 不存在与其他线程并发读环境变量的可能。
    unsafe {
        match env9 {
            Some(v) => std::env::set_var("MORA_9LAYER", v),
            None => std::env::remove_var("MORA_9LAYER"),
        }
    }
    let exe = env!("CARGO_BIN_EXE_mora");
    let out = Command::new(exe)
        .arg("run")
        .arg(FIXTURE)
        .output()
        .expect("应能执行 mora");
    // SAFETY: 同上（本文件单测试）。
    unsafe { std::env::remove_var("MORA_9LAYER") };
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// **D278 主断言**，三条合为一条串行：
///
/// ① 两条编译路径输出**一致**（修前 9 层给 `List([…])`、emit.rs 给 `1, 2, 3`）；
/// ② 具体值含 `code:1, 2, 3`（防两条路径一起错到同一个值）；
/// ③ 其余三种形态（纯引号 / unquote / 带括号）**未受影响**。
#[test]
fn d278_quasiquote_splice_agrees_across_both_paths() {
    let nine_layer = run_with(None);
    let emit_rs = run_with(Some("0"));

    assert_eq!(
        nine_layer, emit_rs,
        "两条编译路径的输出应一致（D278 已修）。\n\
         9layer : {nine_layer}\n\
         emit.rs: {emit_rs}"
    );

    assert!(
        nine_layer.contains("code:1, 2, 3"),
        "quasiquote 的 `,,items` 应展开为 `1, 2, 3`（不是 List 的 Display 转储）。\n\
         实际输出：{nine_layer}"
    );

    // fixture 的 5 行输出依次是：
    //   30.0               ← eval(`10+20)
    //   15.0               ← eval(`10+,x)   = 10+5（普通 unquote 经求值）
    //   code:1, 2, 3       ← `` `,,items ``  ← 本条要保护的那一行
    //   code:hello world   ← 纯引号
    //   30.0               ← eval(`(10+20))
    assert!(
        nine_layer.contains("code:1, 2, 3")
            && nine_layer.contains("code:hello world")
            && nine_layer.contains("15.0")
            && nine_layer.contains("30.0"),
        "quasiquote 的四种形态（纯引号 / unquote / splice / 带括号）都应照常工作。\n\
         实际输出：{nine_layer}"
    );
}
