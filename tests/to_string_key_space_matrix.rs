//! v0.104.6 D333 —— `v.to_string()` 当键/路径的 **4 个模块** × 类型注入矩阵
//!
//! D332 查出「`v.to_string()` 被 **5 个文件 14 处**当作键或路径」，但只测了
//! `memory` 一个。本条把**其余 4 个**（`ccr` / `schedule` / `tool` / `ai`）
//! 补齐，让「空串显示塌陷影响多少键空间」这个问题有**全量数据**。
//!
//! **结论：零产品缺陷。** 四个模块的行为**自洽且有文档依据**，
//! 唯一起始处的显示塌陷是 D332 已登记、待产品裁决的那一条。
//!
//! ## ① 模块内**两种策略并存**（不是遗漏，是各自有据）
//!
//! `ccr` 一个模块里同时存在两种实参策略，**且都写明了理由**：
//!
//! | 入口 | 策略 | 代码里的理由 |
//! |---|---|---|
//! | `ccr.put` / `ccr.get` | **严守 `Value::String`** | 「Avoids lossy `to_string()` of List/Dict that would round-trip into `[...]`」 |
//! | `ccr.marker` / `ccr.extract` | `v.to_string()` 放行任意类型 | 注释无（但二者是**格式化/解析**，本就该接受任意来源的文本）|
//! | `schedule.add` | **严守 `Value::String`**（name/kind/message 三处）| v0.37 加固 |
//! | `tool.register` / `find` / `unregister` | `args[N].to_string()` | 三者都是**标识符查找**，需与 `create` 注册时的键同形 |
//!
//! **判定为自洽而非遗漏**：`put`/`get` 传的是**数据**（数据经 `to_string()`
//! 会不可逆），而 `marker`/`extract` 传的是**文本标记**（本就是文本）；
//! `schedule.add` 的三个字段是**结构性标识**，而 `tool.*` 的平面名/工具名
//! 在 `tool.create` 阶段就已经是 `to_string()` 的结果，查找侧必须同形。
//!
//! 实测佐证（`tool`）：
//! ```text
//! tool.create("ext")                → true
//! tool.unregister(1, 2)             → toolplane.unregister: plane '1.0' not found
//! ```
//! 后者**不是缺陷**——`1` 被 `to_string()` 成 `"1.0"`，而注册时不存在
//! 名为 `"1.0"` 的平面，故「not found」是**正确**诊断，且消息里回显了
//! 归一化后的名字，比原样回显 `1` 更有用。
//!
//! ## ② D332 的空串塌陷在 `ccr` 键空间**重现**，但不造成数据损坏
//!
//! ```text
//! ccr.marker("", 0)   → <<ccr:,0>>
//! ccr.extract(该串)   → ""        ← 提取出空串（往返**没崩**）
//! ccr.get("")         → nil       ← 但空串不是合法 hash，查不到
//! ```
//!
//! ⇒ 空 hash 的 marker 是个**哑值**：能造、能解析，但 `get` 不到。
//! 这**不是**新缺陷 —— `""` 本就不是 `ccr.put` 会产出的 hash
//! （`format!("{:016x}", n)` 恒 16 位十六进制）。
//!
//! ## ③ `ccr.extract` 是**纯字符串切分器**，对畸形 marker 一律放行
//!
//! 实测 7 种畸形输入**全部**被接受：
//!
//! | 输入 | `extract` 返回 |
//! |---|---|
//! | `<<ccr:deadbeefdeadbeef,0>>` | `deadbeefdeadbeef`（**编造的 hash**）|
//! | `<<ccr:xyz,0>>` | `xyz`（长度不对）|
//! | `<<ccr:zzzzzzzzzzzzzzzz,0>>` | `zzzzzzzzzzzzzzzz`（非十六进制）|
//! | `<<ccr:0000000000000001,notanumber>>` | `0000000000000001`（size 是垃圾）|
//! | `<<ccr:0000000000000001>>` | `0000000000000001`（**缺 size**）|
//! | `<<ccr:abc,1,2,3>>` | `abc`（多余逗号）|
//!
//! **判定为现状而非缺陷**：`extract_hash` 的文档注释只承诺
//! 「returns `None` if not a marker」，而它确实是 marker（格式正确）；
//! 校验 hash 是否**真实存在**属于 `get` 的职责，而 `get` 对未知 hash
//! **安全返回 `nil`**。⇒ 链路上**没有数据损坏**，只是提前失败被推后。
//! 改它需先决定「marker 格式是否要版本化/校验」—— 那是产品契约。
//!
//! ## ④ 一次**自我纠正**：`toolplane` 不是缺陷，D74 已查清并修复
//!
//! 我初跑 `toolplane.list()` 得 `Unbound variable 'toolplane'`，一度以为
//! 是又一个「模块名未绑定」缺陷。查 CHANGELOG **D74 已彻底查明**：
//! `MODULE_OBJECTS` 里的注册名是 **`tool`**，`toolplane` 是**枚举变体名**，
//! D74 自查时把变体名当注册名建表，已修，并加了双向判据
//! `module_table_keys_are_registered_names`。
//!
//! ⇒ 换用 `tool.*` 重测即全部可达。**本条钉 `tool`（注册名）**。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 每个用例独立进程，且 `HOME` / `USERPROFILE` 指向临时目录
/// （`tool.create` / `schedule.add` 会落盘到用户目录）。
/// 取 stdout **全部**实质行并以 ` | ` 连接 —— D330 教训：不能用
/// `lines().find(第一行)`，多行探针只取得到第一行。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mora_d333_s_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **现状判据 ①**：`ccr` 的两种实参策略并存，各自给出恰当诊断。
#[test]
fn d333_ccr_strict_data_args_vs_permissive_marker_args() {
    // 数据侧严守 String
    for b in ["print(ccr.put(1))\n", "print(ccr.put([1,2]))\n"] {
        let (code, got) = ev(b);
        assert_eq!(
            code, 1,
            "`{b}` 应报错（数据不能经 to_string 往返）; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("data must be a string"),
            "错误应说明 data 必须是字符串（代码注释明写要避免有损往返）; 实得: {got}"
        );
    }
    let (code, got) = ev("print(ccr.get(1))\n");
    assert_eq!(code, 1, "`ccr.get(1)` 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("hash must be a string"),
        "错误应点名 hash; 实得: {got}"
    );

    // 文本侧放行任意类型，且产出的 marker 合法
    for (b, want) in [
        ("print(ccr.marker(1, 4))\n", "<<ccr:1.0,4>>"),
        ("print(ccr.marker([1,2], 4))\n", "<<ccr:[1.0, 2.0],4>>"),
    ] {
        let (code, got) = ev(b);
        assert_eq!(
            code, 0,
            "`{b}` 应成功（marker 是格式化，本就该接受任意来源）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{b}` 应得 {want}; 实得 {got}");
    }
}

