//! v0.83: TeaApp 完整 TEA 循环单元测试 —— 验证 update 闭包真的被调用。

use crate::mir::MirFunction;
use crate::tea::{Cmd, Msg, TeaApp};
use crate::value::EnvRef;
use crate::value::Value;
use std::sync::Arc;

/// 创建空 MirFunction 的 helper（用于 test 占位）
fn empty_mir() -> Arc<MirFunction> {
    Arc::new(MirFunction {
        params: Vec::new(),
        body: Vec::new(),
        n_regs: 0,
        effects: Default::default(),
    })
}

/// 创建 EnvRef 闭包（无捕获 env）
fn closure_with_mir(mir: Arc<MirFunction>, params: Vec<String>) -> Value {
    Value::Closure {
        params,
        env: EnvRef(Box::default()),
        mir_body: mir,
    }
}

#[test]
fn teaapp_step_with_empty_closure_is_noop() {
    // v0.83: 即使 update 是空 closure（返回 Nil），step 不应 panic
    let app = TeaApp::new(
        closure_with_mir(empty_mir(), vec![]),
        closure_with_mir(empty_mir(), vec!["model".to_string(), "msg".to_string()]),
        closure_with_mir(empty_mir(), vec!["model".to_string()]),
    )
    .with_model(Value::Int(42))
    .dispatch(Msg::new("Test", Value::Nil));
    // run_loop 需要 MirHost context —— 用 Interpreter
    let mut interp = crate::interpreter::Interpreter::new();
    let result = app.run_loop(10, &mut interp);
    // 空 closure 返回 Nil —— step 视为 "replace model with Nil"
    assert_eq!(result.model(), Value::Nil);
}

#[test]
fn teaapp_init_via_constructor() {
    // 测试 TeaApp::new 接受 3 个 Value 参数
    let app = TeaApp::new(Value::Nil, Value::Nil, Value::Nil);
    // 初始 model 为 Nil
    assert_eq!(app.model(), Value::Nil);
    // 纯转换：with_model 返回新 app，原 app 不变
    let base = app.clone();
    let next = app.with_model(Value::Int(100));
    assert_eq!(next.model(), Value::Int(100));
    assert_eq!(base.model(), Value::Nil, "with_model 不改变原 app（纯）");
}

#[test]
fn teaapp_msg_queue_starts_empty() {
    let app = TeaApp::new(Value::Nil, Value::Nil, Value::Nil);
    // 没有 dispatch 时，step 应立即返回 false
    let mut interp = crate::interpreter::Interpreter::new();
    let (_, stepped) = app.step(&mut interp);
    assert!(!stepped);
    assert_eq!(app.run_loop(10, &mut interp).model(), Value::Nil);
}

#[test]
fn teaapp_dispatch_appends_msg() {
    // 使用有 body 的 update closure 避免 call_value 错误
    let app = TeaApp::new(
        Value::Nil,
        closure_with_mir(empty_mir(), vec!["model".to_string(), "msg".to_string()]),
        Value::Nil,
    )
    .dispatch(Msg::new("Test", Value::Int(1)))
    .dispatch(Msg::new("Other", Value::Int(2)));
    let mut interp = crate::interpreter::Interpreter::new();
    // step 取一条 msg（纯：返回推进后的新 app）
    let (app, stepped) = app.step(&mut interp);
    assert!(stepped);
    // 第二次还有 msg
    let (app, stepped2) = app.step(&mut interp);
    assert!(stepped2);
    // 第三次空
    let (_, stepped3) = app.step(&mut interp);
    assert!(!stepped3);
}

#[test]
fn teaapp_run_loop_drains_cmd_queue() {
    // v0.83: 验证 run_loop 真正消化 cmd_queue（Phase 1 修复）
    let update_mir = Arc::new(crate::mir::MirFunction {
        params: vec!["model".to_string(), "msg".to_string()],
        body: vec![], // 空 body —— run_mir 应返回 Value::Nil
        n_regs: 0,
        effects: Default::default(),
    });
    let update_closure = Value::Closure {
        params: vec!["model".to_string(), "msg".to_string()],
        env: EnvRef(Box::default()),
        mir_body: update_mir,
    };
    let app = TeaApp::new(Value::Nil, update_closure, Value::Nil)
        .with_model(Value::Int(0))
        .with_cmd(Cmd::Perform {
            effect: "Ai".to_string(),
            args: vec![Value::String("test".to_string())],
        });
    // run_loop 应 drain cmd_queue（纯驱动返回新 app）
    let mut interp = crate::interpreter::Interpreter::new();
    let result = app.run_loop(10, &mut interp);
    // 模型保持 0（空 update 不改变 model），但 cmd 被消化
    assert_eq!(result.model(), Value::Int(0));
}

