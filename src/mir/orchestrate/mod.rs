//! v0.91: Orchestrate runtime metadata — MirOrchestrateKind + companion types.
//!
//! 这些类型原本位于 `mir/expr/mod.rs`，是编排器（Sequential/Loop/Graph/Pregel/MoA/MoE）
//! 的**运行时元数据结构**，不属于表达式树节点。迁移至独立模块，消除 Expression 层
//! 与 Kernel 层（handlers）之间的跨层耦合。
//!
//! 依赖：
//!   - `MirWitness` — 从 `crate::mir::witness` 引用（v0.92: 字段类型已迁移）
//!   - `MirFunction` — 从 `crate::mir` 直接引用

use crate::mir::MirFunction;
use std::collections::HashMap;

use crate::value::{MergeStrategy, Value};

// ===================================================================
// Orchestrate Kind
// ===================================================================

///  Orchestrate kind (sequential/loop/graph/pregel/moa/moe)
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // MirFunction 是 IR 必要载荷，与 MirInst 同型
pub enum MirOrchestrateKind {
    Sequential {
        agents: Vec<MirAgentDef>,
    },
    Loop {
        agents: Vec<MirAgentDef>,
        rounds: Option<u64>,
        exit_when: Option<crate::mir::witness::MirWitness>,
    },
    Graph {
        agents: Vec<MirAgentDef>,
        edges: Vec<MirEdgeDef>,
    },
    /// v0.50: Pregel BSP-style orchestrate
    Pregel {
        agents: Vec<MirAgentDef>,
        edges: Vec<MirEdgeDef>,
        state_schema: Vec<MirStateChannel>,
        checkpoint: Option<MirCheckpointConfig>,
        interrupt_points: Vec<MirInterruptPoint>,
        adjacency: HashMap<String, Vec<String>>,
    },
    /// v0.75.84: MoA（Mixture-of-Agents，arXiv:2406.04692）— 分层多模型协作。
    /// 每层 N 个 proposer LLM 并行生成 → 聚合器 LLM 综合 → 传下一层。
    /// `h_orchestrate` 展开为 pregel 图（每层 proposer 并行 + 聚合 agent，
    /// 静态边层间传递），零新引擎机制。
    Moa {
        /// MoA 层数（论文：l 层，通常 2-3 层；末层单聚合器）。
        layers: usize,
        /// 每层 proposer 模型列表（同层复用；论文 n 个异构模型并行）。
        proposers: Vec<String>,
        /// 聚合器模型（每层聚合 + 末层最终输出）。
        aggregator: String,
        /// 初始 prompt（MoA 每层基于前层输出的「原文」继续；聚合 prompt
        /// 由引擎按 Aggregate-and-Synthesize 模板生成）。
        /// v0.91: `prompt_fn` 是预 lowering 后的 MirFunction（parser 阶段完成），
        /// `prompt` 保留给 witness/LSP 序列化。handlers 执行走 prompt_fn。
        /// v0.92: prompt 从 MirExpr 迁移到 MirWitness。
        prompt: crate::mir::witness::MirWitness,
        /// v0.91: 预 lowering 的 prompt 函数（handlers 执行用，消除 MirExpr 跨层依赖）。
        prompt_fn: MirFunction,
    },
    /// v0.75.85: MoE（Mixture-of-Experts，Shazeer 2017 稀疏门控）— 稀疏激活。
    /// router 语言面 fn 打分 → top-k 稀疏（只跑被选专家）→ 加权组合
    /// （引擎侧 Float 自由，不受语言数值塔约束）。与 MoA 的区别：MoA 全
    /// 部专家跑 + LLM 聚合综合（协作）；MoE 只跑部分专家 + 数值加权（稀疏）。
    Moe {
        /// 专家定义：名 → 函数闭包（fn(x) → number）或模型配置
        /// （{model: "..."}，输出 String）。见 MirMoeExpert。
        experts: Vec<MirMoeExpert>,
        /// 路由器（门控）：语言面 fn(x) → Dict(专家名 → 分数)。
        /// v0.91: `router_fn` 是预 lowering 后的 MirFunction（parser 阶段完成），
        /// `router` 保留给 witness/LSP 序列化。handlers 执行走 router_fn。
        /// v0.92: router 从 MirExpr 迁移到 MirWitness。
        router: crate::mir::witness::MirWitness,
        /// 稀疏度：只激活分数最高 top_k 个专家（标准配置 2，k=1 可行）。
        top_k: usize,
        /// 模型专家的 prompt（含 {input} 插值）。
        /// v0.91: `prompt_fn` 是预 lowering 后的 MirFunction，`prompt` 保留给 witness。
        /// v0.92: prompt 从 MirExpr 迁移到 MirWitness。
        prompt: crate::mir::witness::MirWitness,
        /// v0.91: 预 lowering 的 router 函数（handlers 执行用，消除 MirExpr 跨层依赖）。
        router_fn: MirFunction,
        /// v0.91: 预 lowering 的 prompt 函数（handlers 执行用）。
        prompt_fn: MirFunction,
    },
}

