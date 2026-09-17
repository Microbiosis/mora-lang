//! v0.83: TEA (The Elm Architecture) — Model/Msg/Update/Cmd 完整架构。
//!
//! 设计目标：Stage 3 必须实现完整 TEA 循环（§0.6 硬性要求）：
//!   - Model（状态容器）— `Type::Concrete { name: "Model", ... }`
//!   - Msg（消息 tagged union）— `Type::Union([TeaMsg, ...])`
//!   - Update(Msg, Model) -> (Model, Cmd<Model>)   ← v0.104 对齐 spec §9.6 / Elm
//!   - Cmd（命令描述副作用）— `Type::Union([Perform, Batch, None])`
//!   - View(Model) -> render target（占位 Value）
//!   - Replay：从 Recorder 还原 Msg 流 + StateMutation diffs

use crate::value::Value;

/// Cmd（命令）— 描述 update 函数想要触发的副作用。
///
/// 设计参考 Elm：Cmd 是数据，不是动作。runtime 解释 Cmd 执行副作用。
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// 无操作（update 无副作用时返回）
    None,
    /// 批量执行多个命令（顺序执行，前一个失败则中止）
    Batch(Vec<Cmd>),
    /// 执行一个具名 effect（对应 algebraic effect）
    Perform { effect: String, args: Vec<Value> },
    /// 触发另一个 Msg（chain update）
    Dispatch(Box<Value>),
}

impl Cmd {
    /// 序列化为 Value 树（Cmd → Value）
    pub fn to_value(&self) -> Value {
        match self {
            Cmd::None => Value::Nil,
            Cmd::Batch(cmds) => {
                let items: Vec<Value> = cmds.iter().map(|c| c.to_value()).collect();
                Value::List(items)
            }
            Cmd::Perform { effect, args } => {
                let mut map = std::collections::HashMap::new();
                map.insert("kind".to_string(), Value::String("Perform".to_string()));
                map.insert("effect".to_string(), Value::String(effect.clone()));
                map.insert("args".to_string(), Value::List(args.clone()));
                Value::Dict(map)
            }
            Cmd::Dispatch(msg) => {
                let mut map = std::collections::HashMap::new();
                map.insert("kind".to_string(), Value::String("Dispatch".to_string()));
                map.insert("msg".to_string(), msg.as_ref().clone());
                Value::Dict(map)
            }
        }
    }

    /// 从 Value 树反序列化（用于 replay）
    pub fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Nil => Ok(Cmd::None),
            Value::List(items) => {
                let mut cmds = Vec::new();
                for item in items {
                    cmds.push(Self::from_value(item)?);
                }
                Ok(Cmd::Batch(cmds))
            }
            Value::Dict(map) => {
                let kind = match map.get("kind") {
                    Some(Value::String(s)) => s.as_str(),
                    _ => return Err("Cmd.from_value: missing kind".to_string()),
                };
                match kind {
                    "Perform" => {
                        let effect = match map.get("effect") {
                            Some(Value::String(s)) => s.clone(),
                            _ => return Err("Cmd.from_value: missing effect".to_string()),
                        };
                        let args = match map.get("args") {
                            Some(Value::List(items)) => items.clone(),
                            _ => Vec::new(),
                        };
                        Ok(Cmd::Perform { effect, args })
                    }
                    "Dispatch" => {
                        let msg = match map.get("msg") {
                            Some(v) => Box::new(v.clone()),
                            _ => return Err("Cmd.from_value: missing msg".to_string()),
                        };
                        Ok(Cmd::Dispatch(msg))
                    }
                    other => Err(format!("Cmd.from_value: unknown kind '{}'", other)),
                }
            }
            other => Err(format!(
                "Cmd.from_value: expected List or Dict, got {:?}",
                other
            )),
        }
    }
}

/// Msg（消息）— 触发 update 函数调用的输入事件。
///
/// 设计为 tagged union：tag 是 variant 名（如 "Increment"），payload 是该 variant 的数据。
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub tag: String,
    pub payload: Box<Value>,
}

impl Msg {
    pub fn new(tag: impl Into<String>, payload: Value) -> Self {
        Msg {
            tag: tag.into(),
            payload: Box::new(payload),
        }
    }

    /// 序列化为 Value 树
    pub fn to_value(&self) -> Value {
        let mut map = std::collections::HashMap::new();
        map.insert("tag".to_string(), Value::String(self.tag.clone()));
        map.insert("payload".to_string(), self.payload.as_ref().clone());
        Value::Dict(map)
    }

