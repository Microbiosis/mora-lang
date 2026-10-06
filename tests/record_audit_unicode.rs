//! v0.104.6 D177：`mora record audit` 在**真正发现 secret 时 panic** —— 密钥扫描器在最坏的时机崩溃（已修）。
//!
//! ## 缺陷
//!
//! `record/audit.rs` 的 `audit_json_value` 把**字符数**当**字节偏移**用：
//!
//! ```text
//! let token_len = value[token_start..].chars().take_while(…).count();  // ← 字符数
//! …
//! &value[token_start..token_start + 5.min(token_len)]                  // ← 当字节数用
//! ```
//!
//! 只要 secret 前缀（`sk-` / `key-` / `Bearer `）之后的尾串里含**非 ASCII
//! 字母数字**，落点就会切在字符中间 → Rust 直接 panic。
//! Rust 的 `char::is_alphanumeric` 对汉字、假名、重音字母**都返回 true**，
//! 所以这不是罕见输入。
//!
//! 真实 `mora record audit` 实测（修前）：
//!
//! ```text
//! $ mora record u.mora d177v        # prompt = "sk-" + 23 个汉字
//! ✓ recorded 3 events
//! $ mora record audit d177v
//! thread 'mora-main' panicked at src\record\audit.rs:156:27:
//!   end byte index 8 is not a char boundary; it is inside '文' (bytes 6..9 of string)
//! audit exit=101
//! ```
//!
//! ## 为什么严重
//!
//! 1. **崩溃在「它真的找到东西」的那一刻。** 阈值 `token_len >= 20` 通过
//!    才会走到那行切片 —— 也就是说**没有 secret 时永远不崩**，一旦检测到
//!    非 ASCII 密钥就崩。
//! 2. **这个命令的用途正是「分享/提交前确认录像里没有密钥」。**
//!    扫描器在需要给出结论时崩掉，CI 里表现为 exit 101（panic）而不是
//!    「发现 1 个疑似密钥」。
//! 3. 修前对**纯 ASCII** secret 工作正常 —— 所以这条路径平时测不出来，
//!    只在真实的中/日/欧语言内容里才暴露。
//!
//! ## 修法
//!
//! `token_len` 继续按**字符**计数（阈值语义就是「至少 20 个字母数字字符」），
//! 但取预览时改成 `.chars().take(5).collect::<String>()`，
//! **绝不把字符数当字节数用**。
//!
//! ## 判据
//!
//! 主判据是**反向对照**：非 ASCII secret 必须被**报出**（不是 panic）。
//! 正向对照（纯 ASCII secret）守住「修复没改坏原有行为」。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 工作目录守卫 —— `Drop` 时删除。
///
/// 不写在测试尾部：断言失败 panic 时尾部不执行，失败的测试反而留下垃圾目录。
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d177_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建工作目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    /// 写脚本（UTF-8，**无 BOM** —— Mora 的 parser 见到 BOM 会报
    /// `Unexpected character '\u{feff}'`）。
    fn script(&self, file: &str, src: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, src).expect("写脚本");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

/// 跑 `mora <args>`，返回 (stdout+stderr, exit code)。
///
/// ⚠ 必须设 `current_dir`：录制落 `.mora/recordings/`，**相对进程 CWD**
/// （D173 实测并记档），不设会污染仓库工作区。
fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 录一份脚本到指定录像名。
fn record(dir: &Path, script: &Path, name: &str) {
    let (out, code) = mora(dir, &["record", script.to_str().unwrap(), name]);
    assert_eq!(code, 0, "录制 {} 应成功: {}", name, out);
    assert!(
        out.contains("recorded"),
        "应确实录到事件（空文件对空文件的比对没有意义）: {}",
        out
    );
}

/// 断言：这次 audit **没有 panic**，且报告了 `n` 个疑似 secret。
fn assert_findings(dir: &Path, name: &str, n: usize) {
    let (out, code) = mora(dir, &["record", "audit", name]);
    assert!(
        !out.contains("panicked"),
        "audit **panic 了** —— 扫描器在发现 secret 时崩溃（exit={}）:\n{}",
        code,
        out
    );
    assert_ne!(
        code, 101,
        "exit 101 = Rust panic，密钥扫描器不该崩:\n{}",
        out
    );
    if n == 0 {
        assert!(
            out.contains("No secrets found"),
            "应报「无 secret」:\n{}",
            out
        );
    } else {
        // v0.104.6 D180：**不再断言精确个数**。
        //
        // 旧断言是「恰好 1 个」。D180 起审计按**真实字段**逐个扫描
        // （`prompt_preview` / `response` 分开报），而 mock 的响应是
        // `[Mock response for: <prompt>]` —— 回显了 prompt，于是
        // **同一份 prompt 里的密钥在两个字段里各算一处**，N 条 2 个。
        // 这是**更准**的（每处都是真实位置），不是回归。
        //
        // 判据该守的是「该报的都报了」，不是「恰好报了几条」——
        // 精确计数把判据绑死在实现的内部形状上（D176 同款）。
        assert!(
            out.contains("potential secret(s) found"),
            "应报出疑似 secret:\n{}",
            out
        );
        assert_eq!(
            code, 1,
            "发现 secret 时退出码应为 1（供 CI 判失败）:\n{}",
            out
        );
    }
}

