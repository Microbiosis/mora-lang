//! v0.75.63: JSON 编解码 — 自 flow.rs 拆出（D6 单文件惯例）。
//! json_to_value（手写递归解析）+ value_to_json（序列化）。
//! 零 flow 依赖（纯 Value 转换），经 pub use 保持 flow:: 路径。

use crate::value::Value;

/// JSON 字符串转 Value
pub fn json_to_value(json: &str) -> Result<Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Empty JSON".to_string());
    }
    parse_json_value(trimmed).map(|(v, _)| v)
}

/// JSON 解析辅助
///
/// v0.52 bug fix: 返回的 `consumed` 包含 trim 掉的 leading whitespace 字节数
/// （之前 `trim_start()` 后 return consumed，但 consumed 是 trim 后偏移，
/// 调用方 `i += consumed` 算原始 s 偏移会少算 trim 字节，导致 dict 内空格错位）
fn parse_json_value(s: &str) -> Result<(Value, usize), String> {
    let ws_consumed = skip_ws(s.as_bytes(), 0);
    let trimmed = &s[ws_consumed..];
    if trimmed.is_empty() {
        return Err("Empty JSON value".to_string());
    }
    let (val, inner_consumed) = match trimmed.as_bytes()[0] {
        b'"' => parse_json_string(trimmed)?,
        b'[' => parse_json_list(trimmed)?,
        b'{' => parse_json_dict(trimmed)?,
        b't' | b'f' => parse_json_bool(trimmed)?,
        b'n' => parse_json_null(trimmed)?,
        b'0'..=b'9' | b'-' => parse_json_number(trimmed)?,
        _ => return Err(format!("Unexpected character in JSON: {}", trimmed)),
    };
    Ok((val, ws_consumed + inner_consumed))
}

fn parse_json_string(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'"' {
        return Err("Expected '\"'".to_string());
    }
    let b = s.as_bytes();
    let mut i = 1;
    let mut result = String::new();
    while i < s.len() {
        match b[i] {
            b'"' => return Ok((Value::String(result), i + 1)),
            b'\\' => {
                if i + 1 >= s.len() {
                    return Err("Unterminated string escape".to_string());
                }
                result.push_str(&decode_escape(s, &mut i)?);
            }
            // v0.104.6 D205：原始字节必须按 **UTF-8** 解码。
            //
            // 修前是 `c => result.push(c as char)` —— `c: u8`，而 Rust 的
            // `u8 as char` 是 **Latin-1 解释**（把字节当成同数值的码点）。
            // 于是每个 UTF-8 字节变成一个码点，实测：
            //
            // ```text
            // json.parse("{\"greeting\": \"你好世界\"}")
            //   → print(v["greeting"])  打印 ä½ å¥½ä¸çå¥½
            //   → print(len(...))      得到 12（正确的值是 4）
            // ```
            //
            // **长度都被改了**（4 个汉字 = 12 字节 → 12 个码点）：任何按长度
            // 索引、切片、比较、哈希的下游全错。这属于 D198 的
            // 「静默给错数」，比乱码本身更危险 —— 乱码还看得见，错误的长度看不见。
            //
            // 非法 UTF-8 **报错**而不是静默产出错的字符串。
            _ => {
                let start = i;
                while i < s.len() && b[i] != b'"' && b[i] != b'\\' {
                    i += 1;
                }
                let chunk = std::str::from_utf8(&b[start..i])
                    .map_err(|e| format!("Invalid UTF-8 in JSON string: {}", e))?;
                result.push_str(chunk);
                // `i` 已停在 `"` / `\` 上，不能再 `+= 1`
                continue;
            }
        }
        i += 1;
    }
    Err("Unterminated string".to_string())
}

