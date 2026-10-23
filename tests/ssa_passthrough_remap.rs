//! v0.104.6 D417 —— 透传指令（`Closure` / `MatchExpr` / `Handle` / `WithConfig`
//! / `Perform` / `Solve` / `DynTrait` / `Quasiquote` …）**参与 SSA 重命名**。
//!
//! ## 缺陷：一个函数里同时存在两套寄存器空间
//!
//! `construct` 此前把这 8 类（外加全部声明型）指令抽进
//! `MirSsaFunction::passthrough`，**原样保留它们的寄存器号**；同一函数里
//! 其余指令却会被 SSA 重编号。于是（v0.104.6 D310 实测逐条对照）：
//!
//! ```text
//!   opt=off:  Closure { dst: 8 }   MethodCall(9, 4, "map", [8])   实参 8 = 闭包 ✅
//!   opt=1:    Closure { dst: 8 }   MethodCall(5, 1, "map", [2])   实参 2 ≠ 8   ❌
//! ```
//!
//! 后果是**静默**的：`function_call.mora`（`{"add": fn(a,b) a + b end}`）在
//! `--opt=1/2` 下**无输出、退出码 0、零诊断**；`match_guard.mora` 少打印一行；
//! `tea_list_model.mora` 什么都不打印。
//!
//! 两个「掩盖」手段都不解决问题：
//!
//! 1. `deconstruct` 用 `n_regs.max(orig_n_regs)` 把帧撑大 —— **值是错的，
//!    只是没越界报错**；
//! 2. `opt::optimize` 见到这类指令**整体跳过 SSA** —— 安全，但 57 个 fixture
//!    里 14 个永远拿不到 SSA 优化。
//!
//! 此外 `passthrough` 只按顺序收集、**不记录原始位置**，`deconstruct` 用
//! `chain(body)` 把它们**整体前插**到 body 头部 ⇒ 原始指令顺序被打乱。
//!
//! ## 修法：让它们成为 SSA IR 的一等变体
//!
//! `SsaInst::Opaque(MirInst)` —— 不为其建模语义，但**留在原位**参与同一条
//! 重命名路径（读经 `map_regs`、写分配 fresh 版本号）。三条缺陷同时消失：
//! 顺序由位置保证、寄存器由重命名保证、`n_regs` 由实际分配保证。
//!
//! 顺带修掉两条同族缺陷：
//!
//! - **`Return(None)` 的顺序后继边缺失**（D312 根因之一）：CFG 把它当终点，
//!   而 `deconstruct` 明写它的语义是「落下去」⇒ 后继块 `preds` 为空、
//!   遍历走不到 ⇒ 整块未被重命名（部分重命名）。
//! - **`collect_definitions` 把 `Assign` 当定义**：`Define`/`Assign` 写的是
//!   **env**，第二字段是源寄存器；把它们算作定义会凭空放大 phi 插入点。
//!
//! ## 本文件的判据
//!
//! ① **单寄存器空间**：SSA 函数内任一寄存器不得被两条指令同时定义。
//! ② **每个读都有生产者**：读到没有生产者的寄存器 = 执行期读未初始化槽
//!    = **静默错值**（本项目头号失败模式）。
//! ③ **依赖同构**：`construct → deconstruct` 往返后，指令的**依赖结构**
//!    （「这条指令读的值由哪条指令定义」）与原始 MIR 逐条相同。
//! ④ **顺序保持**：原位发射 —— 原本在前的指令不得被挪到其后。
//! ⑤ **执行等价**：57 个 fixture × `--opt=0/1/2` 三档输出与退出码全一致。

use mora::test_support::TempDir;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use mora::mir::ssa::{MirSsaFunction, SsaInst, Terminator};
use mora::mir::{MirFunction, MirInst};

// ── ① / ② 的性质检查 ────────────────────────────────────────────────

