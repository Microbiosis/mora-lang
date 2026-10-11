//! v0.104.6 D420 —— SSA 的 `Copy` 不得经 env 中转。
//!
//! ## 缺陷（静默丢值）
//!
//! `deconstruct` 把 `SsaInst::Copy(dst, src)` 降成两条 env 指令：
//! `Assign(".tmp_N", src)` + `Var(dst, ".tmp_N")`。
//!
//! 而 `Environment::assign` 对**尚未绑定**的名字返回 `false` 且**什么都不写**
//! （`value.rs` 的 `assign`：本层没有 → 走父链 → 也没有 → `false`）。
//! `.tmp_N` 从未被 `define` ⇒ **写入被静默丢弃** ⇒ `Var` 读回 Nil。
//!
//! ```text
//! let x = if 1 > 0 / 7 / else / 8 / end
//! print(x)
//!   opt=0 → 7.0      opt=1 → nil        ← 静默错值，退出码 0
//! ```
//!
//! 未经 SSA 的程序里 `.tmp_N` 同样读不到，但那里的值本来就是 Nil
//! （`let` 发射的是 `Assign("__let_result", Const(Nil))`），于是缺陷被
//! env 往返本身掩盖了 —— **只有走 SSA 的 `Copy`（`if` 汇合值、phi copy）
//! 才会暴露**。
//!
//! ## 修法
//!
//! `SsaInst::Copy(dst, src)` 直接降为 `MirInst::Copy(dst, src)`：精确的
//! 寄存器语义，无 env 往返、无临时名污染、少两条指令。phi 前驱 copy 自 D418
//! 起已是这个形式，两条路径现在一致。
//!
//! ## 与三道门的收束
//!
//! 本条是最后一道 phi 门的最后一块绊脚石。修好后 `opt.rs` 的 SSA 主路径
//! **无门**。

use std::path::{Path, PathBuf};
use std::process::Command;

use mora::mir::{MirFunction, MirInst};

/// 手工跑一遍 SSA 管线（不经过 `opt.rs`，因此不受任何守卫影响）。
fn ssa_lower(src: &str) -> MirFunction {
    let (f, _) = mora::cli::compile_and_opt(src, None).expect("编译应当成功");
    let mut ssa = mora::mir::ssa::construct(&f);
    mora::mir::opt::run_pipeline(&mut ssa, &mora::mir::opt::default_basic_pipeline());
    mora::mir::ssa::deconstruct(&ssa)
}

/// **降维产物里不得出现 `.tmp_N` 临时名**。
///
/// 这是本条的结构性判据：只要 SSA 还用 env 中转搬值，就一定会造出
/// `.tmp_N` —— 而那个名字永远不会被 `define`，写入必然被丢弃。
#[test]
fn d420_ssa_output_never_uses_dot_tmp_env_names() {
    for (tag, src) in [
        (
            "if 表达式",
            "let x = if 1 > 0\n  7\nelse\n  8\nend\nprint(x)\n",
        ),
        ("map 闭包", "print([1,2,3].map(fn(v) v + 10))\n"),
        (
            "for 累加",
            "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        ),
        (
            "嵌套 for",
            "for i in [1,2]\n  for j in [1,2]\n    print(i + j)\n  end\nend\nprint(\"done\")\n",
        ),
    ] {
        let out = ssa_lower(src);
        let offenders: Vec<&MirInst> = out
            .body
            .iter()
            .filter(|i| match i {
                MirInst::Assign(n, _) | MirInst::Var(_, n) | MirInst::Define(n, _) => {
                    n.starts_with(".tmp_")
                }
                _ => false,
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "[{tag}] SSA 降维产物里仍有 {} 条指令经 `.tmp_N` 临时名中转：\n  \
             {:#?}\n  \
             `Environment::assign` 对未绑定名字**静默不写** ⇒ 这些值全是 Nil。\n  \
             完整产物：\n{:#?}",
            offenders.len(),
            offenders,
            out.body
        );
    }
}

/// **SSA 的 `Copy` 降成寄存器拷贝，不是两条 env 指令。**
#[test]
fn d420_ssa_copy_lowers_to_register_copy() {
    let out = ssa_lower("let x = if 1 > 0\n  7\nelse\n  8\nend\nprint(x)\n");
    let copies: Vec<&MirInst> = out
        .body
        .iter()
        .filter(|i| matches!(i, MirInst::Copy(_, _)))
        .collect();
    assert!(
        !copies.is_empty(),
        "该程序必然至少含一条 `Copy`（`if` 汇合值 + phi copy）；实得 0 条。\nbody:\n{:#?}",
        out.body
    );
}

/// **执行级：`let x = if … end` 在三档下必须给出同一个值。**
///
/// 修前 `--opt=1/2` 是 `nil` —— 静默错值、退出码 0、零诊断。
#[test]
fn d420_if_expression_value_survives_every_opt_level() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}：集成测试需要 `cargo build --bin mora` 产出的二进制");
    }
    let dir = std::env::temp_dir().join(format!("mora_d420_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    struct WorkDir(PathBuf);
    impl Drop for WorkDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _work = WorkDir(dir.clone());

    for (tag, src) in [
        (
            "if-true",
            "let x = if 1 > 0\n  7\nelse\n  8\nend\nprint(x)\n",
        ),
        (
            "if-var",
            "let n = 1\nlet x = if n > 0\n  7\nelse\n  8\nend\nprint(x)\n",
        ),
        (
            "if-value",
            "let x = if 1 > 0\n  \"a\"\nelse\n  \"b\"\nend\nprint(x)\n",
        ),
    ] {
        let p = dir.join(format!("{tag}.mora"));
        std::fs::write(&p, src).expect("写脚本");
        let run = |opt: &str| -> (i32, Vec<String>) {
            let out = Command::new(exe)
                .arg(format!("--opt={opt}"))
                .arg(&p)
                .output()
                .expect("跑 mora");
            let text = String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr);
            let lines: Vec<String> = text
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
                })
                .map(str::to_string)
                .collect();
            (out.status.code().unwrap_or(-1), lines)
        };
        let (c0, base) = run("0");
        assert_eq!(c0, 0, "[{tag}] 前提：opt=0 应成功; 实得 {c0}");
        assert_eq!(
            base.len(),
            1,
            "[{tag}] 前提：opt=0 应打印一行; 实得 {base:?}"
        );
        for lvl in ["1", "2"] {
            let (c, got) = run(lvl);
            assert_eq!(
                (c, got.clone()),
                (c0, base.clone()),
                "[{tag}] `--opt={lvl}` 与 opt=0 不一致。\n  \
                 **D420**：SSA 的 `Copy` 曾经 `.tmp_N` 走 env 中转，而 \
                 `Environment::assign` 对未绑定名字**静默不写** ⇒ 值变成 Nil。\n  \
                 opt=0  exit={c0} {base:?}\n  opt={lvl} exit={c} {got:?}"
            );
        }
    }
}

