//! v0.104.6 D268：同一超步内向**多个** target 投递消息时，payload 互相覆盖
//!
//! ## 状态：**未修**（库 API 潜伏缺陷，语言层不可达）—— 本文件是**现状判据**
//!
//! 本文件记录的是**当前的错误行为**。它今天通过，恰恰是因为缺陷还在。
//!
//! ## 机制
//!
//! `MirPregelEngine::run` 的 ADVANCE 段：
//!
//! ```text
//! for (target, messages) in by_target {
//!     …
//!     self.channels.insert("input".to_string(), final_value);   // ← 全局唯一槽
//!     *self.channel_versions.entry("input").or_insert(0) += 1;
//!     next_active.insert(target);
//! }
//! ```
//!
//! 投递槽只有一个、且是**引擎全局**的 `input` 通道。循环按 target 逐个写入，
//! 于是**后写覆盖先写** —— 被覆盖的那条消息不是「延后投递」，而是**彻底消失**。
//! 而 `by_target` 是 `HashMap`，遍历顺序不确定 ⇒ **哪一条被吃掉是随机的**。
//!
//! 消费端 `build_node_input` / `inject_channel_inputs` 从同一个 `channels`
//! 读 `input`，因此**所有**本超步被激活的 target 读到的都是最后写入的那一个。
//!
//! ## 实测
//!
//! `a` 在同一超步向 `t1` 发 11、向 `t2` 发 22，40 次运行后 `input` 通道：
//!
//! | 值 | 次数 |
//! |---|---|
//! | `Int(22)` | 20 |
//! | `Int(11)` | 20 |
//!
//! **恰好一半一半，且 11 一次都没能「两个都活」**。
//!
//! 交叉投递（`t1` 自己发 111、同时给 `t2` 发 222），在 `t1` 再次激活、
//! `input` 对它可见的那一超步：
//!
//! ```text
//! max_steps=4 → result = "{\"input\":222,\"result\":\"{}\"}"   ← t1 读到了 222
//! max_steps=5 → result = "{\"input\":111,...}"                 ← 这次读到 111
//! ```
//!
//! 即 **`t1` 收到了本该发给 `t2` 的消息**，且读到哪一个逐次不同。
//!
//! ## 为什么标「未修」而不是直接修
//!
//! ① **语言层不可达**：全仓 `MirInst::Send` **只在 `src/pregel/mod.rs`
//!    自己的 `#[cfg(test)]` 里被构造** —— parser / lower / builtin / optimizer
//!    一处都没有。真实 CLI 实测 `send("b", 1)` 报
//!    `Runtime error (MIR): Undefined function or task: send`、exit 1。
//!    所以整条 `h_send` → `Effects::Send` → `SendTask` → `pending_sends`
//!    → ADVANCE 链路在生产路径上是死的，只有本文件的库级调用能碰到。
//!
//! ② **修法涉及引擎状态模型**：单槽 `input` 装不下两条消息。正确修法
//!    至少要三选一，而每一种都改变可观察行为：
//!    - per-node 待投递表（要改 `versions_seen` / `build_checkpoint` 格式）；
//!    - per-target 通道（要重新定义通道命名契约）；
//!    - 多 target 时**报错**（把静默错值变成响亮失败，代价最小，
//!      但会让当前「碰巧能跑」的图开始失败）。
//!
//!    属产品策略决定，不擅自实施。
//!
//! ## 本文件何时该改写
//!
//! 一旦 `send` 被接进语言层、或上述任一修法落地，**本测试会开始失败**
//! （因为「只有一个 payload 幸存」不再成立）。届时应把它换成隔离性判据：
//! **每个 target 读到的 payload 必须等于发给它的那一个**。

use mora::common::{Literal, Span};
use mora::interpreter::Interpreter;
use mora::mir::orchestrate::{MirAgentDef, MirEdgeDef, MirPregelConfig};
use mora::mir::witness::{MirWitness, WitnessKind};
use mora::mir::{MirFunction, MirInst};
use mora::pregel::MirPregelEngine;
use mora::value::Value;
use std::collections::HashMap;

fn nil_witness() -> MirWitness {
    let span = Span::new(1, 1);
    MirWitness {
        kind: WitnessKind::Literal(Literal::Nil(span)),
        span,
    }
}

fn agent(name: &str, body: MirFunction) -> MirAgentDef {
    MirAgentDef {
        name: name.to_string(),
        task_expr: nil_witness(),
        verify_expr: None,
        with_config: None,
        task_body: body,
        combiner_body: None,
    }
}

fn edge(from: &str, to: &str) -> MirEdgeDef {
    MirEdgeDef {
        from: from.to_string(),
        to: to.to_string(),
        condition_expr: None,
        condition_body: None,
    }
}