impl MirOrchestrateKind {
    /// v0.92: WitnessOrchestrateKind → MirOrchestrateKind。
    /// v0.92: 字段类型已迁移到 MirWitness —— 直接 clone，无需 MirExpr 桥接。
    pub fn from_witness_kind(
        w: &crate::mir::witness::WitnessOrchestrateKind,
    ) -> MirOrchestrateKind {
        use crate::mir::witness::WitnessOrchestrateKind as W;
        match w {
            W::Sequential { agents } => MirOrchestrateKind::Sequential {
                agents: agents.iter().map(mir_agent_from_witness).collect(),
            },
            W::Loop {
                agents,
                rounds,
                exit_when,
            } => MirOrchestrateKind::Loop {
                agents: agents.iter().map(mir_agent_from_witness).collect(),
                rounds: *rounds,
                exit_when: exit_when.clone(),
            },
            W::Graph { agents, edges } => MirOrchestrateKind::Graph {
                agents: agents.iter().map(mir_agent_from_witness).collect(),
                edges: edges.iter().map(mir_edge_from_witness).collect(),
            },
            W::Pregel {
                agents,
                edges,
                state_schema,
                checkpoint,
                interrupt_points,
                adjacency,
            } => MirOrchestrateKind::Pregel {
                agents: agents.iter().map(mir_agent_from_witness).collect(),
                edges: edges.iter().map(mir_edge_from_witness).collect(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
                adjacency: adjacency.clone(),
            },
            W::Moa {
                layers,
                proposers,
                aggregator,
                prompt,
            } => {
                let prompt_fn = crate::mir::lower::lower_mir_witnesses(
                    std::slice::from_ref(prompt.as_ref()),
                )
                .unwrap_or_default();
                MirOrchestrateKind::Moa {
                    layers: *layers,
                    proposers: proposers.clone(),
                    aggregator: aggregator.clone(),
                    prompt: prompt.as_ref().clone(),
                    prompt_fn,
                }
            }
            W::Moe {
                experts,
                router,
                top_k,
                prompt,
            } => {
                let router_fn = crate::mir::lower::lower_mir_witnesses(
                    std::slice::from_ref(router.as_ref()),
                )
                .unwrap_or_default();
                let prompt_fn = crate::mir::lower::lower_mir_witnesses(
                    std::slice::from_ref(prompt.as_ref()),
                )
                .unwrap_or_default();
                MirOrchestrateKind::Moe {
                    experts: experts.iter().map(mir_moe_expert_from_witness).collect(),
                    router: router.as_ref().clone(),
                    top_k: *top_k,
                    prompt: prompt.as_ref().clone(),
                    router_fn,
                    prompt_fn,
                }
            }
        }
    }
}

/// v0.92: WitnessAgentDef → MirAgentDef（task_expr 直接 clone，
/// task_body 由 lower_mir_witnesses 产出）。
fn mir_agent_from_witness(a: &crate::mir::witness::WitnessAgentDef) -> MirAgentDef {
    let task_body = crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(&a.task_expr))
        .unwrap_or_default();
    MirAgentDef {
        name: a.name.clone(),
        task_expr: a.task_expr.clone(),
        verify_expr: None,
        with_config: None,
        task_body,
        combiner_body: None,
    }
}

/// v0.92: WitnessEdgeDef → MirEdgeDef。
fn mir_edge_from_witness(e: &crate::mir::witness::WitnessEdgeDef) -> MirEdgeDef {
    MirEdgeDef {
        from: e.from.clone(),
        to: e.to.clone(),
        condition_expr: None,
        condition_body: None,
    }
}