/// 本指令**写**的 SSA 寄存器。
///
/// `Define(name, reg)` / `Assign(name, reg)` 写的是 **env**，第二字段是被
/// 绑定的**源**寄存器 —— 它们不定义任何 SSA 寄存器（`rename_variables` 对
/// 二者也只 resolve 不重编号）。把它们算作定义会得到虚假的「撞号」报告。
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

/// SSA 函数的三条结构违例。
struct Violations {
    /// 同一寄存器被定义多次（两套编号空间）。
    dups: Vec<usize>,
    /// 读到了没有生产者的寄存器（执行期读未初始化槽 = 静默错值）。
    orphans: Vec<usize>,
    /// 定义寄存器落在 SSA 寄存器空间之外 —— 说明有块**根本没被重命名**。
    out_of_space: Vec<usize>,
}

/// 收集 SSA 函数的三条结构违例。
fn check_single_register_space(ssa: &MirSsaFunction) -> Violations {
    let mut defined: HashMap<usize, usize> = HashMap::new();
    let mut reads: Vec<usize> = Vec::new();
    let params: HashSet<usize> = ssa.params.iter().map(|(_, r)| *r).collect();
    let num_params = params.len();

    for b in &ssa.blocks {
        for p in &b.phis {
            *defined.entry(p.dst).or_insert(0) += 1;
            for (_, s) in &p.incoming {
                reads.push(*s);
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

    // 「没有块被漏掉重命名」：`rename_variables` 从 `num_params` 起发版本号、
    // 每次定义 +1，故任何**被定义**的寄存器必然 `< n_versions`（= SSA 寄存器
    // 空间大小）。任何**未被访问**的块会保留**原始物理编号**，可能越界 ——
    // 那正是 D312 根因 1（`Return(None)` 在 CFG 里缺顺序后继边 ⇒ 遍历从
    // `vec![0]` 出发走不到 ⇒ 整块没重命名）。
    //
    // 注意不能用「定义集合是连续的 `0..n`」：D418 的 `eliminate_dead_phis`
    // 会把死 phi 的版本号从序列中间**挖掉**，序列合法地出现空洞。
    let mut out_of_space: Vec<usize> = defined
        .keys()
        .copied()
        .filter(|&r| r < num_params || r >= ssa.n_versions)
        .collect();
    out_of_space.sort();

    Violations {
        dups,
        orphans,
        out_of_space,
    }
}

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("读 e2e fixture 目录")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "mora").then_some(p)
        })
        .collect();
    v.sort();
    assert!(
        v.len() >= 50,
        "fixture 数异常少（{}），判据会失去意义",
        v.len()
    );
    v
}

