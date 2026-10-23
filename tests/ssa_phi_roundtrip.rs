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

use mora::test_support::TempDir;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use mora::mir::ssa::{MirSsaFunction, SsaInst, Terminator};
use mora::mir::{MirFunction, MirInst};

// ── 通用工具 ─────────────────────────────────────────────────────────

fn ssa_of(src: &str) -> MirSsaFunction {
    let (f, _) = mora::cli::compile_and_opt(src, None).expect("编译应当成功");
    mora::mir::ssa::construct(&f)
}

/// 本指令**定义**的寄存器。`Define` / `Assign` 写 **env**，第二字段是源
/// 寄存器 —— 它们不定义任何 SSA 寄存器（把它们算作定义会报出虚假撞号）。
fn defs(inst: &SsaInst) -> Option<usize> {
    match inst {
        SsaInst::Define(_, _) | SsaInst::Assign(_, _) => None,
        other => mora::mir::ssa::ssa_dst(other),
    }
}

fn uses(inst: &SsaInst) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    match inst {
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
        SsaInst::BinaryOp(_, l, _, r) => v.extend([*l, *r]),
        SsaInst::Call(_, _, a) => v.extend(a),
        SsaInst::CallValue(_, c, a) => {
            v.push(*c);
            v.extend(a);
        }
        SsaInst::ListLit(_, i) => v.extend(i),
        SsaInst::DictLit(_, p) => v.extend(p.iter().map(|(_, v)| *v)),
        SsaInst::Index(_, o, i) => v.extend([*o, *i]),
        SsaInst::IndexAssign(_, o, i) => v.extend([*o, *i]),
        SsaInst::MethodCall(_, r, _, a) => {
            v.push(*r);
            v.extend(a);
        }
        SsaInst::Pipe(_, l, r) => v.extend([*l, *r]),
        SsaInst::Prompt(_, p) => v.extend(p),
        SsaInst::Copy(_, s) | SsaInst::Define(_, s) | SsaInst::Assign(_, s) => v.push(*s),
        SsaInst::Expr(s) => v.push(*s),
        SsaInst::Opaque(m) => v.extend(m.input_regs()),
    }
    v
}

struct Stats {
    dups: Vec<usize>,
    orphans: Vec<usize>,
    out_of_space: Vec<usize>,
}

fn stats(ssa: &MirSsaFunction) -> Stats {
    let mut defined: HashMap<usize, usize> = HashMap::new();
    let mut reads: Vec<usize> = Vec::new();
    let params: HashSet<usize> = ssa.params.iter().map(|(_, r)| *r).collect();
    let num_params = params.len();

    for b in &ssa.blocks {
        for p in &b.phis {
            *defined.entry(p.dst).or_insert(0) += 1;
            for (_, src) in &p.incoming {
                reads.push(*src);
            }
        }
        for i in &b.insts {
            reads.extend(uses(i));
            if let Some(d) = defs(i) {
                *defined.entry(d).or_insert(0) += 1;
            }
        }
        match &b.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => reads.push(*c),
            Terminator::Return(Some(r)) => reads.push(*r),
            _ => {}
        }
    }

    let mut dups: Vec<usize> = defined
        .iter()
        .filter(|&(_, &n)| n > 1)
        .map(|(r, _)| *r)
        .collect();
    dups.sort();
    let mut orphans: Vec<usize> = reads
        .iter()
        .copied()
        .filter(|r| !defined.contains_key(r) && !params.contains(r))
        .collect();
    orphans.sort();
    orphans.dedup();
    // 「没有块被漏掉重命名」：任何被定义的寄存器必然落在 SSA 寄存器空间
    // `[num_params, n_versions)` 内 —— `rename_variables` 从形参号之后起
    // 每定义一次 +1。任何**未被访问**的块会保留原始物理编号，可能越界。
    //
    // 不能用「定义集合必须是连续的 `0..n`」：phi 消除（若将来启用）会在序列
    // 中间挖出合法的空洞。
    let mut out_of_space: Vec<usize> = defined
        .keys()
        .copied()
        .filter(|&r| r < num_params || r >= ssa.n_versions)
        .collect();
    out_of_space.sort();

    Stats {
        dups,
        orphans,
        out_of_space,
    }
}

fn roundtrip(src: &str) -> MirFunction {
    let (f, _) = mora::cli::compile_and_opt(src, None).expect("编译应当成功");
    mora::mir::ssa::deconstruct(&mora::mir::ssa::construct(&f))
}

// ── ① / ②：SSA 的结构不变式 ─────────────────────────────────────────

