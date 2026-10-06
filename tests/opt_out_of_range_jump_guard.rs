//! v0.104.6 D313：常量条件的 `if/else` 在 `--opt≥1` 下**两个分支都执行**。
//!
//! ## 现象（修前）
//!
//! ```text
//! $ mora --opt=1 a.mora     # if 1 > 0 / print(7) / else / print(8) / end   （if 是最后一句）
//! 7.0
//! 8.0
//! $ echo $?
//! 0
//! ```
//!
//! opt=off 只打印 `7.0`。`--opt=1` 与 `--opt=2` 相同，退出码仍是 0。
//!
//! **副作用会重复发生** —— 重复写文件、重复扣款、重复发送。程序看起来还在
//! 正常工作（没有静默中止、没有报错），这比 D312 那种静默截断**更危险**。
//!
//! ## 触发面（实测 12 种形态，opt=1 / opt=2 三档逐行对比）
//!
//! | 修前 | 形态 |
//! |---|---|
//! | ✗ | 常量真 + `else`（`1 > 0` / `3 == 3` / 字面量 `true`） |
//! | ✗ | 同上但 `else` 有两条语句 |
//! | ✅ | 常量**假** + `else` |
//! | ✅ | 常量真、**无** `else` |
//! | ✅ | 同样结构但 `end` 之后还有一条语句 |
//! | ✅ | `let n = 1` 再判断（条件走 `Var` 间接层） |
//! | ✅ | 变量条件（对照组） |
//!
//! 「末尾加一条语句就正常」这条线索直接指向根因：问题出在**跳转目标越界**。
//!
//! ## 根因
//!
//! `apply_rules`（**两档都跑**，`cli/mod.rs:56`）里：
//!
//! 1. `ConstFoldingRule` 把 `1 > 0` 折叠成 `Const(c, Bool(true))`；
//! 2. `IfSimplifyRule`（`rule.rs:119`，`Some(true) if is_not => Vec::new()`）
//!    **删掉** `JumpIfNot` —— 这个局部改写**本身是对的**（`JumpIfNot(true,t)`
//!    永不跳，删掉即落下去）。问题出在它留下的结构被下游误读。
//!
//! 剩下的 body 是 `<then>; Jump(end); <else>`，而 `if` 是最后一句时
//! `end == body.len()` —— **跳转目标越界**。于是：
//!
//! 1. `construct` 的分块规则 `if lbl < body_len` 为假 ⇒ 不在那里起块
//!    ⇒ CFG 断成互不相连的两块（实测 `block 0 succs=[]`、`block 1 preds=[]`）；
//! 2. `deconstruct` 把该 terminal 跳转映成 `Return(None)`，而 `Return(None)`
//!    是**被丢弃**的（`deconstruct.rs:340`）⇒ **那条 Jump 彻底消失**；
//! 3. 第四遍线性拼接各块 ⇒ 两个分支之间再无控制转移 ⇒ DAG 两段都执行。
//!
//! ## 修复
//!
//! `opt.rs::optimize` 在 `construct` **之前**加守卫：body 里有跳到 body 之外的
//! 控制转移就**整体跳过 SSA**（`func` 逐字节不变）。
//!
//! **判据为什么不落在 `BasicBlock.preds` 上**：最直接的修法是让 `deconstruct`
//! 丢弃不可达块，但 `preds` 正是 D261 记档的三处缺口之一（实测 `block 1
//! preds=[]` 而实际存在一条来自 block 0 的边）—— **拿已知不可靠的字段去决定
//! 删除代码**正是 D276「优化器回滚」教训指向的方向。越界跳转目标则是
//! **直接可测的事实**，不依赖任何下游数据结构。
//!
//! 实测 56 个真实 `.mora`：**零命中** ⇒ 对现有程序**零代价**。

use std::process::Command;

