//! v0.55: Builtin dispatch table.
//!
//! This module is the single source of truth for built-in functions,
//! type-level operators (binary, comparison), and method-dispatch
//! tables. The v0.55 Hindley-Milner engine in [`crate::typeck::hm`]
//! consumes the `Signatures` so a `Router::new()` registered here is
//! recognized by the checker.
//!
//! v0.75.47: 移除对已删除 v2 `TypeChecker` 的引用（v0.55 去 AST 化时
//! 删除，注释残留；此处不再引该类型以免 rustdoc broken link）。
//!
//! It is also where `method_return_type` lives, which used to live in
//! `typeck::mod` and was the source of a back-reference from
//! `typeck::hm`. Moving it here keeps the dependency direction clean:
//! `typeck` depends on `typeck::dispatch`, never the other way around.

use crate::typeck::Type;

///  Function signature used by the v0.55 builtin registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// v0.x parameter list as `(name, type)` pairs.
    pub params: Vec<(String, Type)>,
    /// v0.10 raw hint strings (e.g. "T" → number).
    pub raw_params: Vec<Option<String>>,
    pub return_type: Type,
    /// v0.10 raw return hint.
    pub raw_return_type: Option<String>,
    /// v0.103: **变参** —— `params` 描述「至少这么多」，可续传同类型实参。
    ///
    /// `print` 是典型：运行期 `call_builtin_print` 把**全部**实参
    /// `join("\t")` 后输出，而签名只声明 1 个参数 → `builtin_callee_ty`
    /// 生成固定 arity 的 curried arrow → `print("a", b)` 在类型检查期报
    /// "expected nil, got fn(string) -> …"（多余实参无法消耗 Arrow 层）。
    /// 运行期支持、类型系统拒绝 = 契约分叉。
    pub variadic: bool,
}

impl Signature {
    /// Build a signature with no raw-hint metadata (used by HM).
    pub fn new(params: Vec<(String, Type)>, return_type: Type) -> Self {
        let raw_params = params.iter().map(|_| None).collect();
        Self {
            params,
            raw_params,
            return_type,
            raw_return_type: None,
            variadic: false,
        }
    }

    /// v0.103: 变参签名 —— 末位参数类型可重复（`print(...)`）。
    pub fn variadic(params: Vec<(String, Type)>, return_type: Type) -> Self {
        let mut sig = Self::new(params, return_type);
        sig.variadic = true;
        sig
    }
}

