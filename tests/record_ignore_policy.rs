//! v0.104.6 D180：`.moraignore` 里**写了但不生效**的规则被静默接受（已修）。
//!
//! ## 缺陷
//!
//! `parse_moraignore` 解析三种规则（`field:` / `path:` / `pattern:`），
//! 但 `audit_recording` 里：
//!
//! ```rust
//! IgnoreRule::Field(f) => f == "response" || f == "prompt_preview",  // 硬编码两个名字
//! IgnoreRule::Pattern(p) => json_str.contains(p.as_str()),
//! _ => false,                                                        // ← Path 落这里，恒假
//! ```
//!
//! 实测（修前，真实 CLI + 真实 `--policy`）：
//!
//! | 规则 | 修前 |
//! |---|---|
//! | `pattern:<子串>` | 生效 |
//! | `field:response` / `field:prompt_preview` | 生效（但是**整事件**粒度） |
//! | `path:request.messages.*.content` | **静默无效** |
//! | `field:content` / `field:url` / `field:token_usage` | **静默无效** |
//!
//! 两条各有多糟：
//!
//! 1. **不生效的策略文件比没有策略文件更糟。** 用户以为自己限定了扫描范围，
//!    实际上什么都没发生 —— 审计照常报出密钥、退出码照旧，**零提示**。
//!    `field:token_usage` 还是 `parse_moraignore` **自己的文档示例**。
//! 2. **`field:response` 的粒度是错的。** 旧代码把整个事件拍平成一个字符串，
//!    所以「忽略 response」会连 prompt 一起跳过 —— 一条想放过响应的规则，
//!    把**提示词里的密钥也一起盖住了**。
//!
//! ## 修法
//!
//! - `audit_recording` 改为**按真实字段逐个扫描**（`prompt_preview` /
//!   `response` / `url` / `message`），`Field` 才有正确粒度；
//!   报告的 FIELD 列也说真话（原先一律叫 `"content"`）。
//! - 新增 `unsupported_ignore_rules()`：点名 `path:`（扫描器扫扁平文本，
//!   没有 JSON 路径可匹配）与未知 `field:` 名。
//! - 规则披露**移到两个分支都打印** —— 原先它只在「没发现密钥」那支，
//!   恰恰是有发现时你最想知道策略加载情况的时候，它一句话不说。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d180_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, file: &str, body: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, body).expect("写文件");
        p
    }
    fn policy(&self, name: &str, lines: &str) -> PathBuf {
        self.write(name, lines)
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

fn record(dir: &Path, script: &Path, name: &str) {
    let (out, code) = mora(dir, &["record", script.to_str().unwrap(), name]);
    assert_eq!(code, 0, "录制 {} 应成功: {}", name, out);
    assert!(out.contains("recorded"), "应录到事件: {}", out);
}

/// 密钥在 **prompt** 里（mock 响应会回显 prompt，故两处都有）。
fn secret_in_prompt(dir: &WorkDir) -> PathBuf {
    dir.write(
        "p.mora",
        "let a = ai.chat(p\"key sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\")\nprint(a)\n",
    )
}

/// 密钥**只在 response** 里（用 `mock_llm` 队列指定响应，prompt 干净）。
fn secret_in_response_only(dir: &WorkDir) -> PathBuf {
    dir.write(
        "r.mora",
        "with mock_llm = [\"sk-RESPONSEONLYSECRET1234567890AB\"]\n  \
         let a = ai.chat(p\"a harmless question\")\n  print(a)\nend\n",
    )
}

/// **主判据（有牙齿）**：`path:` 规则必须被点名，且**不得**生效。
///
/// 修前：规则被接受、什么都不发生、审计照常报出、零提示。
#[test]
fn d180_path_rule_is_reported_as_unsupported_and_does_not_suppress() {
    let dir = WorkDir::new("path");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");
    let pol = dir.policy("pol", "path:request.messages.*.content\n");

    let (out, code) = mora(
        dir.path(),
        &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
    );
    assert_eq!(code, 1, "规则无效 ⇒ 密钥仍应被判出:\n{}", out);
    assert!(
        out.contains("potential secret(s) found"),
        "密钥必须仍被报出 —— `path:` 规则没有理由抑制它:\n{}",
        out
    );
    assert!(
        out.contains("[ignored]") && out.contains("path:"),
        "不生效的 `path:` 规则必须被点名:\n{}",
        out
    );
    assert!(out.contains("0 usable"), "应明确说这条规则不可用:\n{}", out);
}

/// 未知 `field:` 名（含 `parse_moraignore` **自己的文档示例** `token_usage`）
/// 必须被点名，且不得生效。
#[test]
fn d180_unknown_field_name_is_reported_and_does_not_suppress() {
    let dir = WorkDir::new("field");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");

    // 这些是**根本不存在的字段名** —— 扫什么都碰不到，必须点名。
    for name in ["token_usage", "content", "prompt", "arg_signature"] {
        let pol = dir.policy("pol", &format!("field:{name}\n"));
        let (out, code) = mora(
            dir.path(),
            &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
        );
        assert_eq!(
            code, 1,
            "field:{name} 不是真实字段名 ⇒ 不该抑制密钥:\n{}",
            out
        );
        assert!(
            out.contains("potential secret(s) found"),
            "field:{name} 不该抑制密钥:\n{}",
            out
        );
        assert!(
            out.contains("[ignored]") && out.contains(&format!("field:{name}")),
            "field:{name} 必须被点名:\n{}",
            out
        );
        assert!(
            out.contains("0 usable"),
            "field:{name} 应计为不可用:\n{}",
            out
        );
    }
}

/// `url` / `message` 是**真实字段名**（`WebFetch` / `Note` 事件的），
/// 本录像里恰好没有这两种事件 —— 故规则**合法**（usable），
/// 但也**无从生效**，密钥必须照常报出。
///
/// 与上一条成对：区分「字段名不存在」与「字段名存在但本录像没用到」。
/// 我第一版把这两类混在一起断言，红而**产品没错**。
#[test]
fn d180_real_but_absent_field_name_is_usable_but_inert() {
    let dir = WorkDir::new("fieldabsent");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");

    for name in ["url", "message"] {
        let pol = dir.policy("pol", &format!("field:{name}\n"));
        let (out, code) = mora(
            dir.path(),
            &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
        );
        assert_eq!(
            code, 1,
            "field:{name} 只放过 url/message 字段，本录像的 prompt 不受影响:\n{}",
            out
        );
        assert!(
            out.contains("potential secret(s) found"),
            "密钥必须仍被报出:\n{}",
            out
        );
        assert!(
            out.contains("1 usable"),
            "field:{name} 是合法字段名，应计为 usable:\n{}",
            out
        );
        assert!(
            !out.contains("[ignored]"),
            "合法字段名不该被标成 [ignored]:\n{}",
            out
        );
    }
}

/// **语义修复**：密钥在 prompt 里时，`field:response` **不得**把它一起盖住。
///
/// 修前 `Field` 是**整事件**粒度 —— 写「忽略响应」会连 prompt 一起跳过。
#[test]
fn d180_field_response_does_not_hide_a_prompt_secret() {
    let dir = WorkDir::new("gran");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");
    let pol = dir.policy("pol", "field:response\n");

    let (out, code) = mora(
        dir.path(),
        &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
    );
    assert_eq!(
        code, 1,
        "密钥在 prompt 里，`field:response` 不该盖住它:\n{}",
        out
    );
    assert!(
        out.contains("prompt_preview"),
        "报出的 FIELD 应是真实字段名 `prompt_preview`（修前一律叫 `content`）:\n{}",
        out
    );
    assert!(
        out.contains("1 usable"),
        "`field:response` 是有效规则，应计为 usable:\n{}",
        out
    );
}

/// 反过来：密钥**只在 response** 里时，`field:response` 应真的放过它。
///
/// 与上一条成对 —— 证明粒度是**按字段**的，而不是「永远生效」或「永远不生效」。
#[test]
fn d180_field_response_suppresses_a_response_secret() {
    let dir = WorkDir::new("gran2");
    let script = secret_in_response_only(&dir);
    record(dir.path(), &script, "rec");
    let pol = dir.policy("pol", "field:response\n");

    let (out, code) = mora(
        dir.path(),
        &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "密钥只在 response 里，`field:response` 应当放过:\n{}",
        out
    );
    assert!(out.contains("No secrets found"), "应报「无密钥」:\n{}", out);
}

/// 规则披露必须在**有发现**的那一支也出现。
///
/// 修前它只在「没发现密钥」时打印 —— 恰恰是有发现时你更需要知道
/// 「我的 .moraignore 到底加载了没」的那一刻。
#[test]
fn d180_policy_is_disclosed_even_when_secrets_are_found() {
    let dir = WorkDir::new("disclose");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");
    let pol = dir.policy("pol", "pattern:zzz-never-matches\n");

    let (out, _) = mora(
        dir.path(),
        &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
    );
    assert!(
        out.contains("potential secret(s) found"),
        "前提：有发现:\n{}",
        out
    );
    assert!(
        out.contains("rule(s) from") && out.contains("1 usable"),
        "**有发现时也必须**披露策略已加载:\n{}",
        out
    );
}

/// **正对照**：`pattern:` 规则仍然照常生效（防修复把规则机制整体弄坏）。
#[test]
fn d180_pattern_rule_still_works() {
    let dir = WorkDir::new("pattern");
    let script = secret_in_prompt(&dir);
    record(dir.path(), &script, "rec");
    let pol = dir.policy("pol", "pattern:sk-ABCDEFGHIJ\n");

    let (out, code) = mora(
        dir.path(),
        &["record", "audit", "rec", "--policy", pol.to_str().unwrap()],
    );
    assert_eq!(code, 0, "`pattern:` 应照常生效:\n{}", out);
    assert!(out.contains("No secrets found"), "应报「无密钥」:\n{}", out);
    assert!(out.contains("1 usable"), "应计为 usable:\n{}", out);
}
