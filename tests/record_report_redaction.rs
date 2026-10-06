//! v0.104.6 D179：`redact_secrets` 只认 2/7 种 secret —— 报告里标题写着
//! **「Event Log (redacted)」**，正文却是完整明文（已修）。
//!
//! ## 缺陷
//!
//! `mora record report` 生成的 Markdown 里有一节：
//!
//! ```text
//! ## Event Log (redacted)
//! ```
//!
//! 该节走 `redact_secrets`。而 `redact_secrets` 此前**只认 2 种**前缀
//! （`sk-` / `Bearer `），而 `audit` 检测 **7 种**。
//!
//! 实测（修前）—— 同一份报告，审计报出、三行后印出明文：
//!
//! ```text
//! ## Audit
//! | 1 | content | github-pat | ghp_ABCDE... |                ← 审计报出
//! ## Event Log (redacted)
//! {"kind":"ai.chat",…,"prompt_preview":"my github token is ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"…}
//!                                          ↑ 完整明文，标题却写着 redacted
//! ```
//!
//! 7 种的端到端实测（修前）：
//!
//! | 前缀 | 修前 |
//! |---|---|
//! | `sk-` / `Bearer ` | 已脱敏 |
//! | `key-` / `ghp_` / `gho_` / `xoxb-` / `xoxp-` | **明文泄漏** |
//!
//! ## D177 记的是「3 种」，实测只有 2 种 —— 差在 `key-`
//!
//! D177 读代码**意图**记下「处理 `sk-` / `key-` / `Bearer ` 三种」。
//! 旧判定写的是 `prefix == "ke"`：`prefix` 是取 3 个字符的串
//! （`"key"`），拿它去比 2 字符的字面量 `"ke"` —— **恒假**。
//! 故 `key-` 从来没被脱敏过。**读代码不等于知道行为**，本条是实测推翻的。
//!
//! 另一处不对称：旧 `sk-` 的 token 字符集**不含 `.`**，而 `Bearer ` 的含
//! （JWT 带点号签名）。即同一份 `SECRET_PREFIXES` 派生出两套口径。
//!
//! ## 修法
//!
//! `redact_secrets` 与 `audit_json_value` **共用同一张 `SECRET_PREFIXES`
//! 和同一个 `SECRET_MIN_LEN`** —— 一张表、两个消费者，不会再各自漂移。
//! 输出统一为「保留前缀 + 遮住尾巴」（`ghp_<REDACTED>`），
//! 既有两条单测（`contains("<REDACTED>")` / `contains("Bearer <REDACTED>")`）
//! 仍然通过。
//!
//! ## 判据
//!
//! - **主判据**：7 种前缀的完整 token **都不得**出现在报告里；
//! - **负对照**：普通文本**不得**被误脱敏（防「越脱越多」）；
//! - **正对照**：审计**仍要报出**这些密钥（脱敏不能把告警也吞掉 ——
//!   否则又变成 D177 那种「说没有」）。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d179_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
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

fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .args(args)
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 7 种 secret 前缀，各配一个**超过 20 字符**的 token。
const SECRETS: &[(&str, &str, &str)] = &[
    ("sk", "sk-", "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"),
    ("key", "key-", "key-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"),
    (
        "bearer",
        "Bearer ",
        "Bearer ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
    ),
    ("ghp", "ghp_", "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
    ("gho", "gho_", "gho_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
    ("xoxb", "xoxb-", "xoxb-ABCDEFGHIJKLMNOPQRSTUVWXYZ-123456"),
    ("xoxp", "xoxp-", "xoxp-ABCDEFGHIJKLMNOPQRSTUVWXYZ-123456"),
];

/// 录一份含 `token` 的脚本，返回 `mora record report` 的全文。
fn report_containing(dir: &WorkDir, tag: &str, token: &str) -> String {
    let script = dir.script(
        &format!("{tag}.mora"),
        &format!("let x = ai.chat(p\"my secret is {token}\")\nprint(x)\n"),
    );
    let (out, code) = mora(
        dir.path(),
        &["record", script.to_str().unwrap(), &format!("r_{tag}")],
    );
    assert_eq!(code, 0, "录制 {tag} 应成功: {}", out);
    let (rep, _) = mora(dir.path(), &["record", "report", &format!("r_{tag}")]);
    rep
}

/// **主判据（有牙齿）**：7 种前缀的完整 token 都不得出现在报告里。
///
/// 修前 5 种会红。
#[test]
fn d179_report_redacts_every_supported_secret_prefix() {
    let dir = WorkDir::new("redact");
    for (tag, _prefix, token) in SECRETS {
        let rep = report_containing(&dir, tag, token);
        assert!(
            !rep.contains(token),
            "[{tag}] 完整 token 出现在报告里 —— 标题写着「redacted」却有明文:\n{}",
            rep
        );
        // 前缀本身应当**保留**（便于辨认类型），只是尾巴被遮住。
        // 取「第一个分隔符（含）之前」的部分：`ghp_` / `xoxb-` / `sk-` …。
        let cut = token
            .find(['-', '_', ' '])
            .map(|i| i + 1)
            .unwrap_or(token.len());
        let head = &token[..cut];
        assert!(
            rep.contains(head),
            "[{tag}] 前缀 `{head}` 应保留以利辨认:\n{}",
            rep
        );
    }
}

/// **负对照**：普通文本不得被误脱敏。
///
/// 防「越脱越多」—— 一个把所有长词都打码的脱敏器同样不可用。
#[test]
fn d179_report_keeps_ordinary_text_intact() {
    let dir = WorkDir::new("clean");
    let script = dir.script(
        "a.mora",
        "let x = ai.chat(p\"请帮我总结这段中文文档的要点，不要遗漏任何细节\")\nprint(x)\n",
    );
    let (out, code) = mora(dir.path(), &["record", script.to_str().unwrap(), "r_ok"]);
    assert_eq!(code, 0, "录制应成功: {}", out);
    let (rep, _) = mora(dir.path(), &["record", "report", "r_ok"]);

    assert!(
        rep.contains("请帮我总结这段中文文档的要点"),
        "普通中文内容不得被脱敏:\n{}",
        rep
    );
    assert!(
        !rep.contains("<REDACTED>"),
        "没有 secret 的录像不该出现 <REDACTED>:\n{}",
        rep
    );
}

/// **三处 panic 站点**（D179 顺带查出的同源 bug）都必须对中文内容安全。
///
/// 旧代码三处都是「用 `str::len()`（**字节**）判断长度、用 `[..n]`（**字节**）
/// 切片」，对纯 ASCII 无害，对中文/重音字母必 panic：
///
/// - `mora record report` → `snapshot.rs`（`len()>50` / `[..49]`）
/// - `mora record export --format md` → `analysis.rs`（`len()>40` / `[..39]`）
/// - `mora record timeline` → `cli/mod.rs::truncate`（`len()<=max` / `[..max-1]`）
///
/// 25 个汉字 = 75 字节 → 三处原先都会切在字符中间。
#[test]
fn d179_all_three_truncation_sites_survive_chinese() {
    let dir = WorkDir::new("cjk");
    // 25 个汉字 → 75 字节 > 50，也 > 40，两条阈值都会被触发。
    let script = dir.script(
        "cn.mora",
        "let x = ai.chat(p\"请帮我总结这段中文文档的要点，不要遗漏任何细节信息谢谢\")\nprint(x)\n",
    );
    let (out, code) = mora(dir.path(), &["record", script.to_str().unwrap(), "cn"]);
    assert_eq!(code, 0, "录制应成功: {}", out);

    for args in [
        vec!["record", "report", "cn"],
        vec!["record", "export", "cn", "--format", "md"],
        vec!["record", "timeline", "cn"],
    ] {
        let (o, _) = mora(dir.path(), &args);
        let cmd = args.join(" ");
        assert!(
            !o.contains("panicked") && !o.contains("char boundary"),
            "`mora {}` 在中文内容上 panic（字节数当成字符数切了）:\n{}",
            cmd,
            o
        );
    }
}

/// 显示出来的中文必须**连续完好**，不能被截成碎字节。
///
/// ⚠ 判据别写成「完整显示整句」—— 各命令的 detail 预算不同：
/// `report` / `timeline` 是 **50 字符**，`export` 是 **40**；而 detail 前面
/// 还挂着 `example-model → "[Mock response for: ` 约 38 个字符，
/// 所以 `export` 合法地只放得下**一个**中文字符。那是正确行为，不是缺陷
/// （我第一版写成「必须包含整句」、第二版对三条命令统一要 4 个字，
/// 都是**判据盯错了列** —— 与「内容多寡」无关，D176 同款）。
///
/// 真正该守的不变式：**截断落在字符边界上**。这由「不 panic」保证
/// （字节切错会直接 panic），而**剩余预算**由下表如实标出。
#[test]
fn d179_truncated_chinese_stays_contiguous() {
    let dir = WorkDir::new("cjkintact");
    const PHRASE: &str = "请帮我总结这段中文文档的要点";
    // (args, 至少应可见的中文字符数)
    // 预算 50（report / timeline）：约 38 字符前缀 + 1 个 `…` → 可见 11 个字。
    // 预算 40（export）：约 38 字符前缀 + 1 个 `…` → 只剩 1 个字。
    let cases: &[(&[&str], usize)] = &[
        (&["record", "report", "cn"], 4),
        (&["record", "timeline", "cn"], 4),
        (&["record", "export", "cn", "--format", "md"], 1),
    ];

    for (args, min_visible) in cases {
        let script = dir.script(
            "cn.mora",
            &format!("let x = ai.chat(p\"{PHRASE}\")\nprint(x)\n"),
        );
        let (out, code) = mora(dir.path(), &["record", script.to_str().unwrap(), "cn"]);
        assert_eq!(code, 0, "录制应成功: {}", out);

        let (o, _) = mora(dir.path(), args);
        let cmd = args.join(" ");
        assert!(
            !o.contains("panicked") && !o.contains("char boundary"),
            "`mora {}` panic:\n{}",
            cmd,
            o
        );
        let probe: String = PHRASE.chars().take(*min_visible).collect();
        assert!(
            o.contains(&probe),
            "`mora {}` 应至少显示连续的 `{}`（该命令 detail 预算只够这么多）:\n{}",
            cmd,
            probe,
            o
        );
    }
}

/// **正对照**：审计**仍要报出**这些密钥 —— 脱敏不能把告警也吞掉。
///
/// 否则就退回 D177 那种「说没有」的假绿：文件被遮干净了，
/// 但「有没有 secret」这个结论也一起没了。
#[test]
fn d179_audit_still_reports_secrets_that_are_now_redacted() {
    let dir = WorkDir::new("audit");
    for (tag, _prefix, token) in SECRETS {
        let script = dir.script(
            &format!("a_{tag}.mora"),
            &format!("let x = ai.chat(p\"my secret is {token}\")\nprint(x)\n"),
        );
        let name = format!("a_{tag}");
        let (out, code) = mora(dir.path(), &["record", script.to_str().unwrap(), &name]);
        assert_eq!(code, 0, "录制 {tag} 应成功: {}", out);

        let (rep, acode) = mora(dir.path(), &["record", "audit", &name]);
        assert_eq!(acode, 1, "[{tag}] 审计应判失败（有 secret）:\n{}", rep);
        assert!(
            rep.contains("potential secret(s) found"),
            "[{tag}] 审计应报出密钥 —— 脱敏不得吞掉告警:\n{}",
            rep
        );
    }
}