///  Return the canonical builtin registry. This is a flat snapshot;
///  the underlying collection is intentionally immutable from the
///  outside so both checkers can read it without contention.
pub fn builtin_signatures() -> Vec<(String, Signature)> {
    vec![
        // v0.75.23: M 原语 merge_with(key, strategy) — 声明 per-key CRDT
        // 合并策略（append/add/dict_union/grow_only_set/lww），作用于其后的
        // worker/transaction/observe 块合并。
        // v0.102: 声明式范式目标内建（unify/cons 在 infer_call 特判获得
        // 更精确的类型约束；both/either/fail/succeed 走通用表签名）
        //
        // v0.104.6 D216：`both` / `either` 改为**变参**。
        //
        // 运行期 `call_builtin_both` / `call_builtin_either` 接受**任意个数**
        // 的 goal（`goals_from_args` 对 `args` 逐个收集，≥1 即可），
        // 而此前签名表只声明**恰好 2 个**参数 ⇒ 3 个及以上实参被拒：
        //
        // ```text
        // solve { p(?X, ?Y), q(?Y, ?Z), r(?Z, ?W) }
        //   → Type error: expected goal, got fn (…) -> …
        // ```
        //
        // 即 D216 的修复（多 goal 必须合取）若不改这里，就只对**恰好 2 个**
        // goal 有效 —— 而三表 join 才是常见情形。属 D100 同族的
        // 「typeck 声明窄于运行期」，且是**同一个缺陷面的另一半**。
        (
            "both".to_string(),
            Signature {
                params: vec![("g".to_string(), Type::Goal)],
                raw_params: vec![None],
                return_type: Type::Goal,
                raw_return_type: None,
                variadic: true,
            },
        ),
        (
            "either".to_string(),
            Signature {
                params: vec![("g".to_string(), Type::Goal)],
                raw_params: vec![None],
                return_type: Type::Goal,
                raw_return_type: None,
                variadic: true,
            },
        ),
        ("fail".to_string(), Signature::new(vec![], Type::Goal)),
        ("succeed".to_string(), Signature::new(vec![], Type::Goal)),
        // project(f, arg, result) — f 的效果行在 infer_call 特判并入
        (
            "project".to_string(),
            Signature::new(
                vec![
                    ("f".to_string(), Type::Any),
                    ("arg".to_string(), Type::Any),
                    ("result".to_string(), Type::Any),
                ],
                Type::Goal,
            ),
        ),
        // v0.104.6 D80：`compose(f1, f2, …)` / `partial(fn, args…)`
        //
        // spec §12 只写 `...closure -> compose` / `closure, ...any -> partial`
        // —— **无固定上界**。故必须走 `Signature::variadic`：`infer_call` 的
        // 变参分支逐实参校验（末位类型是 `Any` → 恒过）且**完全不设上界**。
        //
        // v0.104.6 D79 曾在 `builtin_callee_ty` 里用「额外挂 2 层 `Any`
        // slack」的 curried arrow 声明它们，安全扫查出那是**回归**：
        // `compose(1, 2, 3, 4)` / `partial(1, 2, 3, 4, 5)` 实测 exit 2
        // （`expected compose, got fn (float) -> …`）—— slack 用尽后，多传的
        // 实参被拿去和**返回类型**比对。固定 slack 治不了无上界的变参。
        (
            "compose".to_string(),
            Signature {
                params: vec![("f".to_string(), Type::Any)],
                raw_params: vec![None],
                return_type: Type::Compose,
                raw_return_type: None,
                variadic: true,
            },
        ),
        (
            "partial".to_string(),
            Signature {
                params: vec![("f".to_string(), Type::Any)],
                raw_params: vec![None],
                return_type: Type::Partial,
                raw_return_type: None,
                variadic: true,
            },
        ),
        (
            "merge_with".to_string(),
            Signature::new(
                vec![
                    ("key".to_string(), Type::String),
                    ("strategy".to_string(), Type::String),
                ],
                Type::Nil,
            ),
        ),
        // v0.104.6 D68：`compose_prompt(...sections) -> String`
        //
        // 运行期 `Interpreter::new()` 把它 define 进 globals
        // （`interpreter/mod.rs:499-503`），但 typeck 侧此前**完全没有登记**
        // → `builtin_callee_ty` 返 `None` → 调用结果类型是永不解算的
        // TypeVar → `let v: Int = compose_prompt("s")` 被接受（错得离谱）。
        //
        // 它是**真变参**（`call_builtin_compose_prompt` 里 `for arg in args`），
        // curried arrow 表达不了，故置 `variadic` 走 `infer.rs` 的变参分支。
        // `params` 描述「至少这么多」，末位类型可重复。
        (
            "compose_prompt".to_string(),
            Signature {
                params: vec![("section".to_string(), Type::String)],
                raw_params: vec![None],
                return_type: Type::String,
                raw_return_type: None,
                variadic: true,
            },
        ),
        // v0.13: print(x) accepts any printable primitive and returns nil.
        // v0.103: 变参 —— 运行期 call_builtin_print 输出**全部**实参
        //（join("\t")），签名此前只声明 1 个 → `print("a", b)` 被类型检查拒绝。
        // v0.103: 补 BigInt + Int —— v0.91 引入 BigInt 变体时漏加，且 Int
        // 也不在名单里（仅靠 Int<:Float 子类型侥幸通过），
        // `print(999n)` 报 "expected string|float|…, got bigint"。
        //
        // v0.104.6: 补齐其余**有 `Value` 变体且 Display 有专门实现**的类型。
        //
        // 此前这个 Union 只列了 9 个原始类型，于是「打印一个服务器对象」这种
        // 最基本的调试写法被类型检查挡住（真实 CLI `mora run` 实测）：
        //     let r = Router::new()      → print(r) exit 2
        //                                      "expected string|int|… , got router"
        //     let m = McpServer::new()   → print(m) exit 2（同上，got mcp_server）
        // 而运行期**明明能打印** —— `call_builtin_print` 对每个实参调
        // `Value::to_string()`，`value/display.rs` 为 Router / McpServer /
        // Agent / Conversation / Stream / Task / Closure / Builtin 都写了
        // 专门的 Display 臂。旁证：`str(r)` 一直好使，输出 `<router (0 routes)>`。
        //
        // 也就是说 typeck 的声明又一次窄于运行期的实际能力（与 `ai.chat`
        // 声明 `-> AiResult` 那一族同源）。往 Union 里加类型是**放宽**，
        // 不会拒掉原先能通过的任何写法。
        //
        // v0.104.6 D214 复核（`--check` 实测，四条全 exit 0）：
        //     let r = Router::new()                     → exit 0
        //     print(r)                                  → exit 0
        //     let m = McpServer::new()                  → exit 0
        //     m = m.tool("t", {}, fn(a) return 1 end)  → exit 0
        // 即上面这段「修前现象」描述的确实是**已修好**的历史状态，不是现状。
        // 之所以仍留在这里：它记下了「为什么往 Union 里加类型」——
        // 那是本条 Union 的存在理由，删掉会让后来者以为只是随手放宽。
        // （同期我曾把一段 `Router::new("127.0.0.1", 8931)` 的探针脚本失败
        //  误记成「typeck 声明窄于运行期」——`Router::new()` **不收参数**，
        //  那个 exit 2 是**正确拒绝**，不是缺陷。教训：探针脚本自身出错时，
        //  先用 `--check` 复核再用它下结论。）
        (
            "print".to_string(),
            Signature {
                params: vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::Int,
                        Type::Float,
                        Type::BigInt,
                        Type::Bool,
                        Type::Char,
                        Type::Nil,
                        Type::List(Box::new(Type::Union(vec![]))),
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                        // v0.104.6 新增：运行期可 Display、此前被误拒的类型。
                        Type::Task,
                        Type::Closure,
                        Type::Builtin,
                        Type::Conversation,
                        Type::Stream,
                        Type::Agent,
                        Type::Router,
                        Type::McpServer,
                        // 以下两个当前**无构造入口**（同 `Value::AiConfig` 一族），
                        // 先补上免得将来接线时又撞一次。
                        Type::AiConfig,
                        Type::HttpRequest,
                        // v0.104.6 D11 续：机械比对 `value/display.rs` 的
                        // Display 臂与本 Union 后补齐的 9 个 —— `Type` 里
                        // **已有**这些变体，只是 Union 漏了，于是
                        // `print(succeed())` 被拒（实测 exit 2
                        // "expected string|int|…, got goal"），而运行期
                        // `Display` 明明能打（`Value::Goal` → `<goal>`）。
                        // 声明式范式（spec §12）尤其受影响：goal 可以拿去做
                        // `both()` / `solve()`，却**打不出来**。
                        Type::Goal,
                        // 带参数的变体用「任意参数」形态入 Union：
                        // `Relation(vec![])` = 任意关系，`Cons(Any, Any)` = 任意 cons
                        Type::Relation(vec![]),
                        Type::Cons(Box::new(Type::Any), Box::new(Type::Any)),
                        Type::Atom,
                        Type::Compose,
                        Type::Partial,
                        Type::Macro,
                        Type::PromptSection,
                        Type::Document,
                        // v0.104.6 D77：补 TEA / dyn 三兄弟。
                        //
                        // 此前上方那条注释把 `TeaApp` / `TeaMsg` / `TraitObject`
                        // 列进「`Type` 里没有对应变体」的名单 —— **该判断已过期**：
                        // 三个变体都早已存在（`TeaApp` / `TeaMsg` 由 TEA 引入，
                        // `TraitObject` 由 v0.08 的 dyn 引入），且 `Value` 侧都有
                        // Display 臂（`value/display.rs:99` `TraitObject`、
                        // `:155` `<tea_app>`、`:168` `Msg(…)`）—— 运行期**明明
                        // 能打印**。缺的只是 Union 里这一项。
                        //
                        // 真实症状（`tea.init()` 刚在 D73 被声明成
                        // `Type::TeaApp`，于是立刻撞上）：
                        //     print(tea.init())
                        //     → Type error: expected string | int | float | … ,
                        //       got TeaApp
                        Type::TeaApp {
                            name: String::new(),
                            model: Box::new(Type::Any),
                            msg: Box::new(Type::Any),
                            update: Box::new(Type::Any),
                            view: Box::new(Type::Any),
                        },
                        // `TeaMsg { name, variants }` —— 字段形态以
                        // `typeck/mod.rs` 为准，用「任意消息类型」形态入 Union。
                        Type::TeaMsg {
                            name: String::new(),
                            variants: vec![],
                        },
                        // dyn dispatch 的运行时载体
                        Type::TraitObject {
                            trait_name: String::new(),
                            generics: vec![],
                        },
                        // ⚠ 真正**缺 `Type` 变体**的 5 个（`Value` 有 Display 臂、
                        // `typeck::Type` 的 47 个变体里没有）：需要先扩类型系统
                        // 枚举，那是 v1.0 方向的设计决定，不在缺陷修复范围内：
                        //   LogicVar / Code / Curry / Tool / TeaCmd
                        // 补变体时记得同步 `typeck/mod.rs` 的 `name()` 与
                        // 「字符串 → Type」映射。
                    ]),
                )],
                raw_params: vec![None],
                return_type: Type::Nil,
                raw_return_type: None,
                variadic: true,
            },
        ),
        // range(start, end, step) -> list<int>
        //
        // v0.104.6：元素类型 `Type::Int` → `Type::Float`。运行期
        // `call_builtin_range` 推的是 `Value::Float(i as f64)`（实测
        // `range(0, 3)` = `List([Float(0.0), Float(1.0), Float(2.0))]`），
        // 声明与实际不符。
        //
        // ⚠ v0.104.6 D87：下面这段「实测未能构造出用户可见的失败」**已过期**。
        // 原文称 `let n: Int = r[0]` 与 `let n: Float = r[0]` 都通过检查、
        // 「本语言 Int/Float 在 typeck 里可互换」—— 那是**在 D67 修好容器元素
        // 类型之前**的状态。D67 之后 `range(0, 3)` 真的推成 `List(Float)`，
        // 复核结果：
        //     let r = range(0, 3)
        //     let n: Int   = r[0]   → expected int, got float    （被拒）
        //     let n: Float = r[0]   → 通过
        // 即当初「没能构造出失败」的原因**恰恰是 D67 那个缺陷**（推断出的容器
        // 元素类型是未解算的 TypeVar），不是「Int/Float 可互换」——
        // 后者在无标注语境下成立、有标注时**不成立**。
        (
            "range".to_string(),
            Signature::new(
                vec![
                    ("start".to_string(), Type::Int),
                    ("end".to_string(), Type::Int),
                    ("step".to_string(), Type::Int),
                ],
                Type::List(Box::new(Type::Float)),
            ),
        ),
        // len(x) for string / list / dict
        //
        // v0.104.6 修正：返回类型此前写 `Type::Float`，而运行期一直返
        // `Value::Int`。这不是「类型标注宽松」的无关紧要问题 —— 它让 typeck
        // **拒绝正确标注、接受错误标注**：
        //     let n: Int   = len(xs)   → Type error: expected int, got float
        //     let n: Float = len(xs)   → 通过，但运行期拿到的是 Int
        // （实测两条都经真实 CLI `mora run`，前者 exit 2。）
        //
        // 根因是 `len` 的返回类型有**两处互相矛盾的声明**：
        // `typeck/hm/builtin.rs:40` 写的是 `Type::Int`（正确），本文件写
        // `Type::Float`（错误）；而 `HMInference::builtin_callee_ty` 开头就
        // 「prefer the canonical dispatch registry」，即本表优先，于是错误
        // 的那份盖住了正确的那份。
        (
            "len".to_string(),
            Signature::new(
                vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::List(Box::new(Type::Union(vec![]))),
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                    ]),
                )],
                Type::Int,
            ),
        ),
        // str(x) -> string
        (
            "str".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::String),
        ),
        // int(s) -> int
        (
            "int".to_string(),
            Signature::new(vec![("s".to_string(), Type::String)], Type::Int),
        ),
        // float(x) -> float
        (
            "float".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::Float),
        ),
        // bool(x) -> bool
        (
            "bool".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::Bool),
        ),
        // v0.06: ai.chat(cfg: AiConfig, prompt: String) -> AiResult
        //
        // v0.104.6：返回类型 `Type::AiResult` → `Type::String`。
        //
        // 此前 `ai.chat` 在**源语言里完全不可用**，实测（真实 CLI `mora run`）：
        //     let reply = ai.chat("hi")   → exit 2
        //       Type mismatch: expected string | int | float | … , got ai_result
        //     print(ai.chat("hi"))        → exit 2（同上）
        // 连「先赋值再打印」都过不了 —— `print` 的形参类型里没有 `ai_result`。
        //
        // 根因：`Type::AiResult` 是一个**只存在于 typeck 的幽灵类型** ——
        // `Value` 里没有任何对应变体（全仓 `AiResult` 只出现在
        // `typeck/mod.rs` 的枚举与名字映射、以及本文件这两处签名里），
        // 于是这个返回值**没有任何东西能消费**。而运行期
        // `do_ai_chat` 的**每一条**路径（mock / replay / cache / real /
        // tools / agent）都返回 `Value::String`。
        //
        // 运行期自己的记录签名也早就写明了这一点（`ai_chat.rs`）：
        //     "ai.chat(model: string, prompt: string) -> string"
        //
        // 即 typeck 的声明与运行期的自我描述互相矛盾，且**运行期是对的**。
        // （`Type::AiResult` 与 `Type::AiConfig` / `Type::HttpRequest` 同属
        // 一批无运行期表示的类型；前者如今已无人引用。）
        (
            "ai.chat".to_string(),
            Signature::new(
                vec![
                    ("cfg".to_string(), Type::AiConfig),
                    ("prompt".to_string(), Type::String),
                ],
                Type::String,
            ),
        ),
        // v0.06.3: Router::new() -> Router
        (
            "Router::new".to_string(),
            Signature::new(vec![], Type::Router),
        ),
        // v0.06.6: McpServer::new() -> McpServer
        (
            "McpServer::new".to_string(),
            Signature::new(vec![], Type::McpServer),
        ),
        // v0.91: 数学 builtin — math/stats/linalg/random
        // 这些是 domain prefix 模块，typeck 注册为 marker signature（任意参数→Any）
        // 实际方法分派由 dispatch.rs::call_method_builtin 处理
        ("math".to_string(), Signature::new(vec![], Type::Any)),
        ("stats".to_string(), Signature::new(vec![], Type::Any)),
        ("linalg".to_string(), Signature::new(vec![], Type::Any)),
        // v0.99: "random" 不在此表 —— 模块对象不可直接调用（`random(...)`
        // 应报非函数），方法调用经 infer_var → Type::RandomModule 分型。
    ]
}

