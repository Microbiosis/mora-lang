//! v0.110.1: `vendor/` **历史残留的自动清扫**判据。
//!
//! ## 背景
//!
//! v0.110.1 之前的 `mora install` 不校验 HTTP 状态 ⇒ 404 / 502 的**错误页响应体**
//! 被写进 `vendor/<pkg>.mora` 并登记进 `mora.lock`。这些文件在磁盘上与
//! 「正常安装的包」**无从区分** —— 直到有人真的 import 它。
//!
//! ## 本文件守什么
//!
//! 清扫（`mora install --clean`，以及每次 `mora install` 前的自动清扫）必须：
//! - **删掉**旧版装出来的错误页残留（登记在 `mora.lock` + 不是合法 Mora 程序）；
//! - **保留**能编译的正常包；
//! - **保留**用户自己写坏、但既不在登记表也不像错误页的文件（不越权删别人的东西）；
//! - 连带**清掉**登记表里对应的死条目。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use mora::test_support::TempDir;

/// 旧版 install 装出来的 404 错误页。
const ERROR_PAGE: &str = "<html><body>404 Not Found</body></html>";

/// 一个合法、可编译的 Mora 包。
const GOOD_PACKAGE: &str = "export task hello()\n  return \"hi\"\nend\n";

/// 用户自己写的、语法有问题的文件（既不在登记表、也不像错误页）。
const USER_BROKEN_FILE: &str = "export task broken(\n";

/// 起一个只服务一次请求的本地 HTTP 服务，返回 `(port, join_handle)`。
fn serve_once(body: &'static str) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (port, handle)
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建目录");
    }
    std::fs::write(path, content).expect("写文件");
}

/// 铺一套「有残留」的现场：错误页 + 正常包 + 用户写坏的文件 + 登记表。
fn seed_residue(dir: &TempDir) {
    write_file(&dir.path().join("vendor").join("legacy.mora"), ERROR_PAGE);
    write_file(&dir.path().join("vendor").join("good.mora"), GOOD_PACKAGE);
    write_file(
        &dir.path().join("vendor").join("mine.mora"),
        USER_BROKEN_FILE,
    );
    write_file(
        &dir.path().join("mora.lock"),
        "legacy = \"http://h/legacy.mora\"\ngood = \"http://h/good.mora\"\n",
    );
}

fn run_mora(dir: &TempDir, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .args(args)
        .current_dir(dir.path())
        .output()
        .expect("跑 mora");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn install_clean_removes_legacy_error_page_and_keeps_everything_else() {
    let dir = TempDir::new("vendor_clean");
    seed_residue(&dir);

    let (code, stdout, stderr) = run_mora(&dir, &["install", "--clean"]);
    assert_eq!(code, 0, "--clean 必须成功；stdout={stdout} stderr={stderr}");

    let vendor = dir.path().join("vendor");
    assert!(
        !vendor.join("legacy.mora").exists(),
        "旧版装出来的错误页残留必须被删除"
    );
    assert!(vendor.join("good.mora").exists(), "能编译的正常包必须保留");
    assert!(
        vendor.join("mine.mora").exists(),
        "用户自己写坏、且不像错误页的文件不得被越权删除"
    );

    let lock = std::fs::read_to_string(dir.path().join("mora.lock")).unwrap_or_default();
    assert!(
        !lock.contains("legacy"),
        "登记表里的死条目必须一并清掉；实得 {lock:?}"
    );
    assert!(
        lock.contains("good"),
        "正常包的登记条目必须保留；实得 {lock:?}"
    );
    assert!(
        stdout.contains("legacy"),
        "清扫报告必须点名被删的包；stdout={stdout}"
    );
}

#[test]
fn install_also_sweeps_legacy_residue_automatically() {
    let dir = TempDir::new("vendor_auto_clean");
    seed_residue(&dir);

    let (port, server) = serve_once(GOOD_PACKAGE);
    let (code, stdout, stderr) = run_mora(
        &dir,
        &["install", &format!("http://127.0.0.1:{port}/pkg.mora")],
    );
    server.join().ok();

    assert_eq!(code, 0, "安装必须成功；stdout={stdout} stderr={stderr}");
    let vendor = dir.path().join("vendor");
    assert!(
        !vendor.join("legacy.mora").exists(),
        "每次 install 都应自动清掉历史残留"
    );
    assert!(vendor.join("pkg.mora").exists(), "新装的包必须落盘");
    assert!(
        vendor.join("good.mora").exists() && vendor.join("mine.mora").exists(),
        "无关文件不得被删"
    );
    let lock = std::fs::read_to_string(dir.path().join("mora.lock")).unwrap_or_default();
    assert!(
        !lock.contains("legacy") && lock.contains("good") && lock.contains("pkg"),
        "登记表必须只剩活条目；实得 {lock:?}"
    );
}

#[test]
fn clean_on_a_clean_vendor_is_a_no_op() {
    let dir = TempDir::new("vendor_clean_empty");
    write_file(&dir.path().join("vendor").join("hello.mora"), GOOD_PACKAGE);

    let (code, stdout, stderr) = run_mora(&dir, &["install", "--clean"]);
    assert_eq!(code, 0, "--clean 必须成功；stdout={stdout} stderr={stderr}");
    assert!(
        dir.path().join("vendor").join("hello.mora").exists(),
        "合法包不得被删除"
    );
    assert!(
        !dir.path().join("mora.lock").exists(),
        "没有死条目时不应凭空造出登记表"
    );
}
