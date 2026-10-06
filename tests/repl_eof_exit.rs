//! v0.104.6 D190：`mora --repl` 在 **stdin 结束（EOF）后不退出** —— 永久挂起（已修）。
//!
//! ## 缺陷
//!
//! REPL 循环原先只判 `read_line(...).is_err()`：
//!
//! ```rust
//! if handle.read_line(&mut line).is_err() { break; }
//! ```
//!
//! 而 `read_line` 在 **EOF** 时返回 `Ok(0)`，**不是** `Err`。于是循环永远
//! 读到一个空串、trim 后为空、被当作「空行跳过」，**无限转下去**。
//!
//! 实测（真实 `mora --repl`，喂完输入后**关闭 stdin**）：
//!
//! ```text
//! ★ 8 秒后仍未退出 —— 确认挂起
//! ```
//!
//! 后果：`mora --repl < file`、CI 里喂脚本、任何**非交互**的管道用法
//! 都会挂死，只能强杀。启动横幅写着「type 'exit' to quit」—— 用户会以为
//! 必须敲 `exit`，而管道里根本没有机会敲。
//!
//! ## 修法
//!
//! 同时判 `Ok(0)`：
//!
//! ```rust
//! match handle.read_line(&mut line) {
//!     Ok(0) => break,   // EOF
//!     Ok(_) => {}
//!     Err(_) => break,
//! }
//! ```
//!
//! 「stdin 结束即退出」是所有交互式工具的惯例，**也是唯一能让 REPL 可被
//! 脚本驱动的前提**。
//!
//! ## 另一个**尚未定位**的缺陷（只记，不修）
//!
//! 实测同时发现：**REPL 的第一条输入必然** `parse error: 语法错误，已放弃当前输入`，
//! 而同样的文本写成文件跑得通：
//!
//! ```text
//! [print("hi")]        → parse error（首行）
//! [print(1), print(2)] → parse error（首行）；第二行正常打印 2.0
//! [let a = 1, a]       → parse error（首行）；依赖 a 的第二行静默无输出
//! ```
//!
//! 根因**未定位**（`ParserV3::compile` 对同一文本在文件里能过、在 REPL 的
//! 首行上失败），本轮不臆断。已记入 CHANGELOG 待查。

use std::io::Write;
use std::process::{Command, Stdio};

/// 起一个 `--repl`，喂入若干行后**关闭 stdin**（= 触发 EOF），
/// 返回 (stdout+stderr, 是否在时限内自行退出)。
fn run_repl(lines: &[&str], timeout_ms: u64) -> (String, bool) {
    let dir = std::env::temp_dir();
    let mut child = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(&dir)
    .arg("--repl")
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("起 mora --repl");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for l in lines {
            writeln!(stdin, "{l}").expect("写 stdin");
        }
    } // 关闭 stdin —— 这是关键：没有它就测不到 EOF 行为
    drop(child.stdin.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => {
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut out);
                }
                return (out, true);
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (String::new(), false);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// **主判据（有牙齿）**：关闭 stdin 后 REPL 必须**自行退出**。
///
/// 修前会一直挂到超时（本测试用 8 秒；挂起是无限期的）。
#[test]
fn d190_repl_exits_on_eof() {
    let (out, exited) = run_repl(&["print(1)"], 8000);
    assert!(
        exited,
        "stdin 关闭后 REPL 仍在运行 —— 修前只判 `is_err()`，而 EOF 返回 `Ok(0)`。"
    );
    assert!(
        out.contains("Bye!") || !out.trim().is_empty(),
        "退出前应留下输出:\n{}",
        out
    );
}

/// 空输入也必须能退出 —— EOF 是 EOF，与输入内容无关。
#[test]
fn d190_repl_exits_on_eof_with_no_input_at_all() {
    let (_out, exited) = run_repl(&[], 8000);
    assert!(exited, "完全没有输入时，stdin 立即 EOF，REPL 应立刻退出");
}

/// 多行输入后同样必须退出（构造「喂了一整段脚本」的管道用法）。
#[test]
fn d190_repl_exits_on_eof_after_many_lines() {
    let (_out, exited) = run_repl(&["print(1)", "print(2)", "print(3)"], 8000);
    assert!(exited, "喂完整段输入后关闭 stdin，REPL 应自行退出");
}

/// **对照组**：`exit` 一词仍然可用（修的是 EOF，不是把 `exit` 弄坏）。
#[test]
fn d190_repl_still_honours_the_exit_word() {
    let (out, exited) = run_repl(&["exit"], 8000);
    assert!(exited, "敲 `exit` 应当立刻退出");
    assert!(out.contains("Bye!"), "应打印告别语:\n{}", out);
}