// ════════════════════════════════════════════════════════════════════════════
// v0.104.6 D293：模块方法表 —— 名字与签名**同一张表**
// ════════════════════════════════════════════════════════════════════════════
//
// D171 把方法名从 `match method, "a" | "b" | …` 里提成 20 张 `*_GROUPS`
// 分组表（`module_method_signature` 查组号选签名，`module_method_names` 展平
// 同一张表），已经消灭了「两处手写清单互为漂移」。**但签名本身仍靠组号
// 手工对齐**：
//
// ```text
// const MATH_GROUPS = &[ &["PI","E",…], &["sin",…], … ];   // 名字在这里
// "math" => match group_of(MATH_GROUPS, method) {
//              Some(0) => params_variadic(0, Type::Float),   // 签名在这里
//              Some(1) => params_variadic(1, Type::Float),
// ```
//
// 两张表之间**没有任何编译期约束**：把 `Some(1)` 写成 `Some(2)` 不会编译报错，
// 只会让 17 个一元函数静默拿到二元签名（`math.sqrt(x, y)` 通过、
// `math.sqrt()` 变成合法），而 `module_method_names` 照旧报出真名 ——
// **自省说它有、typeck 却不查**。D171 注释里「二者不可能漂移」那句话
// 只对**方法名**成立，对**组号↔签名**不成立。
//
// D293 把两张表**合成一张**：每项直接带自己的签名，组号这个中间层整个消失，
// 上面的失守形态随之**结构上不可能**。
//
// ── 为什么表里存 `Ret` 而不是 `Signature` ──────────────────────────────
//
// `Signature` 含 `String` / `Vec`，而 `to_string()` 不是 `const fn`
// ⇒ `Signature` **无法**在 `const` 上下文构造。`Ret` 是它的 const 可构造
// 投影（`Copy` + `&'static [Ret]` 承载递归），查找时再 `materialize()`。
// 这样 20 张表仍是 `const`（零运行时初始化成本，与 D171 一致）。
//
// ── 行为等价性 ───────────────────────────────────────────────────────────
//
// D293 只改**表述方式**，不改任何一个签名。验收判据是
// `tests/module_method_groups_single_table.rs`：它对 26 个模块名
// （23 个注册名 + `ai`/`agent`/`random` 重复项 + 4 个不存在的名字）
// × 全部方法名 dump 完整 `Signature`，与重构前**逐字节比对**。
//
// ── `bus` 第 0 组在此被拆成两组 ─────────────────────────────────────────
//
// 它是**唯一**一组「同组不同签名」的方法：`emit` / `off` 返 `Nil`，
// `publish` / `subscribe` 返 `Float`（`subscribe` 返订阅 token，
// `publish` 返当前 pattern 数）。原实现靠组内再 match 一次 `method` 兜住，
// 这正是「组 = 同签名」不变量的唯一破口。拆成两组后**返回值逐个不变**
// （见 `tests/module_return_types_match_runtime.rs` 的 `bus.*` 用例），
// 而不变量的破口没有了。
//
// ⚠ 元数一律登记为**下限**（`min_arity` + `variadic: true`）：运行期一律
// 按「至少 N 参」处理，多余实参被 `args.first()` 忽略，typeck 不得更严
// —— 见 `tests/module_method_signatures.rs::module_method_arity_only_enforces_lower_bound`
// 的自我更正说明（D69/D70/D71 最初登记成定长，把原本能跑的程序判成编译不过）。

/// `Signature` 的 **const 可构造投影** —— 见上面的说明。
#[derive(Clone, Copy, Debug)]
enum Ret {
    Any,
    Int,
    Float,
    Bool,
    String,
    Nil,
    Document,
    /// `Type::TeaApp`：TEA app 类型，4 个内部类型（`model` / `msg` /
    /// `update` / `view`）由用户代码决定 ⇒ 源码层面不可静态确定，
    /// 一律填 `Any`（`name` 恒为 `"tea"`）。
    TeaApp,
    /// `Type::List` —— 元素类型唯一，故是 1 元数组而非切片。
    List(&'static [Ret; 1]),
    /// `Type::Dict` —— `[键, 值]`。
    Dict(&'static [Ret; 2]),
    /// `Type::Union` —— 顺序即声明顺序，参与兼容判定，不可重排。
    Union(&'static [Ret]),
}

impl Ret {
    fn materialize(self) -> Type {
        match self {
            Ret::Any => Type::Any,
            Ret::Int => Type::Int,
            Ret::Float => Type::Float,
            Ret::Bool => Type::Bool,
            Ret::String => Type::String,
            Ret::Nil => Type::Nil,
            Ret::Document => Type::Document,
            Ret::TeaApp => Type::TeaApp {
                name: "tea".to_string(),
                model: Box::new(Type::Any),
                msg: Box::new(Type::Any),
                update: Box::new(Type::Any),
                view: Box::new(Type::Any),
            },
            Ret::List([elem]) => Type::List(Box::new(elem.materialize())),
            Ret::Dict([k, v]) => Type::Dict(Box::new(k.materialize()), Box::new(v.materialize())),
            Ret::Union(xs) => Type::Union(xs.iter().map(|r| r.materialize()).collect()),
        }
    }
}

/// 一个方法组：**同签名**的一组模块方法名 + 该签名。
#[derive(Clone, Copy)]
struct MethodGroup {
    names: &'static [&'static str],
    /// 最小元数（**不含** `self`）。
    min_arity: usize,
    ret: Ret,
}

impl MethodGroup {
    const fn new(names: &'static [&'static str], min_arity: usize, ret: Ret) -> Self {
        Self {
            names,
            min_arity,
            ret,
        }
    }

    /// 物化成 `Signature` —— 与 D171 的 `params_variadic` 逐字段等价：
    /// `self: Unknown` 打头（`infer_method_call` 用 `params.len() - 1`
    /// 算 user arity），其余形参一律 `Any`（保守过近似：能拒掉
    /// `String` 之类的错标注，又不会误拒合法实参），
    /// `raw_params` 与 `params` 等长且全 `None`，一律 `variadic`。
    fn signature(self) -> Signature {
        let n = self.min_arity;
        let mut params = Vec::with_capacity(n + 1);
        params.push(("self".to_string(), Type::Unknown));
        for i in 0..n {
            params.push((format!("a{i}"), Type::Any));
        }
        Signature {
            params,
            raw_params: vec![None; n + 1],
            return_type: self.ret.materialize(),
            raw_return_type: None,
            variadic: true,
        }
    }
}

// ── math ── 返回类型逐条核对自 `builtins/math.rs::call_math_method` ──────
const MATH_METHODS: &[MethodGroup] = &[
    // 零参模块常量
    MethodGroup::new(&["PI", "E", "TAU", "INF", "NAN"], 0, Ret::Float),
    // unary_float —— 运行期一律 Ok(Value::Float)
    MethodGroup::new(
        &[
            "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "log",
            "log2", "log10", "log1p", "sqrt", "cbrt", "fract",
        ],
        1,
        Ret::Float,
    ),
    // binary_float —— pow / hypot / atan2 是**双参**，写成 1 参会
    // 让 `math.pow(2.0, 3.0)` 报 "Expected 1 arguments, got 2"
    MethodGroup::new(&["pow", "hypot", "atan2"], 2, Ret::Float),
    // unary_preserve —— `Int` 进 `Int` 出、`Float` 进 `Float` 出，
    // 声明为数值塔（Union[Int, Float]）是**可靠的过近似**：既能拒掉
    // String 之类的错标注，又不会误拒 `Int` / `Float` 任一。
    MethodGroup::new(
        &["abs", "sign", "floor", "ceil", "round", "trunc"],
        1,
        Ret::Union(&[Ret::Int, Ret::Float]),
    ),
    // 类型谓词 —— 运行期一律 Ok(Value::Bool)
    MethodGroup::new(&["is_nan", "is_inf", "is_finite"], 1, Ret::Bool),
];

// ── json ── `call_builtin` 里 (BuiltinKind::Json, …) 两个 arm ────────────
const JSON_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["stringify"], 1, Ret::String),
    // parse 的结果由文本决定（对象/数组/标量），如实声明只能给 Any
    MethodGroup::new(&["parse"], 1, Ret::Any),
];

