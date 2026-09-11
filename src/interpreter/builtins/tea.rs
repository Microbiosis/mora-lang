//! v0.83: tea.* builtin — TEA (The Elm Architecture) runtime entry。
//!
//! 方法：
//! - `tea.init(model)` — 初始化 TeaApp（model 为初始 Model）
//! - `tea.dispatch(app, msg)` — 派发一个 Msg 到 app
//! - `tea.run(app, max_steps)` — 运行 TEA 循环直到队列空
//! - `tea.replay(recorder, env)` — 从 Recorder 还原 Model 状态
//! - `tea.model(app)` — 获取当前 Model
//! - `tea.view(app)` — 调用 view 函数（占位）

use super::*;

impl Interpreter {
    /// v0.83: tea.* builtin dispatch。
    pub fn call_tea_method(
        &mut self,
        method: &str,
        args: &[Value],
    ) -> Result<Value, String> {
        match method {
            "init" => {
                // v0.84 Phase 4c: tea.init(init_closure, model_hint)
                // - 第一个参数是 init closure 时，调用它拿初始 model
                // - 第一个参数是普通值时，直接作为初始 model
                // - 无参数时，model = Nil
                let (init_val, has_init) = if let Some(first) = args.first() {
                    match first {
                        Value::Closure { .. } => (first.clone(), true),
                        _ => (Value::Nil, false),
                    }
                } else {
                    (Value::Nil, false)
                };
                let app = crate::tea::TeaApp::new(
                    init_val,
                    Value::Nil, // update closure
                    Value::Nil, // view closure
                );
                // v0.94: app 是纯值 —— 用 with_model 构造初始 model，返回新 app。
                let app = if has_init {
                    // 调用 init closure 获取初始 model；失败则回落第一个 arg。
                    match self.call_value(
                        &app.init,
                        vec![],
                        &mut crate::mir::effect::Effects::new(),
                    ) {
                        Ok(model) => app.with_model(model),
                        Err(_) => {
                            app.with_model(args.first().cloned().unwrap_or(Value::Nil))
                        }
                    }
                } else {
                    app.with_model(args.first().cloned().unwrap_or(Value::Nil))
                };
                Ok(Value::TeaApp(std::sync::Arc::new(app)))
            }
            "dispatch" => {
                let app = args
                    .first()
                    .ok_or("tea.dispatch: missing app arg")?;
                let msg_val = args
                    .get(1)
                    .ok_or("tea.dispatch: missing msg arg")?;
                let app = match app {
                    Value::TeaApp(a) => a.clone(),
                    _ => return Err("tea.dispatch: first arg must be TeaApp".to_string()),
                };
                let msg = crate::tea::Msg::from_value(msg_val)
                    .map_err(|e| format!("tea.dispatch: {}", e))?;
                // v0.94: 纯追加 —— 返回携带新 Msg 的新 app（调用方需重新绑定）。
                let next = app.as_ref().clone().dispatch(msg);
                Ok(Value::TeaApp(std::sync::Arc::new(next)))
            }
            "run" => {
                let app = args
                    .first()
                    .ok_or("tea.run: missing app arg")?;
                let max_steps = match args.get(1) {
                    Some(Value::Int(n)) => *n as usize,
                    _ => 1000,
                };
                let app = match app {
                    Value::TeaApp(a) => a.clone(),
                    _ => return Err("tea.run: first arg must be TeaApp".to_string()),
                };
                // v0.94: run_loop 是纯驱动 —— 返回推进后的新 app（数据流）。
                let next = app.as_ref().run_loop(max_steps, self);
                Ok(Value::TeaApp(std::sync::Arc::new(next)))
            }
            "model" => {
                let app = args
                    .first()
                    .ok_or("tea.model: missing app arg")?;
                match app {
                    Value::TeaApp(a) => Ok(a.model()),
                    _ => Err("tea.model: first arg must be TeaApp".to_string()),
                }
            }
            "update" => {
                // v0.83: 真正调用 update 闭包 —— self 是 MirHost context
                let app = args
                    .first()
                    .ok_or("tea.update: missing app arg")?;
                let msg_val = args
                    .get(1)
                    .ok_or("tea.update: missing msg arg")?;
                let app = match app {
                    Value::TeaApp(a) => a.clone(),
                    _ => return Err("tea.update: first arg must be TeaApp".to_string()),
                };
                let msg = crate::tea::Msg::from_value(msg_val)
                    .map_err(|e| format!("tea.update: {}", e))?;
                // v0.94: 纯推进 —— dispatch + step，返回携带新 model 的新 app。
                let (next, _stepped) = app.as_ref().clone().dispatch(msg).step(self);
                Ok(Value::TeaApp(std::sync::Arc::new(next)))
            }
            "view" => {
                // v0.83: 真正调用 view 闭包
                let app = args
                    .first()
                    .ok_or("tea.view: missing app arg")?;
                let app = match app {
                    Value::TeaApp(a) => a.clone(),
                    _ => return Err("tea.view: first arg must be TeaApp".to_string()),
                };
                match self.call_value(
                    &app.view,
                    vec![app.model()],
                    &mut crate::mir::effect::Effects::new(),
                ) {
                    Ok(v) => Ok(v),
                    Err(e) => Err(format!("tea.view: {}", e)),
                }
            }
            "model_type" => {
                // tea.model_type(app) —— 返回 Model 类型名（占位）
                let _ = args;
                Ok(Value::String("Model".to_string()))
            }
            "msg_type" => {
                // tea.msg_type(app) —— 返回 Msg 类型名（占位）
                let _ = args;
                Ok(Value::String("Msg".to_string()))
            }
            "replay" => {
                // 占位：返回 Nil（完整实现在 src/tea/replay.rs）
                let _ = args;
                Ok(Value::Nil)
            }
            other => Err(format!("tea has no method: {}", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tea_init_creates_app() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_tea_method("init", &[Value::Int(42)])
            .unwrap();
        assert!(matches!(result, Value::TeaApp(_)));
    }

    #[test]
    fn tea_dispatch_returns_updated_app() {
        // v0.94: dispatch 是纯转换 —— 返回携带新 Msg 的新 app（非 Nil）。
        let mut interp = Interpreter::new();
        let app_val = interp.call_tea_method("init", &[Value::Int(0)]).unwrap();
        let mut msg_map = std::collections::HashMap::new();
        msg_map.insert("tag".to_string(), Value::String("Increment".to_string()));
        let msg = Value::Dict(msg_map);
        let result = interp.call_tea_method("dispatch", &[app_val, msg]).unwrap();
        match result {
            Value::TeaApp(a) => assert_eq!(a.msgs_len(), 1, "新 app 队列含 1 条 Msg"),
            _ => panic!("dispatch 应返回 TeaApp"),
        }
    }

    #[test]
    fn tea_model_returns_initial() {
        let mut interp = Interpreter::new();
        let app_val = interp.call_tea_method("init", &[Value::String("hello".to_string())]).unwrap();
        let model = interp.call_tea_method("model", &[app_val]).unwrap();
        assert_eq!(model, Value::String("hello".to_string()));
    }

    #[test]
    fn tea_run_returns_advanced_app() {
        // v0.94: run 是纯驱动 —— 返回推进后的 app，model 不变（无 pending Msg）。
        let mut interp = Interpreter::new();
        let app_val = interp.call_tea_method("init", &[Value::Int(99)]).unwrap();
        let result = interp.call_tea_method("run", &[app_val]).unwrap();
        match result {
            Value::TeaApp(a) => assert_eq!(a.model(), Value::Int(99)),
            _ => panic!("run 应返回 TeaApp"),
        }
    }

    #[test]
    fn tea_unknown_method_errors() {
        let mut interp = Interpreter::new();
        assert!(interp.call_tea_method("nonexistent", &[]).is_err());
    }
}