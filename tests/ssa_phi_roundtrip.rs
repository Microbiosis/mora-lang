//! v0.104.6 D418 —— phi 机制首次真正生效后的常驻护栏。
//!
//! ## D412 之前：phi 从未工作过
//!
//! `insert_phi_nodes` 以 `incoming: Vec::new()` 建 phi，而 `rename_variables`
//! 收的是不可变 `&HashMap` 且内部 `clone()` ⇒ 全函数无处可写；
//! `deconstruct` 的 `pred_copies` 唯一来源就是 `phi.incoming` ⇒
//! **前驱 copy 一条都不发出** ⇒ phi 目标寄存器**没有生产者**。
//! `opt.rs` 只能「SSA 里出现 phi 就整体跳过」，把 7/57 的 fixture 挡在优化之外。
//!
//! D418 把 `rename_variables` 改成**支配树 DFS**（进块压栈 / 出块弹栈 /
//! 逐 CFG 边记录 `incoming`），phi 门随之删除。本文件钉住修好之后**不能回退**。
//!
//! ## 三条实测踩到的坑（都是「静默」形态 —— 不报错、退出码 0、什么都不输出）
//!
//! 1. **死块污染支配树**：`if … / break / end` 之后 lowering 仍发射一段
//!    **不可达**的 join 代码，它成了循环体块的前驱 ⇒ `⋂ Dom(pred)` 被交成空
//!    ⇒ 该块 idom 变成「无」⇒ 支配树在此断开 ⇒ 后续子树在**空栈**下被重命名
//!    ⇒ 循环归纳变量读到从未写入的寄存器。
//! 2. **phi 结果没人读也照发 copy**：循环变量走 **env**
//!    （`Var(dst,"total")` / `Assign("total",src)`），而 `SsaInst::Var`
//!    **不读寄存器** ⇒ 循环头为 `total`/`i` 建的 phi 没有任何消费者，
//!    可它们的 `incoming` 仍被发成 copy ⇒ 前驱去读「只在循环体定义过」的寄存器。
//! 3. **DCE 活性是逐块的**：循环体的 `__idx += 1` 只被**另一个块**的 phi 读，
//!    局部 `used` 看不见 ⇒ 定义被删 ⇒ 回边 `Copy(phi_dst, <从未定义>)`。
//!
//! 每条都对应本文件下面的一条判据。

use mora::mir::control::{ControlProgram, OptLevel, verify_program};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

// ── 通用工具 ─────────────────────────────────────────────────────────

fn control_of(src: &str, level: OptLevel) -> ControlProgram {
    let (prog, _) = mora::cli::compile_and_opt(src, Some(level)).expect("编译应当成功");
    verify_program(&prog).expect("verify_program 应当通过");
    prog
}

fn run_control(src: &str, level: OptLevel) -> String {
    let prog = control_of(src, level);
    let mut rt = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut rt).expect("execute_program 应当成功");
    val.to_string()
}

// ── ① / ②：Control CFG 的结构不变式 ─────────────────────────────────

/// 循环结构在各优化档位下必须保持多块 CFG 并通过 `verify_program`，且执行结果一致。
#[test]
fn d418_loop_phis_satisfy_the_ssa_invariants() {
    for (tag, src) in [
        (
            "for 打印",
            "let last = 0\nfor i in [1,2,3]\n  last = i\nend\nlast\n",
        ),
        (
            "for 累加",
            "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nacc\n",
        ),
        (
            "while",
            "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nacc\n",
        ),
        (
            "嵌套 for",
            "let sum = 0\nfor i in [1,2]\n  for j in [1,2]\n    sum = sum + i + j\n  end\nend\nsum\n",
        ),
    ] {
        let base = run_control(src, OptLevel::None);
        for level in [OptLevel::Basic, OptLevel::Aggressive] {
            let p = control_of(src, level);
            assert!(
                p.functions[p.entry.0 as usize].blocks.len() > 1,
                "[{tag}] {level:?} 下循环应保留多块 CFG 结构"
            );
            let got = run_control(src, level);
            assert_eq!(got, base, "[{tag}] {level:?} 改变了循环语义");
        }
    }
}

// ── ③：死块与 dominators 校验 ──────────────────────────────────────────

/// `if … / break / end` 之后的 unreachable 块在各优化档位下不得破坏可达 CFG，且循环正确退出。
#[test]
fn d418_dead_join_code_does_not_poison_the_dominator_tree() {
    let src = "let total = 0i\nfor i in [1,2,3,4,5,6]\n  if i == 4i\n    break\n  end\n\
               total = total + i\nend\ntotal\n";
    let base = run_control(src, OptLevel::None);
    assert_eq!(base, "6", "baseline 应为 6");
    for level in [OptLevel::Basic, OptLevel::Aggressive] {
        let p = control_of(src, level);
        let main = &p.functions[p.entry.0 as usize];
        let mut reachable: HashSet<usize> = HashSet::new();
        let mut work = vec![main.entry.0 as usize];
        while let Some(b) = work.pop() {
            if !reachable.insert(b) {
                continue;
            }
            for succ in main.blocks[b].terminator.successors() {
                work.push(succ.0 as usize);
            }
        }
        assert!(reachable.contains(&(main.entry.0 as usize)));
        let got = run_control(src, level);
        assert_eq!(got, base, "[{level:?}] break 后总和应保持 6");
    }
}