// ── file ── 返回类型与元数逐条核对自 `builtins/file.rs::call_file_method`
// 元数由各 arm 里 `expect_str(N, …)` / `args.get(N)` 的最大下标 +1 得出。 ──
const FILE_METHODS: &[MethodGroup] = &[
    // String —— 路径派生与读取
    MethodGroup::new(
        &[
            "read_text",
            "read_bytes",
            "abs",
            "basename",
            "dirname",
            "extname",
        ],
        1,
        Ret::String,
    ),
    // 零参
    MethodGroup::new(&["cwd", "home_dir"], 0, Ret::String),
    // `join` 是**变参**（运行期 `for arg in args`），最小 1 元
    MethodGroup::new(&["join"], 1, Ret::String),
    // Float —— `Ok(Value::Float(meta.len() as f64))`
    MethodGroup::new(&["size"], 1, Ret::Float),
    // List[String] —— 目录列举
    MethodGroup::new(&["list"], 1, Ret::List(&[Ret::String])),
    // Bool —— 三个路径谓词
    MethodGroup::new(&["exists", "is_file", "is_dir"], 1, Ret::Bool),
    // Nil —— 写操作一族（1 元）
    MethodGroup::new(
        &[
            "mkdir",
            "mkdir_all",
            "remove",
            "remove_all",
            "touch",
            "chdir",
        ],
        1,
        Ret::Nil,
    ),
    // Nil —— 写操作一族（2 元）
    MethodGroup::new(
        &["write_text", "append_text", "write_bytes", "rename", "copy"],
        2,
        Ret::Nil,
    ),
];

// ── exec ── `exec_parallel` 的成功路径返回 `Ok(Value::List[Dict])` ────────
//
// v0.104.6 D337：`min_arity` 由 **2 改为 1**。
//
// 修前 `exec.parallel(["echo a"])` 被 typeck 拒（"Expected 2 arguments, got 1"），
// 而**运行期明确支持**单参形式：
// - `exec.rs:143-145` 的错误消息自称「requires **at least 1 arg** (cmds list)」；
// - `exec.rs:176` / `:197` 用 `args.len() >= 2` / `>= 3` 把 `max_concurrent` /
//   `timeout_ms` 当**可选**处理；
// - `exec.rs:143` 的 `args.is_empty()` 守卫正是为「1 个参数」这条路径写的；
// - `exec.rs` 的运行期单测（`exec_parallel_runs_all_commands` 等）**全部用 1 个实参**。
//
// ⇒ 运行时契约 =「至少 1 个」，typeck 契约 =「至少 2 个」⇒ **契约分叉**。
// 这正是 `Signature` 文档里那句「运行期支持、类型系统拒绝 = 契约分叉」所指的情况；
// 而 D81 的 `signature_no_over_tightening` 只固化了**上限**那一侧
// （「不得拒绝多余实参」），**没管下限** ⇒ 双参的 `exec.parallel`
// 让这个下限过紧一直没人看见。
//
// 注：`min_arity` 降到 1 **不会**让 `exec.parallel([], -1)` 变成报错 ——
// `exec.rs:164` 的空列表早返回发生在可选参数校验**之前**，
// 那条不一致属另一层问题（D337 已实测并记档），本条不夹带修。
const EXEC_METHODS: &[MethodGroup] = &[MethodGroup::new(
    &["parallel"],
    1,
    Ret::List(&[Ret::Dict(&[Ret::String, Ret::Any])]),
)];

// ── stats ── 逐条核对自 `builtins/stats.rs::call_stats_method` ───────────
// 一元统计量（`expect_number_list(args.first(), …)`，arity 1）返 Float；
// `sum` 走 `sum_value` → `Value::Float(xs.iter().sum())`。 ────────────────
const STATS_METHODS: &[MethodGroup] = &[
    MethodGroup::new(
        &[
            "sum", "mean", "median", "var", "stddev", "min", "max", "quantile",
        ],
        1,
        Ret::Float,
    ),
    // `histogram` 的每个 bin 是 `Dict{lo, hi, count}`，外层是 List
    MethodGroup::new(
        &["histogram"],
        2,
        Ret::List(&[Ret::Dict(&[Ret::String, Ret::Float])]),
    ),
    // 双列相关 / 协方差
    MethodGroup::new(&["corr", "cov"], 2, Ret::Float),
];

// ── linalg ── 逐条核对自 `builtins/linalg.rs::call_linalg_method` ────────
const LINALG_METHODS: &[MethodGroup] = &[
    // 矩阵结果：`Vec<Vec<f64>>` → `List[List[Float]]`
    MethodGroup::new(&["transpose"], 1, Ret::List(&[Ret::List(&[Ret::Float])])),
    MethodGroup::new(&["matmul"], 2, Ret::List(&[Ret::List(&[Ret::Float])])),
    // `cross` 收两个向量 `Vec<f64>` → `List[Float]`
    MethodGroup::new(&["cross"], 2, Ret::List(&[Ret::Float])),
    MethodGroup::new(&["dot"], 2, Ret::Float),
    // `norm(vec, p?)` 的**阶数是可选的**（`args.get(1) … .unwrap_or(2.0)`）。
    // 按固定元数登记会拒掉其中一种合法写法 —— 1 参登记则
    // `linalg.norm(v, 3)` 被拒，2 参登记则 `linalg.norm(v)` 被拒。
    // 用变参（最小 1）同时放行两种；代价是第 3 个实参要到运行期才报
    // （`linalg.norm` 只读 `args.get(1)`），比现状略松但严格优于
    // 「两种写法都不检查、返回类型也丢失」。
    MethodGroup::new(&["norm"], 1, Ret::Float),
];

// ── document ── 模块侧只有 `parse` 一个方法 ─────────────────────────────
// （`method_dispatch.rs` 的 `(BuiltinKind::Document, "parse")`）。
// `Value::Document` **值**侧的 blocks/markdown/metadata/origin/pages/text
// 走 `call_method_document`，接收者不是模块裸名，与本表无交集。 ─────────
const DOCUMENT_METHODS: &[MethodGroup] = &[MethodGroup::new(&["parse"], 1, Ret::Document)];

// ── mora ── 逐条核对自 `builtins/mora.rs::call_mora_method` ──────────────
const MORA_METHODS: &[MethodGroup] = &[
    // ⚠ `refine(path, instruction, count?)` —— 第 1 参是**文件路径**，
    // 不是脚本文本。运行期会 `read <path>`；传脚本文本会得到
    // `mora.refine: read task main() end: 系统找不到指定的文件`。
    // 实测：2 参 → `dict`，3 参（多方案）→ `list`。
    //
    // **返回类型随元数变**：2 参返 `Dict`、3 参返 `List[Dict]`。声明任一
    // 都会拒掉另一种合法写法，故返回 `Any`（放弃这一条的返回类型
    // 检查），但用变参保住「至少 2 参」的下限。
    MethodGroup::new(&["refine"], 2, Ret::Any),
    // `refine_info(script, n?)` → `Ok(Value::Dict(step.to_dict()))`
    MethodGroup::new(&["refine_info"], 1, Ret::Dict(&[Ret::String, Ret::Any])),
    // `list_refines()` → `List[String]`
    MethodGroup::new(&["list_refines"], 0, Ret::List(&[Ret::String])),
];

// ── bus ── `builtins/event.rs::call_event_method` ────────────────────────
//
// ⚠ `emit` / `publish` 的**第二参是可选的**
// （`args.get(1).cloned().unwrap_or(Value::Nil)`）—— 按 2 参登记会
// 拒掉 `bus.emit("evt")` 这种最常见写法。机械扫描在此处给出 2，是错的。
//
// D293：`emit` / `off` 与 `publish` / `subscribe` 的**返回类型不同**
// （前者 `Nil`，后者 `Float`）—— D171 靠组内再 match 一次 `method` 兜住，
// 这里是「组 = 同签名」不变量的唯一破口，故拆成两组（返回值逐个不变）。
const BUS_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["emit", "off"], 1, Ret::Nil),
    // `subscribe` 返回订阅 token；`publish` 返回当前 pattern 数
    MethodGroup::new(&["publish", "subscribe"], 1, Ret::Float),
    MethodGroup::new(&["count"], 0, Ret::Float),
];

// ── mock ── `builtins/mock.rs::call_mock_method` ─────────────────────────
const MOCK_METHODS: &[MethodGroup] = &[
    // `register(name, handler)` —— handler 是**必填**的
    //（`args.get(1).cloned().ok_or("mock.register: requires handler")?`，
    // 不是 `unwrap_or`），最小 2 元。
    MethodGroup::new(&["register"], 2, Ret::String),
    MethodGroup::new(&["unregister"], 1, Ret::Nil),
    // `call(name, args?)` 的返回值是**被 mock 的 handler 决定的**
    // （`Ok(f(&call_args))`），如实声明只能给 Any
    MethodGroup::new(&["call"], 1, Ret::Any),
    MethodGroup::new(&["count"], 0, Ret::Float),
    MethodGroup::new(&["names"], 0, Ret::List(&[Ret::String])),
];

// ── ccr ── `builtins/ccr.rs::call_ccr_method` ────────────────────────────
const CCR_METHODS: &[MethodGroup] = &[
    // `put(data)` → 内容哈希字符串
    MethodGroup::new(&["put"], 1, Ret::String),
    // `get(hash)` → 命中返 String、未命中返 Nil
    MethodGroup::new(&["get"], 1, Ret::Union(&[Ret::String, Ret::Nil])),
    // `len()` → **Int**（全语言少数几个 Int 来源之一，同 `len([…])`）
    MethodGroup::new(&["len"], 0, Ret::Int),
    // `marker(hash, size?)` —— size 可选，最小 1 元
    MethodGroup::new(&["marker"], 1, Ret::String),
    // `extract(marker)` → String（无效 marker 走 Err）
    MethodGroup::new(&["extract"], 1, Ret::String),
];

