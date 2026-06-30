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
        } => {
            let mut s = format!(
                r#"{{"kind":"ai.chat","id":{},"ts_ms":{},"model":"{}","prompt_hash":"{}","prompt_preview":"{}","response":"{}","tokens_in":{},"tokens_out":{},"latency_ms":{}"#,
                id,
                ts_ms,
                esc(model),
                prompt_hash,
                esc(prompt_preview),
                esc(response),
                tokens_in,
                tokens_out,
                latency_ms
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
        } => {
            let mut s = format!(
                r#"{{"kind":"web.fetch","id":{},"ts_ms":{},"url":"{}","method":"{}","status":{},"body_len":{},"latency_ms":{}"#,
                id,
                ts_ms,
                esc(url),
                method,
                status,
                body_len,
                latency_ms
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
    }
}

pub(super) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
                },
            ))
        }
        Event::WebFetch {
            url,
            status,
            body_len,
            latency_ms,
            error,
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
                },
            ))
        }
        Event::Note { .. } => None,
    }
}

/// 从 JSONL 文件加载事件 (简化解析: 因为我们写的格式固定, 用字符串匹配)
pub(super) fn load_jsonl(path: &Path) -> Result<Vec<Event>, String> {
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
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("recorder: read error at line {}: {}", idx + 1, e))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // v0.14: 暂用简化 JSON 解析, 提取关键字段
        if let Some(ev) = parse_event_line(trimmed) {
            events.push(ev);
        }
        // 解析失败的行跳过 (前向兼容)
    }
    Ok(events)
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
    let chars: Vec<char> = inner.chars().collect();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
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
        if c == ',' && !in_string {
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
            error: fields.get("error").cloned(),
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
            error: fields.get("error").cloned(),
        }),
        "note" => Some(Event::Note {
            id: fields.get("id").and_then(|s| s.parse().ok()).unwrap_or(0),
            ts_ms: fields
                .get("ts_ms")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            message: fields.get("message").cloned().unwrap_or_default(),
        }),
        _ => None,
    }
}

pub(super) fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => {
                        out.push('\\');
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}
