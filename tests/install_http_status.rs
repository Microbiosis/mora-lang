//! v0.110.1: `mora install <url>` 的 **HTTP 状态校验**判据。
//!
//! ## 缺陷（本次修复）
//!
//! 旧实现只下载不看状态：`curl -L -o dest url` 对 HTTP 404 / 502 **退出码仍是 0**
//! （curl 默认把错误响应体也写进 `-o` 指向的文件）⇒ CLI 打印 `Installed`、
//! 写入 `mora.lock`，**404/502 被当成安装成功**，错误页以「已安装的包」形态留在
//! `vendor/` 里。
//!
//! ## 本文件守什么
//!
//! 起一个本地 HTTP 服务，按状态码返回响应，跑真实的 `mora install`：
//! - **404 / 502** ⇒ 退出码非 0、不打印 `Installed`、`vendor/` 下**不留任何文件**
//!   （错误页不得落盘）、`mora.lock` 不产生；
//! - **200** ⇒ 退出码 0、打印 `Installed`、包内容逐字落盘、`mora.lock` 登记 URL。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

use mora::test_support::TempDir;

/// 起一个只服务一次请求的本地 HTTP 服务，返回 `(port, join_handle)`。
fn serve_once(status_line: &'static str, body: &'static str) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        // 读掉请求头（读到 \r\n\r\n 为止）——不读会让客户端等不到响应。
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (port, handle)
}

/// 本机是否有可用的下载工具（curl 或 wget）——都没有则跳过本文件。
fn downloader_available() -> bool {
    for tool in ["curl", "wget"] {
        let ok = Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    false
}

/// 跑一次 `mora install <url>`，返回 `(exit_code, stdout, stderr)`。
fn run_install(dir: &TempDir, url: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("install")
        .arg(url)
        .current_dir(dir.path())
        .output()
        .expect("跑 mora install");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn vendor_entries(dir: &TempDir) -> Vec<String> {
    let vendor = dir.path().join("vendor");
    let Ok(rd) = std::fs::read_dir(&vendor) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn install_rejects_http_404_without_writing_the_error_page() {
    if !downloader_available() {
        eprintln!("跳过：本机既无 curl 也无 wget");
        return;
    }
    let dir = TempDir::new("install_404");
    let (port, server) = serve_once("404 Not Found", "<html>404 not found</html>");
    let (code, stdout, stderr) = run_install(&dir, &format!("http://127.0.0.1:{port}/pkg.mora"));
    server.join().ok();

    assert_ne!(
        code, 0,
        "HTTP 404 必须以非 0 退出码失败；stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Installed") && !stderr.contains("Installed"),
        "404 绝不能打印 Installed；stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("404"),
        "失败信息必须报出 HTTP 状态码；stderr={stderr}"
    );
    assert!(
        vendor_entries(&dir).is_empty(),
        "404 不得在 vendor/ 留下任何文件（错误页不得落盘）；实得 {:?}",
        vendor_entries(&dir)
    );
    assert!(
        !dir.path().join("mora.lock").exists(),
        "404 不得写 mora.lock"
    );
}

#[test]
fn install_rejects_http_502_without_writing_the_error_page() {
    if !downloader_available() {
        eprintln!("跳过：本机既无 curl 也无 wget");
        return;
    }
    let dir = TempDir::new("install_502");
    let (port, server) = serve_once("502 Bad Gateway", "<html>502 bad gateway</html>");
    let (code, stdout, stderr) = run_install(&dir, &format!("http://127.0.0.1:{port}/pkg.mora"));
    server.join().ok();

    assert_ne!(
        code, 0,
        "HTTP 502 必须以非 0 退出码失败；stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Installed") && !stderr.contains("Installed"),
        "502 绝不能打印 Installed；stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("502"),
        "失败信息必须报出 HTTP 状态码；stderr={stderr}"
    );
    assert!(
        vendor_entries(&dir).is_empty(),
        "502 不得在 vendor/ 留下任何文件；实得 {:?}",
        vendor_entries(&dir)
    );
    assert!(
        !dir.path().join("mora.lock").exists(),
        "502 不得写 mora.lock"
    );
}

#[test]
fn install_succeeds_on_http_200_and_records_the_lock_entry() {
    if !downloader_available() {
        eprintln!("跳过：本机既无 curl 也无 wget");
        return;
    }
    let dir = TempDir::new("install_200");
    let body = "print(\"hello\")\n";
    let (port, server) = serve_once("200 OK", body);
    let url = format!("http://127.0.0.1:{port}/pkg.mora");
    let (code, stdout, stderr) = run_install(&dir, &url);
    server.join().ok();

    assert_eq!(
        code, 0,
        "HTTP 200 必须成功；stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Installed"),
        "成功必须打印 Installed；stdout={stdout}"
    );
    let installed = dir.path().join("vendor").join("pkg.mora");
    assert!(
        installed.exists(),
        "包必须落到 vendor/pkg.mora；vendor 内容={:?}",
        vendor_entries(&dir)
    );
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap_or_default(),
        body,
        "落盘内容必须与响应体逐字一致"
    );
    assert_eq!(
        vendor_entries(&dir),
        vec!["pkg.mora".to_string()],
        "vendor/ 下只应有落地的包，不得残留暂存文件"
    );
    let lock = std::fs::read_to_string(dir.path().join("mora.lock")).unwrap_or_default();
    assert!(
        lock.contains("pkg") && lock.contains(&url),
        "mora.lock 必须登记包名与 URL；实得 {lock:?}"
    );
}
