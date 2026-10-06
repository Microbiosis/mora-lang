//! v0.104.6 D197：`json.parse` 对超出 i64 的整数**静默降级为 f64** —— 拿到的是
//! **另一个数字**（已修：改产出 `BigInt`）。
//!
//! ## 缺陷
//!
//! `flow/json.rs::parse_json_number` 的整数路径原文：
//!
//! ```rust
//! // 整数路径：先尝试 i64，溢出时回退 Float
//! if let Ok(n) = num_str.parse::<i64>() { Ok((Value::Int(n), i)) }
//! else { Ok((Value::Float(num_str.parse::<f64>()?), i)) }   // ← 静默降级
//! ```
//!
//! 真实 `mora` 实测（修前）：
//!
//! | JSON 里的整数 | 输入值 | 输出 | |
//! |---|---|---|---|
//! | `9223372036854775807`（i64::MAX） | …807 | …807 | ✅ |
//! | `9223372036854775808`（i64::MAX+1） | …808 | …808.0 | 碰巧精确 |
//! | `18446744073709551615`（u64::MAX） | …615 | **…616.0** | ❌ 差 1 |
//! | `-9223372036854775809` | …809 | **…808.0** | ❌ 差 1 |
//! | `12345678901234567890` | …890 | **…67168.0** | ❌ 差 5 位有效数字 |
//!
//! **没有报错、没有警告** —— agent 拿到一个看起来合法的数字，用它算 ID、
//! 金额、序号，然后结果全错。
//!
//! ## 修法：i64 溢出 → `BigInt`
//!
//! 本语言**已有**真任意精度的 `Value::BigInt`（num-bigint 后端，字面量记法
//! `<digits>n`，算术 promotion 已就位）。回落 Float 既无必要、又危险。
//!
//! 修后同一矩阵全部**精确**（BigInt 按语言既有记法带 `n` 显示，类型可见）。
//!
//! ## 判据
//!
//! ① 超 i64 的整数必须**逐位**还原（主判据）；
//! ② i64 以内仍是 `Int`、带小数点仍是 `Float`（**不回归**）；
//! ③ `json.stringify` 能把 BigInt 写回去（往返不断链）。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d197_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn run(&self, tag: &str, body: &str) -> String {
        let p = self.0.join(format!("{tag}.mora"));
        std::fs::write(&p, body).expect("写脚本");
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .current_dir(&self.0)
        .arg(&p)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{tag}] 应正常执行:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("Mora v")
                    && !t.starts_with("AI:")
                    && !t.starts_with("AI 原语")
                    && !t.starts_with("显式 API")
                    && !t.starts_with("Trait 系统")
                    && !t.starts_with("Built-in")
                    && !t.starts_with("v0.15 CLI")
                    && !t.starts_with('⚠')
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **主判据（有牙齿）**：超 i64 的整数必须**逐位**还原。
///
/// 修前：这些值会变成**不同的** f64，且零提示。
#[test]
fn d197_integers_beyond_i64_round_trip_exactly() {
    let dir = WorkDir::new("big");
    // 每个用例：JSON 里的原值 → 期望打印出的文本
    let cases: &[(&str, &str)] = &[
        ("18446744073709551615", "18446744073709551615"), // u64::MAX
        ("18446744073709551616", "18446744073709551616"), // u64::MAX + 1
        ("-9223372036854775809", "-9223372036854775809"), // < i64::MIN
        ("12345678901234567890", "12345678901234567890"), // f64 下差 5 位有效数字
        (
            "123456789012345678901234567890",
            "123456789012345678901234567890",
        ),
        (
            "-123456789012345678901234567890",
            "-123456789012345678901234567890",
        ),
    ];
    for (n, (raw, _want)) in cases.iter().enumerate() {
        let body = format!("print(json.parse(\"{{\\\"a\\\": {}}}\").get(\"a\"))\n", raw);
        let out = dir.run(&format!("c{n}"), &body);
        // BigInt 按语言既有记法带 `n` 后缀；允许有或没有。
        let got = out.trim().trim_end_matches('n');
        assert_eq!(
            got, *raw,
            "`{}` 必须**逐位**还原 —— 修前静默降级为 f64，拿到的是另一个数字",
            raw
        );
    }
}

/// **不回归**：i64 以内仍是 `Int`（打印**不带** `n`）、带小数点仍是 `Float`。
#[test]
fn d197_small_integers_and_floats_keep_their_types() {
    let dir = WorkDir::new("small");
    let out = dir.run(
        "small",
        "print(json.parse(\"{\\\"i\\\": 42, \\\"n\\\": -9007199254740993, \\\"f\\\": 3.14159265358979}\").get(\"i\"))\n\
         print(json.parse(\"{\\\"i\\\": 42, \\\"n\\\": -9007199254740993, \\\"f\\\": 3.14159265358979}\").get(\"n\"))\n\
         print(json.parse(\"{\\\"i\\\": 42, \\\"n\\\": -9007199254740993, \\\"f\\\": 3.14159265358979}\").get(\"f\"))\n",
    );
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(lines[0], "42", "小整数应仍是 Int（不带 n 后缀）:\n{}", out);
    assert_eq!(
        lines[1], "-9007199254740993",
        "i64 范围内的负大整数也应精确（不带 n）:\n{}",
        out
    );
    assert_eq!(
        lines[2], "3.14159265358979",
        "带小数点的应是 Float:\n{}",
        out
    );
}

/// 往返不断链：`json.stringify` 能把 BigInt 写回去，且再解析仍精确。
#[test]
fn d197_bigint_survives_a_stringify_round_trip() {
    let dir = WorkDir::new("rt");
    let out = dir.run(
        "rt",
        "let v = json.parse(\"{\\\"a\\\": 12345678901234567890}\")\n\
         let s = json.stringify(v)\n\
         print(s)\n\
         print(json.parse(s).get(\"a\"))\n",
    );
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    assert!(
        lines[0].contains("12345678901234567890"),
        "stringify 应把 BigInt 原样写出:\n{}",
        out
    );
    assert_eq!(
        lines[1].trim_end_matches('n'),
        "12345678901234567890",
        "往返后仍应逐位精确:\n{}",
        out
    );
}
