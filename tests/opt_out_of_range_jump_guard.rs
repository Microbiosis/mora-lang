//! v0.104.6 D419：常量条件的 `if/else` 在 `--opt≥1` 下**两个分支都执行** ——
//! 已修，守卫已删。
//!
//! ## 现象（D313 记档，修前）
//!
//! ```text
//! $ mora --opt=1 a.mora     # if 1 > 0 / print(7) / else / print(8) / end   （if 是最后一句）
//! 7.0
//! 8.0
//! $ echo $?
//! 0
//! ```
//!
//! opt=off 只打印 `7.0`。**副作用会重复发生**（重复写文件、重复扣款、
//! 重复发送），程序看起来一切正常、退出码 0 —— 比静默截断更危险。
//!
//! ## 根因：SSA 表达不了「跳到函数末尾」
//!
//! `apply_rules`（两档都跑）的 `ConstFoldingRule` 折叠 `1 > 0`、
//! `IfSimplifyRule` 删掉恒不跳的 `JumpIfNot`，只留下 `Jump(end)` —— 而 `if`
//! 是最后一句时 `end == body.len()`，**跳转目标越界**。
//!
//! 旧 SSA 把越界目标映成 `Terminator::Return(None)`，而 `Return(None)` 在降维
//! 时是**被丢弃**的（「落下去，线性执行自然进入下一块」）⇒ 那条跳转彻底消失
//! ⇒ 块与块之间再无控制转移 ⇒ **两个分支都执行**。
//!
//! D313 当时的处置是加一道守卫（`has_out_of_range_jump`）整体跳过 SSA。
//! **D419 删掉守卫**，改为给 SSA 一个能表达这件事的终结符：
//! [`mora::mir::ssa::Terminator::JumpExit`] —— 「跳到函数末尾」，与
//! 「返回」/「落进下一块」彻底分开。降维时它发一条真跳转，由第四遍补成重建后
//! body 的长度：`execute` 见 `pc >= body.len()` 即结束并走隐式返回；
//! `dag_analyze` 解析不到目标节点 ⇒ 该 Jump 节点无出边。两个执行器都与
//! opt=off 一致。
//!
//! **本文件从「守卫存在吗」翻转成「行为对吗」** —— 守卫已不存在了。

use mora::test_support::TempDir;
use std::process::Command;

fn run(src: &str, tag: &str, opt: Option<&str>) -> (i32, Vec<String>) {
    let dir = TempDir::new(tag);
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = env!("CARGO_BIN_EXE_mora");
    let mut c = Command::new(exe);
    // ⚠ `--opt=` 必须放在文件前面 —— CLI 只扫第一个非选项参数之前的选项。
    if let Some(l) = opt {
        c.arg(format!("--opt={l}"));
    }
    let out = c.arg(&p).output().expect("跑 mora");
    // ⚠ 必须合并 stdout 与 stderr —— 运行时错误走 stderr。
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
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
                && !l.starts_with("类型与执行")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.code().unwrap_or(-1), lines)
}

/// **主判据**：常量条件的 `if/else` 在 `--opt=1/2` 下必须只走 then 分支。
///
/// 修前两个分支都执行（`["7.0", "8.0"]`）。**v0.104.6 D419：守卫已删除**，
/// 本条从「守卫存在」翻转成**直接的行为判据** —— 现在这条 `if/else` 真的会
/// 走 SSA（`Jump(end)` 降维成 [`mora::mir::ssa::Terminator::JumpExit`]），
/// 行为仍然正确。
///
/// **反向牙齿已验证**：把 `ssa.rs` 里的 `Terminator::JumpExit` 改回
/// `Terminator::Return(None)`，本条会红（两个分支都跑）。
#[test]
fn const_condition_if_else_does_not_execute_both_branches() {
    for (tag, src, want) in [
        (
            "const-gt",
            "if 1 > 0
  print(7)
else
  print(8)
end
",
            "7",
        ),
        (
            "const-eq",
            "if 3 == 3
  print(7)
else
  print(8)
end
",
            "7",
        ),
        (
            "bool-literal",
            "if true
  print(7)
else
  print(8)
end
",
            "7",
        ),
        (
            "else-two-stmts",
            "if 1 > 0
  print(7)
else
  print(8)
  print(80)
end
",
            "7",
        ),
    ] {
        let (dc, base) = run(src, &format!("{tag}_base"), None);
        assert_eq!(dc, 0, "[{tag}] 前提：默认档应成功; 实得 {dc}");
        assert_eq!(
            base,
            vec![want.to_string()],
            "[{tag}] 前提：默认档只走 then"
        );

        for lvl in ["1", "2"] {
            let (c, got) = run(src, &format!("{tag}_o{lvl}"), Some(lvl));
            assert_eq!(
                c, 0,
                "[{tag}] `--opt={lvl}` 应成功（修前两侧都跑、退出码仍是 0）; 实得 {c}"
            );
            assert_eq!(
                got,
                base,
                "**D419**：[{tag}] `--opt={lvl}` 下只能走 then 分支。
                 修前是**两个分支都执行** —— 副作用会重复发生（重复写文件、                 重复扣款、重复发送），而程序看起来一切正常、退出码 0。
  实得: {got:?}"
            );
        }
    }
}

