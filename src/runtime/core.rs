//! v0.52 ADR-001: CoreRuntime — 语言执行必需的薄核心
//!
//! 从 Interpreter god object 抽出的核心执行字段（environment/tool_registry/
//! current_ai_config/config_stack/current_merge_strategies），
//! 是解释器运行所必需的最小状态容器。
//!
//! v0.70: 移除了 `worker_channels` / `worker_receivers` 死代码分支。
//! v0.93: send/aggregate 效应不再驻留宿主 —— 改为显式 `&mut Effects`
//! 参数沿执行链传递（见 [`crate::mir::effect::Effects`]）。CoreRuntime 因此
//! 不再持有任何效应缓冲。
//! v0.95: 数据流化 —— v0.94 起 [`Environment`] 已是无内部可变性的持久化
//! 纯值（HAMT + 不可变父链），`Arc<Mutex<Environment>>` 包装成为纯值外面
//! 的冗余锁。本结构改持纯 `Environment`：执行 env 由 `take_env` 一次性
//! 取出后按数据流穿线（v0.75.76 约定），不再存在"锁保护的宿主槽"。
//! 同时删除死状态 `globals`（v0.75.76 起无任何读取，仅构造器写入），
//! `gensym_counter` 改为纯 `usize`。
//!
//! # 锁分类决策（v0.95 审计定案）
//!
//! 「纯函数让并发从防御性编程变成自然属性」—— 解释器运行时层的锁只允许
//! 三类存在理由，全部登记如下：
//!
//! 1. **单属主状态**：持纯值，`&mut self` 变更（本文件的 environment /
//!    gensym_counter / random_state 即此类）。禁止 `Arc<Mutex>` 包装。
//! 2. **有意跨克隆/跨线程共享的注册表与缓存**：锁是共享机制本身，不是
//!    冗余防御。包括 orch plans/refine/skill、infra string_interner/ai_cache
//!    （worker 线程共享 AI 响应缓存，拆锁会增加真实 API 调用）、ccr、mock、
//!    sandbox/toolplane、trace_collector（跨 worker 用量聚合）、capability。
//!    它们的 mutator 是 `&self` + 锁 —— 这是设计，不是待修。
//! 3. **真跨线程协调**：pregel worker_pool 的任务队列/共享接收端、
//!    scheduler 的定时器状态。锁即协调原语。
//!
//! 语言级显式可变性（`Value::Atom`、`Value::Router`、`StreamReader`）是
//! Mora 语言自身的能力面，不属于运行时状态机问题。

use std::collections::HashMap;
use std::sync::Arc;

use crate::runtime::types::{AiConfigValue, ToolDef};
use crate::value::{Environment, MergeStrategy};

/// 语言执行必需的薄核心。
/// 注：ToolDef 不含 Debug，所以 CoreRuntime 不 derive Debug。
// v0.80: CoreRuntime 不再 derive Clone —— EffectHandler 是 move-only trait object，
// 整个 EffectRegistry 不可 Clone。手动 impl Clone（克隆时 effect_handlers 取空）。
pub struct CoreRuntime {
    /// 当前执行环境（v0.75.76 起仅在 run 前由 `take_env` 取出，此后按值穿线）
    pub(crate) environment: Environment,
    /// 工具注册表（MCP / builtin tool 的运行时注册）
    pub(crate) tool_registry: Arc<HashMap<String, ToolDef>>,
    /// 当前 with 块 set 的 AiConfig 值
    pub(crate) current_ai_config: Option<AiConfigValue>,
    /// with 块 config 保存/恢复栈（MIR 解释器用）
    pub(crate) config_stack: Vec<Option<AiConfigValue>>,
    /// v0.67: 当前 transaction/worker 的 CRDT 合并策略。
    /// 设值时 `h_transaction`/`h_worker` 使用 `merge_from_with_strategies`；
    /// 为 None 时回退到硬编码 LWW。
    /// v0.95 审计定案：显式全局配置（`merge_with` builtin 持久设置），
    /// 非 set-then-use 临时槽 —— GrowOnlySet 跨 worker 累积依赖其持久性。
    pub(crate) current_merge_strategies: Option<HashMap<String, MergeStrategy>>,
    /// v0.80: algebraic effects handler 注册表（Stage 2/4 落地）。
    /// MirHost trait 的 `perform_effect / install/take/restore_effect_handler`
    /// 全部作用于本字段。嵌套 handle 块走 take+restore 栈模式。
    pub(crate) effect_handlers: crate::runtime::effect::EffectRegistry,
    /// v0.87: gensym 计数器。每次 gensym() 调用递增，保证符号名唯一。
    /// v0.95: 纯 `usize` —— 唯一消费点（gensym builtin）在 `&mut self` 上
    /// 递增，无跨线程共享；Clone 按值复制（Pregel worker 各自独立计数，
    /// 与旧 Arc<Mutex> clone 的分歧语义一致），无需锁。
    pub(crate) gensym_counter: usize,
    /// v0.99: ambient random 状态（纯值）。`random.*` 方法调用的 ambient
    /// effect 由本状态兜底应答 —— 取代 v0.91 的进程级
    /// `static Mutex<Xoshiro256>` 全局状态机。单属主 `&mut self` 线性穿线
    /// （锁分类第 1 类），无锁；Clone 按值复制 —— worker 各自独立推进
    /// 序列，并发从「共享一把锁」变成「值拷贝即隔离」。
    pub(crate) random_state: crate::runtime::random::Xoshiro256,
}