/// v0.104.6 D206：处理一个 `\X` 转义，返回解出的文本。
///
/// `i` 进来时指向**反斜杠**，出去时指向**最后一个被消费的字节**
/// （与 `parse_json_string` 循环末尾的 `i += 1` 配套）。
///
/// 修前这张表只有 `\"` `\\` `\n` `\t` `\r` `\0`，其余一律
/// `Invalid escape` —— 而 **`\uXXXX` 是 JSON 标准的一部分**。
/// 实测：
///
/// ```text
/// json.parse("{\"g\": \"\u4f60\u597d\"}")
///   → Runtime error (MIR): json.parse: Invalid escape: \u
/// ```
///
/// 而 **Python 的 `json.dumps()` 默认 `ensure_ascii=True`**，产出的就是
/// `\uXXXX` —— 即**最常见的 JSON 生成方式**产出的含非 ASCII 文件，
/// 本语言**根本读不了**。同时补齐 JSON 标准的 `\b` / `\f` / `\/`，
/// 以及代理对（`😀` 在 ASCII 转义下是 `\uD83D\uDE00` 两个 UTF-16 码元）。
fn decode_escape(s: &str, i: &mut usize) -> Result<String, String> {
    let b = s.as_bytes();
    let esc = b[*i + 1];
    *i += 1; // 指向转义字符本身
    Ok(match esc {
        b'"' => "\"".to_string(),
        b'\\' => "\\".to_string(),
        b'/' => "/".to_string(),
        b'n' => "\n".to_string(),
        b't' => "\t".to_string(),
        b'r' => "\r".to_string(),
        b'b' => "\u{0008}".to_string(),
        b'f' => "\u{000C}".to_string(),
        b'0' => "\0".to_string(),
        b'u' => {
            let hex4 = s
                .get(*i + 1..*i + 5)
                .ok_or_else(|| "Truncated \\u escape".to_string())?;
            let code = u32::from_str_radix(hex4, 16)
                .map_err(|_| format!("Invalid \\u escape: \\u{}", hex4))?;
            *i += 4; // 指向第 4 个 hex 位
            if (0xD800..0xDC00).contains(&code) {
                // 高代理：后面必须紧跟 `\uDC00`–`\uDFFF`
                if *i + 6 >= s.len() || b[*i + 1] != b'\\' || b[*i + 2] != b'u' {
                    return Err(format!(
                        "Invalid escape: unpaired surrogate \\u{:04X}",
                        code
                    ));
                }
                let hex2 = &s[*i + 3..*i + 7];
                let lo = u32::from_str_radix(hex2, 16)
                    .map_err(|_| format!("Invalid \\u escape: \\u{}", hex2))?;
                if !(0xDC00..0xE000).contains(&lo) {
                    return Err(format!(
                        "Invalid escape: \\u{:04X} is not followed by a low surrogate",
                        code
                    ));
                }
                *i += 6;
                let combined = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                match char::from_u32(combined) {
                    Some(c) => c.to_string(),
                    None => return Err("Invalid escape: bad surrogate pair".to_string()),
                }
            } else if (0xDC00..0xE000).contains(&code) {
                return Err(format!(
                    "Invalid escape: unpaired low surrogate \\u{:04X}",
                    code
                ));
            } else {
                match char::from_u32(code) {
                    Some(c) => c.to_string(),
                    None => return Err(format!("Invalid escape: \\u{:04X}", code)),
                }
            }
        }
        other => return Err(format!("Invalid escape: \\{}", other as char)),
    })
}

/// v0.35 (P0-D1): byte-index whitespace skipper. The old code used
/// `&s[i..].trim_start()` which allocated a new `&str` and re-scanned
/// remaining bytes on every iteration → O(n²) on whitespace-heavy JSON.
/// This scans the byte slice directly with no slicing and no allocation.
fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        match s[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => break,
        }
    }
    i
}

fn parse_json_list(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'[' {
        return Err("Expected '['".to_string());
    }
    let bytes = s.as_bytes();
    let mut items = Vec::new();
    let mut i = 1;
    loop {
        i = skip_ws(bytes, i);
        if i >= bytes.len() {
            return Err("Unterminated list".to_string());
        }
        if bytes[i] == b']' {
            i += 1;
            break;
        }
        if !items.is_empty() {
            if bytes[i] != b',' {
                return Err("Expected ',' in list".to_string());
            }
            i += 1;
            i = skip_ws(bytes, i);
        }
        let (val, consumed) = parse_json_value(&s[i..])?;
        items.push(val);
        i += consumed;
    }
    Ok((Value::List(items.into()), i))
}