fn const_agent(name: &str, v: i64) -> MirAgentDef {
    agent(
        name,
        MirFunction {
            params: Vec::new(),
            body: vec![MirInst::Const(0, Value::Int(v)), MirInst::Return(Some(0))],
            n_regs: 1,
            ..Default::default()
        },
    )
}

/// 造一个 `a` 在同一超步向 `t1`/`t2` 各发一条不同消息的图。
fn fanout_config(p1: i64, p2: i64) -> MirPregelConfig {
    let sender = agent(
        "a",
        MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(p1)),
                MirInst::Const(1, Value::Int(p2)),
                MirInst::Send {
                    value: 0,
                    target: "t1".into(),
                },
                MirInst::Send {
                    value: 1,
                    target: "t2".into(),
                },
                MirInst::Const(2, Value::Nil),
                MirInst::Return(Some(2)),
            ],
            n_regs: 3,
            ..Default::default()
        },
    );
    MirPregelConfig {
        agents: vec![sender, const_agent("t1", 0), const_agent("t2", 0)],
        edges: vec![edge("@start", "a")],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: HashMap::new(),
        aggregators: Vec::new(),
        master_compute: None,
    }
}

/// **D268-A 现状断言**：N 个 target ⇒ `input` 槽被写了 N 次，却只存得下 1 个值。
///
/// 断言刻意避开「哪一条幸存」—— 那是 `HashMap` 遍历顺序决定的、逐次不同的
/// （实测 40 次恰好 20/20，见文件头）。这里锁定的是与顺序**无关**的那一半：
/// 写入次数（`channel_versions`，逐 target 自增）与槽位容量（恒为 1）之间的落差。
///
/// 若本测试失败 ⇒ 多目标投递已被修好，请按文件头的说明改写为隔离性判据。
#[test]
fn d268a_multi_target_fanout_writes_one_slot_n_times() {
    let mut engine = MirPregelEngine::new(fanout_config(11, 22));
    let mut interp = Interpreter::new();
    engine.run(&mut interp).unwrap();
    let cp = engine.build_checkpoint();

    let writes = *cp
        .channel_versions
        .get("input")
        .expect("input 通道应有版本号");
    assert_eq!(
        writes, 2,
        "两个 target 各让 `input` 版本自增一次 ⇒ ADVANCE 对这个单槽写了 2 次"
    );
    let stored = cp.channel_values.get("input").cloned();
    assert!(
        matches!(stored, Some(Value::Int(11)) | Some(Value::Int(22))),
        "槽里存下的必然是两条消息之一，实际：{:?}",
        stored
    );
    // 2 次写入、1 个槽 ⇒ 至少一条消息被销毁。这条不依赖遍历顺序。
    assert!(writes > 1, "写入次数超过槽位容量 ⇒ 必然有消息被覆盖销毁");
}

/// 对照组：**单** target 时 `input` 槽正确收到那一条。
///
/// 这是上面那条断言的对照组 —— 证明覆盖问题只在多 target 时出现，
/// 而不是 `input` 通道本身坏了。
#[test]
fn d268a_single_target_delivery_is_correct() {
    let sender = agent(
        "a",
        MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(11)),
                MirInst::Send {
                    value: 0,
                    target: "t1".into(),
                },
                MirInst::Const(1, Value::Nil),
                MirInst::Return(Some(1)),
            ],
            n_regs: 2,
            ..Default::default()
        },
    );
    let cfg = MirPregelConfig {
        agents: vec![sender, const_agent("t1", 0)],
        edges: vec![edge("@start", "a")],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: HashMap::new(),
        aggregators: Vec::new(),
        master_compute: None,
    };
    for _ in 0..10 {
        let mut engine = MirPregelEngine::new(cfg.clone_shallow());
        let mut interp = Interpreter::new();
        engine.run(&mut interp).unwrap();
        let cp = engine.build_checkpoint();
        assert_eq!(
            cp.channel_values.get("input"),
            Some(&Value::Int(11)),
            "单 target 投递必须完整无损（对照组）"
        );
    }
}

/// `MirPregelConfig` 未实现 `Clone`（`MirAgentDef` 内部含 `MirFunction`，
/// 仓库刻意没给它派生 Clone）。测试里只需要一个等价的重建。
trait CloneShallow {
    fn clone_shallow(&self) -> MirPregelConfig;
}
impl CloneShallow for MirPregelConfig {
    fn clone_shallow(&self) -> MirPregelConfig {
        MirPregelConfig {
            agents: self
                .agents
                .iter()
                .map(|a| {
                    agent(
                        &a.name,
                        MirFunction {
                            params: a.task_body.params.clone(),
                            body: a.task_body.body.clone(),
                            n_regs: a.task_body.n_regs,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            edges: self.edges.clone(),
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        }
    }
}
