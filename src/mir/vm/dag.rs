//! v0.75.61: DAG 超步执行器 — 原 vm.rs 的 DAG 区（BSP 超步模型，生产主路径）。
//! 自 vm.rs 拆出（D6 单文件惯例）。所有指令逻辑委托 handlers::dispatch；
//! 本层只控制执行顺序（拓扑 + BSP 超步）。线性区（run_mir）仍在 vm.rs。

use std::collections::HashMap;
use std::sync::Arc;

use super::{MirSignal, build_task_registry, run_main_task};

use crate::mir::host::MirHost;
use crate::mir::{MirFunction, MirInst};
use crate::value::{Environment, Value};

// v0.59: DAG-aware MIR interpreter（原 dag_interp.rs）
// ===================================================================
// v0.59 部分（原 dag_interp.rs 模块文档，已并入 vm.rs）：
// v0.59: DAG-aware MIR interpreter.
//
// Executes a `MirDag` using a BSP super-step model. All instruction
// logic is delegated to `handlers::dispatch()`. The DAG layer only
// controls execution ORDER: topological + BSP super-steps.
//
// v0.104.6 复核：本段原文是
//   「With `dag.add_sequential_edges()`, this degenerates to linear
//     execution, making `run_mir ≡ run_dag`.」
// **该表述为假** —— 线性化后语义会变：实测 9 例中 3 例与生产路径发散
// （if/else、for+break、while+continue），且**错的总是线性化那一侧**。
// 机制是线性链让 `break` / `continue` 这类「跳出链」的控制转移失效。
// 证据：`tests/run_mir_equiv_run_dag.rs`（3 条 `#[ignore]` 记录分歧）、
// 哪边对的独立判据见 `tests/continue_semantics.rs`。
// 详见 `MirDag::add_sequential_edges` 的文档 —— 那里是这条错误表述的源头。
//
// # 执行边界（v0.75.33，v0.75.36 修正）
//
// 本解释器为 **pregel 顶点执行 + 生产主路径** 双用途：pregel BSP 引擎
// 逐超步调用，同时 `run_mir`（main.rs/REPL/import）经 `run_dag_with_signal`
// 也走本解释器——**生产路径全部经过 DAG 解释器，不存在「循环走线性
// fallback」**。
// - 无循环的直线/分支程序：正确（Sequence 前驱判定保证 Define/Var 顺序）。
// - 含 `MirInst` 循环（for/while 降级到 JumpIf 回边）的程序：v0.75.34 起
//   正确（块内全序 + 控制转移 handler 决定 + wave 去重），循环累加验证
//   输出 6/45。回归保护：`tests/tier0_replacement.rs`、`orchestrate_v3_pipeline.rs`。
// - 优化器（CSE/DeadNode/ConstFolding）删除/合并节点时不得破坏控制目标
//   与寄存器消费者（dag_rule/dag_search 的 guard + reg_rename 负责）。

use crate::mir::dag::{EdgeKind, MirDag, MirDagNode};
use crate::mir::handlers::{self, Flow};

/// v0.75.10: 寄存器级增量执行器状态（跨调用/超步记忆化）。
///
/// 只对「可证明纯计算」节点记忆化（白名单，见 `is_memoizable_pure`）：
/// 当节点的输入寄存器值与上次执行相等时跳过执行、复用上次输出。纯节点的
/// 输出完全由输入决定（零 env 读取、零副作用），因此跳过不改变任何可观察
/// 语义。副作用 / env 读取节点（Var/Call/Prompt/Send/...）永远重跑 —
/// 保守白名单保证增量安全。
///
/// 正确性关键：记忆按「输入值」判断，而非按超步号 — 即使 fault-retry
/// 回滚了引擎状态，被记录的输入由重跑的 Var 节点重建，与记录时相等 →
/// 跳过仍然正确（输入决定输出）。
pub struct DagExecMemo {
    /// node_id → 上次执行的输入指纹（相等性判断依据，v0.103 起为
    /// [`InputFp`] 而非深拷贝 `Vec<Value>` —— 见 InputFp 文档）。
    last_inputs: HashMap<usize, Vec<InputFp>>,
    /// node_id → 上次输出（跳过时复用）
    last_outputs: HashMap<usize, Value>,
    /// 记忆化跳过的节点执行次数（stats 可观测性）
    pub skipped_nodes: usize,
    /// 实际执行的节点次数（stats 可观测性）
    pub executed_nodes: usize,
}

