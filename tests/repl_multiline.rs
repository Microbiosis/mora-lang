//! v0.104.6 D17：**REPL 逐行编译，多行构造在交互式里根本进不去**。
//!
//! ## 现象
//!
//! `mora --repl` 用 `read_line` **一行一行**读，每行独立走
//! `ParserV3::compile`（`src/interpreter/mod.rs:736-748`），**没有多行累积**。
//! 于是 REPL 里最基本的东西用不了：
//!
//! ```text
//! mora> for x in [1,2]
//! parse error: ...
//! mora>   print(x)
//! type error: Unbound variable: x
//! mora> end
//! parse error: ...
//! ```
//!
//! `task` / `fn` / `for` / `while` / `if … end` / `handle … end` / `match` /
//! `with` / `worker` / `transaction` / `macro` / `observe` / `parallel`
//! ——凡是 spec §14.2 里**跨行**的 statement 产生式，交互式一律进不去。
//!
//! ## 为什么这是真缺陷而不是设计
//!
//! - REPL 是 CLI 的一等入口（`mora --repl`，`main.rs:89`），不是遗留功能；
//! - spec 的 EBNF（`docs/mora-spec.md:1228-1233`）里绝大多数 statement
//!   产生式**本身就是跨行的**；
//! - `run_repl_with` 已经在做**跨行状态保持**（`let x = 5` 后 `x + 1` 能用，
//!   靠 `repl_task_defs` 累积 `Define`）—— 说明**跨行累积的机制本来就存在**，
//!   只是没用来做「续行」：它累积的是已完成的定义，不是不完整的输入。
//!
//! 换句话说：把「解析失败」升级成「等下一行」是一个局部的状态机补充，
//! 而不是一个新功能。本文件是**缺陷复现与护栏**，修复见
//! `REPL 多行续行` 段落（见 `run_repl_with` 的实现）。

use std::io::Write;
use std::process::{Command, Stdio};