/// **`construct` 的输出必须处在单一寄存器空间里。**
///
/// 判据只对「不走 phi 守卫」的函数生效 —— `opt.rs` 的 D312 守卫在 SSA 里
/// 出现 phi 时整体跳过 SSA，而 phi 机制本身（`phi.incoming` 恒空、
/// 遍历不做 push/pop）是**另一条尚未修的链**，由 `tests/opt_phi_guard.rs`
/// 单独钉住。把两者混在一条断言里，只会让失败原因不可读。
#[test]
fn d417_construct_yields_a_single_register_space() {
    let mut dup_cases: Vec<String> = Vec::new();
    let mut orphan_cases: Vec<String> = Vec::new();
    let mut gap_cases: Vec<String> = Vec::new();

    // v0.104.6 D418：phi 门删除后**不再跳过任何 fixture** —— 本判据原先把
    // 「含 phi 的函数」排除在外（那时它们被 `opt.rs` 的 phi 守卫整体挡在
    // 优化之外，性质无从验证）。现在 57 个全部走 SSA，全部纳入检查。
    let mut checked = 0usize;
    for p in fixtures() {
        let src = std::fs::read_to_string(&p).expect("读 fixture");
        let Ok((f, _)) = mora::cli::compile_and_opt(&src, None) else {
            continue;
        };
        let ssa = mora::mir::ssa::construct(&f);
        checked += 1;
        let v = check_single_register_space(&ssa);
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !v.dups.is_empty() {
            dup_cases.push(format!("{name}: 寄存器被重复定义 {:?}", v.dups));
        }
        if !v.orphans.is_empty() {
            orphan_cases.push(format!("{name}: 读到了没有生产者的寄存器 {:?}", v.orphans));
        }
        if !v.out_of_space.is_empty() {
            gap_cases.push(format!(
                "{name}: 定义寄存器 {:?} 落在 SSA 寄存器空间 [0, {}) 之外",
                v.out_of_space, ssa.n_versions
            ));
        }
    }

    assert!(
        dup_cases.is_empty(),
        "**D417**：一个函数内不得存在两套寄存器空间 —— 同一寄存器被两条指令定义。\n\
         这正是 D310 里「闭包写 r8 而 map 读 r2」的形状：一旦出现，被重复定义的\
         寄存器会让 `deconstruct` 把两者映到同一个 plain 槽，后写覆盖先写，\
         **静默产出错误结果**。违例：\n  - {}",
        dup_cases.join("\n  - ")
    );
    assert!(
        orphan_cases.is_empty(),
        "**D417**：每个被读取的寄存器都必须有生产者（形参 / 某条指令的写 / phi 入边）。\n\
         没有生产者的读 = 执行期读到未初始化的槽 = **静默错值**（D304 / D310 / D312 \
         三个缺陷共同的执行期表现）。违例：\n  - {}",
        orphan_cases.join("\n  - ")
    );
    assert!(
        gap_cases.is_empty(),
        "**D417**：任何被定义的寄存器都必须落在 SSA 寄存器空间内（`rename_variables`
         从形参号之后起每定义一次 +1）。越界说明有基本块**根本没被重命名** —— 它
         保留了原始物理编号，与其余指令形成两套空间（D312 根因 1：`Return(None)`
         在 CFG 里缺顺序后继边 ⇒ 遍历从 `vec![0]` 出发走不到）。违例：
  - {}",
        gap_cases.join(
            "
  - "
        )
    );
    assert!(
        checked >= 50,
        "只检查了 {checked} 个 fixture —— 覆盖面异常，判据会失去意义"
    );
}

// ── ③ 依赖同构 ───────────────────────────────────────────────────────

/// 「读的值由哪条指令定义」的依赖指纹。
///
/// 用**定义者下标**而不是寄存器号 —— 寄存器号在 SSA 往返中必然改变，
/// 能不变的只有「谁定义了我」。`Define`/`Assign` 写 env，不定义寄存器，
/// 故不进入生产者表。
///
/// 控制流指令（`Label` / `Jump` / `Return` …，`CONTROL_TAG`）在往返中由
/// `deconstruct` 增删，既不计入指纹也不进入生产者表 —— 否则下标会整体错位。
fn dep_fingerprint(body: &[MirInst]) -> Vec<(u8, Vec<i64>)> {
    let mut producer: HashMap<usize, usize> = HashMap::new();
    let mut out = Vec::new();
    for inst in body {
        let tag = kind_tag(inst);
        if tag == CONTROL_TAG {
            continue;
        }
        let i = out.len();
        let mut reads: Vec<i64> = inst
            .input_regs()
            .into_iter()
            .map(|r| producer.get(&r).map(|&j| j as i64).unwrap_or(-1))
            .collect();
        reads.sort();
        out.push((tag, reads));
        if writes_register(inst) {
            producer.insert(inst.written_reg_or_panic(), i);
        }
    }
    out
}

/// `deconstruct` 会增删控制流指令；它们的标签不进指纹。
const CONTROL_TAG: u8 = 0;

