//! v0.104.6 D317：`match` 的**字面量模式匹配**有两处缺陷。
//!
//! ## 怎么找到的
//!
//! D317 本来是去搜 D312/D313 根因（「SSA 只重命名 block 0 ⇒ 跨块寄存器被拆成
//! 两套」）的**剩余实例**，为此构造了 31 个「跨块寄存器流动」的针对性形状。
//! 那 31 个形状 **0 分叉**（否定轮），但其中一个用例的**期望值是我按记忆写的**，
//! 实测打回后才暴露出下面这个**与 `--opt` 完全无关**的独立缺陷。
//!
//! ## 缺陷一（已修）：`bigint:` 模式**根本没有 handler**
//!
//! `pattern_to_string`（`lower.rs:1255`）会把 `Literal::BigInt` 序列化成
//! `bigint:{n}`，而 `self_match_pattern`（`vm.rs`）**没有任何 `bigint:` 分支**
//! —— 该模式穿过全部分支后落到函数末尾的 `false`。
//!
//! ```mora
//! print(match 123n { 123n => "big", _ => "other" })   // 修前 other / 修后 big
//! ```
//!
//! **同类型、同值也不匹配。** 这是序列化器与匹配器之间的**不对称**
//! （一端能产出、另一端认不出），不涉及任何语义取舍。
//!
//! ## 缺陷二（**D318 已否证并撤回**）：数字模式不跨类型
//!
//! D317 曾把「`match len("xy") { 2 => "two", _ => "other" }` 恒得 `other`」
//! 记成第二处缺陷，并列为「待裁决的语言语义决定」。
//!
//! **D318 否证了这个结论。** 漏掉的事实是 **v0.38 数值塔的后缀语法**：
//! `lexer.rs:797-800` 把**无后缀**的数字归成 `TokenType::Float`，而
//! `2i`/`2u` → `Int`、`2f` → `Float`、`123n` → `BigInt`。所以
//! `match n { 2i => … }`（n 来自 `len`）**能命中** —— `2` 只是 Float 模式。
//!
//! ⇒ **模式匹配类型严格是正确行为**，`2i` 才是匹配 Int 值的写法。
//! 详见 `d318_numeric_tower_patterns_are_type_strict_operators_promote`。
//! **D317 列出的「待裁决项」撤回。**
//!
//! ## 序列化器 ↔ 匹配器的覆盖审计（D318）
//!
//! D318 顺带穷尽审计了**两个**模式序列化器
//! （`lower.rs::pattern_to_string` 与 `fcfg_lower::fcfg_pattern_to_string`）
//! 与唯一运行时匹配器 `self_match_pattern` 的**格式覆盖面**：
//!
//! | 格式 | 序列化器 | 匹配器 | 源码可达 |
//! |---|---|---|---|
//! | `bigint:{n}` | 有 | 有（**D317 补**） | 可达 |
//! | `tuple:(...)` | 有（两个都产） | **无分支** | 解析器拒绝 `(1,2)` |
//! | `list:[h\|t]` | 有（仅 lower） | 显式 `false`（注释「legacy」） | 解析器拒绝 `[a\|b]` |
//! | `char:{c}` | 有 | 有 | 解析器拒绝 `'a'` 作 arm 模式 |
//! | `{name}:{inner}` | 有 | 无通用前缀分支 | 未见源码语法 |
//! | `list:[a,b]` 逗号式 | **无** | 有 | 无生产者 |
//! | `guard:inner` | **无** | 有 | 无生产者 |
//!
//! ⇒ **可达范围内只有 `bigint:` 一处不对称，已修**；其余差集全是
//! **不可达的潜伏死路径**（解析器层就不让写出来）。
//!
use std::process::Command;