/// v0.92: MirWitness(MoE expert) → MirMoeExpert。
fn mir_moe_expert_from_witness(w: &crate::mir::witness::MirWitness) -> MirMoeExpert {
    let def = w.clone();
    let def_fn = crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(w)).unwrap_or_default();
    MirMoeExpert {
        name: String::new(),
        def,
        def_fn,
    }
}

// ===================================================================
// Agent / Edge Definitions
// ===================================================================

/// v0.75.85: MoE 专家定义 — 名 + 定义表达式。
/// def 执行后为 Value::Closure（函数专家，数值输出）或 Value::Dict
/// （{model: "..."}，模型专家，String 输出）。
/// v0.91: `def_fn` 是预 lowering 后的 MirFunction（parser 阶段完成）。
/// v0.92: `def` 类型从 MirExpr 迁移到 MirWitness（canonical 类型）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirMoeExpert {
    pub name: String,
    pub def: crate::mir::witness::MirWitness,
    pub def_fn: MirFunction,
}

///  Agent definition in orchestrate
/// v0.92: task_expr/verify_expr/with_config 从 MirExpr 迁移到 MirWitness。
#[derive(Debug, Clone, PartialEq)]
pub struct MirAgentDef {
    pub name: String,
    pub task_expr: crate::mir::witness::MirWitness,
    pub verify_expr: Option<crate::mir::witness::MirWitness>,
    pub with_config: Option<HashMap<String, crate::mir::witness::MirWitness>>,

    /// Pre-lowered task body (populated during lowering, starts empty)
    pub task_body: MirFunction,
    /// v0.72: Pre-lowered combiner body. When multiple sends target this
    /// vertex, the engine folds them with `(current, incoming) -> Value`
    /// before delivering. Identity (default): last-write-wins (current = incoming).
    pub combiner_body: Option<MirFunction>,
}

///  Edge definition in orchestrate graph
/// v0.92: condition_expr 从 MirExpr 迁移到 MirWitness。
#[derive(Debug, Clone, PartialEq)]
pub struct MirEdgeDef {
    pub from: String,
    pub to: String,
    pub condition_expr: Option<crate::mir::witness::MirWitness>,
    pub condition_body: Option<MirFunction>,
}

// ===================================================================
// Pregel Infrastructure
// ===================================================================

///  Checkpoint configuration (placeholder for v0.50)
/// v0.92: thread_id 从 Box<MirExpr> 迁移到 Box<MirWitness>。
#[derive(Debug, Clone, PartialEq)]
pub struct MirCheckpointConfig {
    pub saver: String,
    pub thread_id: Option<Box<crate::mir::witness::MirWitness>>,
    pub interval: Option<u64>,
    pub max_checkpoints: Option<usize>,
}

///  Interrupt point definition (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub struct MirInterruptPoint {
    pub node_name: String,
    pub when: MirInterruptWhen,
}

///  Interrupt when condition (placeholder for v0.50)
/// v0.92: Condition 从 MirExpr 迁移到 MirWitness。
#[derive(Debug, Clone, PartialEq)]
pub enum MirInterruptWhen {
    Before,
    After,
    Timeout(u64),
    Condition(crate::mir::witness::MirWitness),
    Manual,
}

///  Reducer kind for dynamic edges (placeholder for v0.50)
/// v0.92: Merge 从 MirExpr 迁移到 MirWitness。
#[derive(Debug, Clone, PartialEq)]
pub enum MirReducerKind {
    Last,
    Append,
    Add,
    /// v0.75.5: G-Set（grow-only set）— 通道上并集累积（List/Dict 语义），
    /// 对应 `MergeStrategy::GrowOnlySet`。
    GrowOnly,
    Merge(crate::mir::witness::MirWitness),
    Sum,
    Product,
    Concat,
    Custom(String),
}