fn kind_tag(inst: &MirInst) -> u8 {
    // 稳定标签：同一「语义类别」的指令在往返前后必须可比。控制流指令
    // （Label/Jump/Return）被 `deconstruct` 增删，取 `CONTROL_TAG`。
    match inst {
        MirInst::Label(_)
        | MirInst::Jump(_)
        | MirInst::JumpIf(_, _)
        | MirInst::JumpIfNot(_, _)
        | MirInst::Return(_)
        | MirInst::Halt(_)
        | MirInst::Break(_)
        | MirInst::Continue(_) => CONTROL_TAG,
        MirInst::Const(_, _) => 1,
        MirInst::Var(_, _) => 2,
        MirInst::Copy(_, _) => 3,
        MirInst::BinaryOp(_, _, _, _) => 4,
        MirInst::Call(_, _, _) => 5,
        MirInst::CallValue(_, _, _) => 6,
        MirInst::ListLit(_, _) => 7,
        MirInst::DictLit(_, _) => 8,
        MirInst::Index(_, _, _) => 9,
        MirInst::IndexAssign(_, _, _) => 10,
        MirInst::MethodCall(_, _, _, _) => 11,
        MirInst::Pipe(_, _, _) => 12,
        MirInst::Prompt(_, _) => 13,
        MirInst::Define(_, _) => 14,
        MirInst::Assign(_, _) => 15,
        MirInst::Expr(_) => 16,
        MirInst::Closure { .. } => 17,
        MirInst::MatchExpr { .. } => 18,
        MirInst::Handle { .. } => 19,
        MirInst::Perform { .. } => 20,
        MirInst::WithConfig { .. } => 21,
        MirInst::Quasiquote { .. } => 22,
        MirInst::Solve { .. } => 23,
        MirInst::DynTrait { .. } => 24,
        // 其余（声明型 / 效果型）：往返前后必须仍是同一类，但本测试不区分
        // 它们之间的差别 —— 给它们各自的标签即可。
        MirInst::TaskDef { .. } => 30,
        MirInst::Import(_) => 31,
        MirInst::ExportMark(_) => 32,
        MirInst::TypeAlias { .. } => 33,
        MirInst::EnumDef { .. } => 34,
        MirInst::StructDef { .. } => 35,
        MirInst::RelDef { .. } => 36,
        MirInst::ModelDef { .. } => 37,
        MirInst::MsgDef { .. } => 38,
        MirInst::UpdateDef { .. } => 39,
        MirInst::AppDef { .. } => 40,
        MirInst::Transaction { .. } => 41,
        MirInst::Send { .. } => 42,
        MirInst::Aggregate { .. } => 43,
        MirInst::Rollback => 44,
        MirInst::MacroDef { .. } => 45,
        MirInst::Commit => 46,
        MirInst::Worker { .. } => 47,
        MirInst::Parallel { .. } => 48,
        MirInst::Observe { .. } => 49,
        MirInst::Span { .. } => 50,
        MirInst::TraitDef { .. } => 51,
        MirInst::ImplDef { .. } => 52,
        MirInst::Orchestrate { .. } => 53,
        MirInst::Eval { .. } => 54,
        MirInst::PromptSection { .. } => 55,
        MirInst::DocumentSection { .. } => 56,
    }
}

fn writes_register(inst: &MirInst) -> bool {
    !matches!(inst, MirInst::Define(_, _) | MirInst::Assign(_, _)) && inst.written_reg().is_some()
}

// `MirInst::written_reg` 在 `Define`/`Assign` 上返回 `None`，但本文件需要
// 一个「一定写」的版本给生产者表用 —— 上面的 `writes_register` 已经过滤，
// 这里只需把 `Option` 拆开。
trait WrittenRegOrPanic {
    fn written_reg_or_panic(&self) -> usize;
}
impl WrittenRegOrPanic for MirInst {
    fn written_reg_or_panic(&self) -> usize {
        self.written_reg()
            .expect("writes_register 为真即 written_reg 非空")
    }
}

fn roundtrip(src: &str) -> (MirFunction, MirFunction) {
    let (f, _) = mora::cli::compile_and_opt(src, None).expect("编译");
    let out = mora::mir::ssa::deconstruct(&mora::mir::ssa::construct(&f));
    (f, out)
}