/// v0.103: 输入值的**指纹** —— 记忆化比较键。
///
/// **缺陷背景**：此前 `DagExecMemo` 直接存 `Vec<Value>` 深拷贝并逐元素
/// 深比较。循环 `for x in xs` 中 `Index(xs, i)` 节点的输入含整个 `xs`
/// 列表：每轮 O(|xs|) 克隆 + O(|xs|) 比较 → 循环 |xs| 轮即 **O(|xs|²)**
/// （实测 8000 元素 5.3s、16000 元素 26s，而 user CPU 时间近零 —— 开销全
/// 在分配/比较而非计算）。
///
/// 指纹取值的**廉价位宽**而非全部内容：
/// - 堆值（List/Dict/Cons/Closure/...）用 **Arc/指针地址**（列表在循环中
///   不可变，同一列表即同一地址 → 稳定且 O(1)）；
/// - 标量（Int/Float/Bool/Nil/Char）用自身值；
/// - String/BigInt 用长度 + 前缀若干字节的哈希（避免长字符串 O(|s|) 比较）。
///
/// 指纹碰撞会导致**误跳过**（把不同输入当相同）—— 故对标量取精确值、
/// 对堆值取身份（地址唯一），仅对长字符串/大整数用截断哈希并在其后附
/// 长度，使碰撞概率在实际使用中可忽略；`reuse` 命中后仍会返回上次输出，
/// 而 memo 的语义前提是「纯节点 + 输入决定输出」，输入身份相同即输出相同。
#[derive(PartialEq, Eq)]
enum InputFp {
    Nil,
    Int(i64),
    Float(u64),
    Bool(bool),
    Char(u32),
    /// 字符串：长度 + 内容哈希（O(1) 于长度，`DefaultHasher` 对 &str 是 O(len)，
    /// 故只喂前 32 字节 + 长度 —— 长字符串下远快于全量深比较）。
    Str(usize, u64),
    /// 堆值身份（Arc 指针 / 列表地址）。
    Heap(usize),
}

fn value_fp(v: &Value) -> InputFp {
    use std::hash::{Hash, Hasher};
    match v {
        Value::Nil => InputFp::Nil,
        Value::Int(i) => InputFp::Int(*i),
        Value::Float(f) => InputFp::Float(f.to_bits()),
        Value::Bool(b) => InputFp::Bool(*b),
        Value::Char(c) => InputFp::Char(*c as u32),
        Value::String(s) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            let head = &s.as_bytes()[..s.len().min(32)];
            head.hash(&mut h);
            InputFp::Str(s.len(), h.finish())
        }
        // 堆值：地址身份（列表/字典/闭包/关系等在循环中不可变，同值同址）
        Value::List(l) => InputFp::Heap(l.id()),
        Value::Dict(d) => {
            // v0.104.6 修复：此前是 `InputFp::Str(d.len(), 0)` —— **只记长度**。
            //
            // **缺陷（静默错值）**：`MirInst::Index(..)` 在 `is_memoizable_pure`
            // 白名单里，而 `d["a"]` 正好降级成 `Index`。循环里
            //
            // ```text
            // let n = 0
            // let s = 0
            // while n < 4
            //   let d = {a: n}      // 长度恒为 1，内容逐轮变
            //   let s = s + d["a"]
            //   let n = n + 1
            // end
            // s                       // 实测 0.0，应为 6.0（0+1+2+3）
            // ```
            //
            // 长度恒定 → 指纹每轮相同 → memo 命中 → `d["a"]` 永远返回**第一轮**
            // 的 0。同程序的 `d.get("a")` 走 `Call`（不在白名单）→ 正确得 6.0。
            // 两种写法语义相同、结果不同，即本缺陷的直接证据。
            //
            // 原注释「输入相同才跳过，长度不同必不跳过」的前提是「长度相同 ⇒
            // 输入相同」，对 dict 显然不成立。
            //
            // 修法：用**与顺序无关的内容哈希**（逐项哈希后异或，交换律保证
            // HashMap 迭代序不影响结果），长度混入高位。碰撞概率同 `Str`
            // 分支（2^-64 量级），可忽略。
            //
            // 代价：dict 指纹从 O(1) 变 O(k)（k = 键数）。但 dict 作为纯节点
            // 输入极罕见（这正是原注释的判断），正确性优先于这点常数。
            let mut h = std::collections::hash_map::DefaultHasher::new();
            d.len().hash(&mut h);
            for (k, v) in d {
                k.hash(&mut h);
                let mut item = std::collections::hash_map::DefaultHasher::new();
                std::mem::discriminant(&value_fp(v)).hash(&mut item);
                match value_fp(v) {
                    InputFp::Nil => 0u64.hash(&mut item),
                    InputFp::Int(i) => i.hash(&mut item),
                    InputFp::Float(b) => b.hash(&mut item),
                    InputFp::Bool(b) => b.hash(&mut item),
                    InputFp::Char(c) => c.hash(&mut item),
                    InputFp::Str(l, hsh) => {
                        l.hash(&mut item);
                        hsh.hash(&mut item);
                    }
                    InputFp::Heap(p) => p.hash(&mut item),
                }
                // 异或 → 与 HashMap 的迭代顺序无关
                h.write_u64(item.finish());
            }
            InputFp::Str(d.len(), h.finish())
        }
        Value::Closure { .. } | Value::Task { .. } | Value::Macro { .. } => {
            InputFp::Heap(v as *const Value as usize)
        }
        _ => InputFp::Heap(v as *const Value as usize),
    }
}