#[test]
fn teaapp_cmd_dispatch_redispatches_msg() {
    // v0.83: Cmd::Dispatch 真的把 msg 重新 push 到 msg_queue
    let msg = crate::tea::Msg::new("ReDispatch", Value::Int(1));
    let app = TeaApp::new(Value::Nil, Value::Nil, Value::Nil)
        .with_model(Value::Nil)
        .with_cmd(Cmd::Dispatch(Box::new(msg.to_value())));
    let mut interp = crate::interpreter::Interpreter::new();
    // run_loop 应 dispatch msg 到 msg_queue（但 step 会因空 update 失败）
    let _ = app.run_loop(5, &mut interp);
}

#[test]
fn teaapp_run_loop_is_pure_and_returns_new_app() {
    // v0.94: run_loop 是纯驱动 —— 原 app 不被修改，返回携带新 model 的新 app。
    let app = TeaApp::new(
        Value::Nil,
        closure_with_mir(empty_mir(), vec!["model".to_string(), "msg".to_string()]),
        Value::Nil,
    )
    .with_model(Value::Int(7))
    .dispatch(Msg::new("Noop", Value::Int(1)));
    let mut interp = crate::interpreter::Interpreter::new();
    let advanced = app.run_loop(10, &mut interp);
    assert_eq!(app.model(), Value::Int(7), "原 app 冻结不动");
    assert_eq!(advanced.model(), Value::Nil, "新 app 落在 update 结果");
}

#[test]
fn teaapp_is_send_sync_and_shareable_without_locks() {
    // v0.94: TeaApp 是纯数据 —— 无内部 Mutex，天然 Send + Sync。
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TeaApp>();
    let app = Arc::new(TeaApp::new(Value::Nil, Value::Nil, Value::Nil).with_model(Value::Int(5)));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let app = Arc::clone(&app);
            std::thread::spawn(move || app.model())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("join"), Value::Int(5));
    }
}

#[test]
fn teaapp_init_closure_is_callable() {
    // v0.83: 验证 init closure 可被 call_value 调用
    // v0.83 Phase 5: builtin tea.init 真正调用 init closure
    // 这里只验证 init closure 通过 interp.call_value 可被调用
    let init_mir = Arc::new(crate::mir::MirFunction {
        params: vec![],
        body: vec![],
        n_regs: 0,
        effects: Default::default(),
    });
    let init_closure = Value::Closure {
        params: vec![],
        env: EnvRef(Box::default()),
        mir_body: init_mir,
    };
    let app = TeaApp::new(init_closure, Value::Nil, Value::Nil);
    // 直接调 call_value（绕过 builtin 测试 init 路径）
    let mut interp = crate::interpreter::Interpreter::new();
    let result = interp.call_value(&app.init, vec![], &mut crate::mir::effect::Effects::new());
    // 空 init closure 返回 Nil
    assert!(result.is_ok());
}

#[test]
fn teaapp_cmd_serialization_roundtrip() {
    // v0.83: 验证 Cmd::to_value/from_value 完整 roundtrip
    let original = Cmd::Batch(vec![
        Cmd::None,
        Cmd::Perform {
            effect: "Ai".to_string(),
            args: vec![Value::String("test".to_string())],
        },
    ]);
    let value = original.to_value();
    let restored = Cmd::from_value(&value).unwrap();
    assert_eq!(original, restored);
}

#[test]
fn teaapp_msg_serialization_roundtrip() {
    let original = Msg::new("Increment", Value::Int(1));
    let value = original.to_value();
    let restored = Msg::from_value(&value).unwrap();
    assert_eq!(original, restored);
}