/// **对照组 1**：`put` / `get` / `marker` / `extract` 的**正常往返**逐字不变。
#[test]
fn d333_ccr_normal_roundtrip_unchanged() {
    let (code, got) =
        ev("let h = ccr.put(\"data\")\nprint(ccr.get(h))\nprint(ccr.len())\nprint(len(h))\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "data | 1 | 16",
        "hash 恒为 16 位十六进制（`format!(\"{{:016x}}\", n)`）; 实得 {got}"
    );

    let (code, got) = ev(
        "let h = ccr.put(\"abc\")\nlet m = ccr.marker(h, 4)\nprint(m)\nprint(ccr.extract(m))\nprint(ccr.extract(m) == h)\n",
    );
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "<<ccr:0000000000000001,4>> | 0000000000000001 | true",
        "往返应无损; 实得 {got}"
    );
}

/// **现状判据 ②**：`extract` 是纯切分器，对 7 种畸形 marker **一律放行**。
///
/// 钉它是为了让「marker 格式到底校不校验」这个**待裁决**项有基线。
/// 若将来决定加校验，本条会红 —— 那是**有意的**格式契约变更。
#[test]
fn d333_ccr_extract_is_a_pure_splitter_accepting_malformed_markers() {
    for (b, want) in [
        // 编造的 hash（格式合法但不存在）
        (
            "print(ccr.extract(\"<<ccr:deadbeefdeadbeef,0>>\"))\n",
            "deadbeefdeadbeef",
        ),
        // 长度不对
        ("print(ccr.extract(\"<<ccr:xyz,0>>\"))\n", "xyz"),
        // 非十六进制
        (
            "print(ccr.extract(\"<<ccr:zzzzzzzzzzzzzzzz,0>>\"))\n",
            "zzzzzzzzzzzzzzzz",
        ),
        // size 是垃圾
        (
            "print(ccr.extract(\"<<ccr:0000000000000001,notanumber>>\"))\n",
            "0000000000000001",
        ),
        // 缺 size
        (
            "print(ccr.extract(\"<<ccr:0000000000000001>>\"))\n",
            "0000000000000001",
        ),
        // 多余逗号
        ("print(ccr.extract(\"<<ccr:abc,1,2,3>>\"))\n", "abc"),
    ] {
        let (code, got) = ev(b);
        assert_eq!(
            code, 0,
            "`{b}` 现状**放行**（`extract_hash` 只承诺「不是 marker 才 None」）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{b}` 应切出 {want}; 实得 {got}");
    }
    // 真正非 marker 的输入才报错
    let (code, got) = ev("print(ccr.extract(\"notamarker\"))\n");
    assert_eq!(code, 1, "非 marker 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("not a valid CCR marker"),
        "错误应说明不是合法 marker; 实得: {got}"
    );
    let (code, got) = ev("print(ccr.extract(1))\n");
    assert_eq!(code, 1, "非 marker 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("not a valid CCR marker"),
        "错误应说明不是合法 marker; 实得: {got}"
    );
}

