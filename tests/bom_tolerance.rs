//! v0.104.6 D191：**UTF-8 BOM 会让第一行被静默丢弃** —— 且它先被误记成「REPL 的产品缺陷」
//!
//! ## 事情的经过（这本身是本轮最值得记的部分）
//!
//! D190 我记下一条「缺陷」：**REPL 的第一条输入必然 `parse error`**，并注明
//! 「根因未定位，本轮不臆断」。D191 接着查，加临时探针打出了真相：
//!
//! ```text
//! [D191DBG] pending="\u{feff}print(\"hi\")\n"
//!          err="Unexpected character '\u{feff}' at line 1, column 1"
//! ```
//!
//! —— 那是**我自己的 PowerShell 探针**往 stdin 注入的 BOM。改用字节精确的
//! 文件重定向（首 3 字节 `112,114,105` = `pin`）后，REPL **完全正常**：
//!
//! ```text
//! mora> hi
//! mora> 2.0
//! ```
//!
//! **「缺陷」是我的测量装置造的。** 教训与 D154/D166 同源，只是方向反过来：
//! 那两次是「已被否证的结论被重新误判」，这次是**从未存在的缺陷被凭空测出来**。
//! 差一步就把它当产品问题写进 CHANGELOG。
//!
//! ## 但 BOM 本身是真问题（不是产品缺陷的那一半之外）
//!
//! 剥掉那层装置问题之后，剩下一个**真实**的健壮性缺口：
//!
//! - 文件带 BOM → `mora file.mora` 整个解析失败
//!   （`Unexpected character '\u{feff}' at line 1, column 1`）；
//! - 管道/REPL 带 BOM → **第一行被静默丢弃**，无任何提示。
//!
//! 触发面很常见：Windows PowerShell 的 `Set-Content` / `Out-File`（默认
//! UTF-8 带 BOM）、部分编辑器与工具链的「另存为」。**BOM 是编码产物，
//! 不是源码内容** —— 在工具边界容忍它是对的，用户不该因为工具多写了一个
//! 编码标记而收到语法错误。
//!
//! ## 修法
//!
//! - `cli::read_source()`：读文件后剥**开头**一个 U+FEFF（`run_file` /
//!   `run_check` / `record` / `replay` / `snapshot` 全部走它）；
//! - REPL：首行剥一次。**必须剥在 `line` 本身**上 —— `pending.push_str(line.as_str())`
//!   用的是 `line`，只改 `trimmed` 不生效（我第一版就写错了这个位置）。
//!
//! 源码**中间**的 U+FEFF 仍是真实字符，仍应被拒。
//!
//! ## 判据
//!
//! ① 带 BOM 的**文件**必须能跑；② 带 BOM 的 **REPL 管道**第一行不得丢；
//! ③ 无 BOM 输入不受影响（负对照）；④ 只剥开头一个（中间的不剥）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 工作目录守卫。
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d191_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    /// 按**精确字节**写文件：`bom` 决定是否加 UTF-8 BOM。
    fn script(&self, name: &str, body: &str, bom: bool) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, body.as_bytes()).expect("写字节");
        if bom {
            let mut b = vec![0xEF, 0xBB, 0xBF];
            b.extend_from_slice(body.as_bytes());
            std::fs::write(&p, &b).expect("写 BOM");
        }
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .args(args)
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 起一个 `--repl`，喂入**精确字节**后关闭 stdin。
fn run_repl(dir: &Path, body: &str, timeout_ms: u64) -> (String, bool) {
    let mut child = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .arg("--repl")
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("起 mora --repl");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(body.as_bytes()).expect("写 stdin");
    }
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

/// **主判据**：带 BOM 的**文件**必须能跑（修前整份解析失败）。
#[test]
fn d191_bom_prefixed_file_runs() {
    let dir = WorkDir::new("file");
    let f = dir.script("b.mora", "print(42)\n", true);
    let bytes = std::fs::read(&f).expect("读");
    assert_eq!(
        &bytes[0..3],
        &[0xEF, 0xBB, 0xBF],
        "前提：文件确实带 BOM，否则本测试无意义"
    );

    let (out, code) = mora(dir.path(), &[f.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "带 BOM 的文件应能运行（修前报 Unexpected character + BOM 字符）:\n{}",
        out
    );
    assert!(out.contains("42.0"), "应正常输出:\n{}", out);
    assert!(!out.contains("feff"), "不应再报 BOM 字符错误:\n{}", out);
}

/// **主判据**：带 BOM 的 **REPL 管道**第一行**不得丢**（修前静默丢失）。
#[test]
fn d191_bom_prefixed_repl_input_keeps_the_first_line() {
    let dir = WorkDir::new("repl");
    // 显式带 BOM 的字节流（不是靠编码器碰运气）。
    let mut body: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    body.extend_from_slice(b"print(7)\nprint(8)\n");
    let body = String::from_utf8(body).expect("utf8");

    let (out, exited) = run_repl(dir.path(), &body, 8000);
    assert!(exited, "REPL 应在 EOF 后退出");
    assert!(
        out.contains("7.0"),
        "**第一行被静默丢弃** —— 带 BOM 的管道输入下 `print(7)` 必须执行:\n{}",
        out
    );
    assert!(out.contains("8.0"), "第二行也应执行:\n{}", out);
}

/// **负对照**：无 BOM 输入完全不受影响。
#[test]
fn d191_bomless_input_is_unaffected() {
    let dir = WorkDir::new("nobom");
    let (out, exited) = run_repl(dir.path(), "print(7)\nprint(8)\n", 8000);
    assert!(exited, "应正常退出");
    assert!(
        out.contains("7.0") && out.contains("8.0"),
        "两行都该执行:\n{}",
        out
    );

    let f = dir.script("n.mora", "print(42)\n", false);
    let (out, code) = mora(dir.path(), &[f.to_str().unwrap()]);
    assert_eq!(code, 0, "无 BOM 文件应照常运行:\n{}", out);
    assert!(out.contains("42.0"), "应正常输出:\n{}", out);
}

/// 只剥**开头**一个 —— 源码中间的 U+FEFF 仍是真实字符，应被拒。
#[test]
fn d191_only_the_leading_bom_is_stripped() {
    let dir = WorkDir::new("mid");
    // 第一行正常，第二行开头放一个 U+FEFF（真正的内容）。
    let f = dir.script("m.mora", "print(1)\n\u{FEFF}print(2)\n", false);
    let (out, code) = mora(dir.path(), &[f.to_str().unwrap()]);
    assert_ne!(
        code, 0,
        "源码中间的真实 U+FEFF 应仍被拒 —— 剥的范围只限开头一个:\n{}",
        out
    );
    // 报错里印的是**原始** U+FEFF 字符（不可见），不是 `feff` 六个字母。
    assert!(
        out.contains('\u{FEFF}'),
        "应报出源码中间那个真实的 U+FEFF:\n{}",
        out
    );
}
