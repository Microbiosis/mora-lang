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

use crate::checkpoint::{Checkpoint, CheckpointSaver};
use crate::common::trait_info::TraitInfo;
use crate::value::{Environment, MergeStrategy, Value};

/// MIR 解释器执行所需的宿主能力。
///
/// `Interpreter` 是主实现（见 `interpreter/mod.rs`）；测试可提供轻量假实现。
pub trait MirHost {
    /// 函数调用桥（`h_call` task 分支之外的用户函数/builtin 调用）。
    /// `env` 为当前执行环境（call_function 兜底查找用户函数的单一来源——
    /// v0.75.76：不查询宿主全局环境，杜绝 take_env 空壳造成的双环境分歧）。
    /// v0.93: `effects` 是显式效应累加器 —— 嵌套调用（闭包/task/macro）产生的
    /// send/aggregate 经它向上传播，不再依赖宿主侧信道。
    fn mir_call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String>;
    /// 方法调用桥（`h_method_call`）。
    fn mir_call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String>;
    /// 可调用值调用桥（`h_pipe` 的 `|>` 右操作数）。
    fn call_value(
        &mut self,
        value: &Value,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String>;
    /// 模块导入桥（`h_import`）。导入执行的模块代码可产生效应。
    fn mir_import(
        &mut self,
        path: &str,
        env: &mut Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<(), String>;
    /// with 块 config 设置（`h_with_config`）。
    fn mir_with_config(&mut self, bindings: &[(String, Value)]) -> Result<(), String>;
    /// with 块 config 恢复（`h_with_config` 末尾）。
    fn mir_restore_config(&mut self);
    /// 当前 CRDT 合并策略（`h_worker`/`h_transaction`/`h_observe`/`h_span` 用）。
    /// v0.95 审计定案：这是**显式全局配置**（`merge_with` builtin 设置，
    /// 持久生效直至再次设置），不是 set-then-use 临时槽 —— GrowOnlySet
    /// 跨多个 worker 累积合并依赖其持久性（tests/tier0_replacement.rs 钉住）。
    fn current_merge_strategies(&self) -> Option<HashMap<String, MergeStrategy>>;
    /// 当前执行环境的纯值快照。
    /// v0.95: 返回 [`Environment`] 值（O(1) 结构共享克隆）—— v0.94 起
    /// Environment 无内部可变性，不再经 `Arc<Mutex<>>` 共享。
    /// 现有消费者：Pregel 引擎未注入 base_env 时的执行环境回落。
    /// （`h_closure` 捕获 / `h_receive` 自 v0.75.76 起走显式穿线的 env 参数，
    /// 不再读宿主槽。）
    fn environment(&self) -> Environment;
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
    /// 内核 DAG 缓存（宿主单属主纯值，v1.00）。
    /// `run_mir_with_signal` 从宿主取缓存构建优化 DAG —— 取代 v0.75.27 的
    /// 进程级 `static DAG_CACHE` 状态机（OnceLock + Mutex 防御式加锁）。
    /// 缓存是对同一 `Arc<MirFunction>` 确定的纯函数的 memo，单属主
    /// `&mut self` 线性访问无锁；宿主克隆按值复制缓存（memo 透明）。
    fn dag_cache(&mut self) -> &mut crate::mir::cache::DagCache;
    /// 克隆宿主（Pregel 并行 worker 需要每 worker 一份独立宿主状态）。
    /// object-safe：返回 `Box<dyn MirHost + Send>`，让 `dyn MirHost` 也能被
    /// 复制进 worker 线程（`Interpreter` 实现 = `self.clone()`）。
    fn clone_box(&self) -> Box<dyn MirHost + Send>;

    // ── v0.80: algebraic effects 完整接口（Stage 2/4 落地）──
    //
    // v0.80 设计契约：
    // - perform_effect: body 内的 Perform 指令统一调用。
    //   返回 Some(reply) = handler 已处理；None = 未处理（编译期漏检）。
    // - install/take/restore_effect_handler: handle 块的注册表操作。
    //   take+restore 配对使用，支持嵌套 handle（Stack 模型）。
    //
    // 实现位于 interpreter/mod.rs::Interpreter — 通过 CoreRuntime::effect_handlers
    // HashMap<String, Box<dyn EffectHandler>> 存储。
    // EffectHandler trait 在 src/runtime/effect.rs。
    fn perform_effect(
        &mut self,
        effect: &str,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Option<Value>;
    fn install_effect_handler(
        &mut self,
        effect: String,
        handler: Box<dyn crate::runtime::effect::EffectHandler>,
    );
    fn take_effect_handler(&mut self, effect: &str) -> Option<Box<dyn crate::runtime::effect::EffectHandler>>;
    fn restore_effect_handler(
        &mut self,
        effect: String,
        prev: Option<Box<dyn crate::runtime::effect::EffectHandler>>,
    );

    /// v0.83: 访问 Recorder（用于 h_define/h_assign/h_send emit StateMutation/Msg）。
    /// 默认 None——只有 Interpreter 的 Record 模式才会返回 Some。
    fn recorder_mut(&mut self) -> Option<&mut crate::record::Recorder> {
        None
    }
}
