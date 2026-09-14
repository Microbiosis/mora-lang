//! Side-effect instructions — modify env / interp state.

use std::collections::HashMap;

use crate::mir::vm::run_mir;

use crate::mir::{MirFunction, Reg};

use crate::mir::host::MirHost;

use crate::value::{Environment, Value};

// ============================================================
// Side-effect instructions (modify env / interp state)
// ============================================================

pub fn h_define(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    name: &str,
    regs: &[Value],
    src: Reg,
) {
    // v0.83: 录制 StateMutation（前值为 Nil（新变量），后值为 regs[src]）
    let new_val = regs[src].clone();
    env.define(name.to_string(), new_val.clone(), false);
    if let Some(rec) = interp.recorder_mut() {
        rec.record_state_mutation(name.to_string(), Value::Nil, new_val);
    }
}

pub fn h_assign(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    name: &str,
    regs: &[Value],
    src: Reg,
) {
    // v0.83: 录制 StateMutation（前值 = env 当前值，后值 = regs[src]）
    let old_val = env.get(name).unwrap_or(Value::Nil);
    let new_val = regs[src].clone();
    env.assign(name, new_val.clone());
    if let Some(rec) = interp.recorder_mut() {
        rec.record_state_mutation(name.to_string(), old_val, new_val);
    }
}

pub fn h_type_alias(env: &mut Environment, name: &str, target: &str) {
    env.define(name.to_string(), Value::String(target.to_string()), false);
}

pub fn h_enum_def(env: &mut Environment, name: &str, variants: &[crate::common::EnumVariant]) {
    let mut map = HashMap::new();
    for v in variants {
        map.insert(v.name.clone(), Value::String(v.name.clone()));
    }
    env.define(name.to_string(), Value::Dict(map), false);
}

pub fn h_struct_def(env: &mut Environment, name: &str, fields: &[crate::common::StructField]) {
    let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
    env.define(
        name.to_string(),
        Value::Dict(HashMap::from([(
            "__struct_fields__".to_string(),
            Value::List(
                field_names
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        )])),
        false,
    );
}

// v0.83: TEA Model 定义 —— 注册到 env 为 Value::Dict 含 fields 元数据
// + Type::TeaModel 注解（完整 TEA type 系统在阶段 E）
pub fn h_model_def(env: &mut Environment, name: &str, fields: &[crate::common::StructField]) {
    let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
    env.define(
        name.to_string(),
        Value::Dict(HashMap::from([
            ("__model_fields__".to_string(), Value::List(
                field_names.iter().map(|s| Value::String(s.clone())).collect(),
            )),
        ])),
        false,
    );
}

// v0.83: TEA Msg 定义 —— 注册到 env 为 Value::List 含 variants
pub fn h_msg_def(env: &mut Environment, name: &str, variants: &[crate::common::MsgVariant]) {
    let variant_names: Vec<Value> = variants
        .iter()
        .map(|v| {
            let mut map = std::collections::HashMap::new();
            map.insert("name".to_string(), Value::String(v.name.clone()));
            if let Some(ref t) = v.payload_type {
                map.insert("payload_type".to_string(), Value::String(t.clone()));
            }
            Value::Dict(map)
        })
        .collect();
    env.define(name.to_string(), Value::List(variant_names), false);
}

// v0.83: TEA Update 函数定义 —— 注册到 env 为 Value::Closure (name 关联)
// 完整 type: Model × Msg -> (Model, Cmd) 在阶段 E 加 Type::TeaUpdate
pub fn h_update_def(
    env: &mut Environment,
    name: &str,
    params: &[String],
    body: &crate::mir::MirFunction,
) {
    // 注册到 env：name → Dict { params, body_mir }
    let mut map = std::collections::HashMap::new();
    map.insert(
        "__update_params__".to_string(),
        Value::List(params.iter().map(|s| Value::String(s.clone())).collect()),
    );
    map.insert(
        "__update_body__".to_string(),
        Value::String(format!("<MirFunction:{}>", body.params.len())),
    );
    env.define(name.to_string(), Value::Dict(map), false);
}

// v0.83: TEA App 定义 —— 构造完整 TeaApp，存为 Value::TeaApp
// v0.84 Phase 4: 构造后立即调用 initialize() 自动执行 init closure

/// h_app_def 入参结构（避免 8 参数导致的 clippy too-many-arguments）。
/// 与 MirInst::AppDef 字段一一对应。
pub struct AppDefArgs<'a> {
    pub interp: &'a mut dyn crate::mir::host::MirHost,
    pub env: &'a mut Environment,
    pub name: &'a str,
    pub model_name: &'a str,
    pub msg_name: &'a str,
    pub init_mir: &'a crate::mir::MirFunction,
    pub update_mir: &'a crate::mir::MirFunction,
    pub view_mir: &'a crate::mir::MirFunction,
}

