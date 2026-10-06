//! v0.25: record 脱敏审计 — redact_secrets/audit_recording + moraignore 策略解析。

use super::*;

/// 把一段文本里的 secret 换成 `<REDACTED>`，**保留前缀**便于辨认类型
/// （`sk-<REDACTED>` / `Bearer <REDACTED>` / `ghp_<REDACTED>` …）。
///
/// v0.104.6 D179 重构。此前它**只认 3 种**（`sk-` / `key-` / `Bearer `），
/// 而 audit 检测 **7 种** —— `ghp_` / `gho_` / `xoxb-` / `xoxp-`（GitHub PAT /
/// Slack token）**审计会报出、导出时却原样保留**。而
/// `mora record report` 有一节标题就叫 **「Event Log (redacted)」** ——
/// 实测（修前）：
///
/// ```text
/// ## Audit
/// | 1 | content | github-pat | ghp_ABCDE... |          ← 审计报出
/// ## Event Log (redacted)
/// {"kind":"ai.chat",…,"prompt_preview":"… ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"…}
///                              ↑ 三行之后，同一份报告里是完整明文
/// ```
///
/// 而且比 D177 记的更糟：连 `key-` 也没生效。旧判定写的是
/// `prefix == "ke"` —— 拿 3 字符的 `prefix` 去比 2 字符的字面量，**恒假**。
/// 故实测只有 `sk-` / `Bearer ` 2 种生效，不是 3 种。
///
/// 现与 `audit_json_value` **共用 `SECRET_PREFIXES` / `SECRET_MIN_LEN`**：
/// 一张表、两个消费者，不会再各自漂移（与 D175 的教训同源）。
/// 阈值与审计一致（`SECRET_MIN_LEN`），token 可含 `.`（JWT 带点号签名），
/// 与审计的 token 扫描口径对齐 —— 此前 sk- 那条**不含** `.`，是另一个不对称。
pub fn redact_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        // 取**最长**匹配的前缀（当前 7 个互不为前缀，但将来若加新前缀，
        // 「先命中短的」会留下一个只遮一半的尾巴）。
        let mut hit: Option<Vec<char>> = None;
        for (prefix, _name) in SECRET_PREFIXES {
            let pc: Vec<char> = prefix.chars().collect();
            if i + pc.len() > len || chars[i..i + pc.len()] != pc[..] {
                continue;
            }
            if hit.as_ref().is_none_or(|h| h.len() < pc.len()) {
                hit = Some(pc);
            }
        }
        if let Some(pc) = hit {
            let start = i;
            i += pc.len();
            while i < len
                && (chars[i].is_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '-'
                    || chars[i] == '.')
            {
                i += 1;
            }
            let token_len = i - start - pc.len();
            if token_len >= SECRET_MIN_LEN {
                out.extend(pc.iter());
                out.push_str("<REDACTED>");
            } else {
                // 不够长 —— 不是 secret，原样输出。
                for c in &chars[start..i] {
                    out.push(*c);
                }
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Audit 发现项
#[derive(Clone, Debug)]
pub struct AuditFinding {
    pub event_id: u64,
    pub field: String,
    pub pattern: String,
    pub preview: String, // 脱敏后的预览
}

/// .moraignore 策略条目
#[derive(Clone, Debug)]
pub enum IgnoreRule {
    /// 忽略整个顶层字段: field:token_usage
    Field(String),
    /// 忽略 JSON 路径: path:request.messages.*.content
    Path(String),
    /// 按名称禁用模式: pattern:github-token
    Pattern(String),
}

/// 解析 .moraignore 文件
pub fn parse_moraignore(content: &str) -> Vec<IgnoreRule> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            if let Some(field) = line.strip_prefix("field:") {
                Some(IgnoreRule::Field(field.trim().to_string()))
            } else if let Some(path) = line.strip_prefix("path:") {
                Some(IgnoreRule::Path(path.trim().to_string()))
            } else {
                line.strip_prefix("pattern:")
                    .map(|pat| IgnoreRule::Pattern(pat.trim().to_string()))
            }
        })
        .collect()
}