impl DagExecMemo {
    pub fn new() -> Self {
        Self {
            last_inputs: HashMap::new(),
            last_outputs: HashMap::new(),
            skipped_nodes: 0,
            executed_nodes: 0,
        }
    }

    /// 输入指纹与上次相等则返回缓存输出（并计入 skipped），否则 None。
    ///
    /// v0.103: 比较**指纹**（O(1)/O(min(len,32))) 而非深拷贝值 —— 见
    /// [`InputFp`] 的缺陷背景。
    fn reuse(&mut self, node_id: usize, inputs: &[InputFp]) -> Option<Value> {
        if self.last_inputs.get(&node_id).map(Vec::as_slice) == Some(inputs) {
            self.skipped_nodes += 1;
            self.last_outputs.get(&node_id).cloned()
        } else {
            None
        }
    }

    /// 记录本次执行的输入指纹 + 输出，供下次比较/复用。
    fn record(&mut self, node_id: usize, inputs: Vec<InputFp>, output: Value) {
        self.last_inputs.insert(node_id, inputs);
        self.last_outputs.insert(node_id, output);
        self.executed_nodes += 1;
    }

    /// 是否积累了任何记忆（并行 RECONCILE 用于区分「跳过路径的空 memo」）。
    pub fn is_empty(&self) -> bool {
        self.last_inputs.is_empty()
    }
}

impl Default for DagExecMemo {
    fn default() -> Self {
        Self::new()
    }
}

/// 白名单：可证明纯计算的 MIR 指令（零 env 读取、零副作用、输出 = 输入函数）。
/// 对应 handlers::dispatch 中恒返回 `Flow::Continue` 的值指令。
/// 保守原则：不确定即排除（Var 读 env、Call/Pipe 可能副作用、Prompt 副作用、
/// MatchExpr 执行 arm body、Define/Assign 写 env、IndexAssign 就地修改 regs）。
fn is_memoizable_pure(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Const(..)
            | MirInst::BinaryOp(..)
            | MirInst::ListLit(..)
            | MirInst::DictLit(..)
            | MirInst::Index(..)
            | MirInst::Expr(..)
    )
}

