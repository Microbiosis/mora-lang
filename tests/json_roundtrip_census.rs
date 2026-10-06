//! v0.104.6 D223：`value_to_json` → `json_to_value` 的**穷举往返不变式**。
//!
//! ## 为什么补这条
//!
//! D206 修了 JSON **读端**的 `\uXXXX`，D209 修了**写端**的控制字符转义 ——
//! 但这一对**从没被互相验证过**。今天这套「穷举往返」的判据形状已经两次
//! 钓出真缺陷（`memory` 的往返 D220/D221、`\uXXXX` 缺失 D206），
//! 所以这里**不**手挑几个「正常」例子。
//!
//! 穷举 `0x00..=0x2FF`（控制字符 + 拉丁 + 希腊 + 西里尔 + 标点）
//! 加一批非 BMP 码点（星平面代理对场景），外加一组**值形状**：
//! 空容器、嵌套、含特殊字符的 dict key、浮点。

use std::collections::HashMap;

use mora::flow::{json_to_value, value_to_json};
use mora::value::Value;

/// `Value::List` 收的是 `List` 新类型，不是 `Vec`。
fn lst(v: Vec<Value>) -> Value {
    Value::List(v.into())
}

/// 单个字符串的往返：写 → 读，返回读回的值或错误描述。
fn rt_string(s: &str) -> Result<Value, String> {
    let json = value_to_json(&Value::String(s.to_string()));
    json_to_value(&json)
}

/// **主判据（有牙齿）**：逐码点穷举，字符串往返必须**恒等**。
#[test]
fn d223_string_round_trip_is_lossless_for_every_code_point() {
    let mut bad: Vec<String> = Vec::new();
    let mut checked = 0usize;

    let probe = |cp: u32, c: char, bad: &mut Vec<String>, checked: &mut usize| {
        *checked += 1;
        // 前后各包一个字符，确保不是「首/尾字符」边界才出问题
        let s = format!("a{}b", c);
        match rt_string(&s) {
            Ok(Value::String(g)) if g == s => {}
            Ok(other) => {
                let got = value_to_json(&other);
                bad.push(format!("U+{cp:04X} 往返不等：原 {s:?}，读回 {got}"));
            }
            Err(e) => bad.push(format!("U+{cp:04X} 往返解析失败：{e}")),
        }
    };

    for cp in 0u32..=0x2FF {
        if let Some(c) = char::from_u32(cp) {
            probe(cp, c, &mut bad, &mut checked);
        }
    }
    // 非 BMP（代理对 / 星平面）
    for cp in [0x1F300u32, 0x1F600, 0x20000, 0x2A700, 0x10FFFF] {
        if let Some(c) = char::from_u32(cp) {
            probe(cp, c, &mut bad, &mut checked);
        }
    }

    assert!(
        bad.is_empty(),
        "JSON 字符串往返**有损**：{} / {} 个码点不恒等。\n前 10 条:\n  {}\n\
         这一对函数被 D206（读端补 \\uXXXX）与 D209（写端补控制字符）各修过一次，\
         却直到现在才第一次被**穷举**验证 —— 手挑的「正常」例子全都通过。",
        bad.len(),
        checked,
        bad.iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// **值形状**的往返：容器、嵌套、特殊 key、数字。
#[test]
fn d223_value_shapes_round_trip() {
    let cases: Vec<(&str, Value)> = vec![
        ("空 list", lst(vec![])),
        ("空 dict", Value::Dict(HashMap::new())),
        (
            "嵌套 list",
            lst(vec![lst(vec![Value::Int(1), Value::Int(2)])]),
        ),
        ("bool", Value::Bool(true)),
        ("nil", Value::Nil),
        ("int", Value::Int(-42)),
        ("float", Value::Float(1.5)),
        (
            "含特殊字符的 key",
            Value::Dict(
                [
                    ("a\"b".to_string(), Value::Int(1)),
                    ("c\\d".to_string(), Value::Int(2)),
                    ("e\nf".to_string(), Value::Int(3)),
                    (String::new(), Value::Int(4)),
                ]
                .into_iter()
                .collect(),
            ),
        ),
        (
            "含空串的 list",
            lst(vec![Value::String(String::new()), Value::Int(0)]),
        ),
    ];
    for (name, v) in cases {
        let json = value_to_json(&v);
        match json_to_value(&json) {
            Ok(got) if got == v => {}
            Ok(got) => panic!(
                "[{name}] 往返不等：\n  写端产出 {json}\n  读回 {}",
                value_to_json(&got)
            ),
            Err(e) => panic!("[{name}] 往返解析失败（{e}），中间 JSON：{json}"),
        }
    }
}

/// **产出必须被真解析器接受** —— 自洽但不合规的 JSON 在外部无用。
///
/// 这是 D209 的延伸：D209 只用 Python 验过三个 case，这里把
/// 「写端产出的 JSON 都必须被标准解析器接受」变成判据。
#[test]
fn d223_written_json_is_accepted_by_python() {
    let cases: Vec<(&str, Value)> = vec![
        ("换行", Value::String("a\nb".into())),
        ("制表", Value::String("a\tb".into())),
        ("回车", Value::String("a\rb".into())),
        ("退格", Value::String("a\u{8}b".into())),
        ("换页", Value::String("a\u{c}b".into())),
        ("竖直制表", Value::String("a\u{b}b".into())),
        ("NUL", Value::String("a\u{0}b".into())),
        ("DEL", Value::String("a\u{7f}b".into())),
        ("引号+反斜杠", Value::String("a\"\\b".into())),
        ("嵌套", lst(vec![Value::String("x\ny".into())])),
        (
            "含控制字符的 key",
            Value::Dict([("k\nx".to_string(), Value::Int(1))].into_iter().collect()),
        ),
    ];
    let dir = std::env::temp_dir().join("mora_d223_py");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let mut manifest = String::new();
    for (i, (name, v)) in cases.iter().enumerate() {
        let json = value_to_json(v);
        let p = dir.join(format!("c{i}.json"));
        std::fs::write(&p, &json).expect("写");
        manifest.push_str(&format!("{}\t{}\t{}\n", i, name, p.display()));
    }
    let mpath = dir.join("manifest.tsv");
    std::fs::write(&mpath, &manifest).expect("写 manifest");

    let script = "import json, sys\n\
         lines = open(sys.argv[1], encoding='utf-8').read().splitlines()\n\
         bad = 0\n\
         for line in lines:\n\
         \x20   i, name, path = line.split('\t')\n\
         \x20   raw = open(path, 'rb').read()\n\
         \x20   try:\n\
         \x20       json.loads(raw.decode('utf-8'))\n\
         \x20   except Exception as e:\n\
         \x20       bad += 1\n\
         \x20       print('REJECTED %s (%s): %s :: %r' % (name, i, e, raw[:60]))\n\
         print('TOTAL %d BAD %d' % (len(lines), bad))\n";
    let out = std::process::Command::new("python")
        .arg("-c")
        .arg(script)
        .arg(&mpath)
        .output();
    let _ = std::fs::remove_dir_all(&dir);
    let out = out.expect("跑 python");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "跑 python 失败：\n{stdout}\n{stderr}");
    assert!(
        !stdout.contains("REJECTED"),
        "[D223] 写端产出的 JSON 被**标准解析器拒绝** —— 自洽但不合规。\n{stdout}"
    );
}