// ── plan ── 逐条核对自 `builtins/plan.rs::call_plan_method` ──────────────
const PLAN_METHODS: &[MethodGroup] = &[
    // ⚠ `create(name, steps)` —— 第 2 参是 **steps 列表**
    // （元素为 `{id, text[, status]}` 字典），**不是**「kind / 类型」。
    // 运行期错误消息原文：`plan.create: requires 2 args (name, steps)`
    // 与 `plan.create: steps must be a list of {id, text} dicts`。
    // （本注释曾把第 2 参写成 `kind?`，D88 实测纠正。）
    MethodGroup::new(&["create"], 2, Ret::String),
    // `update` / `remove` 两个改动操作都返 Bool
    MethodGroup::new(&["update", "remove"], 2, Ret::Bool),
    // `add(plan, step, …)` 至少 3 参
    MethodGroup::new(&["add"], 3, Ret::Bool),
    // `list()` 的两条分支返回形态不同 —— 带 plan 名返回该计划的步骤
    // （List[Dict]），不带则返回全部计划名（List[String]）。`List[Any]`
    // 是能同时覆盖两者的唯一诚实的声明。
    MethodGroup::new(&["list"], 0, Ret::List(&[Ret::Any])),
    // `info(name)` → Dict
    MethodGroup::new(&["info"], 1, Ret::Dict(&[Ret::String, Ret::Any])),
];

// ── tea ── 逐条核对自 `builtins/tea.rs::call_tea_method` ────────────────
const TEA_METHODS: &[MethodGroup] = &[
    // `init(init?, update?, view?)` —— 三参**全部**可选
    // （`args.first()` / `args.get(1)` / `args.get(2)` 都带
    // `.unwrap_or(Value::Nil)`），故下限是 0。
    MethodGroup::new(&["init"], 0, Ret::TeaApp),
    // `dispatch(app, msg)` / `update(app, msg)` —— 两个 `ok_or` 都必填
    MethodGroup::new(&["dispatch", "update"], 2, Ret::TeaApp),
    // `run(app, max_steps?)` —— 步数可选
    MethodGroup::new(&["run"], 1, Ret::TeaApp),
    // `model(app)` / `view(app)` 返回 app 内部的 model / view 值，
    // 其类型由用户 TEA 代码决定 → Any
    MethodGroup::new(&["model", "view"], 1, Ret::Any),
    // 类型名常量
    MethodGroup::new(&["model_type", "msg_type"], 0, Ret::String),
    // `replay(…)` → Nil
    MethodGroup::new(&["replay"], 0, Ret::Nil),
];

// ── sandbox ── 逐条**人工读** `builtins/sandbox.rs::call_sandbox_method`
const SANDBOX_METHODS: &[MethodGroup] = &[
    // `mode()` → 当前模式名
    MethodGroup::new(&["mode"], 0, Ret::String),
    // 两个路径谓词
    MethodGroup::new(&["check_builtin", "check_path"], 1, Ret::Bool),
    // `check_call(token_id, capability)` —— `args.len() != 2`
    MethodGroup::new(&["check_call"], 2, Ret::Bool),
    // `revoke(token_id)` —— `args.len() != 1`
    MethodGroup::new(&["revoke"], 1, Ret::Bool),
    // `token_count()` → Float
    MethodGroup::new(&["token_count"], 0, Ret::Float),
    // `audit_emit(actor, action, target?, payload?)` —— `args.len() < 2` 必填
    MethodGroup::new(&["audit_emit"], 2, Ret::Bool),
    // `audit_flush()` / `audit_verify()` 都有**两条返回分支**：
    // 成功 `Ok(Value::Bool(true))`、失败 `Err(e) => Ok(Value::String(e))`
    // —— 也就是说「审计链校验失败」是以**返回值**而非 Err 表达的。
    MethodGroup::new(
        &["audit_flush", "audit_verify"],
        0,
        Ret::Union(&[Ret::Bool, Ret::String]),
    ),
    // `containerize(backend, mounts?, net?, …)` → 后端算出的 id（Float）
    MethodGroup::new(&["containerize"], 1, Ret::Float),
    // `container_exec(cmd, …)` → 结果 Dict
    MethodGroup::new(&["container_exec"], 1, Ret::Dict(&[Ret::String, Ret::Any])),
    // `container_info(id)` → 命中 Dict / 未命中 Nil
    MethodGroup::new(
        &["container_info"],
        1,
        Ret::Union(&[Ret::Dict(&[Ret::String, Ret::Any]), Ret::Nil]),
    ),
    // `container_clear()` → Bool
    MethodGroup::new(&["container_clear"], 0, Ret::Bool),
];

// ── memory ── 逐条**人工读** `builtins/memory.rs::call_memory_method` ────
// （D70 曾推迟这一组：机械扫描在本文件给出过 3 处错误元数。
//  完整读一遍后确认，其中一处**返回值**也扫错了 —— `load` 早期被
//  报成 `Dict`，实为恒返 `Ok(Value::Bool(true))` 或 Err。） ──────────────
const MEMORY_METHODS: &[MethodGroup] = &[
    // `store(key, value)` —— 两参都是 `ok_or`（必填），返回 Nil
    MethodGroup::new(&["store"], 2, Ret::Nil),
    // `recall(key)` —— 命中返存储值、未命中返 Nil，值类型由调用方决定
    MethodGroup::new(&["recall"], 1, Ret::Any),
    // `search(query)` → `List[Dict{key, value}]`
    MethodGroup::new(
        &["search"],
        1,
        Ret::List(&[Ret::Dict(&[Ret::String, Ret::Any])]),
    ),
    // `forget(key)` → Nil
    MethodGroup::new(&["forget"], 1, Ret::Nil),
    // `clear()` → Nil
    MethodGroup::new(&["clear"], 0, Ret::Nil),
    // `size()` → 条目数（Float）
    MethodGroup::new(&["size"], 0, Ret::Float),
    // `remember(category, text)` 两参都 `ok_or`（必填）→ Bool
    MethodGroup::new(&["remember"], 2, Ret::Bool),
    // `recall_markdown(category)` → 拼接后的文本
    MethodGroup::new(&["recall_markdown"], 1, Ret::String),
    // `list_markdown()` / `keys()` → List[String]
    MethodGroup::new(&["list_markdown", "keys"], 0, Ret::List(&[Ret::String])),
    // `save(path)` / `load(path)` → Bool
    // ⚠ `load` 读的是 JSON 对象，写入 store 后**恒返 true**，非对象时
    // 走 Err —— 从不返回 Dict。
    MethodGroup::new(&["save", "load"], 1, Ret::Bool),
];

// ── schedule ── 逐条核对自 `builtins/schedule.rs::call_schedule_method` ─
//
// `add(name, kind, message, interval_s?, at_epoch?)`
// 前三参都是 `return Err(...)`（**必填**，不是 `unwrap_or`），
// 后两参走 `if let Some(Value::Float(n)) = args.get(N) … else { 0 }`
// 即可选。返回 `.map(Value::String)` —— 新建 job 的 id。 ─────────────────
const SCHEDULE_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["add"], 3, Ret::String),
    // `list()` 返 `List[Dict]`（id/name/kind/message/interval_s/at_epoch）
    MethodGroup::new(
        &["list"],
        0,
        Ret::List(&[Ret::Dict(&[Ret::String, Ret::Any])]),
    ),
    // `tick()` 返 `List[…]`（元素类型由调度器决定）
    MethodGroup::new(&["tick"], 0, Ret::List(&[Ret::Any])),
    // `remove(id)` 返是否真的删掉了
    MethodGroup::new(&["remove"], 1, Ret::Bool),
    // `count()` 返 job 数（Float）
    MethodGroup::new(&["count"], 0, Ret::Float),
];

// ── web ── `fetch(url)` → `real_web_fetch` 的成功路径是
// `Ok(Value::String(text))`（`interpreter/ai_chat.rs:59`），故可精确声明。
//
// ⚠ v0.104.6 D85 更正：D75 的普查表把 `web` 归进「永久不进本表的 4 个
// 模块」，理由写的是「它有自己的 `Type` 变体」—— **这个理由是错的**：
// `infer_var` 的特例只有 `ai` / `agent` / `random` 三个，`web` 落到
// 兜底的 `Type::Unknown`，与 `file` / `math` 等**完全同款**。
// 判据来自实测：`let v: Bool = web.fetch("http://x")` 在修前**通过**
// typeck（只栽在运行期的 DNS 错误上）。 ─────────────────────────────────
const WEB_METHODS: &[MethodGroup] = &[MethodGroup::new(&["fetch"], 1, Ret::String)];

// ── tool（`MODULE_OBJECTS` 里的注册名是 **`tool`**，枚举变体叫
// `BuiltinKind::Toolplane`）── 逐条核对自
// `builtins/toolplane.rs::call_toolplane_method`
//
// ⚠ v0.104.6 D74 自查：最初把这段建成 `"toolplane"`，而源语言里
// `toolplane` 是**未绑定变量**（`Unbound variable 'toolplane'`）——
// 按**枚举变体名**而不是**注册名**建表，整段永远匹配不到。
// 判据：模块名的唯一事实源是 `crate::value::MODULE_OBJECTS`。
// `tests/module_method_signatures.rs::module_table_keys_are_registered_names`
// 会把这类错误当场揪出来。 ───────────────────────────────────────────────
const TOOL_METHODS: &[MethodGroup] = &[
    // `create(name, kind?)` → 成功与否
    MethodGroup::new(&["create"], 1, Ret::Bool),
    // `register(plane, name, kind, …)` 至少 4 参
    MethodGroup::new(&["register"], 4, Ret::Bool),
    MethodGroup::new(&["unregister"], 2, Ret::Bool),
    MethodGroup::new(&["remove"], 1, Ret::Bool),
    MethodGroup::new(&["list"], 0, Ret::List(&[Ret::String])),
    MethodGroup::new(&["list_tools"], 1, Ret::List(&[Ret::String])),
    // `info` / `find` 各有**两条返回分支**：命中返 Dict、未命中返 Nil
    MethodGroup::new(
        &["info"],
        1,
        Ret::Union(&[Ret::Dict(&[Ret::String, Ret::Any]), Ret::Nil]),
    ),
    MethodGroup::new(
        &["find"],
        2,
        Ret::Union(&[Ret::Dict(&[Ret::String, Ret::Any]), Ret::Nil]),
    ),
];