/// 带记忆化的 `run_dag_with_signal` 变体。`memo` 跨调用保持（pregel 每超步
/// 传同一 agent 的 memo），输入未变的纯节点被跳过。传 `&mut DagExecMemo::new()`
/// 即退化为普通执行（零增量）。
pub fn run_dag_with_signal_memo(
    dag: &MirDag,
    func: &MirFunction,
    memo: &mut DagExecMemo,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(MirSignal, Value), String> {
    use MirSignal;
    // v0.104.6 D40：寄存器引用必须落在 `dag.n_regs` 内，否则下面
    // `node_ready` 的 `reg_ready[*r]` 索引越界 **panic**，把一个通过
    // 解析 + typeck 的合法程序打成 exit=101。实测触发路径：unit 语句
    // （`transaction`）返回未分配的哨兵寄存器 0，见
    // `parser_v3/emit.rs::emit_transaction_w`。
    //
    // 不改成「软失败」：`node_ready` 若对该寄存器返回 false，节点永不就绪，
    // 主循环会空转到 `MAX_STEPS`（1e7）后**静默**返回 Nil —— 静默错值比
    // panic 更难查。提前一次性校验并返回干净错误。
    //
    // 对合法程序零影响：能跑通的程序不可能引用越界寄存器（越界的读会在
    // `node_ready` 越界、越界的写会在 `regs[d]` 越界），故此检查不可能
    // 拒绝任何当前可运行的程序。开销为每节点一次比较，远低于主循环里
    // `node_ready` 每 wave 对同一批节点做的遍历。
    if let Some((node, reg, kind)) = first_out_of_range_reg(dag) {
        return Err(format!(
            "internal: instruction at DAG node {node} references register {reg} \
             ({kind}) but the function only has {} register(s) — \
             a unit-statement emitter returned an unallocated sentinel register",
            dag.n_regs
        ));
    }
    let task_registry = build_task_registry(&func.body);
    let mut regs: Vec<Value> = vec![Value::Nil; dag.n_regs];
    let mut reg_ready: Vec<bool> = vec![false; dag.n_regs];
    let mut active: Vec<usize> = dag.entry.clone();
    let mut exec_count: Vec<usize> = vec![0; dag.nodes.len()];
    // v0.75.33: 每节点是否已执行 — Sequence 前驱就绪判定用（见 ready 过滤）。
    let mut executed: Vec<bool> = vec![false; dag.nodes.len()];

    // v0.103 修复：**去除 ready 过滤器里的 `exec_count < MAX` 静默剔除**
    // （其后果是循环超过 500 次时 DAG 提前正常返回，循环之后的语句整体
    // 蒸发 —— 无错误、无输出）。这是 DAG 执行器唯一真正的语义缺陷。
    //
    // 不设上限报错：循环是用户意图，「运行很久」本身不是错。无限循环由
    // 用户通过 Ctrl+C 中断。`ready` 不再隐式剔除节点 → 长循环正常收尾
    // → 循环之后语句正常执行。
    const MAX_STEPS: u32 = 10_000_000;
    let mut step = 0;
    let mut result: Value = Value::Nil;
    let mut signal: MirSignal = MirSignal::None;

    // ── 循环不变量：以下三张表只依赖 `dag`，与 wave 无关 ──────────────
    // v0.104.6 性能修复：此前 `seq_preds` 在 while 体内**每 wave 重建**
    // （`vec![Vec::new(); nodes]` + 扫全量 edges），而它自身注释写的正是
    // 「一次构建」—— 代码与注释矛盾。实测一个 while 循环体要跑多个 wave，
    // 每 wave 数十次 Vec 分配 + O(E) 扫描，构成解释器每次迭代的主要开销
    // （实测 ~24 µs/迭代，其中 env 操作仅 ~0.4 µs）。
    //
    // 1) Sequence 前驱索引 —— 供就绪判定 O(1) 查找。
    //
    // v0.104.6：只收录**可达**的前驱。不可达的死块前驱必须排除：就绪门槛是
    // `seq_preds[n].iter().all(|&p| executed[p])`，而不可达节点永不执行 ——
    // 若把死块里的前驱算进去，被它挡住的节点永远 not-ready、其后整条尾部
    // 静默消失（无报错、退出码 0）。实例：常量条件被 `IfSimplifyRule` 折叠掉
    // `JumpIfNot` 后 else 臂成死块，尾语句的 Sequence 前驱恰在该死块内。
    //
    // v0.104.6 E1（**已修**）：控制流**汇合点**上、来自未被选中分支臂的 Sequence
    // 前驱造成的饿死 —— 症状 `let c = 1 / let x = if c == 1 then 5 else 7 end /
    // print(x + 1)` 里 `print(x + 1)` **整句不执行**（无报错、退出码 0）；
    // `c = 2` 走 else 时反而正常（两条路径不对称）。
    //
    // **根因不在执行器，在 `dag_analyze` 的分块**（修在 `src/mir/dag.rs`，
    // 执行器一行未改）。`partition_blocks` 此前只按「Label」与「终结符之后」
    // 切块，而 `lower` / `fcfg_lower` 的控制转移用**裸 pc 数字**做目标
    // （不插 Label）—— 于是 **if/else 的汇合点落进 else 臂那一块内部**。
    // 两处「块内顺序」机制随之越界：
    //   * Step 1 的块内 Sequence 链从 else 臂末尾**直连**汇合点；
    //   * Step 3 的 `last_effect` 扇出（Effect 之后的每个节点各连一条
    //     Sequence）越过汇合点**继续**连向尾部。
    //
    // 实测 dump（`let c = 1 / let x = 0 / if c == 1 then x = 5 else x = 7 end /
    // print(x + 1)`）：汇合点 node 21 (`Var "x"`) 的
    // `seq_preds = [20, 19]`，而 19/20 全在 **else 臂**内
    // （18=`Const 7` / 19=`Assign x` / 20=`Copy`）。`c == 1` 成立时 else 臂不执行
    // → `executed[19]`、`executed[20]` 恒 false → 汇合点永不就绪；
    // 22/23/24 又各自被 `19→22/23/24` 扇出边一并挡住，**整条尾部蒸发**。
    //
    // **修法**：① `partition_blocks` 把裸 pc 跳转目标也当块首；② `dag_analyze`
    // 对「前块末尾 → 下一块首」补一条 `Control`（而非 `Sequence`）的 fall-through
    // 边。激活语义不变（`should_push = is_control_edge || Sequence`），但跨块
    // 边不再进入 `seq_preds` 门槛 —— 就绪门槛重新只在**块内**成立，而「同块内
    // 必然先执行」是真正的不变量。
    //
    // **循环体的承重门控一字未动**（这是前五次尝试全部翻车的地方）：
    // `Jump(header)` 让 header 成为块首、`header-1 → header` 由 Sequence 改成
    // Control，但**循环体自己仍是一整块**，其
    // `seq_preds[body] = [前驱, block_entry]` 原样保留 —— 那正是让
    // `Index(list, i)` 不早于 `i = i + 1` 的约束（放松它会得到
    // `run_mir: index 2 out of bounds (len 2)`）。
    //
    // **四次失败的判据为何全都不对**（保留记录：它们都不是「差一点」）：
    //   1. 「同 Sequence 分量」—— 判据本身是同义反复：把 Sequence 连通分量
    //      称作「块」，那么「汇合点是否在同一块」永远为真。
    //   2. 「分量单入口才门控」—— 太粗，多入口分量被整体关掉门控。
    //   3. **全图支配**—— 结论本身**是错的**，此前记为「支配在有环图上弱于
    //      执行顺序」。真实机制：`Step 3` 的 `last_effect` 扇出在块内造出
    //      `9→10/11/12/13` 这类**捷径**，直接**破坏支配**（`Dom(12)` 少了 11）。
    //      即：不是「支配不足以表达循环体顺序」，是**图本身被污染**。修好分块
    //      后，块内支配与执行顺序重新一致，支配即精确判据。
    //   4. **分量内必现路径**（无 SCC）—— 环无外部入口 → `must` 全空。
    //   5. **+ Tarjan SCC 入口识别**—— 入口判据写错：直线代码里每节点自成一个
    //      SCC，「有来自不同 SCC 的入边」使**每个**节点都成入口。
    //
    // 共同的错处：都在**执行器**里找判据，而缺陷在**建图**。一旦分块正确，
    // 「Sequence = 块内全序」重新成立，现有谓词无需改动。
    let mut seq_preds: Vec<Vec<usize>> = vec![Vec::new(); dag.nodes.len()];
    for e in &dag.edges {
        if matches!(e.kind, crate::mir::dag::EdgeKind::Sequence)
            && dag.reachable.get(e.from).copied().unwrap_or(true)
        {
            seq_preds[e.to].push(e.from);
        }
    }
    // 2) 控制边出边邻接表 —— 供「本 wave 无就绪节点」时的前沿推进
    //    从 O(active × E) 全边扫描降为 O(active × 出度)。
    let mut control_out: Vec<Vec<usize>> = vec![Vec::new(); dag.nodes.len()];
    for e in &dag.edges {
        if is_control_edge(&e.kind) {
            control_out[e.from].push(e.to);
        }
    }
    // 3) `pushed` 的代际标记 —— 用递增代号代替每 wave `vec![false; nodes]`
    //    分配与清零。`pushed[n]` 语义为 `pushed_gen[n] == cur_gen`。
    let mut pushed_gen: Vec<u32> = vec![0; dag.nodes.len()];
    let mut cur_gen: u32 = 0;
    // 4) `ready` 成员标记 —— 替代边传播里 `ready.contains(&edge.from)`
    //    的线性查找（此前是 O(E × |ready|) 的每 wave 二次扫描）。
    let mut ready_gen: Vec<u32> = vec![0; dag.nodes.len()];

    while !active.is_empty() && step < MAX_STEPS {
        step += 1;
        cur_gen += 1;

        let ready: Vec<usize> = active
            .iter()
            .filter(|&&n| {
                node_ready(&dag.nodes[n], &reg_ready)
                    // v0.75.33: Sequence 前驱必须已执行 — 仅 data-ready 不够：
                    // 无输入寄存器的节点（Var/Define 等）一激活即可执行，若其
                    // Sequence 前驱（如 Define 语句）仍在本波未执行，会提前
                    // 读脏值。示例：`let c = 5` 的 Define(c) 与下一句
                    // `let d = c + 1` 的 Var(c) 同波就绪 → Var(c) 先跑读 Nil。
                    //
                    // v0.103 修复：**不再在此过滤 exec_count**（那是静默
                    // 剔除的根源，详见上方注释）。超限错误在执行处显式抛出。
                    && seq_preds[n].iter().all(|&p| executed[p])
            })
            .copied()
            .collect();

        if ready.is_empty() {
            let mut next: Vec<usize> = Vec::new();
            for &n in &active {
                // v0.104.6：用循环外的 `control_out` 邻接表替代全量 edges
                // 扫描（O(active × E) → O(active × 出度)）。
                for &t in &control_out[n] {
                    next.push(t);
                }
            }
            active = next;
            continue;
        }

        let mut next_active: Vec<usize> = Vec::new();
        let mut saw_return = false;

        // v0.75.33: 统一去重 — 本 wave 已执行的节点（ready）不再被重调度；
        // Branch/Jump handler 的 push 与 scan 的 push 共用同一 pushed 标记，
        // 防止同 wave 重复执行（此前 scan 会把 25→26 的 Sequence 边把已执行的
        // n26 重新推入 → body 链每轮重复激活、归纳变量漂移 → 越界）。
        //
        // v0.104.6 性能修复：`pushed` 由「每 wave 新建 `vec![false; nodes]`」
        // 改为循环外的代际标记 `pushed_gen[n] == cur_gen`。语义等价
        // （每 wave 以新代号表示全新 false 集），消除每 wave 一次
        // O(nodes) 分配与清零。下方所有 `pushed[x]` 读、`pushed[x] = true`
        // 写分别改为读 / 写代号。
        for &n in &ready {
            pushed_gen[n] = cur_gen;
            ready_gen[n] = cur_gen;
        }

        for &node_id in &ready {
            exec_count[node_id] += 1;
            executed[node_id] = true;

            match &dag.nodes[node_id] {
                MirDagNode::Compute { inst, .. } | MirDagNode::Effect { inst } => {
                    // v0.75.10: 纯节点输入与上次相等 → 跳过执行，复用输出。
                    // Compute 的 input_regs 存于节点；Effect 无该字段，
                    // 从 inst.input_regs() 推导（同一输入集合）。
                    let pure = is_memoizable_pure(inst);
                    let inputs: Vec<InputFp> = if pure {
                        match &dag.nodes[node_id] {
                            MirDagNode::Compute { input_regs, .. } => {
                                input_regs.iter().map(|r| value_fp(&regs[*r])).collect()
                            }
                            MirDagNode::Effect { inst } => inst
                                .input_regs()
                                .iter()
                                .map(|r| value_fp(&regs[*r]))
                                .collect(),
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    if pure && let Some(cached) = memo.reuse(node_id, &inputs) {
                        if let Some(d) = inst.dst() {
                            regs[d] = cached;
                            reg_ready[d] = true;
                            result = regs[d].clone();
                        }
                        // 纯节点恒 Flow::Continue — 无控制流副作用可跳过。
                        continue;
                    }

                    let flow =
                        handlers::dispatch(inst, &mut regs, interp, env, &task_registry, effects)?;
                    if pure {
                        if let Some(d) = inst.dst() {
                            memo.record(node_id, inputs, regs[d].clone());
                        } else {
                            memo.record(node_id, inputs, Value::Nil);
                        }
                    }
                    // v0.104.6 D35：用 `written_reg()` 而非 `dst()` —— `Handle`
                    // 被 `is_effect()` 归为 Effect 节点（`dst()` 够不着它），
                    // 但 `h_handle` 确实写 `regs[k_dst] = v`。漏掉这一步会让
                    // `reg_ready[k_dst]` 永远 false →
                    // `let r = handle …` 的 `Define` 永不激活 →
                    // 它所在的 Sequence 链断裂 → 其后所有 Effect 节点
                    // （含 `print`）静默饿死，退出码 0。
                    if let Some(d) = inst.written_reg() {
                        reg_ready[d] = true;
                        result = regs[d].clone();
                    }
                    match flow {
                        Flow::Return(v) => {
                            signal = MirSignal::Return(v.clone());
                            result = v;
                            saw_return = true;
                        }
                        Flow::Continue => {}
                        Flow::Jump(_) => {}
                        Flow::Halt(v) => {
                            signal = MirSignal::Halt(v.clone());
                            result = v.unwrap_or(Value::Nil);
                            saw_return = true;
                        }
                    }
                }
                MirDagNode::Branch {
                    cond,
                    true_target,
                    false_target,
                } => {
                    let chosen = if crate::flow::is_truthy(&regs[*cond]) {
                        true_target
                    } else {
                        false_target
                    };
                    if let Some(t) = chosen
                        && pushed_gen[*t] != cur_gen
                    {
                        pushed_gen[*t] = cur_gen;
                        next_active.push(*t);
                    }
                }
                MirDagNode::Jump { target } => {
                    if let Some(t) = target
                        && pushed_gen[*t] != cur_gen
                    {
                        pushed_gen[*t] = cur_gen;
                        next_active.push(*t);
                    }
                }
                MirDagNode::Label { .. } | MirDagNode::Phi { .. } | MirDagNode::Removed => {}
            }
        }

        if saw_return {
            break;
        }

        // 边传播：只调度本 wave 已执行节点的消费者（Branch/Jump 的转移
        // 已由 handler 决定，见下）。`pushed` 在 wave 开头创建并标记了
        // ready 节点，scan 不会把已执行/已调度的节点重复推入。
        for edge in &dag.edges {
            // v0.104.6 性能修复：`ready.contains(&edge.from)` 是对 Vec 的
            // 线性查找，使边传播退化为每 wave O(E × |ready|) 的二次扫描。
            // 改用本 wave 已填好的 `ready_gen` 代际标记做 O(1) 成员判定。
            if ready_gen[edge.from] == cur_gen {
                // v0.75.33: 分支/Jump 节点的控制转移完全由 handler 决定
                // （Branch 只推选中的 target、Jump 只推 target）。此处若再
                // 无条件推送其出边，会把两个分支目标都激活 — exit 与 body
                // 同 wave 竞态执行（after-loop 读脏值、body 用越界 i 再跑）。
                // 示例：for 循环 i==len 时 exit 被推 27、body 同时被
                // ControlIfFalse/Sequence 推 19 → Index 越界 OOB。
                if matches!(
                    dag.nodes[edge.from],
                    MirDagNode::Branch { .. } | MirDagNode::Jump { .. }
                ) {
                    continue;
                }
                // v0.103: **Data 边只决定「就绪」，不决定「激活」**。
                //
                // 激活（把节点放进下一波的 active）只由控制边与 Sequence 边
                // 决定 —— 它们编码「控制流是否到达」。Data 边编码「输入值是否
                // 可用」，只供 `node_ready` 判断，不构成可达性。
                //
                // **缺陷背景（`while ... if ... break ... end` 死循环）**：
                // 此前 `EdgeKind::Data { reg } => reg_ready[*reg]` 会在「寄存器
                // 曾被写过」时立刻激活消费者 —— 无视控制流是否到达。循环体内
                // 的递增语句 `t = t + 1` 因此被「常量 1」的 Data 边提前激活，
                // 与内层 `break` 分支同波执行；递增把循环变量推回去、回边再次
                // 点火 → `break` 被架空、挂死。
                //
                // 结构上 Sequence 边足以保序：`dag_analyze` 给每个基本块内部
                // 的相邻节点连 Sequence（块内全序），块入口由控制边（Branch/
                // Jump 的选中目标与 fall-through）激活，`prune_sequence_edges`
                // 又明确「Sequence 全保留」。因此去掉 Data 激活不会让任何
                // 可达节点失去激活来源。
                let should_push =
                    is_control_edge(&edge.kind) || matches!(edge.kind, EdgeKind::Sequence);
                if should_push && pushed_gen[edge.to] != cur_gen {
                    next_active.push(edge.to);
                    pushed_gen[edge.to] = cur_gen;
                }
            }
        }
        // v0.103: 已激活但本波未就绪的节点保留到下波 —— 「控制已到达、输入
        // 尚未就绪」的节点若被丢弃将永不执行（此前靠 Data 激活的重复推送
        // 掩盖了这一点）。`pushed[n]` 为假即「在 active 中但未就绪/未执行」。
        for &n in &active {
            if pushed_gen[n] != cur_gen {
                next_active.push(n);
                pushed_gen[n] = cur_gen;
            }
        }
        active = next_active;
    }

    Ok((signal, result))
}

/// 「顶层 DAG + main task」组合执行入口。
///
/// v0.104.6：**改用 `MirHost::dag_cache()` 取 DAG**，即
/// `dag_analyze → dag_optimize → prune_sequence_edges`，与生产路径
/// （`run_mir` + `run_main_task`，见 `main.rs::run_file`）**同链**。
///
/// 动机：本函数此前只在 `tests/dag_integration.rs` 用，且直调 `dag_analyze`
/// **绕过 `dag_optimize`**，于是该文件测的是**未优化**的图 —— 优化阶段的语义
/// 缺陷（CSE 跨互斥控制区域合并、追加节点可达性、Removed 前驱阻塞就绪门槛）
/// 在它上面**根本无法暴露**。
///
/// 曾两次因「Removed 前驱阻塞就绪门槛」而回退（见本文件 `seq_preds` 注释 b
/// 项）。该问题已定位并修复：根因是 `dag.reachable` 从 pc 0 出发，而优化会
/// 改写入口 —— 不可达判据必须是「**能否被当前入口激活**」，已由
/// `MirDag::recompute_reachable_from_entry` + `apply_rewrite` 中的不动点迭代
/// 落实。回归测试：`tests/run_mir_equiv_run_dag.rs::optimize_preserves_handwritten_mir_chain`。
///
/// 注：`run_main_task` 内部 `let _ = run_mir(...)`，**丢弃 main task 的返回值**
/// （`vm.rs`），故本函数返回的始终是**顶层 body 的值**；对纯 `task main()`
/// 程序该值为 `Nil`。生产路径同样丢弃两者返回值、只检查错误，故两者在返回值
/// 语义上一致。
pub fn run_mir_dag(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<Value, String> {
    let dag = interp.dag_cache().get_or_build(func);
    let val = run_dag(&dag, func, interp, env, effects)?;
    if func
        .body
        .iter()
        .any(|i| matches!(i, MirInst::TaskDef { name, params, .. } if name == "main" && params.is_empty()))
    {
        run_main_task(func, interp, env, effects)?;
    }
    Ok(val)
}

pub fn run_dag(
    dag: &MirDag,
    func: &MirFunction,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<Value, String> {
    Ok(run_dag_with_signal(dag, func, interp, env, effects)?.1)
}

/// v0.75: `run_dag` 的信号感知变体。
///
/// 返回 `(MirSignal, Value)`。此前 `run_mir_with_signal` 无条件包装成
/// `MirSignal::Return`，导致 `Flow::Halt`（vote_to_halt）信号被丢弃、
/// 引擎永远无法将顶点置为 Halted。此变体真正传播 Return/Halt 信号。
///
/// v0.75.10: 委托给 [`run_dag_with_signal_memo`]（每次新 memo = 无增量，
/// 语义与旧实现完全一致）。需要跨调用增量的调用方（pregel）用 memo 变体。
pub fn run_dag_with_signal(
    dag: &MirDag,
    func: &MirFunction,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(MirSignal, Value), String> {
    run_dag_with_signal_memo(dag, func, &mut DagExecMemo::new(), interp, env, effects)
}

fn node_ready(node: &MirDagNode, reg_ready: &[bool]) -> bool {
    match node {
        MirDagNode::Compute { input_regs, .. } => input_regs.iter().all(|r| reg_ready[*r]),
        MirDagNode::Branch { cond, .. } => reg_ready[*cond],
        MirDagNode::Effect { inst } => inst.input_regs().iter().all(|r| reg_ready[*r]),
        _ => true,
    }
}

/// v0.104.6 D40：找第一个落在 `dag.n_regs` 之外的寄存器引用。
///
/// 返回 `(节点下标, 寄存器号, 引用种类)`。`node_ready`（读）与执行器的
/// `regs[d]`（写）都直接按下标索引，故**读与写都要查**。
///
/// 顺序与 `node_ready` 一致地覆盖三类会按下标索引的节点；其余节点
/// （Label / Jump / Return 等）不索引寄存器，无需检查。
fn first_out_of_range_reg(dag: &MirDag) -> Option<(usize, usize, &'static str)> {
    for (i, node) in dag.nodes.iter().enumerate() {
        let hit = match node {
            MirDagNode::Compute {
                input_regs, dst, ..
            } => input_regs
                .iter()
                .chain(std::iter::once(dst))
                .find(|r| **r >= dag.n_regs)
                .map(|r| (*r, "read/write")),
            MirDagNode::Branch { cond, .. } => (*cond >= dag.n_regs).then_some((*cond, "read")),
            MirDagNode::Effect { inst } => {
                let inputs = inst.input_regs();
                inputs
                    .iter()
                    .copied()
                    .chain(inst.written_reg())
                    .find(|r| *r >= dag.n_regs)
                    .map(|r| (r, "read/write"))
            }
            _ => None,
        };
        if let Some((reg, kind)) = hit {
            return Some((i, reg, kind));
        }
    }
    None
}

fn is_control_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Control | EdgeKind::ControlIfTrue | EdgeKind::ControlIfFalse | EdgeKind::BackEdge
    )
}
