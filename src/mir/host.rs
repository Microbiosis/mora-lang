//! v0.75.x: MIR 解释器宿主抽象（MirHost）
//!
//! 解耦 `mir/` ↔ `interpreter/` 双向依赖的枢纽：
//! - 此前 `mir/interp.rs` / `mir/handlers.rs` / `mir/dag_interp.rs` 直接持有
//!   `&mut Interpreter`，而 `interpreter/` 又调用 `mir::interp::run_mir`，
//!   构成编译级循环。
//! - 现在 MIR 解释器只依赖本 trait（定义在 mir 侧），`Interpreter` 实现它。
//!   mir 侧不再 import `crate::interpreter`。
//!
//! trait 方法集合 = handlers 需要的宿主能力（方法桥 / config / checkpoint /
//! 环境访问 / BSP send 缓冲 / trait registry 写）。非需求的能力不入 trait，
//! 保持最小面。

use std::collections::HashMap;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, CheckpointSaver, SendTask};
use crate::mir::expr::AggregatorContribution;
use crate::runtime::types::TraitInfo;
use crate::value::{Environment, MergeStrategy, Value};

/// MIR 解释器执行所需的宿主能力。
///
/// `Interpreter` 是主实现（见 `interpreter/mod.rs`）；测试可提供轻量假实现。
pub trait MirHost {
    /// 函数调用桥（`h_call` task 分支之外的用户函数/builtin 调用）。
    /// `env` 为当前执行环境（call_function 兜底查找用户函数的单一来源——
    /// v0.75.76：不查询宿主全局环境，杜绝 take_env 空壳造成的双环境分歧）。
    fn mir_call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
    ) -> Result<Value, String>;
    /// 方法调用桥（`h_method_call`）。
    fn mir_call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String>;
    /// 可调用值调用桥（`h_pipe` 的 `|>` 右操作数）。
    fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String>;
    /// 模块导入桥（`h_import`）。
    fn mir_import(&mut self, path: &str, env: &mut Environment) -> Result<(), String>;
    /// with 块 config 设置（`h_with_config`）。
    fn mir_with_config(&mut self, bindings: &[(String, Value)]) -> Result<(), String>;
    /// with 块 config 恢复（`h_with_config` 末尾）。
    fn mir_restore_config(&mut self);
    /// 当前 CRDT 合并策略（`h_worker`/`h_transaction` 用）。
    fn current_merge_strategies(&self) -> Option<HashMap<String, MergeStrategy>>;
    /// 当前执行环境（`h_closure` 捕获 / `h_receive` 读消息）。
    fn environment(&self) -> Arc<parking_lot::Mutex<Environment>>;
    /// BSP send 缓冲（`h_send` push / `h_orchestrate` flush）。
    fn dynamic_sends(&mut self) -> &mut Vec<SendTask>;
    /// v0.75.83: BSP 聚合器贡献缓冲（`h_aggregate` push / pregel 超步末
    /// 收集归约）。与 dynamic_sends 同构 —— agent 无法直接访问引擎，
    /// 经宿主缓冲提交贡献。
    fn aggregator_contributions(&mut self) -> &mut Vec<AggregatorContribution>;
    /// checkpoint saver（`h_orchestrate` 注入 Pregel 引擎）。
    fn checkpoint_saver(&self) -> Option<Arc<dyn CheckpointSaver>>;
    /// 从 saver 恢复 checkpoint（`h_orchestrate`）。
    fn load_checkpoint(
        &self,
        thread_id: &str,
    ) -> Result<Option<Checkpoint>, crate::error::MoraError>;
    /// trait 注册表（`h_trait_def` 用 `Arc::make_mut` 写入）。
    fn trait_registry(&mut self) -> &mut Arc<HashMap<String, TraitInfo>>;
    /// impl 表（`h_impl_def` 用 `Arc::make_mut` 写入）。
    fn impl_table(&mut self) -> &mut Arc<HashMap<String, Vec<String>>>;
    /// 克隆宿主（Pregel 并行 worker 需要每 worker 一份独立宿主状态）。
    /// object-safe：返回 `Box<dyn MirHost + Send>`，让 `dyn MirHost` 也能被
    /// 复制进 worker 线程（`Interpreter` 实现 = `self.clone()`）。
    fn clone_box(&self) -> Box<dyn MirHost + Send>;
}