/// **循环携带寄存器 ⇒ 每条 phi 的 incoming 覆盖该块全部前驱，且 SSA 整体
/// 满足「单一定义 + 每个读都有生产者 + 编号连续」。**
#[test]
fn d418_loop_phis_satisfy_the_ssa_invariants() {
    for (tag, src) in [
        (
            "for 打印",
            "for i in [1,2,3]\n  print(i)\nend\nprint(\"done\")\n",
        ),
        (
            "for 累加",
            "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        ),
        (
            "while",
            "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nprint(acc)\n",
        ),
        (
            "嵌套 for",
            "for i in [1,2]\n  for j in [1,2]\n    print(i + j)\n  end\nend\nprint(\"done\")\n",
        ),
    ] {
        let s = ssa_of(src);
        let phi_count: usize = s.blocks.iter().map(|b| b.phis.len()).sum();
        assert!(
            phi_count > 0,
            "[{tag}] 前提失效：竟没有产生 phi（块划分可能变了）"
        );

        for b in &s.blocks {
            let mut preds = b.preds.clone();
            preds.sort();
            preds.dedup();
            for phi in &b.phis {
                let mut from: Vec<usize> = phi.incoming.iter().map(|(p, _)| *p).collect();
                from.sort();
                from.dedup();
                assert_eq!(
                    from, preds,
                    "[{tag}] 块 {} 的 phi(var={}) 的 incoming 前驱 {:?} != 该块前驱 {:?}。\n\
                     少一条 ⇒ `deconstruct` 少插一条回边 copy ⇒ 该值在回边上没有\
                     生产者（D412 观测到的「`--opt=1` 在 for 处静默中止」）。",
                    b.id, phi.var, from, preds
                );
            }
        }

        let st = stats(&s);
        assert!(
            st.dups.is_empty(),
            "[{tag}] SSA 里同一寄存器被定义多次：{:?} —— 两套编号空间",
            st.dups
        );
        assert!(
            st.orphans.is_empty(),
            "[{tag}] SSA 里读到了没有生产者的寄存器：{:?} —— 执行期读未初始化槽 = \
             **静默错值**",
            st.orphans
        );
        assert!(
            st.out_of_space.is_empty(),
            "[{tag}] 定义寄存器 {:?} 落在 SSA 寄存器空间 [{}, {}) 之外 —— 有基本块\
             **根本没被重命名**（多半是 CFG 被死块污染，或支配集迭代到了极小\
             不动点，支配树在该处断开）",
            st.out_of_space,
            0,
            s.n_versions
        );
    }
}

// ── ③：死块不得进入 CFG ─────────────────────────────────────────────

/// `if … / break / end` 之后 lowering 会留下一段**不可达**的 join 代码。
/// 它不得成为循环体块的前驱 —— 否则 `⋂ Dom(pred)` 被交成空、支配树断开。
#[test]
fn d418_dead_join_code_does_not_poison_the_dominator_tree() {
    let src = "let total = 0i\nfor i in [1,2,3,4,5,6]\n  if i == 4i\n    break\n  end\n\
               total = total + i\nend\nprint(total)\n";
    let s = ssa_of(src);

    // 从入口出发可达的块集合
    let mut reachable: HashSet<usize> = HashSet::new();
    let mut work = vec![s.entry];
    while let Some(b) = work.pop() {
        if !reachable.insert(b) {
            continue;
        }
        work.extend(s.blocks[b].succs.iter().copied());
    }

    for b in &s.blocks {
        if reachable.contains(&b.id) {
            continue;
        }
        assert!(
            b.preds.is_empty(),
            "块 {} 不可达却仍有前驱 {:?} —— 死块进了 CFG，会把可达块的支配集交成空",
            b.id,
            b.preds
        );
        assert!(
            b.succs.is_empty(),
            "块 {} 不可达却仍有后继 {:?} —— 它的边会给可达后继的 phi 记上一条\
             来自死块的 incoming，而那条 incoming 的源版本是在空栈下解析出来的",
            b.id,
            b.succs
        );
    }

    // 循环携带寄存器的 phi 必须**至少有一条 incoming 的源是有生产者的**
    // —— 那条边才发得出前驱 copy。若支配集迭代到了极小不动点，支配树会碎成
    // 一堆单点，每棵树都在**空栈**下重命名，incoming 的源就会指向一个谁都不
    // 写的号（实测：嵌套 `for` 的外层归纳变量 `__idx += 1` 读到从未写入的
    // 寄存器 ⇒ `--opt=1` 下内层循环只跑一半 ⇒ **静默少输出、退出码 0**）。
    let mut produced: HashSet<usize> = HashSet::new();
    for b in &s.blocks {
        for p in &b.phis {
            produced.insert(p.dst);
        }
        for i in &b.insts {
            if let Some(d) = defs(i) {
                produced.insert(d);
            }
        }
    }
    let mut starved: Vec<String> = Vec::new();
    for b in &s.blocks {
        for p in &b.phis {
            if !b.preds.is_empty() && !p.incoming.iter().any(|(_, src)| produced.contains(src)) {
                starved.push(format!(
                    "块 {} 的 phi(var={}) incoming={:?}",
                    b.id, p.var, p.incoming
                ));
            }
        }
    }
    assert!(
        starved.is_empty(),
        "这些 phi 所在块有前驱，但它的 incoming 源**全部**没有生产者 ⇒ 前驱 copy \
         一条都发不出来 ⇒ 循环变量读未初始化槽 ⇒ 静默无输出。\n  - {}",
        starved.join("\n  - ")
    );
}

// ── ④：死 phi 不发 copy ─────────────────────────────────────────────

/// 循环变量走 **env**（`Var`/`Assign`）时，`SsaInst::Var` **不读寄存器** ⇒
/// 循环头为这些变量建的 phi **没有任何消费者**。
///
/// `deconstruct` 无法从外部复算 SSA→plain 的编号分配，因此本条只断言
/// **前提确实存在**（否则下面的判据会因为「场景不存在」而空转），
/// **症状**由 `d418_no_copy_reads_an_unproduced_register` 逐条检查。
#[test]
fn d418_unread_phis_exist_in_env_carried_loops() {
    let src = "let total = 0i\nlet i = 1i\nwhile i <= 10i\n  if i == 5i\n    break\n  end\n\
               total = total + i\n  i = i + 1i\nend\nprint(total)\n";
    let s = ssa_of(src);

    let mut read: HashSet<usize> = HashSet::new();
    for b in &s.blocks {
        for i in &b.insts {
            read.extend(uses(i));
        }
        match &b.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => {
                read.insert(*c);
            }
            Terminator::Return(Some(r)) => {
                read.insert(*r);
            }
            _ => {}
        }
    }
    let dead: Vec<usize> = s
        .blocks
        .iter()
        .flat_map(|b| b.phis.iter())
        .filter(|p| !read.contains(&p.dst))
        .map(|p| p.dst)
        .collect();

    assert!(
        !dead.is_empty(),
        "**前提失效**：这个程序的 SSA 里一条「没人读的 phi」都没有 —— 那说明 \
         `SsaInst::Var` / `SsaInst::Assign` 的语义变了（不再走 env），\
         本判据描述的场景已不存在，请重写。\n  ssa = {s:#?}"
    );
}