/// **主判据（有牙齿）**：`sk-` + 汉字 必须被**报出**，不能 panic。
///
/// 修前这里 panic / exit 101。
#[test]
fn d177_audit_reports_non_ascii_secret_instead_of_panicking() {
    let dir = WorkDir::new("cjk");
    // 23 个汉字 —— 超过 `token_len >= 20` 阈值，故会走到切片那一行。
    let zh = "中文字符测试密码密钥值重复再补几个字加更多更多";
    assert!(zh.chars().count() >= 20, "前提：汉字数须过阈值");
    let script = dir.script(
        "u.mora",
        &format!("let x = ai.chat(p\"sk-{}\")\nprint(x)\n", zh),
    );
    record(dir.path(), &script, "d_cjk");

    assert_findings(dir.path(), "d_cjk", 1);
}

/// 另一种非 ASCII 形态：**重音字母**（UTF-8 2 字节）也必须安全。
///
/// 与汉字（3 字节）分开测 —— 两者落点不同，能覆盖到不同的字节偏移。
#[test]
fn d177_audit_handles_accented_letters() {
    let dir = WorkDir::new("accent");
    let script = dir.script(
        "u.mora",
        "let x = ai.chat(p\"sk-éééééééééééééééééééé\")\nprint(x)\n",
    );
    record(dir.path(), &script, "d_acc");

    assert_findings(dir.path(), "d_acc", 1);
}

/// `Bearer ` 前缀同样走这段代码（同一个 `token_start` 切片）。
#[test]
fn d177_audit_handles_bearer_with_non_ascii() {
    let dir = WorkDir::new("bearer");
    // ⚠ 尾串必须 **≥ 20 个字母数字字符**（`SECRET_MIN_LEN`），否则压根不构成
    // finding —— 我第一版只写了 18 个汉字，测试红而**产品没错**。
    let zh = "中文字符测试密码密钥值重复再补几个字加更多";
    assert!(zh.chars().count() >= 20, "前提：须过 20 字符阈值");
    let script = dir.script(
        "u.mora",
        &format!("let x = ai.chat(p\"Bearer {}\")\nprint(x)\n", zh),
    );
    record(dir.path(), &script, "d_bear");

    assert_findings(dir.path(), "d_bear", 1);
}

/// `Bearer` 模式在修前是**死条目**（`name.contains("Bearer")` 大小写不匹配
/// `"bearer-token"` → 恒假 → `continue`），所以纯 ASCII 也检不出。
///
/// 这条专门盯那个死条目：它是**静默**失败（报「No secrets found」），
/// 比 panic 更难发现 —— panic 会 exit 101，沉默不会。
#[test]
fn d177_audit_detects_plain_ascii_bearer_token() {
    let dir = WorkDir::new("bearascii");
    let script = dir.script(
        "u.mora",
        "let x = ai.chat(p\"Authorization: Bearer ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\")\nprint(x)\n",
    );
    record(dir.path(), &script, "d_bear");

    let (out, _) = mora(dir.path(), &["record", "audit", "d_bear"]);
    assert!(
        out.contains("bearer-token"),
        "Bearer token 必须被识别为 `bearer-token` 模式 —— 修前该条目是死代码:\n{}",
        out
    );
    assert_findings(dir.path(), "d_bear", 1);
}

/// **正对照**：纯 ASCII secret —— 修前就正常，守住「修复没改坏原有行为」。
#[test]
fn d177_audit_still_reports_plain_ascii_secret() {
    let dir = WorkDir::new("ascii");
    let script = dir.script(
        "u.mora",
        "let x = ai.chat(p\"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\")\nprint(x)\n",
    );
    record(dir.path(), &script, "d_asc");

    assert_findings(dir.path(), "d_asc", 1);
}

/// **负对照**：普通文本不得被误报。
///
/// 守的是「修复没有把阈值/前缀判定放宽」—— 顺带覆盖非 ASCII 普通内容
/// （中文说明文字里不该冒出 secret）。
#[test]
fn d177_audit_does_not_flag_ordinary_text() {
    let dir = WorkDir::new("clean");
    let script = dir.script(
        "u.mora",
        "let x = ai.chat(p\"请帮我总结这段中文文档的要点，谢谢\")\nprint(x)\n",
    );
    record(dir.path(), &script, "d_ok");

    assert_findings(dir.path(), "d_ok", 0);
}