/// secret 模式表：`(前缀, 展示名)`。
///
/// v0.104.6 D177 重构。此前这里是一张**看起来像正则**的表
/// （`("sk-[a-zA-Z0-9]{20,}", "openai-api-key")` 等），但那个"正则"
/// **从未被应用** —— 代码走的是下面 `name.contains(…)` 那套从前缀串反推
/// 前缀的推导。两处后果：
///
/// 1. **`bearer-token` 是死条目。** 推导里写的是 `name.contains("Bearer")`
///    （大写 B），而表里的名字是小写的 `"bearer-token"` → 恒假 →
///    一路落到 `else { continue; }`，**该模式永远不会被求值**。
///    实测（修前，纯 ASCII，32 字符，排除 Unicode 变量）：
///    ```text
///    $ mora record audit bear
///    ✓ No secrets found in recording 'bear'      ← Bearer token 就在录像里
///    ```
/// 2. **表里的 `{20,}` / `{36}` 是装饰品。** 改它们对行为零影响，
///    却让人以为阈值就写在这儿 —— 真正的阈值是下面那个 `token_len >= 20`。
///
/// 故改成**把前缀直接写在表里**，并删掉那些从未生效的"正则"字面量：
/// 前缀与展示名不再互相推导，也就没有推导失配的可能。
const SECRET_PREFIXES: &[(&str, &str)] = &[
    ("sk-", "openai-api-key"),
    ("key-", "generic-api-key"),
    ("Bearer ", "bearer-token"),
    ("ghp_", "github-pat"),
    ("gho_", "github-oauth"),
    ("xoxb-", "slack-bot-token"),
    ("xoxp-", "slack-user-token"),
];

/// token 的最小长度（**字符数**，非字节）。见 `audit_json_value` 里的说明。
const SECRET_MIN_LEN: usize = 20;

