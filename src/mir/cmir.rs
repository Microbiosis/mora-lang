//! v0.89: CMIR — 9 层架构第 5 层（并发降维）。
//!
//! 将串行的 Core SSA 指令流降维为并发原语图。
//! BSP 超步、Agent 编排、并发安全策略在这一层显式编码。
//!
//! 设计原则：
//! - 纯计算节点透传为 Pure(CoreInst)
//! - 并发原语（BSP/Agent/效果处理）显式降维
//! - phi 推迟到本层生成（用户建议 #2：BSP 超步边界是自然同步点）
//! - SIMD 作为数据并行原语在本层表达（用户建议 #3）

use crate::mir::core::{CoreInst, EffectLabel};
use crate::mir::fcfg::Reg;

// ===================================================================
// CmirNode — 并发感知的 IR 节点
// ===================================================================

/// CMIR 节点 — 串行指令 + 并发原语。
#[derive(Debug, Clone)]
pub enum CmirNode {
    // ── 纯计算透传 ──
    /// 继承 CoreInst 的纯值操作。
    Pure(CoreInst),

    // ── BSP 原语 ──
    /// BSP 超步 — Mora 的核心并发单元。
    BspSuperstep {
        /// 本步计算指令。
        computes: Vec<CoreInst>,
        /// 发送操作：(value_reg, target_agent_id)。
        sends: Vec<(Reg, String)>,
        /// 聚合操作：(name, value_reg)。
        aggregates: Vec<(String, Reg)>,
        /// 终止条件（None = 继续）。
        halt_cond: Option<Reg>,
    },

    // ── Agent 原语 ──
    /// 生成 Agent。
    AgentSpawn {
        id: String,
        config: AgentConfig,
        body: CmirBlock,
    },
    /// Agent 同步屏障。
    AgentSync {
        agents: Vec<String>,
        barrier: SyncBarrier,
    },
    /// 收集 Agent 结果。
    AgentCollect { agent: String, result: Reg },

    // ── 效果处理（并发感知）──
    /// 效果处理器安装 — 并发安全策略由 ConcurrencyMode 决定。
    EffectHandle {
        label: EffectLabel,
        handler: Reg,
        body: CmirBlock,
        concurrency: ConcurrencyMode,
    },

    // ── 编排模式降维 ──
    /// Pregel 图计算 → BSP 超步序列。
    PregelGraph {
        vertices: Vec<String>,
        edges: Vec<(String, String)>,
        compute_fn: Reg,
        combine_fn: Option<Reg>,
        supersteps: Vec<CmirBlock>,
    },
    /// MoA (Mixture-of-Agents) → 并行 propose + aggregate。
    MoAPipeline { layers: Vec<MoALayer> },
    /// MoE (Mixture-of-Experts) → router + sparse activation。
    MoERouter {
        experts: Vec<String>,
        router_fn: Reg,
        top_k: usize,
    },

    // ── 数据并行原语（SIMD 上提，用户建议 #3）──
    /// 向量映射：对 src 的每个元素执行 op，结果写入 dst。
    SimdMap {
        dst: Reg,
        src: Reg,
        op: SimdOp,
        lanes: usize,
    },
    /// 向量归约：将 src 归约为标量。
    SimdReduce { dst: Reg, src: Reg, op: ReduceOp },
}

// ===================================================================
// 辅助类型
// ===================================================================

/// CMIR 基本块。
#[derive(Debug, Clone)]
pub struct CmirBlock {
    pub nodes: Vec<CmirNode>,
    pub result: Option<Reg>,
}

/// 并发安全模式。
#[derive(Debug, Clone)]
pub enum ConcurrencyMode {
    /// 无并发，env 直接共享（纯函数效果处理）。
    Sequential,
    /// Mutex<Arc<Env>> 保护（当前 Handle 实现）。
    Protected,
    /// Clone env，隔离执行（Transaction/Worker）。
    Isolated,
}

/// Agent 配置。
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_supersteps: Option<usize>,
    pub checkpoint_interval: Option<usize>,
    pub resource_limits: Option<ResourceLimits>,
}

/// 资源限制。
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_memory_bytes: Option<usize>,
    pub max_cpu_ms: Option<usize>,
}

/// 同步屏障类型。
#[derive(Debug, Clone)]
pub enum SyncBarrier {
    /// 等待所有指定 Agent 完成当前超步。
    All,
    /// 等待任意一个 Agent 完成。
    Any,
    /// 等待指定数量的 Agent 完成。
    Count(usize),
}

/// MoA 层。
#[derive(Debug, Clone)]
pub struct MoALayer {
    pub proposers: Vec<Reg>,
    pub aggregator: Reg,
}

/// SIMD 操作。
#[derive(Debug, Clone)]
pub enum SimdOp {
    Add,
    Sub,
    Mul,
    Div,
    Map(Box<CoreInst>), // 自定义映射
}

/// 归约操作。
#[derive(Debug, Clone)]
pub enum ReduceOp {
    Sum,
    Product,
    Max,
    Min,
    Custom(String),
}
