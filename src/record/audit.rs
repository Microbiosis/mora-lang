use super::*;

pub fn redact_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        // 检查 sk-xxx, key-xxx 模式
        if i + 3 <= len {
            let prefix: String = chars[i..i + 3.min(len - i)].iter().collect();
            if (prefix == "sk-" || prefix == "ke") && i + 4 <= len {
                // key-
                let longer: String = chars[i..i + 4.min(len - i)].iter().collect();
                if prefix == "sk-" || longer == "key-" {
                    // 找到前缀，收集后续字母数字
                    let start = i;
                    let p = if prefix == "sk-" { "sk-" } else { "key-" };
                    i += p.len();
                    while i < len
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-')
                    {
                        i += 1;
                    }
                    if i - start > 20 {
                        // 长度足够，视为 secret
                        out.push_str("<REDACTED>");
                        continue;
                    } else {
                        // 不够长，原样输出
                        for c in &chars[start..i] {
                            out.push(*c);
                        }
                        continue;
                    }
                }
            }
        }
        // 检查 Bearer xxx
        if i + 7 <= len {
            let bearer: String = chars[i..i + 7].iter().collect();
            if bearer == "Bearer " {
                let start = i;
                i += 7;
                while i < len
                    && (chars[i].is_alphanumeric()
                        || chars[i] == '_'
                        || chars[i] == '-'
                        || chars[i] == '.')
                {
                    i += 1;
                }
                if i - start > 27 {
                    // "Bearer " (7) + 20+ chars
                    out.push_str("Bearer <REDACTED>");
                    continue;
                } else {
                    for c in &chars[start..i] {
                        out.push(*c);
                    }
                    continue;
                }
            }
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

/// 审计单个事件的 JSON 字符串
fn audit_json_value(event_id: u64, field: &str, value: &str, findings: &mut Vec<AuditFinding>) {
    // 检查常见 secret 模式
    let patterns = [
        ("sk-[a-zA-Z0-9]{20,}", "openai-api-key"),
        ("key-[a-zA-Z0-9]{20,}", "generic-api-key"),
        ("Bearer [a-zA-Z0-9_\\-\\.]{20,}", "bearer-token"),
        ("ghp_[a-zA-Z0-9]{36}", "github-pat"),
        ("gho_[a-zA-Z0-9]{36}", "github-oauth"),
        ("xoxb-[a-zA-Z0-9\\-]+", "slack-bot-token"),
        ("xoxp-[a-zA-Z0-9\\-]+", "slack-user-token"),
    ];

    for (_pattern, name) in &patterns {
        // 简单匹配: 检查值中是否包含模式前缀
        let prefix = if name.contains("sk-") || name == &"openai-api-key" {
            "sk-"
        } else if name.contains("key-") || name == &"generic-api-key" {
            "key-"
        } else if name.contains("Bearer") {
            "Bearer "
        } else if name.contains("ghp_") {
            "ghp_"
        } else if name.contains("gho_") {
            "gho_"
        } else if name.contains("xoxb") {
            "xoxb-"
        } else if name.contains("xoxp") {
            "xoxp-"
        } else {
            continue;
        };

        if let Some(pos) = value.find(prefix) {
            // 计算 token 长度
            let token_start = pos + prefix.len();
            let token_len = value[token_start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .count();
            if token_len >= 20 {
                let preview = format!(
                    "{}{}",
                    prefix,
                    &value[token_start..token_start + 5.min(token_len)]
                );
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

/// 审计整个录制
pub fn audit_recording(events: &[Event], ignore_rules: &[IgnoreRule]) -> Vec<AuditFinding> {
    let mut findings = Vec::new();
    for ev in events {
        let (id, json_str) = match ev {
            Event::AiChat {
                id,
                response,
                prompt_preview,
                ..
            } => (*id, format!("{} {}", prompt_preview, response)),
            Event::WebFetch { id, url, .. } => (*id, url.clone()),
            Event::Note { id, message, .. } => (*id, message.clone()),
        };
        // 检查是否被忽略
        let should_ignore = ignore_rules.iter().any(|rule| match rule {
            IgnoreRule::Field(f) => f == "response" || f == "prompt_preview",
            IgnoreRule::Pattern(p) => json_str.contains(p.as_str()),
            _ => false,
        });
        if !should_ignore {
            audit_json_value(id, "content", &json_str, &mut findings);
        }
    }
    findings
}