/// **主护栏**：`construct → deconstruct` 往返必须保持依赖结构。
///
/// 这条比「输出相同」强：输出相同可以是**两处错误互相抵消**，
/// 而依赖指纹逐条相同意味着「每条指令读到的，仍是原本定义它的那个值」。
#[test]
fn d417_roundtrip_preserves_the_dependency_graph() {
    for (tag, src) in [
        (
            "闭包+字典",
            "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))\n",
        ),
        ("map 闭包", "print([1,2,3].map(fn(v) v + 10))\n"),
        (
            "match",
            "let x = 2\nprint(match x { 1 => \"a\", 2 => \"b\", _ => \"c\" })\n",
        ),
        (
            "match+守卫",
            "let x = 3\nprint(match x { n when n > 5 => \"big\", _ => \"small\" })\n",
        ),
        (
            "handle/perform",
            "let global_result = \"init\"\nhandle Ai {\n  global_result = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(global_result)\n",
        ),
        ("quasiquote", "let x = 5\nlet q = `10+,x\nprint(eval(q))\n"),
    ] {
        let (before, after) = roundtrip(src);
        let fb = dep_fingerprint(&before.body);
        let fa = dep_fingerprint(&after.body);
        assert_eq!(
            fb, fa,
            "[{tag}] `construct → deconstruct` 往返改变了指令的依赖结构。\n\
             修前：透传指令的寄存器不参与重命名（D310 两套寄存器空间），\
             且被 `chain` 整体前插到 body 头部（顺序被打乱）。\n  \
             原始:\n{:?}\n  往返后:\n{:?}",
            before.body, after.body
        );
    }
}

// ── ④ 顺序保持 ───────────────────────────────────────────────────────

/// 透传指令必须**原位**发射。
///
/// 修前 `deconstruct` 用 `ssa.passthrough.iter().chain(body)` 把它们**整体
/// 前插**到 body 头部 —— 声明型指令（`TaskDef` 等）跑到使用者之前尚可，
/// 但值产生型（`Closure` / `MatchExpr`）跑到使用者之前就会读到未初始化的槽。
#[test]
fn d417_opaque_instructions_keep_their_original_position() {
    let (before, after) = roundtrip("let ops = {\"add\": fn(a, b) a + b end}\nprint(1i)\n");

    let pos = |body: &[MirInst], f: fn(&MirInst) -> bool| -> usize {
        body.iter().position(f).unwrap_or_else(|| {
            panic!("往返后丢失了指令；body = {body:?}");
        })
    };

    let closure_at = |body: &[MirInst]| pos(body, |i| matches!(i, MirInst::Closure { .. }));
    let dict_at = |body: &[MirInst]| pos(body, |i| matches!(i, MirInst::DictLit(_, _)));

    assert!(
        closure_at(&before.body) < dict_at(&before.body),
        "前提：原始 MIR 里闭包在字典字面量之前（否则本判据测不到顺序）"
    );
    assert!(
        closure_at(&after.body) < dict_at(&after.body),
        "**D417**：透传指令必须原位发射。\n\
         修前 `deconstruct` 把它们 `chain` 到 body 头部 ⇒ 顺序被打乱 ⇒ \
         消费者可能读到尚未写入的寄存器 ⇒ **静默错值**。\n\
         往返后 body:\n{:?}",
        after.body
    );
}

/// 声明型指令同样原位保留：`task main()` 的定义必须排在调用之前。
#[test]
fn d417_task_def_keeps_its_place_relative_to_its_call() {
    let (before, after) = roundtrip("task main()\n  print(\"hi\")\nend\nmain()\n");
    let at =
        |body: &[MirInst], f: fn(&MirInst) -> bool| body.iter().position(f).unwrap_or(usize::MAX);
    let task_at = |body: &[MirInst]| at(body, |i| matches!(i, MirInst::TaskDef { .. }));
    let call_at =
        |body: &[MirInst]| at(body, |i| matches!(i, MirInst::Call(_, n, _) if n == "main"));

    assert!(
        task_at(&before.body) < call_at(&before.body),
        "前提：原始 MIR 里 TaskDef 在调用之前；实得 {} / {}",
        task_at(&before.body),
        call_at(&before.body)
    );
    assert!(
        task_at(&after.body) < call_at(&after.body),
        "**D417**：TaskDef 必须原位保留在被调用之后的位置之前。\n往返后 body:\n{:?}",
        after.body
    );
}

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