/// **控制流 + 值形状在三档下必须完全一致。**
///
/// 这组形状是**实测抓到过回归**的：`let x = if … end`（D420）、
/// `while` 里 `break`（D419）、`for` 里 `break`（D418）都在其中。
/// 57 个 e2e fixture 覆盖不到这些组合 —— fixture 里没有「`if` 表达式赋值给
/// 变量后立即打印」这种形状，所以本条是它们的第一道差分网。
#[test]
fn d420_control_flow_shapes_are_opt_invariant() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}：集成测试需要 `cargo build --bin mora` 产出的二进制");
    }
    let dir = std::env::temp_dir().join(format!("mora_d420s_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    struct WorkDir(PathBuf);
    impl Drop for WorkDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _work = WorkDir(dir.clone());

    for (tag, src) in [
        (
            "if-value",
            "let x = if 1 > 0\n  7\nelse\n  8\nend\nprint(x)\n",
        ),
        (
            "if-value-var",
            "let n = 1\nlet x = if n > 0\n  7\nelse\n  8\nend\nprint(x)\n",
        ),
        ("if-else", "if 1 > 0\n  print(7)\nelse\n  print(8)\nend\n"),
        (
            "if-else-nested",
            "if 1 > 0\n  if 1 > 0\n    print(7)\n  else\n    print(8)\n  end\nelse\n  print(9)\nend\n",
        ),
        (
            "while-break",
            "let i = 0\nwhile i < 3\n  i = i + 1\n  if i == 2\n    break\n  end\nend\nprint(i)\n",
        ),
        (
            "while-continue",
            "let t = 0i\nlet i = 0i\nwhile i < 5i\n  i = i + 1i\n  if i == 2i\n    continue\n  end\n  t = t + i\nend\nprint(t)\n",
        ),
        (
            "for-break",
            "let t = 0i\nfor i in [1,2,3]\n  if i == 2\n    break\n  end\n  t = t + i\nend\nprint(t)\n",
        ),
        (
            "nested-for",
            "for i in [1,2]\n  for j in [1,2]\n    print(i + j)\n  end\nend\nprint(\"done\")\n",
        ),
        ("map-closure", "print([1,2,3].map(fn(v) v + 10))\n"),
        (
            "match-expr",
            "let x = 2\nprint(match x { 1 => \"a\", 2 => \"b\", _ => \"c\" })\n",
        ),
    ] {
        let p = dir.join(format!("{tag}.mora"));
        std::fs::write(&p, src).expect("写脚本");
        let run = |opt: &str| -> (i32, Vec<String>) {
            let out = Command::new(exe)
                .arg(format!("--opt={opt}"))
                .arg(&p)
                .output()
                .expect("跑 mora");
            let text = String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr);
            let lines: Vec<String> = text
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
                })
                .map(str::to_string)
                .collect();
            (out.status.code().unwrap_or(-1), lines)
        };
        let (c0, base) = run("0");
        assert_eq!(c0, 0, "[{tag}] 前提：opt=0 应成功; 实得 {c0}");
        assert!(!base.is_empty(), "[{tag}] 前提：opt=0 应有输出");
        for lvl in ["1", "2"] {
            let (c, got) = run(lvl);
            assert_eq!(
                (c, got.clone()),
                (c0, base.clone()),
                "[{tag}] `--opt={lvl}` 与 opt=0 不一致 —— SSA 主路径**无门**，\
                 `--opt` 不得改变任何可观察行为。\n  \
                 opt=0   exit={c0} {base:?}\n  opt={lvl} exit={c} {got:?}"
            );
        }
    }
}
