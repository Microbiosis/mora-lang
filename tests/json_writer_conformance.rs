//! v0.104.6 D209：`json.stringify` 产出**非法 JSON** —— 裸控制字符直接躺在
//! 字符串字面量里（已修）。
//!
//! ## 缺陷
//!
//! `flow/json.rs::value_to_json` 修前的三个位点各写各的转义：
//!
//! ```rust
//! Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),  // 只转 2 种
//! Value::Char(c)   => format!("\"{}\"", c),                                             // 一种都不转
//! // Dict 的 key：与 String 同一套不完整逻辑                                              // 只转 2 种
//! ```
//!
//! RFC 8259 §7 要求：字符串字面量内**不得**出现未转义的控制字符（`U+0000`–`U+001F`）。
//!
//! ## 后果：**自洽但不合规** —— 这是它一直没被发现的原因
//!
//! **Mora 自己的 `json.parse` 接受这些输出**（`parse_json_string` 按字节收集到 `"` 为止，
//! 裸换行原样进结果），所以**语言内部往返是通的**，任何内部往返测试都是绿的。
//! 只有**外部**解析器会拒绝 —— 而那正是 `json.stringify` 的用途。
//!
//! 真实 `mora run` + **Python `json.loads`（RFC 8259 参考实现）**实测：
//!
//! ```text
//! json.stringify("a\nb")       → 22 61 0A 62 22
//!   Python: JSONDecodeError: Invalid control character at: line 1 column 3
//! json.stringify('"')          → 22 22 22
//!   Python: JSONDecodeError: Extra data: line 1 column 3
//! json.stringify({"k\ny":"v"}) → 7B 22 6B 0A 79 22 3A 22 76 22 7D
//!   Python: JSONDecodeError: Invalid control character at: line 1 column 4
//! ```
//!
//! ## 同族：6 处手写转义链，逐字符集各不相同
//!
//! | 位置 | 修前转义的字符 |
//! |---|---|
//! | `flow/json.rs` `String` | `\` `"` |
//! | `flow/json.rs` `Char` | **无** |
//! | `flow/json.rs` Dict key | `\` `"` |
//! | `compress/text.rs` 请求体 | `\` `"` `\n` |
//! | `ai_chat.rs`（3 处） | `\` `"` `\n` [`\r` `\t`] |
//! | `ai_helpers.rs`（4 处） | `\` `"` `\n` `\r` `\t` |
//! | `http_server.rs::json_error` | `\` `"` |
//!
//! **没有一处**处理除 `\n` `\r` `\t` 外的控制字符 —— 即含换页符的 prompt
//! 会让发往 OpenAI API 的请求体成为非法 JSON。
//!
//! 修法：**一份** `flow::escape_json_string`（RFC 8259），7 处全部改用它。
//! 从此转义规则只有**一处**实现，不再可能各自漂移。
//!
//! ## 判据
//!
//! **主判据**把 RFC 8259 的规则直接写成断言：**产出的 JSON 文本里不得出现
//! 任何 `U+0000`–`U+001F`**。这条不依赖任何外部解析器，且穷举码点 ——
//! 只挑几个例子会漏掉「下一个」坏字符。
//!
//! 另有：`Char` 与 Dict key 两个位点单独钉（**只修 String 会漏掉它们**）、
//! Mora 自身 parser 的往返、真实 CLI 路径。

use mora::flow::{json_to_value, value_to_json};
use std::path::PathBuf;
use std::process::Command;

/// 产出里出现的**未转义控制字符** —— 修前应为空。
fn raw_control_chars(json: &str) -> Vec<char> {
    json.chars().filter(|c| (*c as u32) < 0x20).collect()
}