/// 审计单个事件的 JSON 字符串
fn audit_json_value(event_id: u64, field: &str, value: &str, findings: &mut Vec<AuditFinding>) {
    for (prefix, name) in SECRET_PREFIXES {
        if let Some(pos) = value.find(prefix) {
            // 计算 token 长度
            let token_start = pos + prefix.len();
            // v0.104.6 D177：**`token_len` 是字符数**（`.chars()` 逐字符数），
            // 而下一行的切片把它当**字节偏移**用 —— 两者混用。
            // 当 token 尾串含非 ASCII 字母数字（汉字/假名/重音字母…，
            // `char::is_alphanumeric` 对它们返回 true）时，落点会切在
            // 字符中间 → Rust 直接 panic：
            //
            // ```text
            // $ mora record audit <含 "sk-" + 23 个汉字的录像>
            // thread 'mora-main' panicked at src/record/audit.rs:156:
            //   end byte index 8 is not a char boundary; it is inside '文'
            // exit 101
            // ```
            //
            // 即：**扫描器恰好在它真正发现 secret 的那一刻崩掉** ——
            // 对「分享/提交前确认录像含不含密钥」这个用途是最坏的时机。
            //
            // 修法：`token_len` 继续按**字符**计数（阈值语义就是
            // 「至少 20 个字母数字字符」），但取预览时按**字符**切，
            // 绝不把字符数当字节数用。
            let token_len = value[token_start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .count();
            if token_len >= SECRET_MIN_LEN {
                let preview_tail: String = value[token_start..]
                    .chars()
                    .take(5.min(token_len))
                    .collect();
                let preview = format!("{}{}", prefix, preview_tail);
                findings.push(AuditFinding {
                    event_id,
                    field: field.to_string(),
                    pattern: name.to_string(),
                    preview: format!("{}...", preview),
                });
            }
        }
    }
}

/// 审计**实际**扫描的字段名。
///
/// v0.104.6 D180：此前 `audit_recording` 把每个事件拍平成**一个**字符串
/// 并一律标成 `"content"`，于是 `IgnoreRule::Field` 无从判断「忽略的是哪个字段」
/// —— 它只能硬编码两个名字，且是**整事件**粒度。本表是「被扫描字段」的
/// 单一事实源：`is_ignored` 匹配它，`unsupported_ignore_rules` 校验它。
pub const AUDITED_FIELDS: &[&str] = &["prompt_preview", "response", "url", "message"];

/// 该规则是否被忽略。
fn is_ignored(rules: &[IgnoreRule], field: &str, text: &str) -> bool {
    rules.iter().any(|rule| match rule {
        // v0.104.6 D180：原先是 `f == "response" || f == "prompt_preview"`
        // —— 硬编码两个名字、且**整事件**粒度（写 `field:response` 会连
        // prompt 一起跳过）。现按**正在扫描的那个字段**匹配，
        // 粒度与规则字面意思一致。
        IgnoreRule::Field(f) => f == field,
        IgnoreRule::Pattern(p) => text.contains(p.as_str()),
        // `Path` 在当前扫描器里**无法生效** —— 被扫的是扁平字符串，
        // 根本没有 JSON 路径可匹配。交由 `unsupported_ignore_rules` 告知用户，
        // 不在这里默默丢弃。
        IgnoreRule::Path(_) => false,
    })
}

/// `.moraignore` 里**写了但不会被用上**的规则，须如实报告。
///
/// v0.104.6 D180。这不是小事：在安全工具里，一个不生效的策略文件
/// **比没有策略文件更糟** —— 用户以为已经限定了扫描范围，实际上什么都没发生。
/// 实测（修前）：`path:request.messages.*.content` 与 `field:content` /
/// `field:token_usage`（`parse_moraignore` 的**文档示例**本身）全部**静默无效**，
/// 审计照常报出密钥、退出码照旧，**零提示**。
pub fn unsupported_ignore_rules(rules: &[IgnoreRule]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rules {
        match r {
            IgnoreRule::Path(p) => out.push(format!(
                "path:{p} —— 不支持：扫描器扫的是扁平文本，没有 JSON 路径可匹配"
            )),
            IgnoreRule::Field(f) => {
                if !AUDITED_FIELDS.contains(&f.as_str()) {
                    out.push(format!(
                        "field:{f} —— 未知字段名；可用的有 {}",
                        AUDITED_FIELDS.join(" / ")
                    ));
                }
            }
            // `pattern:` 对任何被扫文本做子串匹配，恒可生效。
            IgnoreRule::Pattern(_) => {}
        }
    }
    out
}

/// 审计整个录制
pub fn audit_recording(events: &[Event], ignore_rules: &[IgnoreRule]) -> Vec<AuditFinding> {
    let mut findings = Vec::new();
    for ev in events {
        // v0.104.6 D180：按**真实字段**逐个扫描，而不是拍平成一个 `"content"`。
        // 这是 `field:` 规则能有正确粒度（`field:response` 只放过 response）
        // 的前提，也让报告里的 FIELD 列说真话。
        let (id, fields): (u64, Vec<(&str, &str)>) = match ev {
            Event::AiChat {
                id,
                response,
                prompt_preview,
                ..
            } => (
                *id,
                vec![
                    ("prompt_preview", prompt_preview.as_str()),
                    ("response", response.as_str()),
                ],
            ),
            Event::WebFetch { id, url, .. } => (*id, vec![("url", url.as_str())]),
            Event::Note { id, message, .. } => (*id, vec![("message", message.as_str())]),
            // v0.83: Msg + StateMutation 不进审计（应用层消息无 secrets）
            Event::Msg { .. } | Event::StateMutation { .. } => continue,
        };
        for (field, text) in fields {
            if is_ignored(ignore_rules, field, text) {
                continue;
            }
            audit_json_value(id, field, text, &mut findings);
        }
    }
    findings
}