// ── skill ── 逐条核对自 `builtins/skill.rs::call_skill_method` ───────────
const SKILL_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["list"], 0, Ret::List(&[Ret::String])),
    // `find` 命中返 Dict、未命中返 Nil
    MethodGroup::new(
        &["find"],
        1,
        Ret::Union(&[Ret::Dict(&[Ret::String, Ret::Any]), Ret::Nil]),
    ),
    // `load` / `uninstall` / `set_hub` 都返 Bool
    MethodGroup::new(&["load", "uninstall", "set_hub"], 1, Ret::Bool),
    MethodGroup::new(&["install"], 2, Ret::Bool),
    // `refresh_hub` 返刷新的条目数（Float）
    MethodGroup::new(&["refresh_hub"], 0, Ret::Float),
];

// ── xform ── 逐条核对自 `builtins/xform.rs::call_xform_method` ──────────
//
// ⚠ v0.104.6 D74：`map` / `filter` / `take` / `comp` 四个返回的是
// **调试占位串**（`Ok(Value::String(format!("<xform.map({:?})>", fn_val)))`），
// 不是 transducer —— 即 spec §12 的 transducer 组合子在本运行期是**桩实现**。
// 本表如实声明 `String`（与运行期一致），但这**不代表该功能可用**；
// 与 D66 的 `Conversation` 同类，属功能未实现，不是签名缺陷。 ───────────
const XFORM_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["map", "filter", "take", "comp"], 1, Ret::String),
    // `attach(stream)` 原样返回入参
    MethodGroup::new(&["attach"], 1, Ret::Any),
];

/// 模块名 → 方法组表。**这是模块名唯一的查表入口**。
///
/// `ai` / `agent` / `random` 三个**不在此列**（见 `AI_MODULE_METHODS` 的
/// 注释）：它们有精确的 `Type` 变体，签名在 `method_signature` 的 `Type`
/// 级 match 里，不走模块分组表。查表返回 `None` ⇒ 签名查找同样返回
/// `None`，与 D171 修前的 `match module` 缺臂行为一致。
fn module_groups(module: &str) -> Option<&'static [MethodGroup]> {
    Some(match module {
        "math" => MATH_METHODS,
        "json" => JSON_METHODS,
        "file" => FILE_METHODS,
        "exec" => EXEC_METHODS,
        "stats" => STATS_METHODS,
        "linalg" => LINALG_METHODS,
        "document" => DOCUMENT_METHODS,
        "mora" => MORA_METHODS,
        "bus" => BUS_METHODS,
        "mock" => MOCK_METHODS,
        "ccr" => CCR_METHODS,
        "plan" => PLAN_METHODS,
        "tea" => TEA_METHODS,
        "sandbox" => SANDBOX_METHODS,
        "memory" => MEMORY_METHODS,
        "schedule" => SCHEDULE_METHODS,
        "web" => WEB_METHODS,
        "tool" => TOOL_METHODS,
        "skill" => SKILL_METHODS,
        "xform" => XFORM_METHODS,
        _ => return None,
    })
}

// ── v0.104.6 D175：`ai` / `agent` / `random` 三个模块的方法名 ──
//
// 这三个**不进** `*_METHODS`：它们有精确的 `Type` 变体
// （`Type::AiModule` / `Type::Agent` / `Type::RandomModule`），
// 签名在下面 `method_signature` 的 `Type` 级 match 里，不走模块分组表。
// 但「有哪些方法可调」是确定的，逐个经**运行期实测**确认可达
// （D175 的修前/修后两列见 CHANGELOG）。名字集中在这里，
// 让签名查找、枚举、以及 `hm/infer.rs` 的报错文案共用**一处**事实源。
//
// ⚠ 两处**刻意的收窄**，别当成漏写：
//
// 1. `ai` 只有 3 个。`ai.retry` / `ai.role` / `ai.dag` / `ai.heartbeat` /
//    `ai.context.*` 虽在 `call_ai_method` 里实现且单测全绿，但源码**不可达**
//    （`ai` 裸名 → `BuiltinKind::AiChat`，而 `call_ai_method` 只挂在
//    `(BuiltinKind::Ai, _)` 上）—— 见 `tests/ai_namespace_reachability.rs`（D59）。
//    把它们列进来会让自省**宣称**一批调不通的方法，比空集更坏。
// 2. `agent` 只有 2 个。`run` / `name` / `max_steps` 是 **Agent 值**的方法
//    （`call_method_agent`），不是**模块**的方法。`Type::Agent` 同时表示二者
//    是已知的设计代价（见下面 `Type::Agent` 签名处的说明），
//    但自省必须报**接收者确实是模块**的那一批。

const AI_MODULE_METHODS: &[&str] = &["chat", "tokens", "critic"];
const AGENT_MODULE_METHODS: &[&str] = &["create", "critic"];
pub const RANDOM_METHODS: &[&str] = &["rand_int", "rand_float", "rand_choice", "seed", "shuffle"];

/// v0.104.6 D171：模块对象（`Value::Builtin`）的**全部方法名**。
///
/// 供 `Value::methods()` 用 —— 此前 `methods_of(math)` 等 23 个模块**全是 `[]`**，
/// 而 `math.floor` / `json.parse` / `file.read_text` 都可用（D170 普查）。
///
/// D293：签名查找读**同一张表**（`module_groups`），故二者不可能漂移；
/// `tests/module_methods_introspection.rs` 的普查在有任何模块补上方法名时会红。
pub fn module_method_names(module: &str) -> Vec<&'static str> {
    // v0.104.6 D175：这三个不走分组表（签名在 `Type` 级 match 里），
    // 但方法名是确定的 —— 从各自的名字表取，同样是一处事实源。
    // 收窄理由（哪些**不**列、为什么）见上面三张表的注释。
    match module {
        "ai" => return AI_MODULE_METHODS.to_vec(),
        "agent" => return AGENT_MODULE_METHODS.to_vec(),
        "random" => return RANDOM_METHODS.to_vec(),
        _ => {}
    }
    module_groups(module)
        .map(|gs| gs.iter().flat_map(|g| g.names.iter().copied()).collect())
        .unwrap_or_default()
}

/// v0.104.6 D69：**模块对象**的方法签名，按**模块名**索引。
///
/// 与 [`method_signature`] 的区别：后者按 `Type` 索引，而 `infer_var`
/// 把 23 个模块对象里的 20 个解析成 `Type::Unknown`（`Type` 里根本没有
/// `MathModule` / `JsonModule` / `FileModule` … 变体，只有 `AiModule` /
/// `RandomModule` / `Agent`），于是模块方法调用的返回类型全部退化成
/// 永不解算的 TypeVar —— `let v: String = math.floor(1.5)` 被静默接受。
///
/// 形参一律写 `Any`：**本表只负责给出结果类型**，不收紧实参（收紧实参需要
/// 与运行期 `call_*_method` 的逐分支校验逐条对齐，收益与风险不成比例）。
/// `self` 保留在首位，与 `method_signature` 的调用约定一致
/// （`infer_method_call` 用 `params.len() - 1` 算 user arity）。
///
/// 覆盖范围刻意保守：只登记**运行期返回类型确定且已被逐条核对**的模块。
/// 其余模块宁可继续返回 TypeVar，也不要写一个没核对过的签名 —— 那会把
/// 「不检查」换成「检查错」。
///
/// 接收者是**模块裸名**（如 `math`），不是 `Value` —— 本函数同时驱动
/// arity 与返回类型**两条路**。
///
/// D293：实现从「`match module` → `match group_of(GROUPS, method)` →
/// `Some(N) => 签名`」压平成一次线性查找，返回值逐字段与 D172 等价
/// （等价性判据见 `tests/module_method_groups_single_table.rs`）。
///
/// 找不到时返回 `None`：调用方据此把结果类型留给 `TypeVar`（不检查），
/// **而不是**报「未知方法」—— 后者由运行期的 `unknown method` 负责。
pub fn module_method_signature(module: &str, method: &str) -> Option<Signature> {
    module_groups(module)?
        .iter()
        .find(|g| g.names.contains(&method))
        .map(|g| g.signature())
}

pub fn lookup_builtin(name: &str) -> Option<Signature> {
    builtin_signatures()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s)
}

///  Number of declared parameters for a known receiver method, used by
///  `infer_method_call` to enforce arity.
///  Returns `None` if the method is unknown to the dispatch table.
pub fn method_arity(receiver: &Type, method: &str) -> Option<usize> {
    if let Some(sig) = method_signature(receiver, method) {
        Some(sig.params.len())
    } else {
        None
    }
}