/// 把用例名变成**合法的 Windows 路径片段**。
///
/// D321：必须在这里（`run()` 内部）施加，**不能**只靠调用点各自 slug ——
/// tag 里一旦带 `/`（D318 的 `float-pat/int-val` 就是），
/// `mora_d317_match_{tag}` 会变成**嵌套路径**，`remove_dir_all` 只删叶子，
/// **父目录留下**（实测每跑一次全量 +1）。收进函数内才是「修整类」。
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 删除临时目录，**带重试**。
///
/// v0.104.6 D321：`mora.exe` 子进程在 `.output()` 返回后可能**尚未释放**
/// `p.mora` 的文件句柄；Windows 上此时 `remove_dir_all` 直接失败，而
/// `let _ =` 会把错误**静默吞掉** ⇒ 每跑一次判据就漏一个目录
/// （实测 `cargo test --no-fail-fast` 一次 +4 个）。
///
/// Windows 的句柄释放是异步的，重试即可覆盖；仍失败则**如实暴露**，
/// 不再伪装成「清理过了」。
fn cleanup_dir(dir: &std::path::Path) {
    for attempt in 0..8 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 7 => {
                panic!(
                    "D321：临时目录 {dir:?} 清理失败（{e}）。\n\
                     句柄可能仍被 `mora.exe` 子进程占用；重试 8 次仍失败。\n\
                     该目录会逐次累积 —— 请勿忽略。"
                );
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
}

/// 每个 case 必须用**独立**目录。
///
/// ⚠ 本文件三条 `#[test]` 在同一测试二进制里**并行**跑；共用一个目录时，
/// 三个测试会互相 `remove_dir_all` / 写同一个 `p.mora` ⇒ 读到的可能是
/// 别人的输出（本轮第一版就这么炸的：`[literal]` 拿到了 string 用例的 `A`）。
/// D315 的 `nine_layer_unblocked.rs` 用 `mora_d315_{tag}` 避开了，本条沿用。
fn run(src: &str, tag: &str) -> (i32, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mora_d317_match_{}", slug(tag)));
    cleanup_dir(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    cleanup_dir(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let lines = text
        .lines()
        .map(|l| l.trim().to_string())
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
        })
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

/// **主判据（已修）**：`bigint:` 模式必须能匹配 `Value::BigInt`。
///
/// 修前恒得 `other` —— 同类型、同值也不匹配。
/// **反向牙齿已验证**：删掉 `self_match_pattern` 里新增的 `bigint:` 分支即红。
#[test]
fn d317_bigint_pattern_matches_bigint_values() {
    for (tag, src, want) in [
        (
            "literal",
            "print(match 123n { 123n => \"big\", _ => \"other\" })\n",
            "big",
        ),
        (
            "let-bound",
            "let n = 123n\nprint(match n { 123n => \"big\", _ => \"other\" })\n",
            "big",
        ),
        (
            "different-value-goes-else",
            "print(match 123n { 456n => \"big\", _ => \"other\" })\n",
            "other",
        ),
        (
            "string-value-goes-else",
            "print(match \"123\" { 123n => \"big\", _ => \"other\" })\n",
            "other",
        ),
        (
            "in-value-position",
            "let r = match 7n { 7n => \"hit\", _ => \"miss\" }\nprint(r)\nprint(\"done\")\n",
            "hit",
        ),
    ] {
        let (code, got) = run(src, tag);
        assert_eq!(code, 0, "[{tag}] 应成功; 实得 {code}");
        assert_eq!(
            got.first().map(String::as_str),
            Some(want),
            "**D317**：BigInt 字面量模式必须匹配 BigInt 值。\n\
             修前 `self_match_pattern` 没有任何 `bigint:` 分支 —— \
             `pattern_to_string` 能产出该模式，匹配器认不出 ⇒ 同类型同值也不匹配。\n\
             `[{tag}]` 实得: {got:?}"
        );
    }
}

/// 字符串 / 布尔 / nil 模式**未受影响**（对照组，防止补 `bigint:` 时改坏别的）。
#[test]
fn d317_other_pattern_kinds_are_unaffected() {
    for (tag, src, want) in [
        (
            "string",
            "print(match \"a\" { \"a\" => \"A\", _ => \"other\" })\n",
            "A",
        ),
        (
            "bool-true",
            "print(match true { true => \"T\", _ => \"F\" })\n",
            "T",
        ),
        (
            "bool-false",
            "print(match false { true => \"T\", _ => \"F\" })\n",
            "F",
        ),
        (
            "nil",
            "let z = 0\nprint(match nil { nil => \"N\", _ => \"other\" })\n",
            "N",
        ),
        (
            "list",
            "print(match [1,2] { [1,2] => \"L\", _ => \"other\" })\n",
            "L",
        ),
    ] {
        let (code, got) = run(src, tag);
        assert_eq!(code, 0, "[{tag}] 应成功; 实得 {code}");
        assert_eq!(
            got.first().map(String::as_str),
            Some(want),
            "[{tag}] 非数值模式的行为不应因 D317 改动而变化; 实得 {got:?}"
        );
    }
}

/// **数值塔契约**：模式匹配**类型严格**，运算符**有提升**，两者共存且自洽。
///
/// ## D317 的一处自我更正
///
/// D317 曾把「`match len("xy") { 2 => "two", _ => "other" }` 恒得 `other`」
/// 记成**缺陷二**，并列为「待裁决的语言语义决定」（match 要不要跟 `==`
/// 一样做 Int↔Float 提升）。
///
/// **D318 否证了这个结论。** 漏掉的关键事实是 **v0.38 数值塔的后缀语法**：
/// 裸整数 `2` 被 `lexer.rs:797-800` 归成 **`TokenType::Float`**，
/// 而 `2i` / `2u` → `TokenType::Int`、`123n` → `BigInt`。所以：
///
/// | 源码 | 词法 | 模式 | 值 | 结果 |
/// |---|---|---|---|---|
/// | `match 2 { 2 => … }` | Float | `float:2.0` | Float | ✅ 命中 |
/// | `match n { 2i => … }`（n 来自 `len`） | Int | `int:2` | Int | ✅ 命中 |
/// | `match n { 2 => … }`（n 来自 `len`） | — | `float:2.0` | Int | ✅ **不命中（正确）** |
/// | `match n { 2i => … }`（n 来自 `1+1`） | — | `int:2` | Float | ✅ **不命中（正确）** |
/// | `match 123n { 123n => … }` | BigInt | `bigint:123` | BigInt | ✅ 命中（D317 修） |
///
/// ⇒ **模式匹配类型严格是正确行为**，`2i` 才是匹配 Int 值的写法。
/// D317 的「待裁决」**撤回**。
///
/// 两条路径**并存且自洽**，不构成不一致：
/// - **模式匹配类型严格** —— 模式是「形状」声明，类型不符即不匹配；
/// - **运算符有数值提升** —— `len("xy") == 2` 为 `true` 是「数值相等」。
///
/// 本条把这条契约钉住，防止将来有人**善意地**给模式匹配加提升（那会把
/// `match x { 2i => A, 2f => B }` 这类「按类型分派」的写法静默合并）。
#[test]
fn d318_numeric_tower_patterns_are_type_strict_operators_promote() {
    // ── 后缀语法决定模式类型 ──
    let (_, t) = run("print(2)\n", "t_bare");
    assert_eq!(
        t.first().map(String::as_str),
        Some("2.0"),
        "裸整数应是 Float"
    );
    let (_, t) = run("print(2i)\n", "t_i");
    assert_eq!(t.first().map(String::as_str), Some("2"), "`2i` 应是 Int");
    let (_, t) = run("print(123n)\n", "t_n");
    assert_eq!(
        t.first().map(String::as_str),
        Some("123n"),
        "`123n` 应是 BigInt"
    );

    // ── 类型相符 ⇒ 命中 ──
    for (tag, src, want) in [
        (
            "int-pat/int-val",
            "let n = len(\"xy\")\nprint(match n { 2i => \"hit\", _ => \"other\" })\n",
            "hit",
        ),
        (
            "int-pat/int-fn",
            "let n = int(\"42\")\nprint(match n { 42i => \"hit\", _ => \"other\" })\n",
            "hit",
        ),
        (
            "float-pat/float-val",
            "print(match 1 + 1 { 2 => \"hit\", _ => \"other\" })\n",
            "hit",
        ),
        (
            "bigint-pat/bigint-val",
            "print(match 123n { 123n => \"hit\", _ => \"other\" })\n",
            "hit",
        ),
    ] {
        let (code, got) = run(src, tag);
        assert_eq!(code, 0, "[{tag}] 应成功; 实得 {code}");
        assert_eq!(
            got.first().map(String::as_str),
            Some(want),
            "[{tag}] 类型相符时模式必须命中; 实得 {got:?}"
        );
    }

    // ── 类型不符 ⇒ 不命中（这是**正确行为**，不是缺陷）──
    for (tag, src) in [
        (
            "float-pat/int-val",
            "let n = len(\"xy\")\nprint(match n { 2 => \"hit\", _ => \"other\" })\n",
        ),
        (
            "int-pat/float-val",
            "let n = 1 + 1\nprint(match n { 2i => \"hit\", _ => \"other\" })\n",
        ),
        (
            "int-pat/other-int",
            "let n = len(\"xyz\")\nprint(match n { 2i => \"hit\", _ => \"other\" })\n",
        ),
        (
            "bigint-pat/int-val",
            "let n = len(\"xy\")\nprint(match n { 2n => \"hit\", _ => \"other\" })\n",
        ),
    ] {
        let (code, got) = run(src, tag);
        assert_eq!(code, 0, "[{tag}] 应成功; 实得 {code}");
        assert_eq!(
            got.first().map(String::as_str),
            Some("other"),
            "[{tag}] 类型不符（或值不等）时必须落 `_`。\n\
             ⚠ 若本条失败，说明有人给模式匹配加了数值提升 —— 那是**语义变更**，\n\
             请先撤回并回到 CHANGELOG D318 的裁决说明。\n  实得: {got:?}"
        );
    }

    // ── 运算符的提升**独立于**模式匹配的严格性（两者并存）──
    let (code, got) = run("print(len(\"xy\") == 2)\n", "t_promote");
    assert_eq!(code, 0, "应成功; 实得 {code}");
    assert_eq!(
        got.first().map(String::as_str),
        Some("true"),
        "⚠ 若本条失败，说明 `==` 的数值提升也没了 —— 那是另一个独立变更。\n\
         「模式匹配类型严格 + 运算符有提升」是 D318 钉住的**并存契约**。"
    );
}