pub fn h_app_def(args: AppDefArgs) {
    use crate::tea::TeaApp;
    let AppDefArgs {
        interp,
        env,
        name,
        model_name,
        msg_name,
        init_mir,
        update_mir,
        view_mir,
    } = args;
    // v0.83: 把 init/update/view 转成 Value::Closure —— 完整闭包语义
    // (Arc<MirFunction> 共享 body，EnvRef 捕获当前 env 支持外层变量)。
    // init: () -> Model —— 0 个参数
    let init_closure = Value::Closure {
        params: Vec::new(),
        env: crate::value::EnvRef(Box::new(env.clone())),
        mir_body: std::sync::Arc::new(init_mir.clone()),
    };
    // update: (Model, Msg) -> (Model, Cmd) —— 2 个参数
    let update_closure = Value::Closure {
        params: vec!["model".to_string(), "msg".to_string()],
        env: crate::value::EnvRef(Box::new(env.clone())),
        mir_body: std::sync::Arc::new(update_mir.clone()),
    };
    // view: (Model) -> Value —— 1 个参数
    let view_closure = Value::Closure {
        params: vec!["model".to_string()],
        env: crate::value::EnvRef(Box::new(env.clone())),
        mir_body: std::sync::Arc::new(view_mir.clone()),
    };
    let app = TeaApp::new(
        init_closure.clone(),
        update_closure.clone(),
        view_closure.clone(),
    );
    // v0.84 Phase 4: 自动执行 init closure 获取初始 model（v0.94: 纯转换返回新 app）
    let app = app.initialized(interp);
    // 注册到 env：app + 三个独立闭包（供 builtin tea.* 通过 name 调用）
    env.define(
        name.to_string(),
        Value::TeaApp(std::sync::Arc::new(app)),
        false,
    );
    env.define(
        format!("{}.init", name),
        init_closure,
        false,
    );
    env.define(
        format!("{}.update", name),
        update_closure,
        false,
    );
    env.define(
        format!("{}.view", name),
        view_closure,
        false,
    );
    // 引用 model_name/msg_name（供 typeck 后续扩展检查 Model/Msg 字段匹配）
    let _ = (model_name, msg_name);
}

pub fn h_import(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    path: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    interp.mir_import(path, env, effects)?;
    Ok(())
}

pub fn h_with_config(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    bindings: &[(String, Reg)],
    body: &MirFunction,
    jit: bool,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let binding_vals: Vec<(String, Value)> = bindings
        .iter()
        .map(|(k, r)| (k.clone(), regs[*r].clone()))
        .collect();
    interp.mir_with_config(&binding_vals)?;
    let mut child_env = env.clone();

    // v0.95: 先捕获 body 结果再恢复 config —— 保存/恢复对必须在**所有**
    // 路径上平衡（与 h_handle 的 take/restore 同模式）。此前 body 返回
    // Err 时 `?` 提前冒泡，mir_restore_config 被跳过，失败 with 块的
    // config 泄漏进 config_stack 并驻留到后续无关代码。
    let body_result = if jit {
        // v0.75.43: copy-and-patch JIT（零 LLVM）— 直接编译 MirFunction，
        // 未覆盖指令回落解释器（run_jit Err → run_mir）。
        match crate::mir::jit::run_jit(body, interp, &mut child_env) {
            Ok(v) => Ok(v),
            Err(e) => {
                eprintln!(
                    "JIT compilation failed ({}), falling back to MIR interpreter",
                    e
                );
                // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                run_mir(&std::sync::Arc::new((*body).clone()), interp, &mut child_env, effects)
            }
        }
    } else {
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存
        run_mir(&std::sync::Arc::new((*body).clone()), interp, &mut child_env, effects)
    };
    interp.mir_restore_config();
    let _result = body_result?;
    Ok(())
}

// ============================================================
// v0.102: 声明式范式（逻辑式/关系式）— RelDef / Solve
// ============================================================

use std::sync::Arc;