///  Look up a method's `Signature` (parameter list + return type) for
///  `receiver`. Returns `None` for unknown `(receiver, method)` pairs.
pub fn method_signature(receiver: &Type, method: &str) -> Option<Signature> {
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return Some(sig);
    }
    if let Some(sig) = method_signature_via_type(receiver, method) {
        return Some(sig);
    }
    if method == "len" {
        // v0.104.6：`Float` → `Int`，与运行期及本文件上方的 builtin 签名一致。
        return Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        ));
    }
    None
}

fn method_signature_builtin(receiver: &Type, method: &str) -> Option<Signature> {
    match (receiver, method) {
        // v0.75.16: 保留 List 元素类型（此前 map/filter/push 返回 List<Any>、
        // reduce/pop/get 返回 Any — 元素类型信息丢失）。
        // v0.75.16: 保留 List 元素类型。map/filter 接收闭包参数（M2 前
        // 用 Any 宽松约束）；push 接收元素参数（elem 类型）。
        (Type::List(elem), "map" | "filter") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("f".to_string(), Type::Any),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "push") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), elem.as_ref().clone()),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "pop") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**elem).clone(),
        )),
        // ── v0.104.6 D130：补齐 spec §12 承诺、运行期已实现、但 typeck 无签名的
        // 9 个 List 方法 ──────────────────────────────────────────────
        //
        // 缺陷形态是**假阴性**（与 D54 的 `let n: String = d.len()` 同型）：
        // 无签名 → 返回未解算的 TypeVar → 与**任何**标注都「相容」→
        // 类型标注形同虚设。实测：
        //   let xs = [1, 2, 3]
        //   let bad: string = xs.take(2)     → exit 0，**零诊断**，而运行时
        //                                          `bad` 是个 list
        //
        // 运行期全部可用（已实测）：`reduce`=6.0、`take`=[1,2]、`drop`=[2,3]、
        // `window`=[[1,2],[2,3]]、`batch`=[[1,2],[3]]、`flatten`=[1,2,3]、
        // `transpose`（要求矩形）、`reshape`、`shape`。
        //
        // 返回类型尽量保留元素类型 `elem`（与上方 map/filter/push 同做法）；
        // 元素类型本身不可知的（flatten 的内层可异质）用 `Any`。
        (Type::List(elem), "take" | "drop") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("n".to_string(), Type::Union(vec![Type::Int, Type::Float])),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "window" | "batch") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "size".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            Type::List(Box::new(Type::List(elem.clone()))),
        )),
        (Type::List(elem), "reduce") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("f".to_string(), Type::Any),
                // 累加初值必须与元素同类型（`[1,2,3].reduce(f, "")` 是错的）
                ("init".to_string(), elem.as_ref().clone()),
            ],
            (**elem).clone(),
        )),
        (Type::List(elem), "reshape") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "rows".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
                (
                    "cols".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "transpose") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::List(elem.clone()))),
        )),
        (Type::List(_), "flatten") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            // 内层元素类型不可知（`[[1],["x"]]` 是合法的），故用 Any
            Type::List(Box::new(Type::Any)),
        )),
        (Type::List(_), "shape") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::Int)),
        )),
        // v0.104.6 D52：下标收 **Int | Float**，与运行期 `index_value` 的
        // 两个分支（`(Value::List, Value::Int)` / `(Value::List, Value::Float)`，
        // 两者都走 `checked_index`）对齐。
        //
        // 此前声明 `Type::Int` 单边，而**本语言所有数值字面量都是 `Float`**
        // （D1 已确立：`len()` 返 Int 是全语言唯一的 Int 来源）——于是
        // `xs.get(1)`、`xs.get(len(xs) - 1)` 这类**最自然的写法直接编译不过**
        // （`Type error: expected int, got float`），而等价的 `xs[1]` 却能过
        // （`[]` 在 typeck 里没有签名，落 `Any` 放行）。**同一操作的两种写法，
        // 一种通一种不通。**
        //
        // `tests/equivalent_spellings.rs` 里恰好有 `list_index_vs_get` 这条
        // 配对却一直「通过」——因为该文件的 `run` 只调 `ParserV3::compile` +
        // `run_mir`，**完全绕过 typeck**。已一并补上（见该文件）。
        (Type::List(elem), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "index".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            (**elem).clone(),
        )),
        // v0.104.6：以下三条 `len` 方法签名此前声明 `Type::Float`，与运行期的
        // `Value::Int` 不符（详见上方 builtin 签名处的完整说明）。
        (Type::List(_), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        // get 接收 key（String）；返回 Union<V, Nil>（v0.75.16 unify 成员合一）
        (Type::Dict(_, v), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), Type::String),
            ],
            Type::Union(vec![v.as_ref().clone(), Type::Nil]),
        )),
        // set 接收 key + value
        //
        // v0.104.6 D127：形参 `value` 此前绑成 **dict 自身的 `V`**，与 spec §12
        // 方法表 `.set(key, val) | string, any -> dict` 明确写的 **`any`** 相悖。
        // 后果是一条**假阳性**：
        //   let d = {a: 1}
        //   let e = d.set("b", "text")     → 报 expected float, got string
        // 而运行期 `Value::Dict(HashMap<String, Value>)` **完全支持**异质，
        // `json.parse` 也能产出异质 dict（它的 `V` 未受约束，故那条路径可用）——
        // 于是「从字面量 dict 出发就不能再加异质键」，同一门语言里两套行为。
        //
        // `keys` / `values` / `len` / `get` 四条签名都正确，`set` 是**孤例**，
        // 故按疏漏修正而非设计决定。
        //
        // 返回类型同步放宽为 `Dict(k, Any)`：形参放宽后若仍返回 `Dict(k, v)`，
        // 存进去的 `Any` 值与声明的 `v` 会对不上（`e["b"]` 又会被按 `v` 检查而
        // 再次报错）。spec 的 `-> dict` 未指定 `V`，且运行时确实可异质。
        (Type::Dict(k, _), "set") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), k.as_ref().clone()),
                ("value".to_string(), Type::Any),
            ],
            Type::Dict(k.clone(), Box::new(Type::Any)),
        )),
        (Type::Dict(k, _), "keys") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(k.as_ref().clone())),
        )),
        (Type::Dict(_, v), "values") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(v.as_ref().clone())),
        )),
        (Type::Dict(_, _), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        (Type::String, "len") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::Int,
        )),
        // v0.104.2: String 方法签名补全**实参**。此前 `upper/lower/trim/replace`
        // 与 `starts_with/ends_with/contains/split` 各自共用一条只声明 `self`
        // 的签名，而运行期 `call_method_string` 对这些名字都要读实参：
        //   replace(from, to) / starts_with(p) / ends_with(p) / contains(n) / split(sep)
        // 于是 `"a,b" |> split(",")` 报 "Expected 0 arguments, got 1"
        //（spec §7.6 的管道示例正是这个形状）。按运行期真实 arity 拆开。
        (Type::String, "upper" | "lower" | "trim") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::String,
        )),
        (Type::String, "replace") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("from".to_string(), Type::String),
                ("to".to_string(), Type::String),
            ],
            Type::String,
        )),
        (Type::String, "starts_with" | "ends_with" | "contains") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("needle".to_string(), Type::String),
            ],
            Type::Bool,
        )),
        (Type::String, "split") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("sep".to_string(), Type::String),
            ],
            Type::List(Box::new(Type::String)),
        )),
        // ⚠ v0.104.6 D66：**以下 3 条签名不可达** —— `Value::Conversation`
        // 在全仓**零构造点**（`grep -rn 'Value::Conversation\s*{' src/` 只命中
        // 模式匹配、`match` arm、Display/JSON 序列化与 typeck 自身，无一处构造）。
        //
        // 意味着源语言里**拿不到**一个 conversation 值，本组签名与运行期
        // `method_dispatch.rs:38` → `call_method_conversation`（chat / history /
        // clear / model / len 五条 arm）整体不可达。
        //
        // 与 D59 / D60 同族：`ai.create(...)`（spec §3.1 :120 承诺的唯一生产者）
        // 未实现。**要接通得先实现 `ai.create`** —— 属语义设计，不是接线活。
        // 与 `Value::AiConfig` / `Value::HttpRequest` 并列为同一批幽灵类型。
        (Type::Conversation, "chat") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::Unknown,
        )),
        (Type::Conversation, "history" | "len") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::List(Box::new(Type::Unknown)),
        )),
        (Type::Conversation, "model") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::String,
        )),
        // v0.104.6 D85：`Type::Agent` 的方法此前**一条签名都没有** ——
        // `agent.create(...)` / `a.run(...)` 的返回类型全是 TypeVar。
        // 返回类型逐条核对自运行期：
        //   agent.create(name, cfg) → Ok(Value::Agent { … })      ⇒ Agent
        //   agent.critic(ans, ctx?) → run_critic → String        ⇒ String
        //   a.run(task)            → run_agent   → String        ⇒ String
        //   a.name()               → Ok(Value::String(name))     ⇒ String
        //   a.max_steps()          → Ok(Value::Float(..))       ⇒ Float
        //
        // ⚠ **一处同类型合并**：`Type::Agent` 同时表示「agent 模块」与
        // 「Agent 值」，故两套方法名登记在**同一个接收者**上。后果是模块上
        // 不存在的 `agent.run` / `agent.name` / `agent.max_steps` 现在会
        // **通过 typeck**（运行期仍会拒 `Agent.run`）。这是把两种东西塞进一个
        // `Type` 变体的固有代价；要彻底解决需要给模块对象单独的 `Type`
        // （v1.0 方向的设计决定），此处如实记录而不是假装没有。
        (Type::Agent, "create") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Agent),
                ("a".to_string(), Type::Any),
            ],
            Type::Agent,
        )),
        (Type::Agent, "critic" | "run") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Agent),
                ("a".to_string(), Type::Any),
            ],
            Type::String,
        )),
        (Type::Agent, "name") => Some(Signature::new(
            vec![("self".to_string(), Type::Agent)],
            Type::String,
        )),
        (Type::Agent, "max_steps") => Some(Signature::new(
            vec![("self".to_string(), Type::Agent)],
            Type::Float,
        )),
        // v0.104.6 D76：`ai.tokens()` 与 `AiTokens` 值上的四个方法此前
        // **完全没有签名** —— `let v: Int = ai.tokens()` 静默通过。
        //
        // `ai.tokens()` 运行期返回 `Value::Builtin(BuiltinKind::AiTokens)`
        // → 声明为 `Type::Builtin`（与 `Value::Builtin` 对应的那个变体）。
        // 四个计数器方法（`ai_tokens.rs`）一律返 `Float`。
        //
        // ⚠ `tokens` 必须是**变参**（最小 0）：运行期
        // `(BuiltinKind::AiChat, "tokens") => Ok(Value::Builtin(BuiltinKind::AiTokens))`
        // **忽略全部实参**，所以 `ai.tokens()` 与 `ai.tokens("hi")` 等价。
        // 声明成定长 0 参会把后者判成元数错 ——
        // `tests/ai_namespace_reachability.rs` 恰好钉着这一条。
        (Type::AiModule, "tokens") => Some(Signature {
            params: vec![("self".to_string(), Type::AiModule)],
            raw_params: vec![None],
            return_type: Type::Builtin,
            raw_return_type: None,
            variadic: true,
        }),
        (Type::Builtin, "input" | "output" | "total" | "calls") => Some(Signature::new(
            vec![("self".to_string(), Type::Builtin)],
            Type::Float,
        )),
        (Type::AiModule, "chat") => Some(Signature::new(
            // v0.75.84: ai.chat(prompt[, {model: "..."}]) — prompt 必选；
            // 可选 dict 参数不受 arity 强制（infer_method_call 只核对
            // user_arity，多传不报）。
            //
            // v0.104.6：返回类型 `Type::AiResult` → `Type::String`，
            // 与运行期 `do_ai_chat` 的实际返回及上方同名 builtin 签名对齐。
            // 详见上面 `"ai.chat"` 处的完整说明（该缺陷让 `ai.chat` 在
            // 源语言里任何用法都过不了类型检查）。
            vec![
                ("self".to_string(), Type::AiModule),
                ("prompt".to_string(), Type::String),
            ],
            Type::String,
        )),
        // v0.103: ai.critic(answer, ctx?) —— spec §12.5。ctx 可选（多传/少传
        // dict 与字符串都容许），返回结构化裁决 dict。
        (Type::AiModule, "critic") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::AiModule),
                ("answer".to_string(), Type::String),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::String, Type::Nil]),
                ),
            ],
            Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
        )),
        // ⚠ v0.104.6：**不可达的死注册**（`Value::AiConfig` 与
        // `Value::HttpRequest` 的方法签名都属这一类）。
        //
        // 审计实测（`_audit_method_parity` 那类三方对拍 + 行为探针）：
        //   * 这两个 Value 变体**全仓从未被构造** —— 只在 `flow::type_name`、
        //     `flow::json`、`value::display` 里被**模式匹配**，没有任何一处
        //     `Value::AiConfig { … }` 的构造表达式；`AiConfig::new()` 报
        //     `Undefined function or task`，`with model = "m" … end` 返回 Nil
        //     （config 存进 `CoreRuntime.current_ai_config`，是 Rust 侧的
        //     `AiConfigValue`，不是 `Value::AiConfig`）。
        //   * 即便拿到这样的值，`interpreter::method_dispatch::call_method`
        //     也**没有对应分支** —— 它会落进最终的 `_ =>` 臂报
        //     "Can only call methods on lists, dicts, …"。
        //
        // 也就是说：若将来有人补上构造入口，**这些签名会立刻变成
        // 「typeck 放行 → 运行期报错」的陷阱**（与 v0.104.6 修掉的
        // `str()` / `int()` / `float()` / `bool()` 同一族）。
        //
        // 保留它们是因为 typeck 的 `AiConfig` / `HttpRequest` 类型本身仍在用
        // （`ai.chat(cfg: AiConfig, prompt: String)` 的形参就是 `Type::AiConfig`）。
        // **接线构造入口时必须同时补 `call_method` 分支**，否则就是新缺陷。
        (Type::AiConfig, "model" | "temperature" | "max_tokens" | "system" | "budget") => Some(
            Signature::new(vec![("self".to_string(), Type::AiConfig)], Type::AiConfig),
        ),
        // v0.103: Router / McpServer 方法签名补齐用户参数 —— 此前只声明
        // `self`，与运行时 call_method_router / call_method_mcp 的实际实参
        // 数量不一致：`router.route("GET", "/x", h)`（3 用户参）被 typeck
        // 误报 "Expected 0 arguments, got 3"。签名是跨层契约，必须与运行时
        // 的实参消费一致。
        (Type::Router, "route") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Router),
                ("method".to_string(), Type::String),
                ("path".to_string(), Type::String),
                ("handler".to_string(), Type::Any),
            ],
            Type::Router,
        )),
        (Type::Router, "listen") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Router),
                ("addr".to_string(), Type::String),
            ],
            Type::Nil,
        )),
        (Type::McpServer, "tool") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::McpServer),
                ("name".to_string(), Type::String),
                ("schema".to_string(), Type::Any),
                ("handler".to_string(), Type::Any),
            ],
            Type::McpServer,
        )),
        (Type::McpServer, "serve") => Some(Signature::new(
            vec![("self".to_string(), Type::McpServer)],
            Type::Nil,
        )),
        (Type::HttpRequest, "json") => Some(Signature::new(
            vec![("self".to_string(), Type::HttpRequest)],
            Type::Unknown,
        )),
        _ => None,
    }
}