/// v0.60: Map Pregel reducer to CRDT merge strategy.
///
/// `Merge`, `Sum`, `Product`, `Concat`, and `Custom` have no direct
/// static mapping and return `None` — these require custom execution.
///
/// NOTE: `Append` maps to `MergeStrategy::Append` for Environment-level
/// merges (two-dict merge), but the Pregel engine handles `Append`
/// separately in `apply_write()` with stream-accumulation semantics
/// (push individual writes into a list). The two paths are intentionally
/// different.
impl MirReducerKind {
    pub fn to_merge_strategy(&self) -> Option<MergeStrategy> {
        match self {
            MirReducerKind::Last => Some(MergeStrategy::LastWriteWins),
            MirReducerKind::Append => Some(MergeStrategy::Append),
            MirReducerKind::Add => Some(MergeStrategy::Add),
            MirReducerKind::GrowOnly => Some(MergeStrategy::GrowOnlySet),
            MirReducerKind::Merge(_)
            | MirReducerKind::Sum
            | MirReducerKind::Product
            | MirReducerKind::Concat
            | MirReducerKind::Custom(_) => None,
        }
    }
}

///  State channel definition (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub struct MirStateChannel {
    pub name: String,
    pub ty: String,
    pub reducer: MirReducerKind,
}

///  Pregel configuration bundle (v0.57: MIR-native engine entry)
#[derive(Debug, Clone, PartialEq)]
pub struct MirPregelConfig {
    pub agents: Vec<MirAgentDef>,
    pub edges: Vec<MirEdgeDef>,
    pub state_schema: Vec<MirStateChannel>,
    pub checkpoint: Option<MirCheckpointConfig>,
    pub interrupt_points: Vec<MirInterruptPoint>,
    pub adjacency: HashMap<String, Vec<String>>,
    /// v0.71: Per-super-step global aggregators. Each agent can contribute
    /// a value via `h_aggregate(name, value)`; the engine reduces across
    /// all contributions per step and exposes the result as `aggregator_<name>`.
    pub aggregators: Vec<MirAggregatorDef>,
    /// v0.72: Centralized coordinator hook. Runs once per super-step after
    /// UPDATE and before ADVANCE. Used for global coordination logic
    /// (e.g., dynamic topology decisions based on aggregator state).
    pub master_compute: Option<MirFunction>,
}

/// v0.71: Aggregator definition (per-super-step global reducer).
#[derive(Debug, Clone, PartialEq)]
pub struct MirAggregatorDef {
    pub name: String,
    pub ty: String,
    pub initial: Value,
    /// Per-step reducer: Add, Max, Min, Last, Concat.
    pub reducer: AggregatorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AggregatorKind {
    Add,
    Max,
    Min,
    Last,
    Concat,
}

/// v0.75.83: 聚合器贡献 — agent 经 `aggregate name, value` 语句提交。
/// v0.93: h_aggregate 提交 `Effect::Contribute`（effect-as-data，与
/// SendTask 同一 Effects 数据通道），引擎侧 aggregator_contribute 归约。
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorContribution {
    pub name: String,
    pub value: Value,
}

// ===================================================================
// v0.92: Trait/Impl/Skill 元数据类型 — 与 expr/ 平行但属于 Kernel 层
// 运行时元数据（trait impl 注册、skill task 编排）。完整迁移留待
// P0.5（expr/ 删除）阶段统一处理；此处先 re-export 自 expr/ 让消费方
// 可以从 orchestrate::* 直接取，避免跨层细节。
// ===================================================================

/// v0.92: Alias for MirAgentDef (used by parser_v3 and orchestrate code)。
pub type MirOrchestrateAgent = MirAgentDef;

/// v0.92: Alias for MirEdgeDef (used by parser_v3 and orchestrate code)。
pub type MirOrchestrateEdge = MirEdgeDef;

/// v0.92: 参数定义（原在 `mir/expr/mod.rs`）——default 从 MirExpr 迁移到 MirWitness。
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_hint: Option<crate::typeck::Type>,
    pub default: Option<crate::mir::witness::MirWitness>,
}

/// v0.92: Trait method definition（原在 `mir/expr/mod.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirTraitMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<String>,
    pub body: Option<MirFunction>,
}

/// v0.92: Function definition in impl block（原在 `mir/expr/mod.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirFnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<String>,
    pub body: Option<MirFunction>,
}

/// v0.92: Skill task definition（原在 `mir/expr/mod.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirSkillTask {
    pub name: String,
    pub description: Option<String>,
    pub params: Vec<Param>,
    pub body: Option<MirFunction>,
}

/// v0.92: Skill verification definition（原在 `mir/expr/mod.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirSkillVerify {
    pub name: String,
    pub given: Vec<String>,
    pub expects: Vec<String>,
    pub params: Vec<Param>,
    pub body: Option<MirFunction>,
}