use crate::rel::{Clause, ProjectFn, RelHost, Search, Subst, reify};

/// 关系定义：注册 `Value::Relation`。同名定义累积子句（Prolog consult
/// 语义：先 `rel edge("a","b")` 后 `rel edge("b","c")` 是同一关系的两条
/// 事实）。非 Relation 的既有绑定被直接覆盖（与 fn 定义同规则）。
pub fn h_rel_def(env: &mut Environment, name: &str, clauses: &[Clause]) {
    let fresh: Vec<Clause> = clauses.to_vec();
    match env.get(name) {
        Some(Value::Relation { name: rel_name, clauses: existing }) => {
            let mut all = existing.as_ref().clone();
            all.extend(fresh);
            env.assign(
                name,
                Value::Relation { name: rel_name.clone(), clauses: Arc::new(all) },
            );
        }
        _ => {
            env.define(
                name.to_string(),
                Value::Relation { name: name.to_string(), clauses: Arc::new(fresh) },
                false,
            );
        }
    }
}

/// solve 的搜索期宿主：关系子句从 solve 位点环境解析；Project 实参经
/// `MirHost::call_value` 做确定性求值。Project 产生的 BSP 效应先收集在
/// 本地 [`Effects`]，搜索结束后并入 solve 指令的效应通道（效应归属位点
/// 仍是 solve 这条指令）。
struct SolveHost<'a> {
    env: &'a Environment,
    interp: &'a mut dyn MirHost,
    collected: crate::mir::effect::Effects,
}

impl RelHost for SolveHost<'_> {
    fn relation_clauses(&mut self, name: &str) -> Result<Arc<Vec<Clause>>, String> {
        match self.env.get(name) {
            Some(Value::Relation { clauses, .. }) => Ok(clauses.clone()),
            Some(other) => Err(format!(
                "{} 不是关系（是 {}），不能作为关系调用",
                name,
                crate::flow::type_name(&other)
            )),
            None => Err(format!("未定义关系 {}", name)),
        }
    }

    fn run_project(&mut self, func: &ProjectFn, args: Vec<Value>) -> Result<Value, String> {
        let mut local = crate::mir::effect::Effects::default();
        let out = match func {
            // 运行期已求值的可调用值（solve 目标体内 project 的构建产物）
            ProjectFn::Value(v) => self.interp.call_value(v, args, &mut local)?,
            // 编译期具名引用：查宿主环境（用户任务/闭包）；builtin 值走
            // 按名分派（call_value 不含 Builtin 分支）；再回落 builtin 表。
            ProjectFn::Name(n) => match self.env.get(n) {
                Some(Value::Builtin(_)) => {
                    self.interp.mir_call_function(n, args, self.env, &mut local)?
                }
                Some(v) => {
                    let v = v.clone();
                    self.interp.call_value(&v, args, &mut local)?
                }
                None => self.interp.mir_call_function(n, args, self.env, &mut local)?,
            },
        };
        self.collected.absorb(local);
        Ok(out)
    }
}

/// 单个解的查询变量投影：
/// - 0 个查询变量 → `nil`（每个解是一个成功标记）；
/// - 1 个 → 变量值本身；
/// - n ≥ 2 → 值列表（元组）。
///
/// 未绑定的变量经 reify 命名为 `_.N` 符号。
fn project_solution(s: &Subst, query_vars: &[String]) -> Value {
    match query_vars.len() {
        0 => Value::Nil,
        1 => reify(&Value::LogicVar(0), s),
        n => Value::List(
            (0..n as u64)
                .map(|i| reify(&Value::LogicVar(i), s))
                .collect(),
        ),
    }
}