fn method_signature_via_type(receiver: &Type, method: &str) -> Option<Signature> {
    let ret = method_return_type(receiver, method);
    // v0.75.91: top type (Any) 与 escape hatch (Unknown) 都不算「已知签名」，
    // 都应 None —— 调用方走 fallback 路径（unknown_method_returns_none 测试）
    if matches!(ret, Type::Any) || matches!(ret, Type::Unknown) {
        None
    } else {
        Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            ret,
        ))
    }
}

///  Return the result type of a method call. Mirrors the v0.x dispatch
///  table. Returns `Type::Any` for unknown combinations so callers can
///  fall back gracefully.
pub fn method_return_type(receiver: &Type, method: &str) -> Type {
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return sig.return_type;
    }
    method_return_type_fallback(receiver, method)
}

fn method_return_type_fallback(receiver: &Type, method: &str) -> Type {
    if let Type::Union(_) = receiver {
        return Type::Unknown;
    }
    if method == "len" {
        return Type::Float;
    }
    Type::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_signature_is_known() {
        let sig = lookup_builtin("print").expect("print should be registered");
        assert_eq!(sig.params.len(), 1);
        assert!(matches!(sig.return_type, Type::Nil));
    }

    #[test]
    fn router_new_registered() {
        let sig = lookup_builtin("Router::new").expect("Router::new registered");
        assert_eq!(sig.params.len(), 0);
        assert!(matches!(sig.return_type, Type::Router));
    }

    #[test]
    fn mcp_server_new_registered() {
        let sig = lookup_builtin("McpServer::new").expect("McpServer::new registered");
        assert_eq!(sig.params.len(), 0);
        assert!(matches!(sig.return_type, Type::McpServer));
    }

    #[test]
    fn route_method_on_router() {
        let sig = method_signature(&Type::Router, "route").expect("Router.route");
        // v0.103: self + method + path + handler —— 与运行时
        // call_method_router 的实参消费一致（此前只声明 self，导致
        // `router.route("GET", "/x", h)` 被误报 arity 错误）。
        assert_eq!(sig.params.len(), 4);
        assert!(matches!(sig.return_type, Type::Router));
    }

    #[test]
    fn mcp_tool_method_arity_matches_runtime() {
        // McpServer.tool(name, schema, handler) — self + 3 用户参
        let sig = method_signature(&Type::McpServer, "tool").expect("McpServer.tool");
        assert_eq!(sig.params.len(), 4);
        assert!(matches!(sig.return_type, Type::McpServer));
    }

    #[test]
    fn router_listen_accepts_addr() {
        // Router.listen(addr) — self + 1 用户参
        let sig = method_signature(&Type::Router, "listen").expect("Router.listen");
        assert_eq!(sig.params.len(), 2);
    }

    #[test]
    fn list_map_arity_is_two() {
        // v0.75.16: map 接收闭包参数（receiver + f = 2 参）。
        assert_eq!(
            method_arity(&Type::List(Box::new(Type::Unknown)), "map"),
            Some(2)
        );
    }

    #[test]
    fn unknown_method_returns_none() {
        assert!(method_signature(&Type::String, "no_such_method").is_none());
    }
}
