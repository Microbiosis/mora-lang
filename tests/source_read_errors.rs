//! v0.104.6 D192：I/O 错误**丢原因** —— record 家族把「读不到」的所有情形
//! 压成同一句话；而 `mora` / `--check` 透出的是 `stream did not contain valid UTF-8`
//! 这种**没说下一步该做什么**的原始 io::Error（已修）。
//!
//! ## 缺陷一：record 家族把错误**整个丢掉**
//!
//! ```rust
//! fs::read_to_string(path).unwrap_or_else(|_| {
//!     eprintln!("record: failed to read {}", path);   // ← `|_|` 把原因扔了
//! });
//! ```
//!
//! 于是**编码错误 / 权限不足 / 「那是个目录」全部退化成同一句**
//! `record: failed to read <path>`，用户无从判断该做什么。实测：
//!
//! ```text
//! $ mora record gbk.mora gk          # GBK 编码的文件
//! record: failed to read gbk.mora                 ← 原因没了
//! ```
//!
//! ## 缺陷二：透出来的原因**不可操作**
//!
//! `mora` / `--check` 走 `fs::read_to_string` 的原始 io::Error：
//!
//! ```text
//! $ mora gbk.mora
//! gbk.mora: stream did not contain valid UTF-8     ← 准确，但没说该做什么
//! ```
//!
//! 对本语言的读者这不是边角情况：**Windows 的部分工具默认写 GBK/GB18030**。
//!
//! ## 修法
//!
//! - `cli::read_source()` 把 `ErrorKind::InvalidData`（就是 UTF-8 解码失败）
//!   翻译成「请用编辑器**另存为 UTF-8**」并点出常见来源；
//! - `record` / `replay` / `snapshot` 的 `unwrap_or_else(|_| …)` 改成 `|e|`，
//!   把**原因**带出来。
//!
//! **不**尝试猜测/转换编码 —— 那属语言/工具链的**设计决定**（要支持 GBK 得引入
//! 编码探测），未擅自做。
//!
//! ## 判据
//!
//! ① 五个读取路径都必须报出**原因**；② 编码错误的消息必须**可操作**
//! （含「另存为 UTF-8」）；③ **负对照**：文件真的不存在时仍要照实说
//! 「找不到」，不能被守卫吞成「编码问题」。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d192_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 写一个**真实的 GBK 编码** `.mora` 文件（`# 注释\nprint(1)\n`）。
///
/// 「注」= `D7 DA`、「释」= `CA A4`（GBK/CP936）。`D7` 是双字节首字节，
/// 而后继的 `DA` 又是个首字节 —— 这组字节**不可能**是合法 UTF-8，
/// 正是 Windows 中文工具「另存为」默认产出的那类文件。
fn write_gbk(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"# \xD7\xDA\xCA\xA4\n"); // "# 注释"
    bytes.extend_from_slice(b"print(1)\n");
    std::fs::write(&p, &bytes).expect("写 GBK 文件");
    // 前提：它确实**不是** UTF-8（否则本测试无意义）。
    assert!(
        std::str::from_utf8(&bytes).is_err(),
        "前提：这些字节必须是非法 UTF-8"
    );
    p
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

/// 五个会读源文件的入口。
const READERS: &[&[&str]] = &[
    &["__FILE__"],
    &["--check", "__FILE__"],
    &["record", "__FILE__", "rec"],
    &["replay", "__FILE__", "rec"],
    &["snapshot", "__FILE__", "snap"],
];

/// **主判据**：每条路径都必须报出**原因**（而不是只有「读不到」）。
#[test]
fn d192_every_reader_reports_the_reason() {
    let dir = WorkDir::new("reason");
    let f = write_gbk(dir.path(), "gbk.mora");
    let f = f.to_str().unwrap();

    for tmpl in READERS {
        let args: Vec<&str> = tmpl
            .iter()
            .map(|a| if *a == "__FILE__" { f } else { *a })
            .collect();
        let (out, code) = mora(dir.path(), &args);
        assert_ne!(code, 0, "[{}] 应判失败", args.join(" "));
        // 「读不到」后面必须**跟着原因**。
        let after = out
            .split("failed to read")
            .nth(1)
            .or_else(|| out.split(".mora:").nth(1))
            .unwrap_or_else(|| panic!("[{}] 输出里连「读不到」都没有:\n{}", args.join(" "), out));
        assert!(
            after.contains("UTF-8") || after.contains("os error"),
            "[{}] 丢了原因 —— 只有「读不到」没有「为什么」:\n{}",
            args.join(" "),
            out
        );
    }
}

/// 编码错误的消息必须**可操作**（告诉用户下一步做什么）。
#[test]
fn d192_encoding_error_tells_the_user_what_to_do() {
    let dir = WorkDir::new("actionable");
    let f = write_gbk(dir.path(), "gbk.mora");
    let f = f.to_str().unwrap();

    for args in [vec![f], vec!["--check", f], vec!["record", f, "rec"]] {
        let (out, code) = mora(dir.path(), &args);
        assert_ne!(code, 0, "[{}] 应判失败", args.join(" "));
        assert!(
            out.contains("另存为 UTF-8"),
            "[{}] 编码错误必须给出**可操作**的下一步（修前只有 `stream did not \
             contain valid UTF-8`）:\n{}",
            args.join(" "),
            out
        );
    }
}

/// **负对照**：文件**真的**不存在时仍要照实说「找不到」——
/// 不能被新消息吞成「编码问题」。
#[test]
fn d192_missing_file_still_says_not_found() {
    let dir = WorkDir::new("missing");
    for args in [
        vec!["definitely-absent.mora"],
        vec!["--check", "definitely-absent.mora"],
        vec!["record", "definitely-absent.mora", "rec"],
    ] {
        let (out, code) = mora(dir.path(), &args);
        assert_ne!(code, 0, "[{}] 应判失败", args.join(" "));
        assert!(
            out.contains("系统找不到指定的文件") || out.contains("No such file"),
            "[{}] 文件真的不存在时必须照实说，不能被编码消息吞掉:\n{}",
            args.join(" "),
            out
        );
        assert!(
            !out.contains("另存为 UTF-8"),
            "[{}] 这与编码无关，不该提「另存为 UTF-8」:\n{}",
            args.join(" "),
            out
        );
    }
}

/// 正常 UTF-8 文件完全不受影响（负对照）。
#[test]
fn d192_utf8_file_still_runs() {
    let dir = WorkDir::new("ok");
    let p = dir.path().join("ok.mora");
    std::fs::write(&p, "print(1)\n").expect("写");
    let (out, code) = mora(dir.path(), &[p.to_str().unwrap()]);
    assert_eq!(code, 0, "普通 UTF-8 文件应照常运行:\n{}", out);
    assert!(out.contains("1.0"), "应正常输出:\n{}", out);
    assert!(!out.contains("另存为"), "不该出现编码提示:\n{}", out);
}