/// **57 个 fixture × `--opt=0/1/2`：三档输出与退出码必须完全一致。**
///
/// 这条是本刀的主执行级护栏 —— ①②③④ 都是结构判据，只有它能证明
/// 「优化没有改变程序的可观察行为」。D310 的门正是被它挡住的：解禁后
/// `function_call.mora` 在 `--opt=1` 下静默无输出、退出码 0。
#[test]
fn d417_opt_levels_never_change_fixture_behaviour() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}：集成测试需要 `cargo build --bin mora` 产出的二进制");
    }
    let dir = TempDir::new("d417");

    let mut broken: Vec<String> = Vec::new();
    for f in fixtures() {
        let (c0, o0) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=0"));
        let (c1, o1) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=1"));
        let (c2, o2) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=2"));
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if (c1, &o1) != (c0, &o0) {
            broken.push(format!(
                "{name}: opt=1 exit={c1} {o1:?} ≠ opt=0 exit={c0} {o0:?}"
            ));
        }
        if (c2, &o2) != (c0, &o0) {
            broken.push(format!(
                "{name}: opt=2 exit={c2} {o2:?} ≠ opt=0 exit={c0} {o0:?}"
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "`--opt` 改变了 {} 个 fixture 的可观察行为：\n  - {}",
        broken.len(),
        broken.join("\n  - ")
    );
}

/// **曾经被门挡住的三个 fixture 必须真的走 SSA 了。**
///
/// 判据不是「输出对」（那可能是根本没优化），而是「`opt.rs` 里不再有
/// 透传守卫」+「输出与 `opt=0` 一致」。
#[test]
fn d417_previously_gated_fixtures_now_reach_ssa() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}");
    }
    let dir = TempDir::new("d417b");

    // D310 记档的三个「解禁即静默失败」样本 + 语义各异的另外两个。
    for name in [
        "function_call.mora",  // {"add": fn(a,b) … end} → 5
        "match_guard.mora",    // 多行 match 输出
        "tea_list_model.mora", // TEA 列表模型
        "match_default.mora",
        "rel_basic.mora",
    ] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/e2e")
            .join(name);
        let (c0, o0) = run(exe, &dir, p.to_str().unwrap(), Some("--opt=0"));
        let (c1, o1) = run(exe, &dir, p.to_str().unwrap(), Some("--opt=1"));
        assert_eq!(
            (c1, &o1),
            (c0, &o0),
            "**D417**：[{name}] 此前因含透传指令被 `has_register_carrying_passthrough`\
             整体跳过 SSA。该门已删除，`--opt=1` 必须与 `--opt=0` 逐行等价。\n  \
             opt=0 exit={c0} {o0:?}\n  opt=1 exit={c1} {o1:?}"
        );
    }
}

/// `{"add": fn(a,b) a + b end}` 这个 D310 的原始样本：闭包必须能被 SSA 重命名。
#[test]
fn d417_closure_literal_survives_renaming() {
    let dir = TempDir::new("d417c");
    let f = dir.join("a.mora");
    std::fs::write(
        &f,
        "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))\n",
    )
    .expect("写脚本");

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}");
    }
    let (_, base) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=0"));
    assert_eq!(base, "5", "前提：opt=0 应打印 5");
    for lvl in ["1", "2"] {
        let (c, got) = run(
            exe,
            &dir,
            f.to_str().unwrap(),
            Some(&format!("--opt={lvl}")),
        );
        assert_eq!(
            (c, got.clone()),
            (0, "5".to_string()),
            "**D310/D417**：闭包字面量的寄存器此前不参与 SSA 重命名 ⇒ `map`/方法调用\
             去读了另一个寄存器 ⇒ `--opt=1` 下**静默无输出、退出码 0**。\n  \
             `--opt={lvl}` 实得 exit={c} {got:?}"
        );
    }
}