fn parse_json_dict(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'{' {
        return Err("Expected '{'".to_string());
    }
    let bytes = s.as_bytes();
    let mut map = std::collections::HashMap::new();
    let mut i = 1;
    loop {
        i = skip_ws(bytes, i);
        if i >= bytes.len() {
            return Err("Unterminated dict".to_string());
        }
        if bytes[i] == b'}' {
            i += 1;
            break;
        }
        if !map.is_empty() {
            if bytes[i] != b',' {
                return Err("Expected ',' in dict".to_string());
            }
            i += 1;
            i = skip_ws(bytes, i);
        }
        let (key, key_consumed) = parse_json_string(&s[i..])?;
        let key_str = match key {
            Value::String(s) => s,
            _ => return Err("JSON object key must be a string".to_string()),
        };
        i += key_consumed;
        i = skip_ws(bytes, i);
        if i >= bytes.len() || bytes[i] != b':' {
            return Err("Expected ':' in dict".to_string());
        }
        i += 1;
        let (val, val_consumed) = parse_json_value(&s[i..])?;
        map.insert(key_str, val);
        i += val_consumed;
    }
    Ok((Value::Dict(map), i))
}

fn parse_json_bool(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("true") {
        Ok((Value::Bool(true), 4))
    } else if s.starts_with("false") {
        Ok((Value::Bool(false), 5))
    } else {
        Err("Expected boolean".to_string())
    }
}

fn parse_json_null(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("null") {
        Ok((Value::Nil, 4))
    } else {
        Err("Expected null".to_string())
    }
}