fn show(json: &str) -> String {
    json.chars()
        .map(|c| {
            if (c as u32) < 0x20 {
                format!("<{:02X}>", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// **主判据（有牙齿）**：`json.stringify` 的产出**不得含任何裸控制字符**。
///
/// 穷举 `0x00..=0x2FF` + 4 个非 BMP 码点。修前 `0x0A` / `0x09` / `0x0B` …
/// 全部会亮红灯。
#[test]
fn d209_stringify_never_emits_raw_control_chars() {
    let mut bad: Vec<String> = Vec::new();
    for cp in 0u32..=0x2FF {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        for v in [
            value_to_json(&mora::value::Value::String(format!("a{c}b"))),
            // **Dict 的 key 是独立位点** —— 只修 String 会漏掉它
            value_to_json(&dict_with_key(c)),
            value_to_json(&mora::value::Value::Char(c)),
        ] {
            let found = raw_control_chars(&v);
            if !found.is_empty() {
                bad.push(format!(
                    "U+{cp:04X} 产出 `{}` 含裸控制字符 {:?}",
                    show(&v),
                    found
                ));
            }
        }
    }
    for cp in [0x1F600u32, 0x1F680, 0x20000, 0x2A6D6] {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let v = value_to_json(&mora::value::Value::String(format!("a{c}b")));
        if !raw_control_chars(&v).is_empty() {
            bad.push(format!("U+{cp:06X} 产出 `{}` 含裸控制字符", show(&v)));
        }
    }
    assert!(
        bad.is_empty(),
        "`json.stringify` 产出了**非法 JSON**（RFC 8259 §7：字符串字面量内不得有\
         未转义的控制字符）。共 {} 处。\n前 8 条:\n  {}\n\
         注意：Mora 自己的 `json.parse` 接受这些输出，所以**内部往返是通的** ——\
         只有外部解析器（Python / JS / 任何 API）会拒绝。",
        bad.len(),
        bad.iter().take(8).cloned().collect::<Vec<_>>().join("\n  ")
    );
}

fn dict_with_key(c: char) -> mora::value::Value {
    let mut m = std::collections::HashMap::new();
    m.insert(format!("k{c}y"), mora::value::Value::String("v".into()));
    mora::value::Value::Dict(m)
}

/// `Value::Char` 修前**一种字符都没转义** —— 单独钉住。
#[test]
fn d209_char_is_escaped() {
    for (c, want) in [('"', "\\\""), ('\\', "\\\\"), ('\n', "\\n")] {
        let got = value_to_json(&mora::value::Value::Char(c));
        assert_eq!(
            got,
            format!("\"{}\"", want),
            "Char({:?}) 未被正确转义（修前是 `format!` 把 c 原样塞进引号里，一个字符都不转）",
            c
        );
    }
}

/// 字典 key 与 value 走**同一份**转义规则（修前是两套）。
#[test]
fn d209_dict_key_and_value_use_the_same_escaper() {
    let mut m = std::collections::HashMap::new();
    m.insert(
        "a\nb".to_string(),
        mora::value::Value::String("c\nd".into()),
    );
    let got = value_to_json(&mora::value::Value::Dict(m));
    assert_eq!(
        got, r#"{"a\nb":"c\nd"}"#,
        "key 与 value 的转义结果应完全一致"
    );
    assert!(
        raw_control_chars(&got).is_empty(),
        "产出含裸控制字符: {}",
        show(&got)
    );
}

/// **不回归**：Mora 自己的 parser 必须仍能读回（含控制字符与非 ASCII）。
#[test]
fn d209_mora_own_parser_still_round_trips() {
    for s in ["a\nb", "a\tb", "含中文 😀", "q\"q", "back\\slash"] {
        let json = value_to_json(&mora::value::Value::String(s.into()));
        let back = json_to_value(&json).unwrap_or_else(|e| panic!("`{json}` 解析失败: {e}"));
        match back {
            mora::value::Value::String(t) => assert_eq!(t, s, "[{s:?}] 往返内容变了"),
            other => panic!("[{s:?}] 读回类型不对: {other:?}"),
        }
    }
}

/// **真实 CLI 路径**（用户实际用的那条）也必须合规。
struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d209_{tag}"));
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

#[test]
fn d209_cli_stringify_output_is_valid_json() {
    let dir = WorkDir::new("cli");
    // 用标记切分：产出里含换行，按行索引会取错片段
    let body = "\
print(\"<<<\" + json.stringify(\"a\\nb\") + \">>>\")
print(\"<<<\" + json.stringify('\"') + \">>>\")
print(\"<<<\" + json.stringify({\"k\\ny\": \"v\"}) + \">>>\")
";
    let p = dir.0.join("t.mora");
    std::fs::write(&p, body).expect("写脚本");
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(&dir.0)
    .arg(&p)
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    assert_eq!(
        out.status.code(),
        Some(0),
        "应正常执行:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut got = 0;
    for cap in raw.split("<<<").skip(1) {
        let Some(json) = cap.split(">>>").next() else {
            continue;
        };
        got += 1;
        assert!(
            raw_control_chars(json).is_empty(),
            "CLI 的 `json.stringify` 产出含**裸控制字符**（非法 JSON）:\n{}\n原始字节: {:?}",
            show(json),
            json.as_bytes()
        );
        assert!(
            json_to_value(json).is_ok(),
            "连 Mora 自己的 parser 都读不回来: {}",
            show(json)
        );
    }
    assert_eq!(got, 3, "三个用例都应产出结果（实际 {got}）");
}
