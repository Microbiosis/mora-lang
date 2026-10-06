//! v0.104.6 D361 —— `index_value` 边界矩阵 + **索引赋值整条链路是死代码**（否定轮，无产品变更）
//!
//! D360 扫完 `src/mir/handlers/` 的 panic 面后，本轮钉 `values.rs` 的
//! 索引面（`h_index` / `h_index_assign` → `mir/vm.rs` 的
//! `index_value` / `index_assign_value`）。
//!
//! ## 索引**读取**：13 用例全部符合设计
//!
//! | 形态 | 实测 | 判定 |
//! |---|---|---|
//! | `xs[0]` / `xs[1.0]` | 10 / 20 | ✅ |
//! | `xs[2.7]` | 30 | ✅ `checked_index` **截断**（非报错）|
//! | `xs[-1]` / `xs[-0.5]` | exit 1 *negative index* | ✅ |
//! | `xs[3]`（越界）| exit 1 *out of bounds* | ✅ |
//! | `s[0]` / `s[1.0]` | `a` / `b` | ✅ 按**字符**索引（非字节）|
//! | `s[3]` | exit 1 | ✅ 越界按**字符数**报 |
//! | `m["a"]` / `m["zzz"]` | 1 / **nil** | ✅ 缺失返回 nil 不报错（Python 风格）|
//! | `xs[true]` / `5[0]` | exit 1 *cannot index* | ✅ |
//!
//! ## 索引**赋值**：**整条链路是死代码**
//!
//! `WitnessKind::IndexAssign` → `MirInst::IndexAssign` → `h_index_assign`
//! → `index_assign_value` 在 `src/mir/` 与 `src/typeck/` 共 **23 个文件
//! 54 处**都有定义与传递，**但 `src/parser_v3/` 零构造** ——
//! **没有任何语法能产生它**。
//!
//! 实测四种候选写法全部不可达：
//!
//! ```text
//! xs[0] = 9      → exit 2  Failed to parse
//! xs.at(0) = 9   → exit 2  Failed to parse
//! xs.set(0, 9)   → exit 1  List has no method: set
//! m["a"] = 2     → exit 2  Failed to parse
//! ```
//!
//! **判定为「功能未接通」，不是静默错值** ——
//! 没有「本该是 A 却是 B」，是「压根没法表达」⇒ 只报告，不擅动
//! （与 D346 `xform`、D360 `orchestrate loop` 同类）。
//!
//! ## 判据形态
//!
//! 索引读取用**脚本层**（能真正区分）；索引赋值用**源码层**
//! （脚本层只能证明「不可达」，无法区分「未接通」与「不支持」）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d361_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d361_{}_{}", n, slug(body)));
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
    let path_str = p.to_string_lossy().into_owned();
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
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **装置自检**（D356 教训：先证装置有效，再看它测出的数据）。
#[test]
fn d361_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **列表索引**：正常、浮点截断、越界、负数。
#[test]
fn d361_list_index_matrix() {
    let setup = "let xs = [10, 20, 30]\n";
    for (expr, expected) in [("xs[0]", "10.0"), ("xs[1.0]", "20.0"), ("xs[2]", "30.0")] {
        let (code, got) = ev(&format!("{setup}print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应正常; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{expr}` 的值不对; 实得: {got}");
    }
    // `checked_index` 对浮点**截断**（不报错）—— 这是既有设计
    assert_eq!(
        ev(&format!("{setup}print(xs[2.7])\n")).1.trim(),
        "30.0",
        "浮点索引应截断取整（D246 已钉 value_as_usize 的截断为设计）"
    );
    // 越界与负数必须报错
    for expr in ["xs[3]", "xs[-1]", "xs[-0.5]"] {
        let (code, got) = ev(&format!("{setup}print({expr})\n"));
        assert_eq!(code, 1, "`{expr}` 应报错; 实得 exit={code} out={got}");
    }
}

/// **字符串索引**：按**字符**而非字节；越界按字符数报。
#[test]
fn d361_string_index_is_char_based() {
    let (code, got) = ev("let s = \"abc\"\nprint(s[0])\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "a", "s[0] 应是首字符");

    let (code, got) = ev("let s = \"abc\"\nprint(s[1.0])\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "b", "浮点索引应被接受");

    // 多字节：`é` 占 1 个字符（2 字节 UTF-8）⇒ s[1] 必须是 `é`
    let (code, got) = ev("let s = \"héllo\"\nprint(s[1])\n");
    assert_eq!(
        code, 0,
        "多字节字符串索引应正常; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got.trim(),
        "é",
        "`s[1]` 应是 `é`（按字符而非字节）; 实得: {got}"
    );

    // 越界按**字符数**报（5 个字符，不是 6 字节）
    let (code, got) = ev("let s = \"héllo\"\nprint(s[5])\n");
    assert_eq!(code, 1, "越界应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("(len 5)"),
        "越界信息应按**字符数** 5 报（与 len() 口径一致）; 实得: {got}"
    );
}

/// **dict 索引**：缺失返回 `nil` 而非报错（Python 风格，既有设计）。
#[test]
fn d361_dict_index_missing_returns_nil() {
    let (code, got) = ev("let m = {\"a\": 1}\nprint(m[\"a\"])\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "1.0", "命中键应返回值");

    let (code, got) = ev("let m = {\"a\": 1}\nprint(m[\"zzz\"])\n");
    assert_eq!(
        code, 0,
        "缺失键返回 nil 不报错（既有设计）; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "nil", "缺失键应返回 nil; 实得: {got}");
}

/// **类型不匹配的索引必须报错**，且错误信息**不含整个对象的 dump**。
///
/// 后者是 `vm.rs:194-200` 特意修过的（`{:?}` 的 `HashMap` 键序每进程随机，
/// 导致同一条错误信息在不同进程里不同）。本条钉住它。
#[test]
fn d361_type_mismatch_index_errors_without_full_dump() {
    for expr in ["xs[true]", "5[0]"] {
        let (code, got) = ev(&format!("let xs = [10, 20, 30]\nprint({expr})\n"));
        assert_eq!(code, 1, "`{expr}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains("cannot index"),
            "`{expr}` 的诊断应是 cannot index; 实得: {got}"
        );
        // 不能把整个列表 dump 出来（`{:?}` 路径）
        assert!(
            !got.contains("30"),
            "诊断不应 dump 整个对象内容（`{{:?}}` 键序不稳定）; 实得: {got}"
        );
    }
}

/// **索引赋值的四种候选写法全部不可达**。
///
/// 这是**否定轮的核心观测**：整条 `IndexAssign` 链路在 `src/mir/` 与
/// `src/typeck/` 有 54 处定义与传递，但 `src/parser_v3/` **零构造**。
///
/// 判定为「功能未接通」而非「静默错值」—— 没有「本该是 A 却是 B」，
/// 是「压根没法表达」。⇒ 只报告，**不擅动**（与 D346 `xform` 同类）。
#[test]
fn d361_index_assignment_has_no_reachable_syntax() {
    for (tag, body) in [
        ("brackets", "let xs = [1, 2]\nxs[0] = 9\nprint(xs)\n"),
        ("at_call", "let xs = [1, 2]\nxs.at(0) = 9\nprint(xs)\n"),
        ("set_method", "let xs = [1, 2]\nxs.set(0, 9)\nprint(xs)\n"),
        ("dict_brackets", "let m = {a: 1}\nm[\"a\"] = 2\nprint(m)\n"),
    ] {
        let (code, got) = ev(body);
        assert_ne!(
            code, 0,
            "`{tag}` 形态居然成功了 —— 若索引赋值已接通，本判据的「未接通」结论失效，需重新评估: out={got}"
        );
    }
}

/// **源码层断言**：`IndexAssign` 在 MIR/typeck 层有定义，但 parser 侧零构造。
///
/// 脚本层只能证明「不可达」，**无法区分**「未接通」与「设计上不支持」。
/// 源码判据给出关键证据：`WitnessKind::IndexAssign` 在 `witness.rs` 有
/// 完整定义与 children 处理，却没有**任何** parser 侧的构造点。
#[test]
fn d361_index_assign_defined_in_mir_but_never_built_by_parser() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

    // witness 侧：定义完整
    let witness = std::fs::read_to_string(format!("{root}/mir/witness.rs")).expect("读 witness.rs");
    assert!(
        witness.contains("IndexAssign {"),
        "`WitnessKind::IndexAssign` 应仍定义在 witness.rs"
    );

    // parser 侧：零构造
    let mut parser_hits = 0usize;
    for entry in std::fs::read_dir(format!("{root}/parser_v3")).expect("读 parser_v3") {
        let p = entry.expect("dir entry").path();
        if p.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&p).expect("读 rs");
        parser_hits += src.matches("IndexAssign").count();
    }
    assert_eq!(
        parser_hits, 0,
        "parser_v3 里出现了 IndexAssign 构造 —— 索引赋值可能已接通，\
         本文件「未接通」的结论需重新评估（parser 命中 {parser_hits} 处）"
    );

    // 但 MIR 层确实在传递它（否则就不是「死代码」而是「没实现」）
    let lower = std::fs::read_to_string(format!("{root}/mir/lower.rs")).expect("读 lower.rs");
    assert!(
        lower.contains("WitnessKind::IndexAssign") && lower.contains("MirInst::IndexAssign"),
        "MIR 层应在传递 IndexAssign（这正是它属于「完整基础设施 + 零入口」的原因）"
    );
}