/// **降维产物里没有任何 `Copy` 去读一个没有生产者的寄存器。**
///
/// 这是 D418 三个坑的公共症状面：不管根因是死块、死 phi 还是 DCE 活性，
/// 最终形态都是「某条 `Copy` 的源寄存器从未被写过」⇒ DAG 按输入就绪激活
/// ⇒ 永不激活 ⇒ 静默无输出。
#[test]
fn d418_no_copy_reads_an_unproduced_register() {
    for (tag, src) in [
        (
            "for + break",
            "let total = 0i\nfor i in [1,2,3,4,5,6]\n  if i == 4i\n    break\n  end\n\
             total = total + i\nend\nprint(total)\n",
        ),
        (
            "while + continue",
            "let total = 0i\nlet i = 0i\nwhile i < 5i\n  i = i + 1i\n  if i == 2i\n\
             continue\n  end\n  total = total + i\nend\nprint(total)\n",
        ),
        (
            "嵌套 for",
            "for i in [1,2]\n  for j in [1,2]\n    print(i + j)\n  end\nend\nprint(\"done\")\n",
        ),
        (
            "env 循环 + break",
            "let total = 0i\nlet i = 1i\nwhile i <= 10i\n  if i == 5i\n    break\n  end\n\
             total = total + i\n  i = i + 1i\nend\nprint(total)\n",
        ),
    ] {
        let out = roundtrip(src);
        let mut produced: HashSet<usize> = HashSet::new();
        for inst in &out.body {
            if let Some(d) = inst.written_reg() {
                produced.insert(d);
            }
            // 形参也是生产者
            for (i, _) in out.params.iter().enumerate() {
                produced.insert(i);
            }
        }
        let offenders: Vec<&MirInst> = out
            .body
            .iter()
            .filter(|i| i.input_regs().iter().any(|r| !produced.contains(r)))
            .collect();
        assert!(
            offenders.is_empty(),
            "[{tag}] 降维产物里有 {} 条指令读了**从未被写**的寄存器（执行期读到未初始化\
             槽 = 静默错值；DAG 模式下则永不激活 = 静默无输出）。\n  违例:\n    {:#?}\n  \
             产物:\n  {:#?}",
            offenders.len(),
            offenders,
            out.body
        );
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
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
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