    /// 从 Value 树反序列化
    pub fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Dict(map) => {
                let tag = match map.get("tag") {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("Msg.from_value: missing tag".to_string()),
                };
                let payload = Box::new(map.get("payload").cloned().unwrap_or(Value::Nil));
                Ok(Msg { tag, payload })
            }
            other => Err(format!("Msg.from_value: expected Dict, got {:?}", other)),
        }
    }
}

/// TEA Runtime — Model + Msg + Update + View + Cmd 的**不可变值**表示。
///
/// v0.94 数据流化：去掉 `Arc<Mutex<TeaState>>`。TeaApp 现在是纯数据 ——
/// `model` + 待处理 `msg_queue`/`cmd_queue` + 三个闭包。所有转换返回新 app：
/// - [`TeaApp::dispatch`]：纯，追加一条 Msg。
/// - [`TeaApp::fold`]：TEA 核心折叠 `model' = fold(msgs, model, update)`。
/// - [`TeaApp::run_loop`]：纯驱动，折叠全部消息 + 解释 Cmd，返回新 app。
///
/// 因为 app 是值，`Value::TeaApp(Arc<TeaApp>)` 可被多个消费者并发共享同一
/// 不可变快照而无需锁 —— 并发是数据结构的自然属性，而非防御性加锁。
#[derive(Clone, Debug, PartialEq)]
pub struct TeaApp {
    /// 当前 Model（纯数据）
    pub model: Value,
    /// 待处理的 Msg 队列（FIFO）
    pub msg_queue: Vec<Msg>,
    /// 待解释的 Cmd 队列
    pub cmd_queue: Vec<Cmd>,
    /// init 函数 — `() -> Model`
    pub init: Value,
    /// update 函数 — `(Msg, Model) -> (Model, Cmd)`（v0.104 对齐 spec §9.6 / Elm）
    pub update: Value,
    /// view 函数 — `(Model) -> Value`（占位 render target）
    pub view: Value,
}

impl TeaApp {
    /// 创建新 TeaApp（model = Nil，队列为空）。
    pub fn new(init: Value, update: Value, view: Value) -> Self {
        TeaApp {
            model: Value::Nil,
            msg_queue: Vec::new(),
            cmd_queue: Vec::new(),
            init,
            update,
            view,
        }
    }

    // ── 纯转换（无 interp、无副作用）────────────────────────

    /// 纯：返回设置了 model 的新 app。
    pub fn with_model(mut self, model: Value) -> Self {
        self.model = model;
        self
    }

    /// 纯：返回追加一条 Msg 的新 app。
    pub fn dispatch(mut self, msg: Msg) -> Self {
        self.msg_queue.push(msg);
        self
    }

    /// 纯：返回追加多条 Msg 的新 app。
    pub fn dispatch_many(mut self, msgs: Vec<Msg>) -> Self {
        self.msg_queue.extend(msgs);
        self
    }

    /// 纯：返回追加一条 Cmd 的新 app。
    pub fn with_cmd(mut self, cmd: Cmd) -> Self {
        self.cmd_queue.push(cmd);
        self
    }

    /// 获取当前 model（克隆）。
    pub fn model(&self) -> Value {
        self.model.clone()
    }

    pub fn msgs_len(&self) -> usize {
        self.msg_queue.len()
    }

    pub fn cmds_len(&self) -> usize {
        self.cmd_queue.len()
    }

    // ── TEA 核心：纯折叠 ───────────────────────────────────

    /// TEA 核心 —— `model' = fold(msgs, model, update)`。
    ///
    /// 把 `msg_queue` 中每条 Msg 依次喂给 `update` 闭包，返回
    /// `(新 model, 累积产出的 Cmd 流)`。`self` 不被修改（纯函数）。
    fn fold(&self, interp: &mut dyn crate::mir::host::MirHost) -> (Value, Vec<Cmd>) {
        let mut model = self.model.clone();
        let mut cmds = Vec::new();
        for msg in &self.msg_queue {
            match Self::apply_update(&model, msg, &self.update, interp) {
                Ok((next, mut cs)) => {
                    model = next;
                    cmds.append(&mut cs);
                }
                Err(e) => eprintln!("TeaApp::fold: update failed: {}", e),
            }
        }
        (model, cmds)
    }