/// solve 查询执行。
///
/// 1. 查询变量按序分配 `Value::LogicVar(0..n)`，注入 goal 构建环境
///    （新作用域层——查询变量不外泄）；
/// 2. 运行 goal 构建体，产物必须是 `Value::Goal`；
/// 3. 引擎交错搜索（limit 界定解数量上界），逐解投影 reify；
/// 4. 解列表写 `regs[dst]`；Project 效应并入 `effects`。
#[allow(clippy::too_many_arguments)]
pub fn h_solve(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &mut [Value],
    dst: Reg,
    limit: Option<usize>,
    query_vars: &[String],
    anon_vars: &[String],
    goal: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    // 1. 查询变量 + 匿名变量注入新作用域层（匿名变量分配在投影变量之后）
    let mut builder_env = Environment::with_parent_of(Arc::new(env.clone()));
    for (i, name) in query_vars.iter().enumerate() {
        builder_env.define(name.clone(), Value::LogicVar(i as u64), false);
    }
    let base = query_vars.len() as u64;
    for (i, name) in anon_vars.iter().enumerate() {
        builder_env.define(name.clone(), Value::LogicVar(base + i as u64), false);
    }

    // 2. 运行 goal 构建体
    let goal_arc = Arc::new(goal.clone());
    let built = run_mir(&goal_arc, interp, &mut builder_env, effects)?;
    let built_goal = match built {
        Value::Goal(g) => *g,
        other => {
            return Err(format!(
                "solve 的目标构建体必须返回 goal 值，得到 {}",
                crate::flow::type_name(&other)
            ));
        }
    };

    // 3. 交错搜索
    let mut search = Search::new(built_goal, base + anon_vars.len() as u64);
    let mut host = SolveHost { env, interp, collected: crate::mir::effect::Effects::default() };
    let mut solutions: Vec<Value> = Vec::new();
    loop {
        if let Some(cap) = limit
            && solutions.len() >= cap
        {
            break;
        }
        match search.next_solution(&mut host)? {
            Some(s) => solutions.push(project_solution(&s, query_vars)),
            None => break,
        }
    }
    effects.absorb(host.collected);

    // 4. 写解列表
    regs[dst] = Value::List(solutions);
    Ok(())
}