fn run(src: &str, tag: &str, opt: Option<&str>) -> (i32, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mora_d313_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
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
/// 修前两个分支都执行（`["7.0", "8.0"]`）。**反向牙齿已验证**：把
/// `opt.rs` 的 `has_out_of_range_jump` 守卫改成恒假，本条会红。
#[test]
fn const_condition_if_else_does_not_execute_both_branches() {
    for (tag, src, want) in [
        (
            "const-gt",
            "if 1 > 0\n  print(7)\nelse\n  print(8)\nend\n",
            "7.0",
        ),
        (
            "const-eq",
            "if 3 == 3\n  print(7)\nelse\n  print(8)\nend\n",
            "7.0",
        ),
        (
            "bool-literal",
            "if true\n  print(7)\nelse\n  print(8)\nend\n",
            "7.0",
        ),
        (
            "else-two-stmts",
            "if 1 > 0\n  print(7)\nelse\n  print(8)\n  print(80)\nend\n",
            "7.0",
        ),
    ] {
        let (dc, base) = run(src, &format!("{tag}_base"), None);
        assert_eq!(dc, 0, "[{tag}] 前提：默认档应成功; 实得 {dc}");
        assert_eq!(
            base,
            vec![want.to_string()],
            "[{tag}] 前提：默认档应只走 then 分支; 实得 {base:?}"
        );

        for lvl in ["1", "2"] {
            let (c, got) = run(src, &format!("{tag}_opt{lvl}"), Some(lvl));
            assert_eq!(c, 0, "[{tag}] `--opt={lvl}` 应成功; 实得 {c}");
            assert_eq!(
                got, base,
                "**D313**：`--opt={lvl}` 下常量条件的 if/else 必须与 opt=off 等价。\n\
                 修前是**两个分支都执行**（副作用会重复：重复写文件/扣款/发送，\n\
                 而程序看起来一切正常）。\n  [{tag}] 实得: {got:?}"
            );
        }
    }
}

/// **对照组**：这几种形态修前就**已经正确**，本条把缺陷范围**钉死在**
/// 「常量真 + `else` + `if` 是最后一句」这一支，避免把「`if` 整个坏了」
/// 这种过宽的说法钉进断言。
#[test]
fn control_group_shapes_were_already_correct() {
    for (tag, src) in [
        (
            "const-false",
            "if 1 > 2\n  print(7)\nelse\n  print(8)\nend\n",
        ),
        ("no-else", "if 1 > 0\n  print(7)\nend\n"),
        (
            "with-tail",
            "if 1 > 0\n  print(7)\nelse\n  print(8)\nend\nprint(9)\n",
        ),
        (
            "via-var",
            "let n = 1\nif n > 0\n  print(7)\nelse\n  print(8)\nend\n",
        ),
        (
            "variable",
            "let n = 5\nif n > 100\n  print(9)\nelse\n  print(8)\nend\n",
        ),
    ] {
        let (dc, base) = run(src, &format!("ctl_{tag}_base"), None);
        assert_eq!(dc, 0, "[{tag}] 前提：默认档应成功; 实得 {dc}");
        for lvl in ["1", "2"] {
            let (c, got) = run(src, &format!("ctl_{tag}_opt{lvl}"), Some(lvl));
            assert_eq!(c, 0, "[{tag}] `--opt={lvl}` 应成功; 实得 {c}");
            assert_eq!(
                got, base,
                "[{tag}] 对照组：这一支修前就已正确，仍必须保持等价。\n  实得: {got:?} vs {base:?}"
            );
        }
    }
}

// ── 结构不变量：给守卫上牙齿 ─────────────────────────────────────

use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

/// 复刻 D313 缺陷的**结构形状**：`Jump` 的目标越出 body 末尾。
///
/// ⚠ 不是可执行程序 —— 本文件只用它验证守卫谓词，不跑。
fn out_of_range_jump_mir() -> MirFunction {
    let body = vec![
        MirInst::Const(0, Value::Bool(true)), // 0  条件
        MirInst::Const(1, Value::Float(7.0)), // 1  then 分支
        MirInst::Jump(5),                     // 2  跳到 end —— 但 body 只有 0..4 ⇒ 越界
        MirInst::Const(2, Value::Float(8.0)), // 3  else 分支
        MirInst::Const(3, Value::Nil),        // 4
    ];
    MirFunction {
        params: vec![],
        body,
        n_regs: 4,
        ..Default::default()
    }
}

/// **守卫的必要性**：越界跳转必须被谓词认出来。
///
/// 若哪天这条红了（谓词不再命中），守卫会无声地变成死代码，
/// D313 的缺陷就会回来。
#[test]
fn out_of_range_jump_is_detected() {
    let func = out_of_range_jump_mir();
    assert_eq!(func.body.len(), 5, "前提：body 长度");
    assert!(
        mora::mir::ssa::has_out_of_range_jump(&func),
        "body 末尾的 `Jump(5)` 越界（body 只有 0..4），谓词必须命中。\n\
         若本条失败：`opt.rs` 的 D313 守卫会变成死代码，缺陷会回来。"
    );
}

/// **守卫不宽**：目标在 body 内的跳转**不得**被误判。
///
/// 防止有人把谓词写成 `>= 0`（恒真 ⇒ 等于关掉整个 `--opt`）或漏掉某一类
/// 控制转移指令。
#[test]
fn in_range_jumps_are_not_flagged() {
    for (tag, body, n_regs) in [
        (
            "plain",
            vec![MirInst::Const(0, Value::Int(1)), MirInst::Jump(0)],
            1,
        ),
        (
            "jumpif",
            vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::JumpIf(0, 1),
                MirInst::Const(1, Value::Nil),
            ],
            2,
        ),
        (
            "jumpifnot",
            vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::JumpIfNot(0, 1),
                MirInst::Const(1, Value::Nil),
            ],
            2,
        ),
        (
            "break-continue",
            vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Break(0),
                MirInst::Continue(0),
            ],
            1,
        ),
        // `usize::MAX` 是 `deconstruct` 的丢弃哨兵（`Label(usize::MAX)`），不算越界
        (
            "usize-max-sentinel",
            vec![MirInst::Const(0, Value::Int(1)), MirInst::Jump(usize::MAX)],
            1,
        ),
    ] {
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
            ..Default::default()
        };
        assert!(
            !mora::mir::ssa::has_out_of_range_jump(&func),
            "[{tag}] 跳转目标在 body 内（或为 usize::MAX 哨兵），不应被判为越界。\n\
             谓词写宽 ⇒ 所有函数都跳过 SSA ⇒ `--opt` 被无声废掉。"
        );
    }
}