/// 环境变量携带循环含 `if break` 在各优化档位下保持语义一致。
#[test]
fn d418_unread_phis_exist_in_env_carried_loops() {
    let src = "let total = 0i\nlet i = 1i\nwhile i <= 10i\n  if i == 5i\n    break\n  end\n\
               total = total + i\n  i = i + 1i\nend\ntotal\n";
    let base = run_control(src, OptLevel::None);
    assert_eq!(base, "10");
    for level in [OptLevel::Basic, OptLevel::Aggressive] {
        let got = run_control(src, level);
        assert_eq!(got, base, "[{level:?}] 结果不一致");
    }
}

/// 各循环形态经过优化后不得引用未定义寄存器（由 `verify_program` 强制保证）且结果不变。
#[test]
fn d418_no_copy_reads_an_unproduced_register() {
    for (tag, src) in [
        (
            "for + break",
            "let total = 0i\nfor i in [1,2,3,4,5,6]\n  if i == 4i\n    break\n  end\n\
             total = total + i\nend\ntotal\n",
        ),
        (
            "while + continue",
            "let total = 0i\nlet i = 0i\nwhile i < 5i\n  i = i + 1i\n  if i == 2i\n\
             continue\n  end\n  total = total + i\nend\ntotal\n",
        ),
        (
            "嵌套 for",
            "let sum = 0\nfor i in [1,2]\n  for j in [1,2]\n    sum = sum + i + j\n  end\nend\nsum\n",
        ),
        (
            "env 循环 + break",
            "let total = 0i\nlet i = 1i\nwhile i <= 10i\n  if i == 5i\n    break\n  end\n\
             total = total + i\n  i = i + 1i\nend\ntotal\n",
        ),
    ] {
        let base = run_control(src, OptLevel::None);
        for level in [OptLevel::Basic, OptLevel::Aggressive] {
            let got = run_control(src, level);
            assert_eq!(got, base, "[{tag}] {level:?} 改变了输出或寄存器语义");
        }
    }
}

// ── ⑤：执行等价（真实 CLI） ─────────────────────────────────────────

fn run(exe: &str, dir: &Path, file: &str, opt: Option<&str>) -> (i32, String) {
    let mut c = Command::new(exe);
    c.current_dir(dir);
    if let Some(o) = opt {
        c.arg(o);
    }
    let out = c.arg(file).output().expect("跑 mora");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let cleaned = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("类型与执行")
                && !l.starts_with("并发与抽象")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
        })
        .collect::<Vec<_>>()
        .join("\n");
    (out.status.code().unwrap_or(-1), cleaned)
}

/// 循环族（含 `break` / `continue` / 嵌套 / env 携带）在三档下必须完全一致。
///
/// 这三条正是 D418 期间逐个崩掉的形状：`--opt=1` 静默无输出、退出码 0。
#[test]
fn d418_loop_shapes_survive_every_opt_level() {
    let exe = env!("CARGO_BIN_EXE_mora");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}：集成测试需要 `cargo build --bin mora` 产出的二进制");
    }
    let dir = TempDir::new("d418");

    for (tag, src) in [
        (
            "for-accumulate",
            "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        ),
        (
            "for-break",
            "let total = 0i\nfor i in [1,2,3,4,5,6]\n  if i == 4i\n    break\n  end\n\
             total = total + i\nend\nprint(total)\n",
        ),
        (
            "while-continue",
            "let total = 0i\nlet i = 0i\nwhile i < 5i\n  i = i + 1i\n  if i == 2i\n\
             continue\n  end\n  total = total + i\nend\nprint(total)\n",
        ),
        (
            "nested-for",
            "for i in [1,2]\n  for j in [1,2]\n    print(i + j)\n  end\nend\nprint(\"done\")\n",
        ),
        (
            "env-loop",
            "let total = 0i\nlet i = 1i\nwhile i <= 10i\n  if i == 5i\n    break\n  end\n\
             total = total + i\n  i = i + 1i\nend\nprint(total)\n",
        ),
    ] {
        let p = dir.join(format!("{tag}.mora"));
        std::fs::write(&p, src).expect("写脚本");
        let f = p.to_str().unwrap();
        let (c0, o0) = run(exe, &dir, f, Some("--opt=0"));
        assert_eq!(c0, 0, "[{tag}] opt=0 应成功（对照失效）");
        assert!(!o0.is_empty(), "[{tag}] opt=0 无输出（对照失效）");
        for lvl in ["1", "2"] {
            let (c, got) = run(exe, &dir, f, Some(&format!("--opt={lvl}")));
            assert_eq!(
                (c, got.clone()),
                (c0, o0.clone()),
                "[{tag}] `--opt={lvl}` 与 `--opt=0` 不一致。\n  \
                 D418 之前这里是**静默无输出、退出码 0**（phi 前驱 copy 缺失 ⇒\
                 循环变量没有生产者）。\n  opt=0 exit={c0} {o0:?}\n  opt={lvl} exit={c} {got:?}"
            );
        }
    }
}