/// 启动 REPL，喂给它若干行，返回 (退出码, 全部输出)。
fn repl(input: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("--repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// REPL 应当能进入多行 `for` 循环。
///
/// 修前：首行 `for x in [1,2]` 立刻 `parse error`，`print(x)` 报
/// `Unbound variable: x`，`end` 又是一个 `parse error` —— 整个循环进不去。
#[test]
fn repl_accepts_multiline_for_loop() {
    let (_, out) = repl("for x in [1,2]\n  print(x)\nend\nexit\n");
    assert!(
        !out.contains("parse error"),
        "REPL 应能续行进入多行 for 循环，不该逐行报 parse error。实际输出：\n{out}"
    );
    assert!(
        !out.contains("Unbound variable"),
        "`print(x)` 的 x 应来自上一行的循环绑定。实际输出：\n{out}"
    );
    // 循环体应真的执行了两次
    assert!(
        out.matches("1.0").count() >= 1 && out.matches("2.0").count() >= 1,
        "for 循环体应执行并打印 1.0 与 2.0。实际输出：\n{out}"
    );
}

/// 多行 `task` 定义 + 调用。
#[test]
fn repl_accepts_multiline_task_definition() {
    let (_, out) = repl("task twice(n)\n  n * 2\nend\ntwice(21)\nexit\n");
    assert!(
        !out.contains("parse error"),
        "REPL 应能续行定义 task。实际输出：\n{out}"
    );
    assert!(out.contains("42"), "`twice(21)` 应得 42。实际输出：\n{out}");
}

/// 多行 `if … end`。
#[test]
fn repl_accepts_multiline_if_block() {
    let (_, out) =
        repl("let c = 1\nif c == 1\n  print(\"yes\")\nelse\n  print(\"no\")\nend\nexit\n");
    assert!(
        !out.contains("parse error"),
        "REPL 应能续行进入多行 if。实际输出：\n{out}"
    );
    assert!(
        out.contains("yes"),
        "c == 1 时应打印 yes。实际输出：\n{out}"
    );
}

/// 对照组：单行表达式与跨行状态保持**本来就正常**（`repl_task_defs`
/// 累积 `Define`），不能因为修多行续行而把它们弄坏。
#[test]
fn repl_single_line_and_state_carryover_still_work() {
    let (_, out) = repl("1 + 2\nexit\n");
    assert!(out.contains("3"), "单行 `1 + 2` 应得 3。实际输出：\n{out}");

    let (_, out2) = repl("let x = 5\nx + 1\nexit\n");
    assert!(
        out2.contains("6"),
        "跨行状态保持：`let x = 5` 后 `x + 1` 应得 6。实际输出：\n{out2}"
    );
}

/// 交互式 REPL 的另一条独立轴：它走**裸 `ParserV3::compile`**，
/// 不过 9 层管线（与 `import` / `eval()` 同一条路）。D14 已在**根因**
/// （witness 的 `Call("[]")` 未解码）修掉，所以这里只需要确认
/// `handle` 块内的索引在 REPL 里也正常。
#[test]
fn repl_handle_index_works() {
    let (_, out) = repl(
        "let t = [10,20]\nlet x = 0.0\nhandle random_random {\n  x = t[1]\n} {\n  0.5\n}\nx\nexit\n",
    );
    assert!(
        !out.contains("parse error"),
        "REPL 应能续行进入 handle 块。实际输出：\n{out}"
    );
    assert!(
        out.contains("20"),
        "handle body 里的 `t[1]` 应得 20（D14 修复后裸路径也正确）。实际输出：\n{out}"
    );
}

/// 坏输入**不能毒化会话**。
///
/// 这是修 D18 时自己踩到的坑：类型检查不通过时若不清空续行缓冲，那一段
/// 输入会一直留在里面，之后每一行都被追加到它后面、整段一起反复报同一个错
/// —— 用户敲一句 `exit` 之前什么都做不了。
#[test]
fn repl_recovers_after_a_type_error() {
    let (_, out) = repl("let x = 5\nx + \"nope\"\nlet y = 3\ny + 1\nexit\n");
    assert!(
        out.contains("type error"),
        "x + 字符串应报 type error。实际输出：\n{out}"
    );
    // 关键：报完错之后，后续输入仍要能正常工作
    assert!(
        out.contains("4"),
        "一次 type error 之后，`let y = 3` / `y + 1` 仍应得 4（会话未被毒化）。\
         实际输出：\n{out}"
    );
}

/// 放弃续行：空行是「这段写不下去了」的出口 —— 否则用户会被困在续行里
/// （Mora 的空行在 parser 里本就被忽略，拿它当退出信号是安全的）。
#[test]
fn repl_blank_line_abandons_pending_input() {
    let (_, out) = repl("for x in [1,2]\n\nlet y = 9\ny + 1\nexit\n");
    assert!(
        out.contains("10"),
        "空行放弃未完成的 for 之后，`let y = 9` / `y + 1` 仍应得 10。\
         实际输出：\n{out}"
    );
}

/// 配平判据不能被字符串/注释里的关键字骗到。
///
/// `str("end")` 与 `-- end` 都不该被算作块收尾，否则用户敲一行
/// `print(str("end"))` 就可能被误判成「结构闭合了」。
#[test]
fn repl_continuation_is_not_fooled_by_keywords_in_strings() {
    let (_, out) = repl("let a = str(\"end\")\na\nexit\n");
    assert!(
        !out.contains("parse error"),
        "`let a = str(\"end\")` 是完整语句，不该进续行缓冲。实际输出：\n{out}"
    );
    assert!(
        out.contains("= end"),
        "`a` 应得字符串 end。实际输出：\n{out}"
    );
}

/// `handle` 是**花括号**形态、没有 `end` —— 续行判据若按 `end` 计会永远
/// 卡住（这是我修 D18 时自己踩的坑：把 `handle` 误列进 `end` 型 opener）。
#[test]
fn repl_handle_brace_form_is_understood() {
    // `handle E { body } { handler }` 的**准确**形态（`} {` 在同一行）。
    // 若续行判据把 handle 按 `end` 型 opener 计，它会永远卡住
    // —— 这正是我修 D18 时自己踩的坑。
    let (_, out) = repl("handle random_random {\n  1.0\n} {\n  0.5\n}\nexit\n");
    assert!(
        !out.contains("parse error"),
        "花括号形态的 handle 应能续行完成。实际输出：\n{out}"
    );
}
