//! v0.104.6 D269：`orchestrate loop` 的 `max_rounds` 被**静默丢弃**、
//! 多 `agent` 只跑第一个（已修）
//!
//! ## 修前实测（真实 CLI）
//!
//! ```mora
//! let acc = ""
//! orchestrate loop acc -> result
//!   agent a => input + "x"
//!   max_rounds: 5
//! end
//! print(len(result))
//! ```
//!
//! | 程序 | 修前 | 修后 |
//! |---|---|---|
//! | `max_rounds: 5` | **1000** | **5** ✓ |
//! | 不写 `max_rounds` | 1000 | 1000（不变）✓ |
//! | 两个 `agent` 行 | `FIRST` | `SECOND`（两个都跑）✓ |
//! | `max_rounds: abc` | 静默接受，跑 1000 轮 | **解析错误**，exit 2，指到该行 ✓ |
//!
//! ## 机制：两处孤立的漏写
//!
//! **① `max_rounds` 的值从未落地。** `parser_v3/syntax.rs` 里该分支原本是
//! 「识别关键字 → 吃掉冒号 → 把行尾 token 全部 `advance()` 掉」——
//! 整行被吞掉，**值一个字节都没读**。而 `Loop.rounds` 又在 kind 构造处
//! 写死 `Some(1000)`。两处叠加 ⇒ `max_rounds` 语法上被完全接受、
//! 语义上完全无效。
//!
//! **意图证据**（说明这是漏写而非设计）：lexer 早为它准备了专属 token
//! `TokenType::MaxRounds`；handler 侧有 `rounds.unwrap_or(1000)` 的消费点；
//! `runtime.rs` 的注释还写着「rounds 缺省为 1000（**与解析器
//! MirrorOrchestrateKind::Loop 一致**）」—— 作者以为解析器已经会产出这个值。
//!
//! **② 多 agent 被截断。** kind 构造处是 `agents.into_iter().next()`。
//! 而 `MirOrchestrateKind::Loop.agents` 本身是 `Vec`、handler 侧也是
//! `for agent in agents` 逐个执行，且 `sequential`/`graph`/`pregel`
//! 三个兄弟 kind **都原样保留整个 vec**。只有 `loop` 截断，
//! 且只发生在解析器这一处。
//!
//! ## 唯一一条既有测试为什么没抓到
//!
//! `interpreter::builtins::tests::orchestrate::orchestrate_loop_with_on_predicate_parses`
//! 里写着 `orchestrate loop x -> y, max_rounds: 5`，但它只断言
//! `compile_ok(src)` —— **只测「能不能解析」**。
//! 而修复前的解析器恰恰是靠**吞掉整行**才「解析成功」的。
//! ⇒ 该测试对「值有没有被采纳」**零鉴别力**：把它改成真解析或继续丢弃，
//! 它都绿。本文件的行为级断言（数轮数）才有牙齿。
//!
//! ## 观测手法
//!
//! 循环体里的 agent 读的是 `input`（pregel 契约，见 `runtime.rs` 的
//! Sequential 分支注释），而**表头声明的输入变量名只用于给首轮播种**
//! （`env.get(input_var)`）。因此 `let acc = ""` + `agent a => input + "x"`
//! ⇒ `max_rounds: N` 时结果恰为 N 个 `x`，轮数可直接数。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(source: &str) -> Result<mora::value::Value, String> {
    let (func, _w) = ParserV3::compile(source).map_err(|e| format!("compile: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
}

/// 跑一个「每轮给结果加一个 `x`」的循环，返回结果的**字符长度**（即轮数）。
fn round_count(max_rounds: Option<&str>) -> usize {
    let line = match max_rounds {
        Some(v) => format!("  max_rounds: {v}\n"),
        None => String::new(),
    };
    let src = format!(
        "let acc = \"\"\n\
         orchestrate loop acc -> result\n\
         \x20 agent a => input + \"x\"\n\
         {line}\
         end\n\
         result\n"
    );
    match run(&src) {
        // 注意：取 Value 里字符串的**真实长度**，不能用 `format!("{v:?}")` ——
        // 那是 Debug 表示（`String("x")`），会恒定多出 10 个字符。
        Ok(mora::value::Value::String(s)) => s.len(),
        Ok(v) => panic!("期望 String 结果，实际：{v:?}"),
        Err(e) => panic!("循环应跑通，实际报错：{e}"),
    }
}

/// **D269-A 主断言**：`max_rounds: 5` ⇒ 恰好 5 轮。
///
/// 修前为 1000（值被整行吞掉，`rounds` 写死 1000）。
#[test]
fn d269_max_rounds_is_honored() {
    assert_eq!(
        round_count(Some("5")),
        5,
        "`max_rounds: 5` 必须只跑 5 轮 —— 修前跑 1000 轮（值被静默丢弃）"
    );
}

/// D269-A 的第二条：不同取值分别生效，不是一刀切的固定值。
#[test]
fn d269_max_rounds_respects_each_value() {
    for n in [1usize, 2, 7] {
        assert_eq!(
            round_count(Some(&n.to_string())),
            n,
            "`max_rounds: {n}` 应恰好跑 {n} 轮"
        );
    }
}

/// 回归守卫：**不写** `max_rounds` 时缺省仍是 1000。
///
/// 这是本次修改的**行为边界**：默认值必须与修前逐位一致，
/// 否则等于给所有既有的 `orchestrate loop` 程序改了语义。
#[test]
fn d269_max_rounds_default_is_unchanged() {
    assert_eq!(
        round_count(None),
        1000,
        "缺省仍是 1000 轮 —— `runtime.rs` 的 `rounds.unwrap_or(1000)` 契约不得被改动"
    );
}

/// `max_rounds: 0` 必须被钳到 **1** 轮（与同函数内 `top_k` 的 `.max(1)` 一致）。
///
/// 不得退化成「跑 0 轮」—— 那会让 `result` 停在播种值（`""`）上，
/// 看起来像一个完全正常的空结果。
#[test]
fn d269_max_rounds_zero_is_clamped_to_one() {
    assert_eq!(
        round_count(Some("0")),
        1,
        "`max_rounds: 0` 应钳到 1 轮，而不是 0 轮（那会让 result 停在播种值上）"
    );
}

/// 负数轮数（`max_rounds: -3`）必须是**解析错误**。
///
/// 负数在词法上就不是单个整数 token（`-` 与 `3` 分开），因此走到
/// 「非数字字面量 ⇒ 解析错误」那条分支。这正是本轮想要的：修前它被
/// **静默接受并忽略**（照跑 1000 轮）。
#[test]
fn d269_negative_max_rounds_is_a_parse_error() {
    let src = "let acc = \"\"\n\
               orchestrate loop acc -> result\n\
               \x20 agent a => input + \"x\"\n\
               \x20 max_rounds: -3\n\
               end\n\
               result\n";
    ParserV3::compile(src).expect_err("负数 max_rounds 应报解析错误，而非被静默忽略");
}

/// **D269-B 主断言**：`orchestrate loop` 里的**每个** agent 都要跑。
///
/// 修前只跑第一个（`agents.into_iter().next()`），第二个被静默丢弃。
/// 语义与 `orchestrate sequential` 一致：按声明顺序串联，后者是最终结果。
#[test]
fn d269_every_agent_in_loop_runs() {
    let res = run(r#"
let acc = ""
orchestrate loop acc -> result
  agent first => input + "1"
  agent second => input + "2"
  max_rounds: 1
end
result
"#)
    .expect("多 agent 循环应跑通");
    assert_eq!(
        format!("{res:?}"),
        "String(\"12\")",
        "两个 agent 应按序串联执行（修前只跑第一个，result 是 \"1\"）"
    );
}

/// 非数字的 `max_rounds` 必须是**解析错误**，而不是照单全收后丢弃。
///
/// 修前 `max_rounds: abc` 被静默接受并忽略（照跑 1000 轮）——
/// 「值写错」比「值被忽略」更难发现。本条把前者变成显式失败，
/// 与同函数内 `top_k` 的处理风格一致。
#[test]
fn d269_non_numeric_max_rounds_is_a_parse_error() {
    let src = "let acc = \"\"\n\
               orchestrate loop acc -> result\n\
               \x20 agent a => input + \"x\"\n\
               \x20 max_rounds: abc\n\
               end\n\
               result";
    let err = ParserV3::compile(src).expect_err("非数字的 max_rounds 应报解析错误");
    let msg = err.to_string();
    assert!(
        msg.contains("line 4") || msg.contains("line 3"),
        "解析错误应指出出错行号（`max_rounds` 所在行）。实际：{msg}"
    );
}