    /// 调用 update 闭包一次并解析 `(Model, Cmd)`。仅 interp 求值有副作用。
    ///
    /// v0.104: 实参顺序 **(msg, model)** —— 与 spec §9.6 的工作示例一致
    /// （`update(msg, model) …`）。本语言把 TEA 明确声明为「Elm 风格」
    /// （spec §9.6 首句），而 Elm 的签名就是 `update : Msg -> Model -> Model`。
    /// 此前运行时传 `(model, msg)`，与 spec 示例的形参序相反 —— 照规范写的
    /// `update(msg, model)` 会拿到「model=消息、msg=模型」，示例体
    /// `model.count` 因此报 `Dict has no method: count`。
    fn apply_update(
        model: &Value,
        msg: &Msg,
        update: &Value,
        interp: &mut dyn crate::mir::host::MirHost,
    ) -> Result<(Value, Vec<Cmd>), String> {
        let args = vec![msg.to_value(), model.clone()];
        match interp.call_value(update, args, &mut crate::mir::effect::Effects::new())? {
            // update 返回 (Model, Cmd) tuple —— Value::List [model, cmd]
            Value::List(items) => {
                let next = items.first().cloned().unwrap_or(Value::Nil);
                let cmds = items
                    .get(1)
                    .and_then(|v| Cmd::from_value(v).ok())
                    .into_iter()
                    .collect();
                Ok((next, cmds))
            }
            // update 返回裸 model（无 Cmd）—— 也支持
            other => Ok((other, Vec::new())),
        }
    }

    /// 纯：处理队首一条 Msg，返回 (新 app, 是否处理了消息)。
    /// 队列空或 update 失败时，返回的 app 可能未推进（届时第二项为 false）。
    pub fn step(&self, interp: &mut dyn crate::mir::host::MirHost) -> (TeaApp, bool) {
        let mut app = self.clone();
        if app.msg_queue.is_empty() {
            return (app, false);
        }
        let msg = app.msg_queue.remove(0);
        match Self::apply_update(&app.model, &msg, &app.update, interp) {
            Ok((model, cmds)) => {
                app.model = model;
                app.cmd_queue.extend(cmds);
                (app, true)
            }
            Err(e) => {
                eprintln!("TeaApp::step: update failed: {}", e);
                (app, false)
            }
        }
    }

    /// 纯驱动：折叠全部待处理消息 + 解释 Cmd（副作用经 interp），返回新 app。
    ///
    /// 每轮：
    /// 1. `fold` 所有待处理 Msg → 新 model + 产出 Cmd 流（纯数据变换）
    /// 2. 解释 Cmd（`Cmd::Dispatch` 的消息回流到 `msg_queue`，供下一轮折叠）
    pub fn run_loop(&self, max_steps: usize, interp: &mut dyn crate::mir::host::MirHost) -> TeaApp {
        let mut app = self.clone();
        for _ in 0..max_steps {
            if app.msg_queue.is_empty() && app.cmd_queue.is_empty() {
                break;
            }
            // 1. 折叠全部消息（纯）
            let (model, produced) = app.fold(interp);
            app.msg_queue.clear();
            app.model = model;
            app.cmd_queue.extend(produced);
            // 2. 解释 Cmd —— 作为数据流逐个处理
            let to_run = std::mem::take(&mut app.cmd_queue);
            for cmd in to_run {
                app = app.apply_cmd(cmd, interp);
            }
        }
        app
    }

    /// 解释单个 Cmd（递归 Batch）。`Dispatch` 把 Msg 回流到 `msg_queue`。
    fn apply_cmd(mut self, cmd: Cmd, interp: &mut dyn crate::mir::host::MirHost) -> TeaApp {
        match cmd {
            Cmd::None => {}
            Cmd::Batch(cmds) => {
                for c in cmds {
                    self = self.apply_cmd(c, interp);
                }
            }
            Cmd::Perform { effect, args } => {
                let _ =
                    interp.perform_effect(&effect, args, &mut crate::mir::effect::Effects::new());
            }
            Cmd::Dispatch(msg_value) => {
                if let Ok(msg) = Msg::from_value(&msg_value) {
                    self.msg_queue.push(msg);
                }
            }
        }
        self
    }

    /// 纯：调用 init 闭包得到初始 model，返回新 app。
    /// 调用失败时保持 model = Nil。
    pub fn initialized(self, interp: &mut dyn crate::mir::host::MirHost) -> TeaApp {
        let mut app = self;
        if let Ok(model) =
            interp.call_value(&app.init, vec![], &mut crate::mir::effect::Effects::new())
        {
            app.model = model;
        }
        app
    }
}

/// v0.83: TEA Replay Driver — 从 Recorder 还原 Model 状态。
///
/// 加载 Recorder 的 Msg + StateMutation events，按时间顺序 replay：
/// 1. StateMutation events 提供 model 的 diff（var/old/new）
/// 2. Msg events 提供给 update 的输入序列
/// 3. 每步校验 prior_state_hash（TEA-style deterministic replay）
///
/// 返回重建后的 model 与 replay 过程中的 warnings。
pub mod replay;

#[cfg(test)]
mod tests;