fn parse_json_number(s: &str) -> Result<(Value, usize), String> {
    let mut i = 0;
    let mut has_decimal = false;
    let mut has_exponent = false;
    if i < s.len() && s.as_bytes()[i] == b'-' {
        i += 1;
    }
    while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
        i += 1;
    }
    if i < s.len() && s.as_bytes()[i] == b'.' {
        has_decimal = true;
        i += 1;
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < s.len() && (s.as_bytes()[i] == b'e' || s.as_bytes()[i] == b'E') {
        has_exponent = true;
        i += 1;
        if i < s.len() && (s.as_bytes()[i] == b'+' || s.as_bytes()[i] == b'-') {
            i += 1;
        }
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    let num_str = &s[..i];
    // v0.84: 区分整数 vs 浮点数 — 不含小数点和指数时按 Int 解析，
    // 保持 value_to_json / parse_json_number 的类型对称性。
    // value_to_json: Int(42) → "42"; Float(42.0) → "42.0"
    // parse_json_number: "42" → Int(42); "42.0" → Float(42.0)
    if !has_decimal && !has_exponent {
        // 整数路径：i64 → BigInt
        //
        // v0.104.6 D197：修前是「先试 i64，**溢出时回退 Float**」——
        // 静默丢精度，且丢得**无声无息**：
        //
        // ```text
        // 18446744073709551615  (u64::MAX) →  18446744073709551616.0   ← 差 1
        // -9223372036854775809             →  -9223372036854775808.0  ← 差 1
        // 12345678901234567890              →  12345678901234567168.0  ← 差 5 位有效数字
        // ```
        //
        // 本语言**已有**真任意精度的 `Value::BigInt`（num-bigint 后端，
        // 字面量语法 `<digits>n`），算术 promotion 也已就位。回落 Float
        // 既无必要、又危险：对处理 ID / 金额 / 序号的 agent 来说，
        // 「拿到一个不同的数字」比「拿到一个 Float」糟得多。
        if let Ok(n) = num_str.parse::<i64>() {
            Ok((Value::Int(n), i))
        } else {
            // 超出 i64 → BigInt（任意精度，**不丢一位**）
            let b: num_bigint::BigInt = num_str
                .parse()
                .map_err(|_| format!("Invalid number: {}", num_str))?;
            Ok((Value::BigInt(b), i))
        }
    } else {
        let num: f64 = num_str
            .parse()
            .map_err(|_| format!("Invalid number: {}", num_str))?;
        Ok((Value::Float(num), i))
    }
}

/// JSON 字符串转义（RFC 8259）—— 返回**不含**外层引号的转义结果。
///
/// v0.104.6 D209：新增。仓库里原本有**六处**手写转义链，每处转义的字符集
/// 都不同，且**没有一处**处理除 `\n` `\r` `\t` 外的控制字符，于是产出
/// **非法 JSON**（真实 `mora` + Python `json.loads` 实测）：
///
/// ```text
/// json.stringify("a\nb")  →  22 61 0A 62 22    （裸换行在字符串里）
///   Python: JSONDecodeError: Invalid control character
/// json.stringify('"')     →  22 22 22          （Char 一点都没转义）
///   Python: JSONDecodeError: Extra data
/// json.stringify({"k\ny": "v"})  →  7B 22 6B 0A 79 22 ...  （裸换行在 key 里）
///   Python: JSONDecodeError: Invalid control character
/// ```
///
/// ⚠ **Mora 自己的 `json.parse` 接受这些输出** —— `parse_json_string` 按字节
/// 收集到 `"` 为止，裸换行就原样进了结果。所以**语言内部往返是通的**，
/// 这正是它一直没被发现的原因：**自洽但不合规**。只有**外部**解析器
/// （Python / JS / Go / 任何真实 API）才会拒绝，而那正是 `json.stringify`
/// 的用途 —— 把结构化数据交给外部。
///
/// 规则（RFC 8259 §7）：`"` 与 `\` 必须转义；`< 0x20` 的控制字符**必须**
/// 转义（`\b` `\f` `\n` `\r` `\t` 用短形式，其余用 `\u00xx`）；
/// `/` 与非 ASCII **不必**转义，保持原样可读。
pub fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
/// Value 转 JSON 字符串
pub fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", escape_json_string(s)),
        Value::Char(c) => format!("\"{}\"", escape_json_string(&c.to_string())),
        // v0.38: Int formatted without decimal; Float always shows decimal.
        // v0.84: Float 必须始终输出小数点，即使 fract() == 0.0（如 42.0 → "42.0"），
        // 以保持与 parse_json_number 的类型对称性。parse_json_number 对不含小数点的
        // 数字解析为 Int，含小数点的解析为 Float。若 Float 输出 "42"，反序列化后会
        // 变成 Int(42)，类型降级不可逆。
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            // v0.104.6 D99：**非有限浮点不是 JSON**。
            //
            // 此前只判 `f.fract() == 0.0` 决定是否补小数点，而
            // `inf.fract()` 与 `NaN.fract()` **都是 NaN**，`NaN == 0.0` 恒假
            // → 落进 `format!("{}", f)`，Rust 输出裸的 `inf` / `NaN` ——
            // 那不是 JSON 字面量：语言自己的 `json.parse` 读不回来
            // （实测 `Unexpected character in JSON: inf`），PowerShell 等
            // 真实解析器同样拒绝（`Invalid JSON primitive: inf.`）。
            //
            // `1.0 / 0.0` 是最普通的算术，故非边缘情形。
            // 非有限 → `null`：JSON 表示不了它们的通行做法，也与
            // `http_server::value_to_json`（经 `JsonValue` 映射，已输出 `null`）
            // 行为一致 —— 此前两处**不一致**。
            if !f.is_finite() {
                "null".to_string()
            } else if f.fract() == 0.0 {
                // 如果 fract() == 0.0，format!("{}", f) 会输出 "42" 无小数点，
                // 与 Int 不可区分。用 "{:.1}" 强制至少一位小数："42.0"。
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        // v0.91: BigInt → JSON 数字字符串（保留精度）。
        Value::BigInt(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Dict(map) => {
            // v0.104.6 可复现性修复：按 key 排序输出。
            //
            // `Value::Dict` 底层是 `HashMap`，其 `RandomState` 每进程随机 →
            // `map.iter()` 的顺序**每次运行都不同**。JSON 对象的键序在语义上无关
            // （RFC 8259 明确对象是无序的），故这是纯粹的可复现性问题，但后果是
            // 实打实的：`json.*` builtin 的输出不可复现 → 无法写断言 / diff /
            // 哈希签名 / 缓存。仓库内已有正确先例：`http_server.rs::value_to_json`
            // 与 `mcp_server.rs::mora_to_json` 都先收进 `BTreeMap` 再输出。
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("\"{}\":{}", escape_json_string(k), value_to_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Task { name, .. } => format!("\"<task {}>\"", name),
        Value::Tool { name, .. } => format!("\"<tool {}>\"", name),
        Value::Closure { .. } => "\"<closure>\"".to_string(),
        Value::Builtin(name) => format!("\"<builtin {}>\"", name),
        Value::Conversation { model, .. } => format!("\"<conversation {}>\"", model),
        Value::Stream { .. } => "\"<stream>\"".to_string(),
        Value::Agent { name, .. } => format!("\"<agent {}>\"", name),
        Value::AiConfig { .. } => "\"<ai_config>\"".to_string(),
        Value::Router { .. } => "\"<router>\"".to_string(),
        Value::HttpRequest { method, path, .. } => {
            format!("\"<http_request {} {}>\"", method, path)
        }
        Value::McpServer { .. } => "\"<mcp_server>\"".to_string(),
        Value::TraitObject { .. } => "\"<trait_object>\"".to_string(),
        Value::Compose(_) => "null".to_string(),
        Value::Partial(_, _) => "null".to_string(),
        Value::Atom(arc) => value_to_json(&arc.lock()),
        Value::Macro { .. } => "null".to_string(),
        // v0.86: Curry — 柯里化函数无法 JSON 表示，用 null 占位。
        Value::Curry { .. } => "null".to_string(),
        // v0.86: Cons — 链式列表单元用 null 占位（非标准 JSON 结构）。
        Value::Cons { .. } => "null".to_string(),
        // v0.86: Code — 源码文本值用 JSON 字符串表示。
        Value::Code(s) => value_to_json(&Value::String(s.clone())),
        Value::PromptSection { .. } => "null".to_string(),
        Value::Document { backend, .. } => {
            format!("\"<document origin=\\\"{}\\\">\"", backend.origin())
        }
        // v0.83: TEA types — 占位字符串（replay 应通过专用通道恢复）
        Value::TeaApp(_) => "\"<tea_app>\"".to_string(),
        Value::TeaCmd(cmd) => match cmd {
            crate::tea::Cmd::None => "null".to_string(),
            crate::tea::Cmd::Batch(items) => {
                let parts: Vec<String> =
                    items.iter().map(|c| value_to_json(&c.to_value())).collect();
                format!("[{}]", parts.join(","))
            }
            crate::tea::Cmd::Perform { effect, args } => {
                let arg_jsons: Vec<String> = args.iter().map(value_to_json).collect();
                format!(
                    "{{\"kind\":\"Perform\",\"effect\":\"{}\",\"args\":[{}}}",
                    effect,
                    arg_jsons.join(",")
                )
            }
            crate::tea::Cmd::Dispatch(msg) => {
                format!("{{\"kind\":\"Dispatch\",\"msg\":{}}}", value_to_json(msg))
            }
        },
        Value::TeaMsg(msg) => value_to_json(&msg.to_value()),
        // v0.102: 声明式范式值 — 无 JSON 形态，序列化为描述串
        Value::Relation { name, clauses } => {
            format!("\"<relation {}/{}>\"", name, clauses.len())
        }
        Value::Goal(_) => "\"<goal>\"".to_string(),
        Value::LogicVar(id) => format!("\"_.{}\"", id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Int / Float 类型对称性（v0.84） ──

    #[test]
    fn int_roundtrip_symmetry() {
        // Int → JSON → Int，类型不变
        let v = Value::Int(42);
        let json = value_to_json(&v);
        assert_eq!(json, "42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(42));
    }

    #[test]
    fn float_with_fraction_roundtrip_symmetry() {
        // Float(1.5) → JSON → Float(1.5)，类型不变
        let v = Value::Float(1.5);
        let json = value_to_json(&v);
        assert!(json.starts_with("1.5"));
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - 1.5).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn float_integer_value_uses_decimal_point() {
        // Float(42.0) → "42.0"（含小数点，与 Int(42) → "42" 区分）
        let v = Value::Float(42.0);
        let json = value_to_json(&v);
        assert_eq!(json, "42.0");
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - 42.0).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn negative_int_roundtrip() {
        // Int(-42) → "-42" → Int(-42)
        let v = Value::Int(-42);
        let json = value_to_json(&v);
        assert_eq!(json, "-42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(-42));
    }

    #[test]
    fn negative_float_roundtrip() {
        // Float(-1.5) → "-1.5" → Float(-1.5)
        let v = Value::Float(-1.5);
        let json = value_to_json(&v);
        assert!(json.starts_with("-1.5"));
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - (-1.5)).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn zero_int_vs_zero_float_distinct() {
        // Int(0) → "0" → Int(0)
        // Float(0.0) → "0.0" → Float(0.0)
        let ji = value_to_json(&Value::Int(0));
        let jf = value_to_json(&Value::Float(0.0));
        assert_eq!(ji, "0");
        assert_eq!(jf, "0.0");
        assert_eq!(json_to_value(&ji).unwrap(), Value::Int(0));
        match json_to_value(&jf).unwrap() {
            Value::Float(f) => assert!((f - 0.0).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn scientific_notation_parsing_yields_float() {
        // "1e5" → Float(100000.0)，含 e/E 始终 Float
        let v = json_to_value("1e5").unwrap();
        match v {
            Value::Float(f) => assert!((f - 100000.0).abs() < 1e-6),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn large_integer_overflow_becomes_bigint_not_float() {
        // v0.104.6 D197：此测试原先叫 `large_integer_overflow_falls_back_to_float`，
        // 断言「超 i64 → **Float**」—— 也就是把**有缺陷的行为**钉成了期望。
        // 那不是「防 panic」的合理设计，是**静默的数值损坏**：
        //
        // ```text
        // 18446744073709551615  →  18446744073709551616.0   ← 差 1，且无任何提示
        // ```
        //
        // 本语言已有真任意精度的 `BigInt`（num-bigint），故改为产出 BigInt。
        // 期望随之翻转。
        let v = json_to_value("999999999999999999999").unwrap();
        match &v {
            Value::BigInt(b) => assert_eq!(
                b.to_string(),
                "999999999999999999999",
                "BigInt 必须**逐位**保留，不得丢精度"
            ),
            other => panic!("expected BigInt for overflow integer, got {:?}", other),
        }
        // 顺带钉住「不 panic」这条旧意图（它本身是对的，只是载体从 Float 换成 BigInt）。
        assert_ne!(json_to_value("999999999999999999999"), Err(String::new()));
    }

    // ── 嵌套结构中 Int/Float 类型保留 ──

    #[test]
    fn dict_int_value_roundtrip() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("x".to_string(), Value::Int(42));
        map.insert("y".to_string(), Value::Float(1.5));
        let v = Value::Dict(map);
        let json = value_to_json(&v);
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Dict(m) => {
                assert_eq!(m.get("x"), Some(&Value::Int(42)));
                match m.get("y") {
                    Some(Value::Float(f)) => assert!((f - 1.5).abs() < 1e-9),
                    other => panic!("expected Float(1.5), got {:?}", other),
                }
            }
            other => panic!("expected Dict, got {:?}", other),
        }
    }
}
