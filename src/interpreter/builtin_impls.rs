//! v0.92 P1.2: `Interpreter::call_builtin_*` — builtin 函数实现（自 dispatch.rs 迁出）。
//!
//! 按 concern 拆分：dispatch.rs 只保留路由（`call_function` 的 name→handler
//! 分派 + `call_value` 的 Value→调用），本模块承载 30 个 `call_builtin_*`
//! 具体实现 + 兜底查找。`testcase!` 宏定义在 `interpreter` 模块根。

use parking_lot::Mutex;

use super::*;
use crate::value::Value;

impl Interpreter {
    pub(super) fn call_builtin_merge_with(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let key = match args.first() {
            Some(Value::String(s)) => {
                testcase!(true, "merge_with: string key");
                s.clone()
            }
            _ => return Err("merge_with(key, strategy) expects string key".to_string()),
        };
        let strat = match args.get(1) {
            Some(Value::String(s)) => {
                testcase!(true, "merge_with: string strategy");
                s.as_str()
            }
            _ => {
                return Err("merge_with(key, strategy) expects string strategy".to_string());
            }
        };
        // v0.75.24: 策略名解析收敛到 MergeStrategy::from_name
        // （单一事实来源；typeck 对字面量参数已做编译期校验，此处
        // 运行时兜底未类型化用法）。
        let ms = match crate::value::MergeStrategy::from_name(strat) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "merge_with: unknown strategy '{}' (append/add/dict_union/grow_only_set/lww)",
                    strat
                ));
            }
        };
        let mut strategies = self.current_merge_strategies().unwrap_or_default();
        strategies.insert(key, ms);
        self.set_merge_strategies(Some(strategies));
        Ok(Value::Nil)
    }

    pub(super) fn call_builtin_print(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let msg = args
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\t");
        println!("{}", msg);
        Ok(Value::Nil)
    }

    /// `range(start, end, step)` → `[start, start+step, …]`，**左闭右开**。
    ///
    /// v0.104.6 修复：**实参不再只认 `Value::Float`**。
    ///
    /// 此前三个实参各自 `.and_then(|v| match v { Value::Float(n) => …, _ => None })`
    /// 然后 `.unwrap_or(默认值)`。于是任何一个**非 Float** 的实参都被当成
    /// 「没传」，静默取默认值 —— 而本语言里字面量是 `Float`、`len()` 与
    /// `xs.len()` 返的却是 `Int`，所以最自然的那句标准写法：
    ///
    /// ```text
    /// for i in range(0, len(xs))   # xs = [1,2,3,4,5]
    ///   assign s = s + xs[i]
    /// end
    /// ```
    /// 得到 **`s = 0.0`**（应为 15.0）：`end` 退回 `start`，range 恒空，
    /// 循环体一次都不执行。**无任何报错、退出码 0。**
    /// （`range(0, len(vs))` 同理得 0 个元素；把字面量换成 `0, 5` 立刻正确。）
    ///
    /// 修法是消除「静默降级」这一恶劣失败模式本身，而不只补 `Int` 一个洞：
    /// 实参**存在但类型不对**一律报错。只有实参**真的缺席**才用默认值。
    pub(super) fn call_builtin_range(&mut self, args: Vec<Value>) -> Result<Value, String> {
        fn as_i64(v: &Value, which: &str) -> Result<Option<i64>, String> {
            match v {
                Value::Float(n) => {
                    if !n.is_finite() {
                        return Err(format!("range() {which} must be finite, got {n}"));
                    }
                    Ok(Some(*n as i64))
                }
                Value::Int(n) => Ok(Some(*n)),
                Value::BigInt(n) => n
                    .to_i64()
                    .map(Some)
                    .ok_or_else(|| format!("range() {which} out of i64 range: {n}")),
                other => Err(format!(
                    "range() {which} must be a number, got {}",
                    crate::flow::type_name(other)
                )),
            }
        }
        use num_traits::ToPrimitive;

        // v0.104.6 D327：单参 `range(n)` 必须等价于 `range(0, n)`。
        //
        // 修前：`start = args.first()`、`end = args.get(1)`，而 `end` 缺参时
        // 取 `start` ⇒ 单参时 **start == end** ⇒ `while i < end` 恒假 ⇒
        // **静默返回空列表**。实测后果：
        //
        // ```text
        // let t = 0
        // for i in range(5)
        //   t = t + i
        // end
        // print(t)                     →  0.0      （期望 10）
        // for i in range(3) { print(i) } →  一次都不打印
        // len(range(5))                 →  0
        // ```
        //
        // **退出码 0、零诊断**，且与 D312 / D313 / D315 同族：静默失败。
        //
        // 查证（吸取 D320 教训：肯定断言也需要自己的验证）：
        // - `docs/mora-spec.md` 里 **`range` 零命中** —— 规范从未定义它；
        // - 既有判据**全部**用多参形态（`range(0,4)` / `range(0,n,1)` /
        //   `range(3,0,-1)` / `range(0,5,0)`），**无一条覆盖单参**；
        // - `builtin_return_types.rs:153` 明写「range 声明 **3 参**却常被
        //   2 参调用」⇒ 签名是 3 参，1 参落在签名之外。
        //
        // ⇒ 单参是**未文档化、未钉住的漏掉方向**，与 D325 的 `reshape` 截断
        // 同形。Python / JS / Rust 的 `range(n)` 均为 `[0, n)`。
        //
        // 负步长分支（D- 轮已修）不在此列，本次只动单参。
        let (start, end) = match args.len() {
            0 => (0, 0),
            // 单参 = 上界：`range(n)` ≡ `range(0, n)`。
            1 => (0, as_i64(&args[0], "end")?.unwrap_or(0)),
            _ => (
                as_i64(&args[0], "start")?.unwrap_or(0),
                as_i64(&args[1], "end")?.unwrap_or(0),
            ),
        };
        let step = match args.get(2) {
            Some(v) => as_i64(v, "step")?.unwrap_or(1),
            None => 1,
        };
        if step == 0 {
            return Err("range() step must not be 0 (would never terminate)".to_string());
        }
        let mut items = Vec::new();
        if step > 0 {
            let mut i = start;
            while i < end {
                items.push(Value::Float(i as f64));
                i += step;
            }
        } else {
            // 负步长：`range(3, 0, -1)` 递减。旧实现 `i < end` 对负步长恒假，
            // 于是负步长一律静默返回空列表（同样是「静默降级」的一种）。
            let mut i = start;
            while i > end {
                items.push(Value::Float(i as f64));
                i += step;
            }
        }
        Ok(Value::List(items.into()))
    }

    pub(super) fn call_builtin_len(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let len = match args.first() {
            Some(Value::List(list)) => {
                testcase!(true, "len: list");
                list.len()
            }
            Some(Value::String(s)) => {
                // v0.104.6 修复：数**字符**，不是 UTF-8 字节。
                //
                // 旧实现是 `s.len()`（Rust 的字节长度），于是 `len("中文字")`
                // 得 **9**。这与本文件之外的字符串索引空间直接矛盾 ——
                // `index_value` 的字符串分支用 `s.chars().nth(i)`，是字符语义，
                // 合法下标只有 0..2。于是 `len(s)` 报告 9、`s[8]` 却越界，
                // `for i in range(0, len(s))` 会在 i=3 处炸。
                //
                // 字符串长度在所有主流语言里都是字符数（含 CJK 的语言无一例外），
                // 且 `s[0]` 已经按字符取，len 必须与它同口径。
                testcase!(true, "len: string");
                s.chars().count()
            }
            Some(Value::Dict(map)) => {
                testcase!(true, "len: dict");
                map.len()
            }
            _ => return Err("len() expects a list, string, or dict".to_string()),
        };
        Ok(Value::Int(len as i64))
    }

    /// v0.104.6：`str(x)` —— 值 → 显示字符串。
    ///
    /// **补的是一个先存后废的缺口**：`src/typeck/hm/builtin.rs` 早已把
    /// `"str" => α → String` 登记进 HM 内建签名表，于是**任何**用到
    /// `str(x)` 的程序都能过类型检查；运行期却没有对应实现 ——
    /// `dispatch.rs` 的 `match name` 里没有 `"str"` 分支，落到兜底的环境查找，
    /// 报 `Undefined function or task: str`。
    ///
    /// 之所以一直没人发现：`tests/tier0_replacement.rs::
    /// semantics_control_flow_runs_via_mir` 是**全仓库唯一**用到 `str()` 的地方，
    /// 而它那句 `print("sum=" + str(total))` 位于一个 `if total > 0 { ... }`
    /// 的汇合点之后 —— 执行器缺陷 E1（汇合点被未被选中的分支臂饿死）让这整段
    /// 尾部**从未执行过**，测试因此「通过」。E1 修好后这条语句第一次真正跑到，
    /// 缺口才暴露。
    ///
    /// 独立于控制流即可复现：`print(str(45))` 无任何分支，直接报同样的错。
    ///
    /// 语义与 `print` 对单个值的取字一致（`call_builtin_print` 同用
    /// `Value::to_string`），因此 `"x" + str(n)` 的拼接结果与 `print(x, n)` 一致。
    pub(super) fn call_builtin_str(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let v = args.first().cloned().unwrap_or(Value::Nil);
        testcase!(true, "str: any");
        Ok(Value::String(v.to_string()))
    }

    /// v0.104.6：`int(x)` —— 值 → 整数。
    ///
    /// 与 `str()` **完全同源**的缺口：`src/typeck/hm/builtin.rs:48` 早已登记
    /// `"int" => String → Int`，`src/typeck/dispatch.rs:177` 也登记了同名签名，
    /// 于是 `int("42")` 能过类型检查；运行期 `dispatch.rs` 的 `match name` 里
    /// 没有 `"int"` 分支，落到兜底环境查找，报 `Undefined function or task: int`。
    ///
    /// 与 `str()` 一样，它之所以一直藏着：全仓库没有任何测试调用过它。
    /// 本文件 `tests/builtin_gaps.rs` 是全仓库第一个用到 `int/float/bool` 的地方。
    ///
    /// 语义：字符串按十进制解析（允许前后空白与 `+`/`-`）；数值按**截断**
    /// （`int(4.7) = 4`，向零取整，与 Rust `as i64` 一致）；`Bool` 取 1/0；
    /// 其余（List/Dict/Nil/…）报明确错误而非静默给 0。
    pub(super) fn call_builtin_int(&mut self, args: Vec<Value>) -> Result<Value, String> {
        use num_traits::ToPrimitive;
        let v = args.first().cloned().unwrap_or(Value::Nil);
        let out = match v {
            Value::String(s) => {
                let t = s.trim();
                if let Ok(n) = t.parse::<i64>() {
                    n
                } else {
                    // 允许 "42.0" / "4.7"：先按 f64 解析再截断
                    t.parse::<f64>()
                        .map(|f| f as i64)
                        .map_err(|_| format!("int() cannot parse string: {s:?}"))?
                }
            }
            Value::Float(f) => {
                if !f.is_finite() {
                    return Err(format!("int() cannot convert non-finite float: {f}"));
                }
                f as i64
            }
            Value::Int(n) => n,
            Value::BigInt(n) => n
                .to_i64()
                .ok_or_else(|| format!("int() cannot convert bigint out of i64 range: {n}"))?,
            Value::Bool(b) => {
                if b {
                    1
                } else {
                    0
                }
            }
            other => {
                return Err(format!(
                    "int() does not accept {}",
                    crate::flow::type_name(&other)
                ));
            }
        };
        testcase!(true, "int: coercion");
        Ok(Value::Int(out))
    }

    /// v0.104.6：`float(x)` —— 值 → 浮点。与 `int()` 同源的缺口。
    pub(super) fn call_builtin_float(&mut self, args: Vec<Value>) -> Result<Value, String> {
        use num_traits::ToPrimitive;
        let v = args.first().cloned().unwrap_or(Value::Nil);
        let out = match v {
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("float() cannot parse string: {s:?}"))?,
            Value::Float(f) => f,
            Value::Int(n) => n as f64,
            Value::BigInt(n) => n
                .to_f64()
                .ok_or_else(|| format!("float() cannot convert bigint out of range: {n}"))?,
            Value::Bool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            other => {
                return Err(format!(
                    "float() does not accept {}",
                    crate::flow::type_name(&other)
                ));
            }
        };
        testcase!(true, "float: coercion");
        Ok(Value::Float(out))
    }

    /// v0.104.6：`bool(x)` —— 值 → 布尔。与 `int()` 同源的缺口。
    ///
    /// **直接委托 `flow::is_truthy`，不自建真值表。**
    ///
    /// v0.104.6 初版在这里手搓了一张表（`0.0` / `""` / `[]` / `{}` / `nil`
    /// 为假，其余按类型逐个判，未知类型报错）。实测与语言的真值判断在 **4 处**
    /// 分叉：
    ///
    /// ```text
    /// 0n (BigInt)      if 判真 = true    bool() = false   ← 判反了
    /// fn(x) x end      if 判真 = true    bool() 报错
    /// Router::new()    if 判真 = true    bool() 报错
    /// McpServer::new() if 判真 = true    bool() 报错
    /// ```
    ///
    /// 根因：`is_truthy` 的兜底臂是 `_ => true`（未知类型一律为真），而我
    /// 那张表对未知类型**报错**、又给 BigInt 单独判了一套。
    ///
    /// 而 `is_truthy` 自己的文档恰好写着「MIR 条件分支的**单一真值源**
    /// （v0.75.83 收敛）」，并警告「两处语义分叉是隐蔽 bug 温床」—— 我做的
    /// 正是它警告的那件事。委托即可，分叉从根上消失。
    /// 由 `tests/builtin_gaps.rs::bool_agrees_with_language_truthiness` 逐值钉住。
    pub(super) fn call_builtin_bool(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let v = args.first().cloned().unwrap_or(Value::Nil);
        testcase!(true, "bool: coercion");
        Ok(Value::Bool(crate::flow::is_truthy(&v)))
    }

    pub(super) fn call_builtin_compose(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose() requires at least 1 argument".to_string());
        }
        // 返回一个特殊的 Compose 值
        Ok(Value::Compose(args))
    }

    pub(super) fn call_builtin_partial(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("partial() requires at least 1 argument (the function)".to_string());
        }
        let func = args[0].clone();
        let partial_args: Vec<Value> = args[1..].to_vec();
        Ok(Value::Partial(Box::new(func), partial_args))
    }

    pub(super) fn call_builtin_atom(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().cloned().unwrap_or(Value::Nil);
        Ok(Value::Atom(Arc::new(Mutex::new(value))))
    }

    pub(super) fn call_builtin_swap(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("swap() requires 2 arguments: atom and function".to_string());
        }
        match &args[0] {
            Value::Atom(arc) => {
                let func = &args[1];
                let old = arc.lock().clone();
                let new_val = self.call_value(func, vec![old], effects)?;
                *arc.lock() = new_val.clone();
                Ok(new_val)
            }
            _ => Err("swap() first argument must be an atom".to_string()),
        }
    }

    pub(super) fn call_builtin_deref(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("deref() requires 1 argument")?;
        match value {
            Value::Atom(arc) => Ok(arc.lock().clone()),
            _ => Err("deref() argument must be an atom".to_string()),
        }
    }

    pub(super) fn call_builtin_type_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("type_of() requires 1 argument")?;
        Ok(Value::String(value_type_name(value).to_string()))
    }

    pub(super) fn call_builtin_is_instance(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("is_instance() requires 2 arguments".to_string());
        }
        let value = &args[0];
        let type_name = match &args[1] {
            Value::String(s) => s.as_str(),
            _ => return Err("is_instance() second argument must be a string".to_string()),
        };
        Ok(Value::Bool(value_type_name(value) == type_name))
    }

    pub(super) fn call_builtin_methods_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("methods_of() requires 1 argument")?;
        let methods = value.methods();
        Ok(Value::List(
            methods.into_iter().map(Value::String).collect(),
        ))
    }

    pub(super) fn call_builtin_compress(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("compress() requires 2 arguments: input and strategy".to_string());
        }
        let strategy = match &args[1] {
            Value::String(s) => s.clone(),
            other => {
                return Err(format!(
                    "compress: strategy must be a string, got {:?}",
                    other
                ));
            }
        };
        let options_val = args
            .get(2)
            .cloned()
            .unwrap_or(Value::Dict(Default::default()));
        let opts_base =
            crate::compress::options_from_value(&options_val).map_err(|e| e.to_string())?; // v0.75.99: MoraError → String 转换
        let opts = crate::compress::CompressOptions {
            strategy: strategy.clone(),
            ..opts_base
        };
        crate::compress::compress_top(&args[0], &strategy, &opts).map_err(|e| e.to_string())
    }

    pub(super) fn call_builtin_crush_json(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("crush_json() requires 2 arguments: input and max".to_string());
        }
        // v0.104.6 D262：此前只匹配 `Value::Float`。而本仓数字有两个来源 ——
        // 字面量给 `Float`（D98）、`len()` 等运算给 `Int`（D129）⇒
        // `crush_json(xs, len(xs))` 落在 `other` 分支，报
        //   "crush_json: max must be a number, got int"
        // —— **「int 明明是数字」**（与 D249 的 `with temperature` 同型）。
        //
        // 走 D246 立的收口 `flow::value_as_usize`，但**保留两种错误的区分**
        // （D259 教训：收口不该顺手抹掉诊断信息）。
        let max_items = match &args[1] {
            Value::Int(n) if (*n as f64) < 0.0 => {
                return Err("crush_json: max must be non-negative".to_string());
            }
            Value::Float(n) if *n < 0.0 => {
                return Err("crush_json: max must be non-negative".to_string());
            }
            v => match crate::flow::value_as_usize(v) {
                Some(n) => n,
                None => {
                    // v0.104.6：`{:?}` → 类型名。`Value::Dict` 的 `Debug` 按
                    // HashMap 迭代序打印（每进程随机），同一条错误信息跨进程会
                    // 键序不同。本会话早前已修 Display / JSON / keys / values 的
                    // 同类问题，`{:?}` 这条路这里漏了。
                    return Err(format!(
                        "crush_json: max must be a number, got {}",
                        crate::flow::type_name(v)
                    ));
                }
            },
        };
        let options_val = args
            .get(2)
            .cloned()
            .unwrap_or(Value::Dict(Default::default()));
        let opts = crate::compress::options_from_value(&options_val).map_err(|e| e.to_string())?; // v0.75.99: MoraError → String 转换
        let items = match &args[0] {
            Value::List(l) => l.clone(),
            _ => {
                return Err("crush_json: expected List as first argument".to_string());
            }
        };
        let result = crate::compress::crush_json(&items.to_vec(), max_items, &opts);
        let json = crate::compress::value_to_json_simple(&Value::List(result.items.clone().into()));
        Ok(Value::String(format!(
            "{}\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
            json, result.strategy_used, result.items_kept, result.items_total, result.savings_ratio
        )))
    }

    pub(super) fn call_builtin_batch_chat(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let prompts = args
            .first()
            .ok_or("batch_chat() requires 1 argument (list of prompts)")?;
        match prompts {
            Value::List(items) => {
                let mut results = Vec::new();
                for item in items {
                    let prompt = match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let model = std::env::var(AI_MODEL_ENV)
                        .unwrap_or_else(|_| AI_MODEL_DEFAULT.to_string());
                    let result = Self::do_ai_chat(self, &model, &prompt)?;
                    results.push(result);
                }
                Ok(Value::List(results.into()))
            }
            _ => Err("batch_chat() argument must be a list".to_string()),
        }
    }

    pub(super) fn call_builtin_into(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("into() requires 2 arguments: collection and function".to_string());
        }
        let collection = args[0].clone();
        let transform = args[1].clone();
        match collection {
            Value::List(list) => {
                let mut result = Vec::new();
                for item in list {
                    let mapped = self.call_value(&transform, vec![item], effects)?;
                    match mapped {
                        Value::List(items) => result.extend(items),
                        other => result.push(other),
                    }
                }
                Ok(Value::List(result.into()))
            }
            _ => Err("into() first argument must be a list".to_string()),
        }
    }

    pub(super) fn call_builtin_tail(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("tail() requires 2 arguments: path and max".to_string());
        }
        let path = match &args[0] {
            Value::String(s) => s.clone(),
            other => {
                return Err(format!(
                    "tail() first argument must be a string path, got {:?}",
                    other
                ));
            }
        };
        // v0.104.6 D153：此前只匹配 `Float`，`Int` 实参报「second argument 'max'
        // must be a number」—— 而 `Int` 在本语言里**就是**数字类型，这条消息误导。
        // **更正**：本方法在 D148 的饱和转换普查表里被记为「本就正确 · 作样板」，
        // 那行判断是错的 —— 它只审了**负数**那一侧，没审**类型**这一侧。
        let max = crate::interpreter::builtins::required_num_arg(
            &args,
            1,
            "tail()",
            "second argument 'max'",
        )?;
        if max < 0.0 {
            return Err("tail() max must be non-negative".to_string());
        }
        let max = max as usize;
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("tail() cannot read '{}': {}", path, e))?;
        let lines: Vec<&str> = content.lines().collect();
        let start = if lines.len() > max {
            lines.len() - max
        } else {
            0
        };
        let tail_str = lines[start..].join("\n");
        Ok(Value::String(tail_str))
    }

    /// v0.103 修复：section 从**执行环境**解析（与 `h_prompt_section` 写入的
    /// 环境同一处）。此前读 `self.core.environment` —— 而 `take_env` 已把
    /// 宿主的该字段取空交给执行路径，故 `prompt "x" do ... end` 定义的
    /// section 永远查不到（报 "section 'x' not defined"，错误消息还指引
    /// 用户写当时 parser 不产出的语法）。与 eval/macroexpand 同一 env 穿线模式。
    pub(super) fn call_builtin_compose_prompt(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
    ) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose_prompt() requires at least 1 section".to_string());
        }
        let mut buf = String::new();
        for arg in args {
            let (name, role, text, budget_bytes) = match arg {
                Value::String(section_name) => {
                    // 从环境查 section（v0.95: 纯值只读查询，无锁）
                    let looked_up = env.get(&section_name);
                    match looked_up {
                        Some(Value::PromptSection {
                            name,
                            role,
                            text,
                            budget_bytes,
                        }) => (name, role, text, budget_bytes),
                        Some(other) => {
                            return Err(format!(
                                "compose_prompt: '{}' is not a prompt section (got {:?})",
                                section_name, other
                            ));
                        }
                        None => {
                            return Err(format!(
                                "compose_prompt: section '{}' not defined (use 'prompt \"{}\" do ... end' first)",
                                section_name, section_name
                            ));
                        }
                    }
                }
                Value::Dict(map) => {
                    let role = map.get("role").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    let text_val = map
                        .get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new()));
                    let budget = if let Some(b) = map.get("budget") {
                        Some(super::numeric_helpers::parse_budget_dispatch(
                            b.clone(),
                            "budget",
                        )?)
                    } else {
                        None
                    };
                    ("<inline>".to_string(), role, Box::new(text_val), budget)
                }
                Value::PromptSection {
                    name,
                    role,
                    text,
                    budget_bytes,
                } => (name, role, text, budget_bytes),
                other => {
                    return Err(format!(
                        "compose_prompt: section must be name, dict, or PromptSection (got {:?})",
                        other
                    ));
                }
            };
            // 应用 budget 截断
            let resolved_text = super::numeric_helpers::text_to_string(&text);
            let truncated = match budget_bytes {
                Some(b) if resolved_text.len() > b => {
                    let mut t = resolved_text.into_bytes();
                    t.truncate(b);
                    String::from_utf8_lossy(&t).into_owned()
                }
                _ => resolved_text,
            };
            // 拼接
            if let Some(r) = &role {
                buf.push_str(&format!("\n## {} ({})\n\n", name, r));
            } else {
                buf.push_str(&format!("\n## {}\n\n", name));
            }
            buf.push_str(&truncated);
        }
        Ok(Value::String(buf))
    }
    pub(super) fn call_builtin_eval(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        // v0.86: runtime eval — 从 Mora 代码内部动态执行任意 Mora 源码。
        // 这是 Lisp homoiconicity + eval-apply loop 在 Mora 上的落地：
        //   eval("2 + 3") -> Value::Int(5)
        //   eval("function foo(x) x*x end") -> Value::Task/Closure
        // 实现路径：source string -> Lexer -> ParserV3::compile -> run_mir。
        // 执行环境：子 Environment 以当前调用栈的 env 为 parent（用 parking_lot::Mutex 与
        // value.rs 中的 Environment 类型一致），这样 eval 内部可以读取当前 scope
        // 的变量（如 task 内的 let 绑定），但无法修改外层绑定。
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Code(s)) => s.clone(),
            _ => {
                return Err(
                    "eval(source: string|code) expects a string or code argument".to_string(),
                );
            }
        };
        let (func, _witnesses) = match crate::parser_v3::ParserV3::compile(&source) {
            Ok(f) => f,
            Err(e) => return Err(format!("eval: compile error: {}", e)),
        };
        let func = std::sync::Arc::new(func);
        let mut child_env = Environment::with_parent_of(std::sync::Arc::new(env.clone()));
        crate::mir::vm::run_mir(&func, self, &mut child_env, effects)
    }

    // ===================================================================
    // v0.86: Lisp homoiconicity — apply / curry / uncurry / cons / car / cdr
    // ===================================================================

    /// v0.86: apply(fn, [args...]) — Lisp apply。将 args 列表展开为独立参数
    /// 并调用 fn。这是 eval-apply loop 的另一半（eval 已完成）。
    /// 示例：apply(sum, [1, 2, 3]) == sum(1, 2, 3)
    pub(super) fn call_builtin_apply(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("apply(fn, [args...]) expects at least 2 arguments".to_string());
        }
        let fn_val = &args[0];
        let arg_list = &args[1];
        // 提取待展开的参数列表
        let expanded: Vec<Value> = match arg_list {
            Value::List(items) => items.to_vec(),
            // 单个元素也算作一个参数的"列表"
            other => vec![other.clone()],
        };
        self.call_value(fn_val, expanded, effects)
    }

    /// v0.86: curry(fn, arity) — 柯里化包装器。
    /// 返回 Value::Curry(func, arity, bound_args=[])。
    /// 调用时若参数数 < arity，返回新的 Curry（累积参数）；
    /// 若参数数 >= arity，调用内部 fn。
    /// 示例：
    ///   let f = curry(sum3, 3)
    ///   f(1)(2)(3) -> 6
    ///   curry(add, 2)(1)(2) -> 3
    pub(super) fn call_builtin_curry(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("curry(fn, arity) expects fn and arity".to_string());
        }
        let fn_val = args[0].clone();
        // v0.104.6 D148：负数 arity 必须报错。
        //
        // ⚠ 下方 `arity == 0` 的既有检查**只挡住了 Float 一侧**：
        //   `Value::Float(-1.0) as usize` → 饱和成 0     → 被 `arity == 0` 挡住 ✓
        //   `Value::Int(-1)   as usize` → **截断**成 usize::MAX → 绕过检查
        // 于是 `curry(f, -1)` 得到一个**永远凑不齐参数**的 Curry（调用多少次
        // 都不返回值），exit 0、零诊断 —— 静默挂死。
        let arity = match &args[1] {
            Value::Int(n) if *n < 0 => {
                return Err(format!("curry: arity 不能为负数（得到 {n}）"));
            }
            Value::Float(n) if *n < 0.0 => {
                return Err(format!("curry: arity 不能为负数（得到 {n}）"));
            }
            Value::Int(n) => *n as usize,
            Value::Float(n) => *n as usize,
            _ => return Err("curry: arity must be an integer".to_string()),
        };
        if arity == 0 {
            return Err("curry: arity must be > 0".to_string());
        }
        Ok(Value::Curry {
            func: Box::new(fn_val),
            arity,
            bound_args: Vec::new(),
        })
    }

    /// v0.86: uncurry(curried_fn) — 解柯里化。
    /// 若参数是 Curry，返回内部原始 fn；否则原样返回。
    pub(super) fn call_builtin_uncurry(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("uncurry(fn) expects a function argument".to_string());
        }
        match &args[0] {
            Value::Curry { func, .. } => Ok((**func).clone()),
            _ => Ok(args[0].clone()),
        }
    }

    /// v0.86: cons(car, cdr) — Lisp cons cell 构造器。
    /// 返回 Value::Cons(car, cdr)。
    /// 示例：cons(1, cons(2, Nil)) -> (1 . (2))
    pub(super) fn call_builtin_cons(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("cons(car, cdr) expects 2 arguments".to_string());
        }
        Ok(Value::Cons {
            car: Box::new(args[0].clone()),
            cdr: Box::new(args[1].clone()),
        })
    }

    /// v0.86: car(cell) — 提取 cons cell 的头部。
    /// 对 List 退化为取第一个元素，方便过渡。
    pub(super) fn call_builtin_car(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("car(cell) expects a cons cell or list".to_string());
        }
        match &args[0] {
            Value::Cons { car, .. } => Ok((**car).clone()),
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| "car: empty list has no first element".to_string()),
            Value::Nil => Err("car: cannot take car of Nil".to_string()),
            _ => Err(format!("car: expected cons cell or list, got {}", args[0])),
        }
    }

    /// v0.86: cdr(cell) — 提取 cons cell 的尾部。
    /// 对 List 退化为取除第一个元素外的剩余列表。
    pub(super) fn call_builtin_cdr(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("cdr(cell) expects a cons cell or list".to_string());
        }
        match &args[0] {
            Value::Cons { cdr, .. } => Ok((**cdr).clone()),
            Value::List(items) => Ok(Value::List(items.slice(1, items.len()))),
            Value::Nil => Err("cdr: cannot take cdr of Nil".to_string()),
            _ => Err(format!("cdr: expected cons cell or list, got {}", args[0])),
        }
    }

    /// v0.86: quote(s) — Lisp homoiconicity 的第三块基石。
    ///
    /// 将源码文本字符串包装为 Value::Code，与 eval 形成往返对：
    ///   eval(quote(expr)) == expr
    ///   quote(expr) -> Value::Code("expr")
    ///
    /// 注意：`quote(expr)` 语法在解析期已由 emit_quote_w 提取源码文本
    /// 并以 MirInst::Call(dst, "quote", [Const(String(expr))]) 的形式 emit。
    /// 本函数是运行时入口：把 Value::String 转换为 Value::Code。
    pub(super) fn call_builtin_quote(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            _ => {
                return Err("quote(source: string) expects a string argument".to_string());
            }
        };
        Ok(Value::Code(source))
    }

    // ===================================================================
    // v0.87: Lisp homoiconicity 三件套补完 — gensym / read / macroexpand
    // ===================================================================

    /// v0.87: gensym() — 生成唯一符号名。
    ///
    /// Lisp 宏系统的核心原语：保证宏展开时引入的新变量名不与用户代码冲突。
    /// Mora 无 AST 架构下，gensym 返回一个 `Value::String`，形式为 `"g{n}"`，
    /// 保证同一 Interpreter 实例内唯一（gensym_counter 在 CoreRuntime 上，
    /// v0.95 起是纯 `usize`，`&mut self` 递增；Pregel worker 各自独立）。
    ///
    /// 用法：
    ///   let fresh = gensym()   → "g0"
    ///   let fresh2 = gensym()  → "g1"
    ///
    pub(super) fn call_builtin_gensym(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("gensym() expects no arguments".to_string());
        }
        let n = self.core.gensym_counter;
        self.core.gensym_counter += 1;
        Ok(Value::String(format!("g{n}")))
    }

    /// v0.87: read(code_str) — 解析源码字符串为 Value::Code。
    ///
    /// 与 quote(expr) 语义等价（Mora 无 AST，两者都是把源码文本作为可执行代码载体）。
    /// read 是 Lisp 传统的 Reader 层函数（字符流 → 数据），在 Mora 中简化为
    /// "字符串 → Value::Code" 标记。与 eval() 配对使用：eval(read(code_str)) 等价于 eval(code_str)。
    ///
    /// 用法：
    ///   let code = read("2 + 3")     → Value::Code("2 + 3")
    ///   eval(read("2 + 3"))          → Value::Int(5)
    ///   eval(read(read("2 + 3")))    → 嵌套：read 返回 Code，eval 接受 Code
    ///
    pub(super) fn call_builtin_read(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Code(s)) => s.clone(),
            _ => {
                return Err(
                    "read(source: string|code) expects a string or code argument".to_string(),
                );
            }
        };
        Ok(Value::Code(source))
    }

    /// v0.87: macroexpand(name, args...) — 展开宏并返回求值结果。
    ///
    /// Mora 宏是 by-example（运行时求值）而非 compile-time source 变换。
    /// macroexpand 执行宏 body（以 args 绑定 params），返回结果作为"展开值"。
    /// 这对应 CL 中 macroexpand 求值展开形式的语义。
    ///
    /// 用法：
    ///   macro add(a, b)  a + b  end
    ///   macroexpand("add", [1, 2])   → Value::Int(3)
    ///
    ///   macro when(cond, body)  if cond { body } end
    ///   macroexpand("when", [true, 42])  → Value::Int(42)
    ///
    /// 错误路径：
    ///   - 未定义宏 → "macroexpand: undefined macro 'name'"
    ///   - 参数不足 → 缺失参数绑定为 Nil
    ///   - 宏执行错误 → "macro 'name' execution error: ..."
    ///
    pub(super) fn call_builtin_macroexpand(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        let name = match args.first() {
            Some(Value::String(s)) => s.clone(),
            _ => {
                return Err("macroexpand(name: string, args...) expects a string name".to_string());
            }
        };
        let expr_args: Vec<Value> = if args.len() > 1 {
            match &args[1] {
                Value::List(items) => items.to_vec(),
                other => vec![other.clone()],
            }
        } else {
            Vec::new()
        };

        let macro_val = env
            .get(&name)
            .ok_or_else(|| format!("macroexpand: undefined macro '{}'", name))?;

        match macro_val {
            Value::Macro {
                name: mname,
                params,
                body,
            } => {
                // v0.104.6 D50：arity 校验。此前缺参静默填 Nil（症状是宏体里
                // 冒出「Operands must be two numbers...」这类**误导性**错误），
                // 多余实参**静默丢弃**。与 D47 的 task / D48 的 reduce 同型。
                if expr_args.len() != params.len() {
                    return Err(format!(
                        "macroexpand: macro '{}' expects {} args, got {}",
                        mname,
                        params.len(),
                        expr_args.len()
                    ));
                }
                let mut child_env = Environment::with_parent_of(std::sync::Arc::new(env.clone()));
                for (i, param) in params.iter().enumerate() {
                    let val = expr_args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(&body, self, &mut child_env, effects)
                    .map_err(|e| format!("macro '{}' expansion error: {}", mname, e))
            }
            _ => Err(format!("macroexpand: '{}' is not a macro", name)),
        }
    }

    pub(super) fn call_builtin_fallback(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        let looked_up = env.get(name).clone();
        if let Some(value) = looked_up {
            match value {
                Value::Task { .. }
                | Value::Closure { .. }
                | Value::Compose(_)
                | Value::Partial(_, _)
                | Value::Curry { .. } => self.call_value(&value, args, effects),
                // v0.83: 宏展开 — 完整实现。宏体是 MIR 函数（由 parser emit_macro_def_w
                // 经子 EmitContext 编译而来），调用时以 args 绑定 params，在子 env 中
                // run_mir 执行 body（与 Value::Task 语义同构）。
                Value::Macro {
                    name: mname,
                    params,
                    body,
                } => {
                    // v0.104.6 D47：宏体与 task/closure 同构，此前同样
                    // **静默**按 params 逐个绑定、缺参填 Nil、多参丢弃。
                    if args.len() != params.len() {
                        return Err(format!(
                            "macro '{}' expects {} args, got {}",
                            mname,
                            params.len(),
                            args.len()
                        ));
                    }
                    // v0.86: 宏体执行的 parent 环境改为当前调用栈 env（而非全局环境），
                    // 这样宏可以引用同级定义的其他宏和变量（如 square 调 mul）。
                    let mut child_env =
                        Environment::with_parent_of(std::sync::Arc::new(env.clone()));
                    for (i, param) in params.iter().enumerate() {
                        let val = args.get(i).cloned().unwrap_or(Value::Nil);
                        child_env.define(param.clone(), val, false);
                    }
                    crate::mir::vm::run_mir(&body, self, &mut child_env, effects)
                        .map_err(|e| format!("macro '{}' execution error: {}", mname, e))
                }
                // v0.104.6：模块前缀被当自由函数调（`math(2.5)`）时给一条
                // 能指路的错误。走到这里说明按名字取到的**不是**可调用值；
                // 若它恰好是 `MODULE_OBJECTS` 里的模块对象，那就是「用错了
                // 调用形式」，而不是「这个模块不能当函数用」——
                // 说清区别能省掉一次「为什么 math.floor 行、math 不行」的困惑。
                _ if crate::value::MODULE_OBJECTS
                    .iter()
                    .any(|(m, _)| *m == name.split('.').next().unwrap_or(name)) =>
                {
                    Err(format!(
                        "'{name}' is a module, not a function — call a method on it \
                         (e.g. {name}.<method>(...)), not {name}(...)"
                    ))
                }
                _ => Err(format!("'{}' is not callable", name)),
            }
        } else {
            Err(format!("Undefined function or task: {}", name))
        }
    }
}
