//! v0.25: record 序列化 — 事件 JSONL 编解码 + hash_prompt。

use super::*;
use std::path::Path;

pub fn hash_prompt(prompt: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in prompt.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Event → JSONL 字符串 (单行)
pub(super) fn event_to_jsonl(ev: &Event) -> String {
    match ev {
        Event::AiChat {
            id,
            ts_ms,
            model,
            prompt_hash,
            prompt_preview,
            response,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
            // v0.76.05: 序列化 arg_signature 到 JSONL（replay 校验依赖）
            arg_signature,
            ..
        } => {
            let mut s = format!(
                r#"{{"kind":"ai.chat","id":{},"ts_ms":{},"model":"{}","prompt_hash":"{}","prompt_preview":"{}","response":"{}","tokens_in":{},"tokens_out":{},"latency_ms":{},"arg_signature":"{}""#,
                id,
                ts_ms,
                esc(model),
                prompt_hash,
                esc(prompt_preview),
                esc(response),
                tokens_in,
                tokens_out,
                latency_ms,
                esc(arg_signature)
            );
            if let Some(e) = error {
                s.push_str(&format!(r#","error":"{}""#, esc(e)));
            }
            s.push('}');
            s
        }
        Event::WebFetch {
            id,
            ts_ms,
            url,
            method,
            status,
            body_len,
            latency_ms,
            error,
            // v0.76.05: 序列化 arg_signature
            arg_signature,
            ..
        } => {
            let mut s = format!(
                r#"{{"kind":"web.fetch","id":{},"ts_ms":{},"url":"{}","method":"{}","status":{},"body_len":{},"latency_ms":{},"arg_signature":"{}""#,
                id,
                ts_ms,
                esc(url),
                // v0.104.6 D89：`method` 原先**漏了** `esc()` —— 本函数里
                // 每个字符串字段都转义，只有它没有。含 `"` 的 method 会写出
                // 畸形 JSONL，且解码器的引号配对随之错位 → 该行被
                // `load_jsonl` 静默丢弃。
                // **当前不可达**：两个生产调用点（`ai_chat.rs`）都硬编码 `"GET"`，
                // 但 `record_web_fetch` 是 `pub fn`，属**潜在缺陷**而非活 bug ——
                // 记为潜在，不夸大。
                esc(method),
                status,
                body_len,
                latency_ms,
                esc(arg_signature)
            );
            if let Some(e) = error {
                s.push_str(&format!(r#","error":"{}""#, esc(e)));
            }
            s.push('}');
            s
        }
        Event::Note { id, ts_ms, message } => {
            format!(
                r#"{{"kind":"note","id":{},"ts_ms":{},"message":"{}"}}"#,
                id,
                ts_ms,
                esc(message)
            )
        }
        // v0.83: Msg + StateMutation — payload/old/new 序列化为完整 Value JSON
        // （用 flow::value_to_json 而非 Debug 字符串）。旧 JSONL 文件失效。
        Event::Msg {
            id,
            ts_ms,
            channel,
            payload,
            prior_state_hash,
        } => {
            format!(
                r#"{{"kind":"msg","id":{},"ts_ms":{},"channel":"{}","payload":{},"prior_state_hash":{}}}"#,
                id,
                ts_ms,
                esc(channel),
                crate::flow::value_to_json(payload),
                prior_state_hash
            )
        }
        Event::StateMutation {
            id,
            ts_ms,
            var,
            old,
            new,
        } => {
            format!(
                r#"{{"kind":"state_mutation","id":{},"ts_ms":{},"var":"{}","old":{},"new":{}}}"#,
                id,
                ts_ms,
                esc(var),
                crate::flow::value_to_json(old),
                crate::flow::value_to_json(new)
            )
        }
    }
}

/// v0.104.6 D239：转发到共享的 `flow::escape_json_string`。
///
/// 修前这是**同一 JSON 转义规则的第 15 份实现**（D209 已把其余 14 处收敛到
/// `flow::escape_json_string`，本函数是那次收敛的**遗漏**）。全码点空间实测
/// 差异恰为 2 处：
///
/// | 码点 | 修前 `esc` | 共享实现 |
/// |---|---|---|
/// | `U+0008` BACKSPACE | `\u0008` | `\b` |
/// | `U+000C` FORM FEED | `\u000c` | `\f` |
///
/// 两者**都是合法 JSON**、`unquote` 也都能读回 ⇒ 修前**无功能后果**
/// （D239 的两条穷举判据已证往返无损、产出合法）。这是**收敛不彻底**
/// 而非缺陷，但正因如此才危险：下一轮若有人「优化」其中一份，
/// 录制文件的**字节**就会变，而录制比对（`mora record diff`）依赖字节。
///
/// 转发后「同一事实两套算法」从根上消失。
pub(super) fn esc(s: &str) -> String {
    crate::flow::escape_json_string(s)
}

/// Event → (kind, key, RecordedResponse) 用于 replay 索引
pub(super) fn event_to_replay_entry(ev: &Event) -> Option<(String, String, RecordedResponse)> {
    match ev {
        Event::AiChat {
            model,
            prompt_hash,
            response,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
            arg_signature,
            ..
        } => {
            if error.is_some() {
                return None;
            } // 重放不重放错误
            Some((
                "ai.chat".to_string(),
                format!("{}|{}", model, prompt_hash),
                RecordedResponse {
                    response: response.clone(),
                    tokens_in: *tokens_in,
                    tokens_out: *tokens_out,
                    latency_ms: *latency_ms,
                    status: None,
                    body_len: None,
                    // v0.76.05: 录制时签名 → replay 校验用
                    arg_signature: arg_signature.clone(),
                },
            ))
        }
        Event::WebFetch {
            url,
            status,
            body_len,
            latency_ms,
            error,
            arg_signature,
            ..
        } => {
            if error.is_some() {
                return None;
            }
            Some((
                "web.fetch".to_string(),
                url.clone(),
                RecordedResponse {
                    response: String::new(),
                    tokens_in: 0,
                    tokens_out: 0,
                    latency_ms: *latency_ms,
                    status: Some(*status),
                    body_len: Some(*body_len),
                    // v0.76.05: 录制时签名
                    arg_signature: arg_signature.clone(),
                },
            ))
        }
        Event::Note { .. } => None,
        // v0.83: Msg + StateMutation 不进 replay index（应用层消息由 replay_msgs 直接处理）
        Event::Msg { .. } | Event::StateMutation { .. } => None,
    }
}

/// 从 JSONL 文件加载事件 (简化解析: 因为我们写的格式固定, 用字符串匹配)
pub(super) fn load_jsonl(path: &Path) -> Result<(Vec<Event>, Vec<SkippedLine>), String> {
    if !path.exists() {
        return Err(format!(
            "recorder: recording not found at {} (run `mora record <file> <name>` first)",
            path.display()
        ));
    }
    let file = fs::File::open(path)
        .map_err(|e| format!("recorder: failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    // v0.104.6 D178：解析不了的行**仍然跳过**（前向兼容是刻意的 ——
    // `parse_event_line` 只抽取认识的字段，不认识的字段本来就忽略，
    // 所以「新版本写的字段」不会让整行失败；失败的行是**真的畸形**），
    // 但**必须被记录下来**：原先是纯静默丢弃。
    //
    // 为什么不能静默：这是取证工具的数据入口，下游 `replay` / `diff` /
    // `stats` / `export` / **`audit`** 全都建立在它之上。一份被截断的录像
    // （进程被 kill 导致的半行）会让事件数凭空变少，而**每个下游命令都照样
    // 报成功**。实测（修前）：
    //
    // ```text
    // 原文件 3 events（含 ai.chat 行里的密钥），把首行截成半行
    // $ mora record audit full   →  ✓ No secrets found    exit 0
    // $ mora record stats full   →  Events: 2 total
    // ```
    //
    // **安全闸门被静音** —— 密钥扫描器在数据缺了一块的情况下依然一脸
    // 自信地说「没有密钥」。改为：跳过照旧，但把行号与内容交回给调用方。
    let mut skipped: Vec<SkippedLine> = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("recorder: read error at line {}: {}", idx + 1, e))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // v0.14: 暂用简化 JSON 解析, 提取关键字段
        if let Some(ev) = parse_event_line(trimmed) {
            events.push(ev);
        } else {
            skipped.push(SkippedLine {
                line_no: idx + 1,
                excerpt: excerpt(trimmed),
            });
        }
    }
    Ok((events, skipped))
}

/// 一条**没能解析出来**的 JSONL 行。
///
/// v0.104.6 D178：原先这类行被静默丢弃，导致下游（含密钥扫描器）
/// 在不完整数据上报告成功。保留它是为了让调用方能**说出来**。
#[derive(Clone, Debug)]
pub struct SkippedLine {
    /// 1-based 行号（与报错信息里的行号口径一致）。
    pub line_no: usize,
    /// 截断后的原文片段（不存全文：既够定位，又不把可能含密钥的内容
    /// 复制到别处 —— 那会让「脱敏」这个动作本身造成泄漏面）。
    pub excerpt: String,
}

/// 取一段**限长**的行首片段用于报告。
fn excerpt(line: &str) -> String {
    const N: usize = 60;
    if line.chars().count() <= N {
        return line.to_string();
    }
    let head: String = line.chars().take(N).collect();
    format!("{}…", head)
}

/// 极简 JSON 行解析 —— 因为我们的输出格式固定
/// 支持 kind / id / ts_ms / model / prompt_hash / response / tokens_in/out / latency_ms / error / url / method / status / body_len / message
pub(super) fn parse_event_line(line: &str) -> Option<Event> {
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }
    let inner = &line[1..line.len() - 1];
    let mut fields: HashMap<String, String> = HashMap::new();
    // 简易解析: 按 "," 分割但尊重引号
    //
    // ⚠ v0.104.6 D89：只跟踪 `in_string` **不够** —— `Msg.payload` 与
    // `StateMutation.old/new` 是**原样插入的完整 Value JSON**，其内部 `{` / `[`
    // 与 `,` 都在「字符串外」。Dict 有 ≥2 个键时（`{"a":1,"b":2}`）解析器会从
    // payload 中间切一刀，`fields["payload"]` 只拿到截断的 `{"a":1`，随后
    // `json_to_value` 失败 → `.unwrap_or(Value::Nil)` —— **录制的数据静默变 Nil**。
    // 故切分条件必须同时要求：**不在字符串内** 且 **嵌套深度为 0**。
    let chars: Vec<char> = inner.chars().collect();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    let mut depth: usize = 0; // { } [ ] 嵌套深度（仅在字符串外有意义）
    let mut parts = Vec::new();
    let mut idx = 0;
    while idx < chars.len() {
        let c = chars[idx];
        idx += 1;
        if escape {
            current.push(c);
            escape = false;
            continue;
        }
        if c == '\\' && in_string {
            escape = true;
            current.push(c);
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            current.push(c);
            continue;
        }
        if !in_string {
            match c {
                '{' | '[' => depth += 1,
                '}' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if c == ',' && !in_string && depth == 0 {
            parts.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    for part in parts {
        if let Some(idx) = part.find(':') {
            let key = part[..idx].trim().trim_matches('"').to_string();
            let val = part[idx + 1..].trim().to_string();
            fields.insert(key, unquote(&val));
        }
    }
    let kind = fields.get("kind")?.as_str();
    match kind {
        "ai.chat" => Some(Event::AiChat {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            model: fields.get("model").cloned().unwrap_or_default(),
            prompt_hash: fields.get("prompt_hash").cloned().unwrap_or_default(),
            prompt_preview: fields.get("prompt_preview").cloned().unwrap_or_default(),
            response: fields.get("response").cloned().unwrap_or_default(),
            tokens_in: fields
                .get("tokens_in")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            tokens_out: fields
                .get("tokens_out")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            latency_ms: fields
                .get("latency_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            error: opt_error_field(&fields),
            // v0.76.04: 反序列化 arg_signature（旧记录无此字段→空串兼容）
            arg_signature: fields.get("arg_signature").cloned().unwrap_or_default(),
        }),
        "web.fetch" => Some(Event::WebFetch {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            url: fields.get("url").cloned().unwrap_or_default(),
            method: fields.get("method").cloned().unwrap_or_default(),
            status: fields
                .get("status")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            body_len: fields
                .get("body_len")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            latency_ms: fields
                .get("latency_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            error: opt_error_field(&fields),
            arg_signature: fields.get("arg_signature").cloned().unwrap_or_default(),
        }),
        "note" => Some(Event::Note {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            message: fields.get("message").cloned().unwrap_or_default(),
        }),
        // v0.83: msg + state_mutation — 解析 channel/payload/var/old/new
        // payload/old/new 是完整 Value JSON（用 flow::json_to_value 还原）
        "msg" => Some(Event::Msg {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            channel: fields.get("channel").cloned().unwrap_or_default(),
            payload: fields
                .get("payload")
                .and_then(|s| parse_json_value_field(s))
                .unwrap_or(crate::value::Value::Nil),
            prior_state_hash: fields
                .get("prior_state_hash")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        }),
        "state_mutation" => Some(Event::StateMutation {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            var: fields.get("var").cloned().unwrap_or_default(),
            old: fields
                .get("old")
                .and_then(|s| parse_json_value_field(s))
                .unwrap_or(crate::value::Value::Nil),
            new: fields
                .get("new")
                .and_then(|s| parse_json_value_field(s))
                .unwrap_or(crate::value::Value::Nil),
        }),
        _ => None,
    }
}

/// 反转义一个 JSON 字符串字段（去掉外层引号 + 解转义）。
///
/// v0.104.6 D208：补齐 **`\uXXXX`** 分支。
///
/// `esc`（本文件第 143 行）对 `< 0x20` 的控制字符产出 `\u00xx`，而本函数
/// 修前只认 `n` `r` `t` `\\` `\"` —— 其余落进「未知转义原样带出」，
/// 于是 `\u000c` 被读成**字面量 6 个字符**。实测：**29 个码点**往返不无损
/// （`< 0x20` 的 32 个减去 `\n` `\r` `\t`），`a\x0cb` 写出 `a\u000cb`、
/// 读回仍是 7 个字符，字符串被**静默拉长**。
///
/// 写端能产出、读端读不回来 —— 这对函数**互相不认**。
/// 与 D206（`flow/json.rs`）同族：同一条 JSON 规则，三处实现各缺各的。
///
/// 顺带补齐 JSON 标准的 `\b` `\f` `\/` 与**代理对**
/// （`😀` 在 ASCII 转义下是 `\uD83D\uDE00` 两个 UTF-16 码元）。
pub(super) fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        let chars: Vec<char> = inner.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' && i + 1 < chars.len() {
                match chars[i + 1] {
                    'n' => {
                        out.push('\n');
                        i += 2
                    }
                    'r' => {
                        out.push('\r');
                        i += 2
                    }
                    't' => {
                        out.push('\t');
                        i += 2
                    }
                    'b' => {
                        out.push('\u{0008}');
                        i += 2
                    }
                    'f' => {
                        out.push('\u{000C}');
                        i += 2
                    }
                    '\\' => {
                        out.push('\\');
                        i += 2
                    }
                    '/' => {
                        out.push('/');
                        i += 2
                    }
                    '"' => {
                        out.push('"');
                        i += 2
                    }
                    'u' => {
                        // 4 个 hex 位；不足则按「未知转义」处理
                        let hex: Option<String> =
                            chars.get(i + 2..i + 6).map(|s| s.iter().collect());
                        match hex.as_deref().and_then(|h| u32::from_str_radix(h, 16).ok()) {
                            Some(code) => {
                                i += 6; // 越过 `\uXXXX`
                                if (0xD800..0xDC00).contains(&code) {
                                    // 代理对：后面必须紧跟 `\uDC00`–`\uDFFF`
                                    let lo = if chars.get(i + 1) == Some(&'\\')
                                        && chars.get(i + 2) == Some(&'u')
                                    {
                                        chars
                                            .get(i + 3..i + 7)
                                            .map(|s| s.iter().collect::<String>())
                                            .and_then(|h| u32::from_str_radix(&h, 16).ok())
                                    } else {
                                        None
                                    };
                                    match lo {
                                        Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                                            let combined =
                                                0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                                            match char::from_u32(combined) {
                                                Some(c) => out.push(c),
                                                None => {
                                                    out.push_str("\\u");
                                                    out.push_str(&format!("{:04x}", code));
                                                }
                                            }
                                            i += 6;
                                        }
                                        // 落单的高代理：按未知转义处理，原样带出
                                        _ => {
                                            out.push('\\');
                                            out.push('u');
                                            i += 2;
                                        }
                                    }
                                } else if let Some(c) = char::from_u32(code) {
                                    out.push(c);
                                } else {
                                    // 落单的低代理等：原样带出，不静默吞掉
                                    out.push('\\');
                                    out.push('u');
                                    i += 2;
                                }
                            }
                            None => {
                                out.push('\\');
                                out.push('u');
                                i += 2;
                            }
                        }
                    }
                    // 未知转义：按 JSON 规范原样带出
                    other => {
                        out.push('\\');
                        out.push(other);
                        i += 2;
                    }
                }
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}

/// v0.104.6 D226：JSON 的 `null` 表示「**没有**这个可选字段」。
///
/// 修前 `error: fields.get("error").cloned()` 把字面量 `null` 也当成**有错误**，
/// 于是任何写了 `"error":null` 的录制（其它 producer、或手改过的文件）都会被
/// 报出幻影错误数 —— 而 `Errors: N` 是用户会据以行动的数字。
///
/// Mora 自己的写端（`event_to_jsonl`）在无错误时**直接省略**该字段，
/// 所以这个缺口只在**外来/手写**的录制上暴露。
fn opt_error_field(fields: &std::collections::HashMap<String, String>) -> Option<String> {
    let v = fields.get("error")?;
    if v.trim() == "null" {
        None
    } else {
        Some(v.clone())
    }
}

/// v0.83: 解析 JSON value 字段（用于 Msg.payload / StateMutation.old/new）。
///
/// 字段格式：`"json value"` （带外层引号）。json_to_value 直接接受
/// `"hello"` / `{"k":1}` / `[1,2,3]` / `null` / `42.0` 等。
///
/// simple comma-split parser 已经去掉了字段值的外层引号，所以我们需要
/// 把单值（如 `payload`、`42.0`）补回引号再用 json_to_value 解析。
fn parse_json_value_field(s: &str) -> Option<crate::value::Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 如果已经是完整 JSON 值（object/array/null/true/false/数字），
    // 直接解析。如果只是 bare 标识符（如 "payload"），加引号。
    let to_parse = if trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.starts_with('"')
        || trimmed == "null"
        || trimmed == "true"
        || trimmed == "false"
        || trimmed.parse::<f64>().is_ok()
    {
        trimmed.to_string()
    } else {
        // bare 字符串 —— 包成 JSON 字符串
        //
        // v0.104.6 D209：改用共享的 RFC 8259 转义器。此前只转 `\` 与 `"`，
        // 裸控制字符会原样进入，再交给 `json_to_value` 解析。
        format!("\"{}\"", crate::flow::escape_json_string(trimmed))
    };
    crate::flow::json_to_value(&to_parse).ok()
}
// ===================================================================
// v0.104.6 D208：`esc`（写）与 `unquote`（读）的往返契约
// ===================================================================

#[cfg(test)]
mod tests {
    use super::{esc, unquote};

    /// 按录制格式把 `s` 写进一个 JSON 字符串字段，再读回来。
    fn round_trip(s: &str) -> String {
        unquote(&format!("\"{}\"", esc(s)))
    }

    /// **主判据（有牙齿）**：`esc` 写出来的每个字符串，`unquote` 必须原样读回。
    ///
    /// v0.104.6 D208：`esc` 对 `< 0x20` 的控制字符产出 `\u00xx`（第 143 行），
    /// 而 `unquote` 的转义表**没有 `\uXXXX` 分支**（只认 `n` `r` `t` `\\` `\"`）——
    /// 落进 `Some(other) => { out.push('\\'); out.push(other); }`，
    /// 于是 `\u000c` 被读成**字面量 6 个字符** `\u000c`。
    ///
    /// 写端能产出、读端读不回来 —— 这对函数**互相不认**。
    /// 与 D206（`flow/json.rs` 缺 `\uXXXX`）同族：同一条 JSON 规则，
    /// 三处实现各缺各的。
    ///
    /// 用**穷举码点**而不是挑几个例子：判据必须能发现「下一个」坏字符。
    #[test]
    fn d208_esc_unquote_round_trip_is_lossless() {
        let mut bad: Vec<String> = Vec::new();
        // 0x00..=0x2FF 覆盖全部 BMP 控制字符 + 拉丁 + CJK 前段
        for cp in 0u32..=0x2FF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let src = format!("a{}b", c);
            let got = round_trip(&src);
            if got != src {
                bad.push(format!("U+{cp:04X} 写出 {} 读回 {}", esc(&src), got));
            }
        }
        // 再扫一批非 BMP（星平面，含代理对场景）
        for cp in [0x1F600u32, 0x1F680, 0x20000, 0x2A6D6] {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let src = format!("a{}b", c);
            let got = round_trip(&src);
            if got != src {
                bad.push(format!("U+{cp:06X} 写出 {} 读回 {}", esc(&src), got));
            }
        }
        assert!(
            bad.is_empty(),
            "`esc` 写出来的内容 `unquote` 读不回来（往返不无损），共 {} 个码点。\n\
             前 12 条:\n  {}\n\
             根因：`esc` 对控制字符产出 `\\u00xx`，而 `unquote` 没有 `\\uXXXX` 分支，\
             落进「未知转义原样带出」把它读成了字面量文本。",
            bad.len(),
            bad.iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    /// **正对照**：不回归 —— 常见的转义与非 ASCII 必须仍然正确。
    #[test]
    fn d208_common_cases_still_round_trip() {
        for s in [
            "plain ascii",
            "含中文的字符串",
            "emoji 😀 也在里面",
            "tab\there",
            "nl\nhere",
            "quote\"inside",
            "back\\slash",
            "换行\n与制表\t混排",
            "",
        ] {
            assert_eq!(
                round_trip(s),
                s,
                "[{s:?}] 往返不无损 —— 这是**不回归**判据，不该在这里红"
            );
        }
    }

    /// **D239：`esc` / `unquote` 的**全码点空间**往返普查。**
    ///
    /// D208 的穷举只覆盖 `0x00..=0x2FF` + 4 个星平面点
    /// （`😀` / `🚀` / `𠀀` / `⛝`），**其余 110 万个码点从未被扫过**。
    /// 本条把范围扩到 `0x00..=0x10FFFF`（Unicode 全部合法标量值），
    /// 一次跑完（约 111 万次往返，秒级）。
    ///
    /// 判据形态：**穷举**而非抽查 —— 抽查只能证明「这几个字符没问题」，
    /// 穷举才能发现「下一个」坏字符。
    ///
    /// 顺带验证 `esc` 未采用共享的 `flow::escape_json_string`（缺 `\b` `\f`）
    /// 是否造成实际损失：`esc` 对 0x08/0x0C 产出 `\u0008` / `\u000c`
    /// （**合法** JSON，只是非惯用形式），`unquote` 能读回 ⇒ 不损失。
    /// 若这条判据红了，说明问题在**往返**而非形式。
    #[test]
    fn d239_esc_unquote_round_trip_covers_whole_unicode_space() {
        let mut bad: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue; // 代理项（D800–DFFF）不是合法标量值
            };
            // 三种上下文：单独 / 前缀 / 后缀 ——
            // 单独会漏掉「紧邻转义符」的边界问题（`a` + `\` + `b` 等）
            for src in [c.to_string(), format!("a{c}b"), format!("{c}\\\"{c}")] {
                checked += 1;
                let got = round_trip(&src);
                if got != src {
                    bad.push(format!(
                        "U+{cp:06X} 上下文 {:?} 写出 {} 读回 {:?}",
                        src.chars().count(),
                        esc(&src),
                        got
                    ));
                    if bad.len() >= 20 {
                        break;
                    }
                }
            }
            if bad.len() >= 20 {
                break;
            }
        }
        assert!(
            bad.is_empty(),
            "D239: `esc` → `unquote` 在全 Unicode 码点空间（0x00..=0x10FFFF，\
             已检查 {checked} 个上下文）有 {} 处往返不无损：\n  {}\n\
             这是穷举判据（不是抽查），任何新增的坏字符都会被它抓住。",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// **D239 对照组：`esc` 的产出必须是**合法 JSON 字符串**。**
    ///
    /// 往返无损只保证「自己认自己」。若 `esc` 产出的是**非法** JSON
    /// （如漏转义某个控制字符），那么**外部** JSON 解析器
    /// （Python / PowerShell / 别的工具）读这份录制就会失败 ——
    /// 而录制文件是要跨工具流转的。
    ///
    /// 判据用手写的最小 JSON 字符串校验器（只查 RFC 8259 §7 要求的
    /// 两类字符 + 控制字符），**不引入 serde**（本仓零 serde 依赖原则）。
    #[test]
    fn d239_esc_output_is_valid_json_string_body() {
        let mut bad: Vec<String> = Vec::new();
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let body = esc(&c.to_string());
            // 校验：反斜杠后必须跟合法转义首字符；且不得含裸控制字符
            let bytes = body.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    if i + 1 >= bytes.len() {
                        bad.push(format!("U+{cp:06X} 反斜杠在末尾: {body:?}"));
                        break;
                    }
                    let next = bytes[i + 1];
                    if !matches!(
                        next,
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
                    ) {
                        bad.push(format!("U+{cp:06X} 非法转义 \\{}: {body:?}", next as char));
                        break;
                    }
                    i += if next == b'u' { 6 } else { 2 };
                } else if bytes[i] < 0x20 {
                    bad.push(format!(
                        "U+{cp:06X} 裸控制字符 0x{:02x} 未转义: {body:?}",
                        bytes[i]
                    ));
                    break;
                } else {
                    i += 1;
                }
            }
            if bad.len() >= 10 {
                break;
            }
        }
        assert!(
            bad.is_empty(),
            "D239: `esc` 对某些码点产出的不是**合法 JSON 字符串体**：\n  {}\n\
             往返无损只保证「自己认自己」；录制文件要跨工具流转，\
             外部解析器必须也能读。",
            bad.join("\n  ")
        );
    }

    /// **D239：穷举证明 `esc` 与共享 `flow::escape_json_string` 产出等价。**
    ///
    /// D209 把全仓 **14 处**手写 JSON 转义链收敛为唯一的
    /// `flow::escape_json_string`，但 `record` 的 `esc`（本文件，20 个调用点）
    /// **不在其中** —— 它是同一规则的**第 15 份实现**，只差两个码点：
    ///
    /// | 码点 | `esc`（record） | `flow::escape_json_string` |
    /// |---|---|---|
    /// | `U+0008` BACKSPACE | `\u0008` | `\b` |
    /// | `U+000C` FORM FEED | `\u000c` | `\f` |
    ///
    /// 两者**都是合法 JSON**、`unquote` 都能读回（上面两条穷举判据已证）
    /// ⇒ **无功能后果**，是收敛不彻底而非缺陷。
    ///
    /// 本条把「必须等价」变成**可执行断言**：将来任一侧改动而另一侧没跟上，
    /// 立刻变红。
    #[test]
    fn d239_esc_matches_shared_escape_json_string() {
        let mut diffs: Vec<String> = Vec::new();
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let s = c.to_string();
            let a = esc(&s);
            let b = crate::flow::escape_json_string(&s);
            if a != b {
                diffs.push(format!("U+{cp:06X} {c:?}: esc={a:?} flow={b:?}"));
                if diffs.len() >= 10 {
                    break;
                }
            }
        }
        assert!(
            diffs.is_empty(),
            "D239: `record::esc` 与共享的 `flow::escape_json_string` 产出不一致（\
             D209 收敛 14 处后的**遗漏**）。全码点空间共 {} 处差异：\n  {}\n\
             修法：让 `esc` 直接转发到 `flow::escape_json_string`，\
             「同一事实两套算法」即从根上消失。",
            diffs.len(),
            diffs.join("\n  ")
        );
    }

    /// **D239 兼容性：旧格式（\u0008）与新格式（\b）都必须能读回。**
    ///
    /// `esc` 转发到共享实现后，写出的字节变了（`\u0008` → `\b`）。
    /// 这**必须**是安全的：`mora record diff` 比对的是**解析后**的事件
    /// （`summarize_event`），不是原始字节；而 `record::audit` 的 hash 链
    /// 走的是 `audit/mod.rs` 的另一套（`extract_field_skip_escaped`，D207 修过），
    /// 不经过 `esc`。
    ///
    /// 本条**直接喂两种转义形式**给 `unquote`，证明旧的录制文件
    /// 仍可被新代码读回。
    #[test]
    fn d239_both_escape_forms_are_readable() {
        let backspace = "\u{0008}";
        let form_feed = "\u{000c}";
        // 两种转义形式都必须解出同一个字符。
        // ⚠ 输入是 `"a<ESC>b"` 形式（转义 + 后续字符），
        //   期望值必须含那个后续字符 —— 我第一版误写成只期望转义字符本身。
        assert_eq!(
            unquote("\"a\\u0008b\""),
            format!("a{backspace}b"),
            "u0008 转义"
        );
        assert_eq!(
            unquote("\"a\\bb\""),
            format!("a{backspace}b"),
            "标准 \\b 转义"
        );
        assert_eq!(
            unquote("\"a\\u000cb\""),
            format!("a{form_feed}b"),
            "u000c 转义"
        );
        assert_eq!(
            unquote("\"a\\fb\""),
            format!("a{form_feed}b"),
            "标准 \\f 转义"
        );
    }
}
