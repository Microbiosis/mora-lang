//! v0.104.6 D199：**大整数比较静默给出错误的 `false`** —— 而它**上一行的注释**写着「不经 f64」（已修）。
//!
//! ## 缺陷
//!
//! `flow.rs::numeric_cmp` 原文：
//!
//! ```rust
//! // …两个 BigInt 之间保持精确
//! // 大数比较（不经 f64，避免精度丢失）。          ← 注释这么写
//! (BigInt(a), BigInt(b)) => Ok(Bool(op(bigint_to_f64_lossy(&a), bigint_to_f64_lossy(&b)))),
//!                                                 ↑ 函数名自己就写着 lossy
//! ```
//!
//! **实现与自己的注释相反。** 实测（`b = 10^30`）：
//!
//! ```text
//! b > (b - 1)    →  false      ← 错！两个值舍入到同一个 f64，严格大于变 false
//! (b + 1) > b    →  false      ← 错！同上
//! 3n > 2n        →  true       ← 小值碰巧对
//! ```
//!
//! 比 D197 的解析降级**更隐蔽**：错的不是「算出来的数」，而是**比较结论本身**，
//! 代码看起来完全正常。
//!
//! ## 修法：回调从「f64 闭包」改成「序关系」
//!
//! `Fn(f64, f64) -> bool` 逼着**所有**组合先转 f64。改成 `Fn(NumOrd) -> bool`
//! 后，`BigInt ⊕ BigInt` 与 `Int ⊕ BigInt` 走 `BigInt::cmp`（num-bigint 自带
//! `Ord`）**精确**比较；`Float ⊕ BigInt` 在 BigInt 无法无损装进 f64 时**报错**
//! （与 D198 算术侧同一原则：**宁可报错，不返回错的结论**）。
//!
//! `values_equal`（`==` / `!=` 走它，另被 dict 键查找 / `in` / 去重使用）
//! 的 lossy 分支同样改为精确。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d199_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn run(&self, tag: &str, expr: &str) -> (String, i32) {
        let p = self.0.join(format!("{tag}.mora"));
        std::fs::write(&p, format!("print({expr})\n")).expect("写脚本");
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
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        let body = s
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
            .join("\n");
        (body, out.status.code().unwrap_or(-1))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const B: &str = "1000000000000000000000000000000n"; // 10^30
const B_MINUS1: &str = "999999999999999999999999999999n";

/// **主判据（有牙齿）**：大 BigInt 之间的**严格**比较必须正确。
///
/// 修前：`b > (b-1)` 与 `(b+1) > b` 都返回 **false**。
#[test]
fn d199_large_bigint_comparisons_are_exact() {
    let dir = WorkDir::new("big");
    for (tag, expr, want) in [
        ("gt", format!("{B} > {B_MINUS1}"), "true"),
        ("lt", format!("{B_MINUS1} < {B}"), "true"),
        ("ge", format!("{B} >= {B}"), "true"),
        ("le", format!("{B} <= {B}"), "true"),
        ("gt2", format!("{B} + 1 > {B}"), "true"),
        ("false", format!("{B} > {B} + 1"), "false"),
    ] {
        let (out, code) = dir.run(tag, &expr);
        assert_eq!(code, 0, "[{tag}] 应成功:\n{}", out);
        assert_eq!(
            out.trim(),
            want,
            "[{tag}] `{expr}` 大整数比较被降级成 f64 → 静默给出错误的结论"
        );
    }
}

/// **对照组**：小 BigInt 比较本來就對，不能被改坏。
#[test]
fn d199_small_bigint_comparisons_still_work() {
    let dir = WorkDir::new("small");
    for (tag, expr, want) in [
        ("a", "3n > 2n", "true"),
        ("b", "2n > 3n", "false"),
        ("c", "2n == 2n", "true"),
        ("d", "2n != 3n", "true"),
        ("e", "2n <= 2n", "true"),
        ("f", "2n + 1 > 2n", "true"),
    ] {
        let (out, code) = dir.run(tag, expr);
        assert_eq!(code, 0, "[{tag}] 应成功:\n{}", out);
        assert_eq!(out.trim(), want, "[{tag}] `{expr}` 结果被改坏了");
    }
}

/// BigInt 与 **Float** 的比较：运行期路径已修好，但**类型系统根本不允许**
/// 它到达 —— 这是一条**能力缺口**（非静默缺陷：typeck 明确报错）。
///
/// 实测（真实 `mora`）：`12345n < 12346.0` / `1n == 1` / `2n < 1.5` 一律被
/// typeck 拒：「expected BigInt, got Float」。
///
/// 故本测试断言的**不是**比较结果，而是「它被明确拒绝」——
/// **静默放行反而是坏消息**。要让这条能力真正可用，需改 typeck 的数值塔，
/// 属**类型系统设计决定**，本轮不擅自做（已在 CHANGELOG D199 记档）。
#[test]
fn d199_bigint_vs_float_is_rejected_by_typeck_not_silently_wrong() {
    let dir = WorkDir::new("frac");
    for (tag, expr) in [
        ("a", "12345n < 12346.0"),
        ("b", "2n < 1.5"),
        ("c", "1n == 1"),
    ] {
        let (out, code) = dir.run(tag, expr);
        assert_ne!(
            code, 0,
            "[{tag}] `{expr}` 目前被 typeck 拒绝；若将来 typeck 放行了，\
             本测试会失败 —— 那时请把本用例翻转为「比较结果必须正确」:\n{}",
            out
        );
        assert!(
            out.contains("BigInt"),
            "[{tag}] 拒绝理由应指向 BigInt/Float 的 tower 不匹配:\n{}",
            out
        );
        assert!(
            !out.contains("true") && !out.contains("false"),
            "[{tag}] 不得**静默**给出比较结论:\n{}",
            out
        );
    }
}