impl Default for CoreRuntime {
    fn default() -> Self {
        Self {
            environment: Environment::default(),
            tool_registry: Arc::new(HashMap::new()),
            current_ai_config: None,
            config_stack: Vec::new(),
            current_merge_strategies: None,
            effect_handlers: crate::runtime::effect::EffectRegistry::default(),
            gensym_counter: 0,
            random_state: crate::runtime::random::Xoshiro256::from_time(),
        }
    }
}

// v0.80: 手动 Clone impl —— 不能 derive Clone（EffectHandler: !Clone）。
// effect_handlers 克隆时取空（move semantics —— handler 留在原实例）。
// 语义：Pregel worker 复制 → 子线程没有用户注册的 handler（body 直接 perform
// 用户 effect 会报 unhandled effect）。用户 handler 是动态作用域，不随克隆
// 传播；ambient handler 是运行时基础设施，克隆后依然在场（见 random_state）。
// v0.95: environment 克隆是 O(1) 结构共享（持久化 HAMT），共享绑定不再
// 需要共享可变 cell —— 克隆后的 env 相互独立，写不穿透。
// v0.99: random_state 按值复制 —— 每个 worker 独立推进自己的随机序列
// （互不干扰、无锁），不再是旧全局 Mutex 下的交错共享序列。
impl Clone for CoreRuntime {
    fn clone(&self) -> Self {
        Self {
            environment: self.environment.clone(),
            tool_registry: self.tool_registry.clone(),
            current_ai_config: self.current_ai_config.clone(),
            config_stack: self.config_stack.clone(),
            current_merge_strategies: self.current_merge_strategies.clone(),
            effect_handlers: crate::runtime::effect::EffectRegistry::default(),
            gensym_counter: self.gensym_counter,
            random_state: self.random_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    #[test]
    fn core_tool_registry_empty() {
        let core = CoreRuntime::default();
        assert!(core.tool_registry.is_empty());
    }

    #[test]
    fn core_ai_config_default_none() {
        let core = CoreRuntime::default();
        assert!(core.current_ai_config.is_none());
    }

    #[test]
    fn core_config_stack_default_empty() {
        let core = CoreRuntime::default();
        assert!(core.config_stack.is_empty());
    }

    #[test]
    fn core_clone_preserves_environment_bindings() {
        let mut core = CoreRuntime::default();
        core
            .environment
            .define("test".to_string(), Value::Int(42), false);
        core.gensym_counter = 5;
        let cloned = core.clone();
        // 克隆保留绑定
        assert!(matches!(cloned.environment.get("test"), Some(Value::Int(42))));
        // gensym 计数器按值复制（v0.95: 纯 usize，无锁）
        assert_eq!(cloned.gensym_counter, 5);
        // 纯值语义：克隆侧写不穿透原侧（无共享可变 cell）
        let mut cloned = cloned;
        cloned
            .environment
            .define("test".to_string(), Value::Int(43), false);
        assert!(matches!(core.environment.get("test"), Some(Value::Int(42))));
    }
}