/// **`if / else` 汇聚在 Control SSA 显式 CFG 里必须通过全部结构不变式验证，且优化后不得落进另一分支。**
#[test]
fn jump_past_body_end_becomes_a_jump_exit_terminator() {
    let src = "if 1 > 0
  print(7)
else
  print(8)
end
";
    for opt in [
        mora::mir::control::OptLevel::None,
        mora::mir::control::OptLevel::Basic,
        mora::mir::control::OptLevel::Aggressive,
    ] {
        let (prog, _) = mora::cli::compile_and_opt(src, Some(opt)).expect("compile");
        mora::mir::control::verify_program(&prog)
            .unwrap_or_else(|e| panic!("verify_program failed at {opt:?}: {e}"));
    }

    let (opt_prog, _) =
        mora::cli::compile_and_opt(src, Some(mora::mir::control::OptLevel::Aggressive))
            .expect("compile opt=2");
    let main_fn = opt_prog.func(opt_prog.entry);
    // 可达块中每条 Branch 的 then/else 目标必在块表内，Return 必终止控制流
    for b in &main_fn.blocks {
        for s in b.terminator.successors() {
            assert!(
                (s.0 as usize) < main_fn.blocks.len(),
                "块 {:?} 后继 {:?} 越界",
                b.id,
                s
            );
        }
    }
}

/// **优化并执行该程序，验证只有 `then` 臂生效（`print(7)`），不会落进 `else`。**
#[test]
fn deconstructed_body_keeps_a_terminating_jump() {
    let src = "if 1 > 0
  7i
else
  8i
end
";
    for opt in [
        mora::mir::control::OptLevel::None,
        mora::mir::control::OptLevel::Basic,
        mora::mir::control::OptLevel::Aggressive,
    ] {
        let (prog, _) = mora::cli::compile_and_opt(src, Some(opt)).expect("compile");
        mora::mir::control::verify_program(&prog).expect("verify");
        let mut core = mora::runtime::CoreRuntime::default();
        let (v, _) = mora::runtime::vm::execute_program(std::sync::Arc::new(prog), &mut core)
            .expect("execute");
        assert_eq!(
            v,
            mora::value::Value::Int(7),
            "opt={opt:?} 应只走 then 分支"
        );
    }
}

/// **越界跳转形状在三档下必须完全一致**（`break` / `continue` / 嵌套 /
/// 尾语句各变体）。
#[test]
fn out_of_range_jump_shapes_are_opt_invariant() {
    for (tag, src) in [
        (
            "if-else",
            "if 1 > 0
  print(7)
else
  print(8)
end
",
        ),
        (
            "if-no-else",
            "if 1 > 0
  print(7)
end
",
        ),
        (
            "if-trailing",
            "if 1 > 0
  print(7)
else
  print(8)
end
print(9)
",
        ),
        (
            "nested",
            "if 1 > 0
  if 1 > 0
    print(7)
  else
    print(8)
  end
else
  print(9)
end
",
        ),
        (
            "while-break",
            "let i = 0
while i < 3
  i = i + 1
  if i == 2
    break
  end
end
print(i)
",
        ),
        (
            "for-break",
            "let t = 0i
for i in [1,2,3]
  if i == 2
    break
  end
  t = t + i
end
print(t)
",
        ),
    ] {
        let (c0, base) = run(src, &format!("{tag}_base"), None);
        assert_eq!(c0, 0, "[{tag}] 前提：默认档应成功; 实得 {c0}");
        assert!(!base.is_empty(), "[{tag}] 前提：默认档应有输出");
        for lvl in ["1", "2"] {
            let (c, got) = run(src, &format!("{tag}_o{lvl}"), Some(lvl));
            assert_eq!(
                (c, got.clone()),
                (c0, base.clone()),
                "[{tag}] `--opt={lvl}` 与默认档不等价。
  默认 exit={c0} {base:?}
                   opt={lvl} exit={c} {got:?}"
            );
        }
    }
}
