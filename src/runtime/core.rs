//! v0.52 ADR-001: CoreRuntime — 语言执行必需的薄核心
//!
//! 从 Interpreter god object 抽出的核心执行字段（globals/environment/tool_registry/
//! current_ai_config/config_stack/current_merge_strategies/dynamic_sends），
//! 是解释器运行所必需的最小状态容器。
//!
//! v0.70: 移除了 `worker_channels` / `worker_receivers` 死代码分支。
//! 消息传递通过 `dynamic_sends`（BSP Send API，v0.69 接通）完成。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::interpreter::{AiConfigValue, ToolDef};
use crate::value::{Environment, Value, MergeStrategy};

/// 语言执行必需的薄核心。
/// 注：ToolDef 不含 Debug，所以 CoreRuntime 不 derive Debug。
#[derive(Clone)]
pub struct CoreRuntime {
    /// 全局变量环境
    pub(crate) globals: Arc<Mutex<Environment>>,
    /// 当前执行环境（可嵌套）
    pub(crate) environment: Arc<Mutex<Environment>>,
    /// 工具注册表（MCP / builtin tool 的运行时注册）
    pub(crate) tool_registry: Arc<HashMap<String, ToolDef>>,
    /// 当前 with 块 set 的 AiConfig 值
    pub(crate) current_ai_config: Option<AiConfigValue>,
    /// with 块 config 保存/恢复栈（MIR 解释器用）
    pub(crate) config_stack: Vec<Option<AiConfigValue>>,
    /// v0.67: 当前 transaction/worker 的 CRDT 合并策略。
    /// 设值时 `h_transaction`/`h_worker` 使用 `merge_from_with_strategies`；
    /// 为 None 时回退到硬编码 LWW。
    pub(crate) current_merge_strategies: Option<HashMap<String, MergeStrategy>>,
    /// v0.69: Dynamic sends buffer. `h_send` pushes here; `h_orchestrate`
    /// flushes into the BSP engine's pending_sends at the start of each
    /// super-step. Lets agents route messages at runtime without direct
    /// access to the engine.
    pub(crate) dynamic_sends: Vec<crate::checkpoint::SendTask>,
}

impl Default for CoreRuntime {
    fn default() -> Self {
        let env = Arc::new(Mutex::new(Environment::default()));
        Self {
            globals: env.clone(),
            environment: env,
            tool_registry: Arc::new(HashMap::new()),
            current_ai_config: None,
            config_stack: Vec::new(),
            current_merge_strategies: None,
            dynamic_sends: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_default_globals_and_env_share() {
        let core = CoreRuntime::default();
        // globals 和 environment 初始指向同一个 Arc
        assert!(Arc::ptr_eq(&core.globals, &core.environment));
    }

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
    fn core_dynamic_sends_default_empty() {
        let core = CoreRuntime::default();
        assert!(core.dynamic_sends.is_empty());
    }

    #[test]
    fn core_clone_preserves_globals_identity() {
        let core = CoreRuntime::default();
        {
            let mut env = core.environment.lock();
            env.define("test".to_string(), Value::Int(42), false);
        }
        let cloned = core.clone();
        let val = cloned.environment.lock().get("test").clone();
        assert!(matches!(val, Some(Value::Int(42))));
    }
}
