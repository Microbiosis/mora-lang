//! v0.104.6 D214：HTTP 504 应答的 reason phrase 写的是 **`OK`** ——
//! 状态码与文案**互相矛盾**（已修）。
//!
//! ## 缺陷
//!
//! `http_server.rs::send_response` 里的状态码 → 文案映射：
//!
//! ```rust
//! let status_text = match status {
//!     200 => "OK",
//!     400 => "Bad Request",
//!     404 => "Not Found",
//!     500 => "Internal Server Error",
//!     _ => "OK",          // ← 兜底说「OK」
//! };
//! ```
//!
//! 而 **504 会被真实发出**（handler 60s 超时那一条分支），却**不在 match 里**
//! —— 于是落进兜底：
//!
//! ```text
//! HTTP/1.1 504 OK
//! ```
//!
//! reason phrase 虽是 RFC 9110 的遗留字段、多数客户端忽略，但 `curl -v`、
//! 代理与日志都直接显示它；更要紧的是**任何将来新增的状态码都会静默被标成
//! "OK"** —— 与 D193（triggerCharacters）/ D201（legend 越界）/ D213（错误包进
//! result）同族的「声明一样、做成另一个样」。
//!
//! ## 修法
//!
//! 抽成独立纯函数 `pub fn status_text`（可被测试直接钉住）、**补齐 504**、
//! 兜底改成 `"Unknown"` —— 宁可说「不知道」，也不说「OK」。
//!
//! ## 判据
//!
//! 主判据是一条**不变式**而不是逐个码点表：
//! **只有 `200` 的文案允许是 `"OK"`**。这条对**将来新增的任何状态码**都成立，
//! 比「504 必须映射成 Gateway Timeout」更难绕过 —— 后者只锁住一个码点。
//!
//! 另有：真实起一个 HTTP 服务，`curl` 风格的原始字节核对状态行
//! （`HTTP/1.1 404 Not Found` 而不是 `404 OK`）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use mora::http_server::status_text;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d214_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **主判据（有牙齿）**：只有 `200` 的文案允许是 `"OK"`。
///
/// 修前：504（以及任何未列出的码）都得到 `"OK"`。
#[test]
fn d214_only_200_may_say_ok() {
    // 服务器真实会发出的状态码
    for st in [200u16, 400, 404, 500, 504] {
        let phrase = status_text(st);
        if st == 200 {
            assert_eq!(phrase, "OK", "200 的文案应是 OK，实得 {phrase}");
        } else {
            assert_ne!(
                phrase, "OK",
                "状态 {st} 的 reason phrase 不可能是 \"OK\" —— \
                 状态码与文案互相矛盾（`HTTP/1.1 {st} OK`）"
            );
            assert!(!phrase.is_empty(), "状态 {st} 的文案不应为空");
        }
    }
    // **兜底也必须诚实**：未列出的码不能回落到 "OK"
    for st in [418u16, 502, 503, 301] {
        assert_ne!(
            status_text(st),
            "OK",
            "未列出的状态 {st} 竟回落到 \"OK\" —— 新增状态码会被静默标成成功"
        );
    }
}

/// **不回归**：已列出的码必须保持标准文案（不能为了「不撒谎」而全变 Unknown）。
#[test]
fn d214_known_statuses_keep_standard_reason_phrases() {
    for (st, want) in [
        (200u16, "OK"),
        (400, "Bad Request"),
        (404, "Not Found"),
        (500, "Internal Server Error"),
        (504, "Gateway Timeout"),
    ] {
        assert_eq!(status_text(st), want, "状态 {st} 的文案应为 {want:?}");
    }
}

/// **端到端**：真起一个服务，用原始字节核对**状态行**。
///
/// 走真实的 404 分支（不存在的路由）—— 不依赖 60s 超时。
#[test]
fn d214_real_404_status_line_is_not_says_ok() {
    let _d = WorkDir::new("e2e");
    let dir = _d.0.clone();
    // 一个只有一个路由的服务；请求一个**不存在**的路径 → 404
    //
    // ⚠ `Router::new()` **不收参数**，启动方法是 `listen(addr)`（不是 `serve()`）
    // —— 我第一版写成 `Router::new("127.0.0.1", 8931)` + `r.serve()`，
    // 直接报 `Type mismatch: expected router, got fn`，服务压根没起。
    let prog = dir.join("srv.mora");
    std::fs::write(
        &prog,
        "let r = Router::new()\nr = r.route(\"GET\", \"/ping\", fn(req)\n  return \"pong\"\nend)\nr.listen(\"127.0.0.1:8931\")\n",
    )
    .expect("写服务脚本");

    let mut child = std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("run")
    .arg(&prog)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("起服务");

    let mut body = String::new();
    let got = std::thread::spawn(move || {
        // 轮询到端口可连为止（最多 ~5s）
        for _ in 0..50 {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", 8931)) {
                let _ = s.write_all(b"GET /no-such-route HTTP/1.1\r\nHost: x\r\n\r\n");
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                return Some(buf);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    })
    .join();

    let _ = child.kill();
    let _ = child.wait();
    body.push_str(
        &got.unwrap_or_else(|_| panic!("连接线程异常"))
            .expect("连不上服务端口，应有应答"),
    );

    let status_line = body.lines().next().unwrap_or("");
    assert!(
        status_line.starts_with("HTTP/1.1 404 "),
        "404 的状态行应显式写 `Not Found`，实得: {status_line:?}"
    );
    assert!(
        !status_line.contains(" 404 OK"),
        "404 的状态行说 OK —— 状态码与文案矛盾: {status_line:?}"
    );
}
