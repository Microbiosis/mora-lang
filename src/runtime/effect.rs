//! v0.80: algebraic effects — EffectHandler trait + HandlerClosure + Registry。
//!
//! 设计（与 docs/fp-impl-roadmap.md §2.4 一致）：
//!
//! **EffectHandler trait**: 所有 effect handler 的运行时契约。
//! `perform(args, body_env, k_dst)` 接收 Perform 指令的参数 + 当前 env +
//! 处理结果寄存器 dst，返回 Result。
//!
//! **HandlerClosure**: 第一版实现（Stage 2.5）。
//! 把 handler MirFunction + body Arc + body env + k_param 装箱。
//! perform 时 clone 当前 env，运行 handler 拿到结果，写到 body 的 k_dst。
//!
//! **EffectRegistry**: 全局 handler 栈（HashMap<String, Box<dyn EffectHandler>>）。
//! interpreter::CoreRuntime::effect_handlers 持有，由 MirHost trait 暴露。
//!
//! 第一版是 single-shot（handler 末尾表达式的值自动 resume 一次），
//! 后续 Stage 2.x 升级到 multi-shot（resume 可多次调用）时，
//! EffectHandler trait 保持不变（continuation 改为 multi-shot-aware）。

use std::sync::Arc;

use parking_lot::Mutex;

use crate::mir::MirFunction;
use crate::value::{Environment, Value};

/// 单个 effect handler 的运行时契约。
///
/// `perform` 在 body 内遇到 `Perform { effect, args }` 指令时被调用。
/// - `args`: Perform 传递的参数（已从 regs 取值）
/// - `body_env`: 当前 body 的 env（在多线程场景下，需要 Arc<Mutex<Environment>>）
/// - `k_dst`: 结果写入的寄存器（handler 末尾的 resume 续名）
///
/// 返回 `Ok(reply)` 表示 effect 已处理；`Err(msg)` 表示 handler 内部错误。
///
/// v0.80 Stage 2.0: `perform` 签名扩展为带 `&mut dyn MirHost`（Box dyn）——
/// handler 内部需要 run_mir 来执行 handler_mir，必须有 host 句柄。
/// 单 dispatch 入口（避免 trait method 双签名）。
pub trait EffectHandler: Send + Sync {
    /// v0.80: 真正的 handler 执行入口。`host` 是 `MirHost` trait object。
    fn perform_box(
        &mut self,
        host: &mut dyn crate::mir::host::MirHost,
        args: Vec<Value>,
    ) -> Result<Value, String>;
}

/// 第一版 HandlerClosure —— 把 handler MirFunction 装进 trait 容器。
///
/// 携带字段：
/// - `effect`: 注册时绑定的 effect 标签（冗余存储以便调试）
/// - `handler_mir`: handler 实现（MirFunction）
/// - `body_arc`: handler 调用的目标 body（thread-safe reference）
/// - `env`: handler 调用时的 body env（spawn 时克隆）
/// - `k_param`: handler 内 resume 续名的参数名（todo: 后续 stage 用）
pub struct HandlerClosure {
    pub effect: String,
    pub handler_mir: Arc<MirFunction>,
    pub body_arc: Arc<MirFunction>,
    pub env: Arc<Mutex<Environment>>,
    pub k_param: String,
}

impl EffectHandler for HandlerClosure {
    fn perform_box(
        &mut self,
        host: &mut dyn crate::mir::host::MirHost,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        // v0.80 Stage 2.0: 真正的 handler 执行。
        //
        // 1. args 注入 handler_env（按 __arg0, __arg1, ... 命名约定）
        //    handler 用 env.get("__arg0") 等访问 arguments。
        // 2. run_mir(handler_mir, host, env) — handler 末尾表达式的值是返回值。
        //
        // 第一版（single-shot）：handler 一次性消耗，不还原。注意此 take 拿到
        // 的 handler 已从 EffectRegistry 移除 —— 嵌套 handle 用相同 effect
        // 会重新 push（参考 h_handle 的 take+restore 模式）。
        let mut handler_env = (*self.env.lock()).clone();
        for (i, arg) in args.iter().enumerate() {
            handler_env.define(format!("__arg{}", i), arg.clone(), false);
        }

        // 2. 跑 handler_mir（host 通过 &mut dyn MirHost 传入）
        let result = crate::mir::vm::run_mir(&self.handler_mir, host, &mut handler_env);

        // 3. 把可能变更的 handler_env 写回（让 handler 写作用域变量生效）
        *self.env.lock() = handler_env;

        result
    }
}

/// 全局 handler 栈（HashMap 版本）。
///
/// 嵌套 handle 块走 take+restore 模式：
/// 1. `take_effect_handler(effect)` → prev = Some(handler)
/// 2. `install_effect_handler(effect, new_handler)`
/// 3. body 执行
/// 4. `restore_effect_handler(effect, prev)` 恢复
///
/// v0.80 第一版：HashMap<effect, Vec<handler>> 栈结构。
/// Stage 2.x 升级到更高效的 Arena 索引。
///
/// 注意：`EffectHandler` trait 不需要 Clone（handler 是 move-only）。
/// 因此 `EffectRegistry` 不能 derive Clone —— `CoreRuntime::clone_box()`
/// 复制时新建空 registry，handler 不在 cloned CoreRuntime 中共享。
#[derive(Default)]
pub struct EffectRegistry {
    pub stack: std::collections::HashMap<String, Vec<Box<dyn EffectHandler>>>,
}

impl EffectRegistry {
    pub fn install(&mut self, effect: String, handler: Box<dyn EffectHandler>) {
        self.stack.entry(effect).or_default().push(handler);
    }

    pub fn take(&mut self, effect: &str) -> Option<Box<dyn EffectHandler>> {
        self.stack.get_mut(effect).and_then(|v| v.pop())
    }

    pub fn restore(&mut self, effect: String, prev: Option<Box<dyn EffectHandler>>) {
        if let Some(h) = prev {
            self.stack.entry(effect).or_default().push(h);
        }
    }

    /// 取出栈顶 handler 用于执行 Perform（不消耗）。
    /// 实际使用 Box::as_mut 改 handler 状态。
    pub fn top_mut(&mut self, effect: &str) -> Option<&mut Box<dyn EffectHandler>> {
        self.stack.get_mut(effect).and_then(|v| v.last_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_install_take_restore() {
        let mut reg = EffectRegistry::default();
        // 安装一个 dummy handler
        struct DummyHandler;
        impl EffectHandler for DummyHandler {
            fn perform_box(
                &mut self,
                _host: &mut dyn crate::mir::host::MirHost,
                _args: Vec<Value>,
            ) -> Result<Value, String> {
                Ok(Value::Int(42))
            }
        }
        reg.install("Ai".into(), Box::new(DummyHandler));

        // take 后栈为空
        let prev = reg.take("Ai");
        assert!(prev.is_some());
        assert!(reg.take("Ai").is_none());

        // restore 后又有
        reg.restore("Ai".into(), prev);
        assert!(reg.take("Ai").is_some());
    }

    #[test]
    fn empty_registry_returns_none() {
        let mut reg = EffectRegistry::default();
        assert!(reg.take("NonExistent").is_none());
        assert!(reg.top_mut("NonExistent").is_none());
    }
}