// v0.80: algebraic effects 的完整实现（Stage 2/4 Stage 2.5 落地）。
//
// 设计：perform/handle 走单遍解释（single-shot continuation）。
// handler 是 first-class MirFunction，由 interp 的 effect_handler 注册表管理。
// handle 块进入时 install_effect_handler，退出时 restore（嵌套 handle 栈）。
//
// 完整契约（与 docs/fp-impl-roadmap.md §2.3 一致）：
// - h_perform: 从 args 取值，调用 interp.perform_effect(effect, args)
//   → Option<Value>。None = 编译期漏检（handler 未安装）。
// - h_handle: install handler → 执行 body → restore handler。
//   body 内的 perform X 调用都路由到该 handler。
//   handler 末尾的 resume "k" 续名（标准做法是 h_handle 隐式 emit Const(dst, ...)）
//   写结果到 k_dst；当前实现为第一版 single-shot，handler 最后一句表达式自动 resume 一次。
pub fn h_perform(
    regs: &mut [Value],
    dst: Reg,
    effect: &str,
    args: &[Reg],
    interp: &mut dyn crate::mir::host::MirHost,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    // v0.83: 录制 Cmd 事件（perform 是 Cmd::Perform 的运行时派发）
    if let Some(rec) = interp.recorder_mut() {
        // 用 Event::Msg 暂存 perform 事件（channel = effect label）
        rec.record_msg(effect.to_string(), Value::List(arg_vals.clone()), 0);
    }
    match interp.perform_effect(effect, arg_vals, effects) {
        Some(reply) => {
            regs[dst] = reply;
            Ok(())
        }
        None => Err(format!(
            "unhandled effect: {} (no matching handle block in scope; typeck reports this as an EffectRowMismatch at compile time — this runtime fallback only guards dynamically generated code)",
            effect
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn h_handle(
    interp: &mut dyn crate::mir::host::MirHost,
    env: &mut crate::value::Environment,
    regs: &mut [Value],
    effect: &str,
    body: &crate::mir::MirFunction,
    handler: &crate::mir::MirFunction,
    k_param: &str,
    k_dst: Reg,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    // 1. 保存当前 handler（嵌套 handle 栈）
    let prev_handler = interp.take_effect_handler(effect);

    // 2. 安装新 handler（v0.95: env 是 O(1) 结构共享纯值快照，无锁包装）
    interp.install_effect_handler(
        effect.to_string(),
        Box::new(crate::runtime::effect::HandlerClosure {
            effect: effect.to_string(),
            handler_mir: std::sync::Arc::new(handler.clone()),
            body_arc: std::sync::Arc::new(body.clone()),
            env: env.clone(),
            k_param: k_param.to_string(),
        }),
    );

    // 3. 执行 body —— handle body 与 `if` 一样**不创建新作用域**
    //    （spec §6.2：只有 task/fn/with/for 建新作用域）。直接在同一个 `env`
    //    上执行：env 作为线性值穿线，无共享 cell、无克隆回写。body 内对既有
    //    绑定的 `assign` 因此自然回流外层 —— 这正是「用数据流代替状态机」。
    //    （此前用 `env.clone()` 起独立子环境再丢弃，只靠每绑定 Arc<Mutex>
    //    的写穿才让外层赋值可见，是隐式的共享可变状态。）
    let body_arc = std::sync::Arc::new(body.clone());
    let result = crate::mir::vm::run_mir(&body_arc, interp, env, effects);

    // 4. 恢复 handler（无论 body 成功/失败）
    interp.restore_effect_handler(effect.to_string(), prev_handler);

    // 5. 写结果：body 末尾表达式的值（如果有）
    match result {
        Ok(v) => {
            regs[k_dst] = v;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub fn h_macro_def(
    env: &mut Environment,
    name: &str,
    params: &[String],
    body: &crate::mir::MirFunction,
) {
    env.define(
        name.to_string(),
        Value::Macro {
            name: name.to_string(),
            params: params.to_vec(),
            body: std::sync::Arc::new(body.clone()),
        },
        false,
    );
}

/// v0.102 修复：task 定义注册一等值到环境。
///
/// **缺陷背景**：`MirInst::TaskDef` 此前是 dispatch 的 no-op —— task 只存在于
/// `build_task_registry`（从**当前执行函数体**静态收集）。顶层函数执行时
/// registry 含全部顶层 task，但嵌套函数（闭包/fn 体）执行时 registry 由该
/// 嵌套体重建，看不到外层声明 → `let f = fn(x) outer_task(x) end` 报
/// "Undefined function or task"。这是**词法可见性缺口**：声明指令没有
/// 真正声明绑定。
///
/// **修法**：与 `MacroDef` 同一先例 —— 定义处 `env.define` 一等 `Value::Task`，
/// 使任何嵌套代码都能沿 env 父链词法解析到外层 task。
/// `task_registry` 保留为顶层作用域的静态索引（提供前向引用语义），
/// `h_call` 先查 registry 再查 env，两者互补覆盖词法可见性。
///
/// 定义环境即全局环境（`task` 仅顶层语句可见，parser 不在嵌套上下文产出）。
pub fn h_task_def(
    env: &mut Environment,
    name: &str,
    params: &[String],
    body: &crate::mir::MirFunction,
) {
    env.define(
        name.to_string(),
        Value::Task {
            name: name.to_string(),
            params: params.to_vec(),
            mir_body: std::sync::Arc::new(body.clone()),
        },
        false,
    );
}

#[cfg(test)]
mod tests {
    /// v0.95: with 块的 config 保存/恢复对必须在**所有**路径上平衡 ——
    /// body 出错时 restore 不得被 `?` 跳过。此前失败 with 块的 config
    /// 泄漏进 config_stack 并驻留到后续无关代码（状态机卫生缺陷）。
    #[test]
    fn with_config_restored_when_body_errors() {
        let mut interp = crate::interpreter::Interpreter::new();
        assert!(interp.core.current_ai_config.is_none());

        // with 块内 body 运行期错误：[1][5] 列表越界
        let src = "with model = \"m1\"\n  let boom = [1][5]\nend";
        let (func, _witnesses) =
            crate::parser_v3::ParserV3::compile(src).expect("compile with block");
        let mut env = crate::value::Environment::new();
        let result = crate::mir::vm::run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut crate::mir::effect::Effects::new(),
        );
        assert!(result.is_err(), "越界索引应报运行时错误");
        assert!(
            interp.core.current_ai_config.is_none(),
            "with 块出错后 config 必须恢复，不得驻留泄漏"
        );
    }

    /// 对照：body 成功路径 config 同样恢复（保存/恢复对平衡）。
    #[test]
    fn with_config_restored_when_body_succeeds() {
        let mut interp = crate::interpreter::Interpreter::new();
        let src = "with model = \"m1\"\n  let x = 1\nend";
        let (func, _witnesses) =
            crate::parser_v3::ParserV3::compile(src).expect("compile with block");
        let mut env = crate::value::Environment::new();
        crate::mir::vm::run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut crate::mir::effect::Effects::new(),
        )
        .expect("with block should succeed");
        assert!(
            interp.core.current_ai_config.is_none(),
            "with 块结束后 config 应恢复到块前状态"
        );
    }
}
