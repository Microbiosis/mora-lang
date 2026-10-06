//! v0.104.6 D302：恒等运算（`x + 0` / `x - 0` / `x * 1` / `0 + x` / `x / 1`）
//! 曾让**其后所有语句静默消失**（退出码 0、零诊断）。
//!
//! ## 现象（修前，默认档 = `OptLevel::None`）
//!
//! ```mora
//! print("A")
//! let a = 100i + 0i
//! print(a)
//! print("B")
//! ```
//!
//! 只输出 `A`；`a` 与 `B` 两条**都没执行**。阈值实测恰好是 **100**
//! （`99i + 0i` 正常，`100i + 0i` 起失效）。
//!
//! ## 根因（`src/mir/optimize/dag_rule.rs`）
//!
//! `AlgebraicSimplifyDagRule` 的 `ReplaceWithSource` 分支是 **`MirInst::Copy`
//! 时代的遗留物** —— `Copy` 在 v0.55 已删除（见 `optimize/rule.rs` 的
//! `DeadAssignRule` 注释）。该分支 `added: vec![]`：**不添加任何节点**，
//! 只把原节点标 `Removed` 并把出边改指到源节点。于是没有任何指令写 `dst`，
//! 而消费者（`reg_rename: None` ⇒ 读寄存器没变）仍在读 `dst`
//! ⇒ 该节点永不 ready ⇒ 整条 Sequence 静默卡死 ⇒ 其后语句全部不执行。
//!
//! ⚠ 曾试过「把边的 `reg` 从源寄存器改成 `dst`」——**无效**：执行器要的是一个
//! `dst` 匹配的**节点**，不是一条声称携带 `dst` 的边。故最终**停用该分支**
//! （`ReplaceWithConst` 即 `x*0 → 0` 那一支保留，它一直是正确的）。
//!
//! ## 代价
//!
//! 恒等运算不再被化简（`x + 0` 保留 `BinaryOp`）。**语义完全等价** —— 执行器
//! 照常算出 `x`，只是少了这一处优化。要恢复它需要重新引入「把源的值搬进
//! `dst`」的机制（恢复 `Copy` 或加等价节点），属**架构决定**，未擅自实施。
//!
//! ## 判据形态
//!
//! 断言的是「**三条语句都执行了**」（按序出现 A / 100 / B），而不是精确的
//! 输出行数 —— 因为 `--opt=1/2` 下另有一个**已上报未修**的尾块重复执行缺陷
//! （`let` 后跟 ≥2 条 print 时尾部会多跑一遍），两者互不相关，不应互相绑死。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mora_d302_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    let lines = s
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.contains("AI: mock")
                && !l.contains("AI 原语")
                && !l.contains("显式 API")
                && !l.contains("Trait 系统")
                && !l.contains("Built-in")
                && !l.contains("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
        })
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

/// 前 n 个输出行必须逐字等于 `want` —— 只看**前缀**，容忍已上报的尾块重复缺陷。
fn assert_prefix(got: &[String], want: &[&str], ctx: &str) {
    assert!(
        got.len() >= want.len(),
        "{}\n  实际只输出了 {:?}（应有至少 {} 行）—— 语句被静默丢弃了",
        ctx,
        got,
        want.len()
    );
    for (i, w) in want.iter().enumerate() {
        assert_eq!(&got[i], w, "{}：第 {} 行不符", ctx, i + 1);
    }
}

/// **主判据**：恒等运算之后，程序其余部分必须照常执行。
#[test]
fn identity_operations_do_not_silently_drop_the_rest_of_the_program() {
    for (src, want) in [
        // D302 原始复现（阈值 100）
        (
            "print(\"A\")\nlet a = 100i + 0i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "100", "B"],
        ),
        // 另外四种恒等式，以及 `(Add, Some(0), _)` 那一支（0 + x）
        (
            "print(\"A\")\nlet a = 100i - 0i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "100", "B"],
        ),
        (
            "print(\"A\")\nlet a = 100i * 1i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "100", "B"],
        ),
        (
            "print(\"A\")\nlet a = 0i + 100i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "100", "B"],
        ),
        (
            "print(\"A\")\nlet a = 100i / 1i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "100", "B"],
        ),
        // 阈值两侧
        (
            "print(\"A\")\nlet a = 99i + 0i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "99", "B"],
        ),
        (
            "print(\"A\")\nlet a = 1000000i + 0i\nprint(a)\nprint(\"B\")\n",
            vec!["A", "1000000", "B"],
        ),
    ] {
        let (code, got) = run(src, "identity");
        assert_eq!(code, 0, "[{src}] 退出码应为 0; 实得 {code}");
        assert_prefix(&got, &want, &format!("[{src}]\n  实得: {got:?}"));
    }
}

/// `x * 0 → 0` 走的是同文件的 `ReplaceWithConst` 分支（**未被停用**），
/// 必须仍然正确 —— 防「停用 Source 分支时把 Const 分支一起弄坏」。
#[test]
fn multiply_by_zero_still_folds_to_zero() {
    let (code, got) = run(
        "print(\"A\")\nlet a = 100i * 0i\nprint(a)\nprint(\"B\")\n",
        "mul_zero",
    );
    assert_eq!(code, 0, "退出码应为 0; 实得 {code}");
    assert_prefix(&got, &["A", "0", "B"], "x*0 应折成 0");
}

/// 对照组：普通算术在任何恒等形态附近都必须不受影响。
#[test]
fn ordinary_arithmetic_is_unaffected() {
    for (src, want) in [
        ("print(100i + 1i)\n", "101"),
        ("print(100i * 2i)\n", "200"),
        ("print(100i / 2i)\n", "50"),
        ("print(100i % 7i)\n", "2"),
        ("print(3i + 0i)\n", "3"),
        ("print(100.0 + 1.0)\n", "101.0"),
    ] {
        let (code, got) = run(src, "ordinary");
        assert_eq!(code, 0, "[{src}] 应成功; 实得 {code}");
        assert_prefix(&got, &[want], &format!("[{src}]\n  实得: {got:?}"));
    }
}

/// `--opt=1` 也必须正确（那边原本就靠 MIR 层常量折叠免疫，本条防回归）。
#[test]
fn basic_opt_level_also_executes_the_rest() {
    let dir = std::env::temp_dir().join("mora_d302_opt1");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(
        &p,
        "print(\"A\")\nlet a = 100i + 0i\nprint(a)\nprint(\"B\")\n",
    )
    .expect("写");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg("--opt=1")
        .arg(&p)
        .output()
        .expect("跑");
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && l.len() < 30)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_prefix(&got, &["A", "100", "B"], "--opt=1");
}