/// **配对判据**：畸形 hash 被 `extract` 接受，但 `get` **安全返回 nil**
/// ——⇒ 链路上**没有数据损坏**，这才是「不修」的技术依据。
#[test]
fn d333_bogus_extracted_hash_gets_return_nil_safely() {
    for h in ["deadbeefdeadbeef", "xyz", "zzzzzzzzzzzzzzzz"] {
        let b = format!("print(ccr.get(\"{h}\"))\n");
        let (code, got) = ev(&b);
        assert_eq!(
            code, 0,
            "`ccr.get(\"{h}\")` 对未知 hash 应**安全返回 nil**（不是崩溃）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, "nil", "`ccr.get(\"{h}\")` 应得 nil; 实得 {got}");
    }
}

/// **现状判据 ③**：空串在 `ccr` 键空间的重现（D332 的显示塌陷）。
///
/// 空 hash 的 marker 是个**哑值**：能造、能解析、但 `get` 不到 ——
/// 因为 `""` 永远不是 `ccr.put` 会产出的 hash。
#[test]
fn d333_empty_hash_marker_is_a_dummy_value() {
    let (code, got) = ev("print(ccr.marker(\"\", 0))\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(got, "<<ccr:,0>>", "空 hash 的 marker 现状如此; 实得 {got}");

    let (code, got) = ev(
        "let m = ccr.marker(\"\", 0)\nlet h = ccr.extract(m)\nprint(h == \"\")\nprint(ccr.get(h))\n",
    );
    assert_eq!(code, 0, "往返应**不崩**; 实得 exit={code} out={got}");
    assert_eq!(
        got, "true | nil",
        "空 hash 往返不崩但查不到（哑值）; 实得 {got}"
    );
}

/// **现状判据 ④**：`schedule.add` 三个字符串实参**各自**给出点名诊断。
#[test]
fn d333_schedule_string_args_are_strictly_checked() {
    for (b, needle) in [
        (
            "print(schedule.add(1, \"every\", \"msg\"))\n",
            "name must be a string",
        ),
        (
            "print(schedule.add(\"j\", 2, \"msg\"))\n",
            "kind must be a string",
        ),
        (
            "print(schedule.add(\"j\", \"every\", 3))\n",
            "message must be a string",
        ),
        (
            "print(schedule.add(\"j\", \"wrong\", \"msg\"))\n",
            "kind must be 'every' or 'at'",
        ),
    ] {
        let (code, got) = ev(b);
        assert_eq!(code, 1, "`{b}` 应报错; 实得 exit={code} out={got}");
        assert!(got.contains(needle), "`{b}` 应点明 `{needle}`; 实得: {got}");
    }
}

/// **对照组 2**：`tool.*`（**注册名是 `tool`**，不是枚举变体名 `toolplane`）。
///
/// D74 已把「把变体名当注册名」这个错误建表方式修掉，并加了双向判据
/// `module_table_keys_are_registered_names`。本条钉住**注册名可达**，
/// 且 `unregister` 对未知平面**回显归一化后的名字**（比原样回显更有用）。
#[test]
fn d333_tool_module_is_reachable_under_its_registered_name() {
    let (code, got) = ev("print(tool.create(\"ext\"))\n");
    assert_eq!(code, 0, "`tool.create` 应可达; 实得 exit={code} out={got}");
    assert_eq!(got, "true", "`tool.create(\"ext\")` 应成功; 实得 {got}");

    // 数字平面名被归一化成 "1.0"，且诊断回显归一化结果
    let (code, got) = ev("print(tool.unregister(1, 2))\n");
    assert_eq!(code, 1, "未知平面应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("'1.0'"),
        "诊断应回显**归一化后**的平面名（比回显原样 `1` 更有用）; 实得: {got}"
    );

    // 未知工具查找返回 nil（不报错）
    let (code, got) = ev("print(tool.find(\"nope\", \"nope\"))\n");
    assert_eq!(
        code, 0,
        "未知工具查找应返回 nil; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "nil", "应得 nil; 实得 {got}");
}
