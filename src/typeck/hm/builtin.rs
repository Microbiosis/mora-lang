//! v0.75.70: HM 类型推断 builtin 类型 — 自 hm/mod.rs 拆出（D6 单文件惯例）。
//! builtin_callee_ty（按名字查 callee 类型）+ builtin_type（op 类型）。
//!
//! v0.80: 全部返回 curried Arrow（Type::Arrow）而非 ClosureSig 侧表。
//! 多参数 builtin 用嵌套 Arrow：`Arrow(A, Arrow(B, C, Empty), Empty)`。

use super::*;

/// v0.80: 构建 curried Arrow 类型。
/// `curried_arrow([A, B], C, Empty)` → `Arrow(A, Arrow(B, C, Empty), Empty)`
fn curried_arrow(params: Vec<Type>, ret: Type) -> Type {
    let mut ty = ret;
    for p in params.into_iter().rev() {
        ty = Type::Arrow(
            Box::new(p),
            Box::new(ty),
            crate::mir::effect::EffectRow::Empty,
        );
    }
    ty
}

/// v0.104.6 D78：**变参**内建的 curried arrow。
///
/// 形参是 `Any`、**不限上界**：除了 `min_args` 个声明层，额外挂
/// `EXTRA_SLACK` 个 `Any` 层，让多传的实参有层可消耗 —— 否则多传的那个实参会被
/// 拿去和**返回类型**比对而误报元数错（这正是 D72 在 `file.join`、D76 的
/// `ai.tokens` 上各踩一次的那个坑）。
///
/// `EXTRA_SLACK = 2` 覆盖 `compose(f)` / `compose(f, g)` / `compose(f, g, h)`
/// 与 `partial(fn)` / `partial(fn, a)` / `partial(fn, a, b)`。
fn variadic_arrow(min_args: usize, ret: Type) -> Type {
    variadic_arrow_with(min_args, Type::Any, ret)
}

/// 同上，但**首个**实参是 fresh var（`macroexpand` 的宏名需要参与推断），
/// 其余 slack 层用 `Any` 兜住「多传实参」。
fn variadic_arrow_with(min_args: usize, first: Type, ret: Type) -> Type {
    const EXTRA_SLACK: usize = 2;
    let mut params = vec![first];
    params.extend(std::iter::repeat_n(
        Type::Any,
        min_args.saturating_sub(1) + EXTRA_SLACK,
    ));
    curried_arrow(params, ret)
}

impl HMInference {
    pub(super) fn builtin_callee_ty(&mut self, name: &str) -> Option<Type> {
        // v0.55: prefer the canonical dispatch registry for the
        // canonical arity / return type, but mint fresh type variables
        // for every parameter so the HM unifier can still infer
        // concrete argument types instead of being pinned to a Union
        // annotation.
        if let Some(sig) = crate::typeck::dispatch::lookup_builtin(name) {
            let param_count = sig.params.len();
            let param_types: Vec<Type> = (0..param_count).map(|_| self.fresh_type_var()).collect();
            return Some(curried_arrow(param_types, sig.return_type.clone()));
        }
        match name {
            "print" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::Nil))
            }
            "len" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::Int))
            }
            "str" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::String))
            }
            "int" => Some(curried_arrow(vec![Type::String], Type::Int)),
            "float" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::Float))
            }
            "bool" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::Bool))
            }
            // v0.102: 声明式范式 — 目标内建
            "unify" => {
                let a = self.fresh_type_var();
                let b = self.fresh_type_var();
                Some(curried_arrow(vec![a, b], Type::Goal))
            }
            "cons" => {
                let h = self.fresh_type_var();
                let t = self.fresh_type_var();
                Some(curried_arrow(
                    vec![h.clone(), t.clone()],
                    Type::Cons(Box::new(h), Box::new(t)),
                ))
            }
            "range" => {
                let a = self.fresh_type_var();
                let b = self.fresh_type_var();
                let c = self.fresh_type_var();
                let elem = self.fresh_type_var();
                Some(curried_arrow(vec![a, b, c], Type::List(Box::new(elem))))
            }
            // ─── v0.104.6 D68：运行期 globals 里**遗漏**的 4 个内建 ───
            //
            // `Interpreter::new()` 把 `print` / `range` / `len` /
            // `compose_prompt` / `tail` / `compress` / `crush_json` 七个
            // 一起 define 进 globals（`interpreter/mod.rs:495-511`），但本表
            // （及 `builtin_signatures()`）只登记了前三个 —— 后四个落到
            // `None`，调用方 `unwrap_or_else(|| self.fresh_type_var())` 返回
            // **永不解算的 TypeVar**。后果与 D67 同型，且更刺眼：
            //
            // ```mora
            // let v = compress("abcdef", "head_tail")   → 运行期得 "abcdef"（String）
            // let v: Int = compress("abcdef", "head_tail")  → **被接受**  ❌
            // ```
            //
            // 与 D67 的区别：D67 是「类型没算出来」，这里是**压根没查表**。
            // 判据可用「把返回值标成明显错误的类型，看是否被拒」。
            //
            // 四个的运行期返回类型逐个核对过（`builtin_impls.rs`）：
            //   compress      → compress_top() 两条 Ok 都是 Value::String
            //   crush_json    → Ok(Value::String(format!(...)))
            //   tail          → Ok(Value::String(tail_str))
            //   compose_prompt→ Ok(Value::String(buf))
            //
            // 形参一律 mint fresh var（沿用本文件既有约定，v0.80 注释：
            // 让 unifier 去推实参类型）。
            //
            // **元数声明的是「至少这么多」**（`Signature::variadic` 文档同款
            // 语义，`range(0, 3)` 在 3 形参下即可用即为既有先例），
            // 所以可选的第 3 实参也**必须**占一格，否则它会落去和返回类型
            // 比对 —— 实测 `compress(txt, strat, opts)` 报
            // "expected string, got fn (dict<…>) -> …"。
            "compress" => {
                let input = self.fresh_type_var();
                let strategy = self.fresh_type_var();
                let opts = self.fresh_type_var();
                Some(curried_arrow(vec![input, strategy, opts], Type::String))
            }
            "crush_json" => {
                let input = self.fresh_type_var();
                let max = self.fresh_type_var();
                let opts = self.fresh_type_var();
                Some(curried_arrow(vec![input, max, opts], Type::String))
            }
            "tail" => {
                let path = self.fresh_type_var();
                let max = self.fresh_type_var();
                Some(curried_arrow(vec![path, max], Type::String))
            }
            // `compose_prompt` 是**真变参**（`for arg in args`，可续传任意个
            // section），curried arrow 表达不了，登记在 `builtin_signatures()`
            // 里并置 `variadic`，由 `infer.rs` 的变参分支逐实参校验。
            // 登记见 dispatch.rs 的 `builtin_signatures()`。
            // `uncurry` 的结果由**被调函数**决定 → fresh var。
            //
            // ─── v0.104.6 D80：`compose` / `partial` 改走 `builtin_signatures` ───
            //
            // v0.104.6 D79 曾在**这里**用 `variadic_arrow(1, …)` 声明它们，
            // 靠「额外挂 2 层 `Any` slack」容纳多传实参。D80 的安全扫查出
            // **这是回归**：`compose(1, 2, 3, 4)` / `partial(1, 2, 3, 4, 5)`
            // 实测 exit 2（`expected compose, got fn (float) -> …`）——
            // slack 用尽后，多传的实参被拿去和**返回类型**比对。
            //
            // 固定 slack 治不了**无上界**的变参（spec §12 只说 `...`）。正确
            // 做法是登记进 `builtin_signatures()` 并置 `variadic`：
            // `infer_call` 的变参分支**逐实参校验、完全不设上界**，返回声明的
            // 结果类型。见 dispatch.rs 里那两条。
            //
            // ─── v0.104.6 D78：spec §12 承诺但**此前完全没登记**的内建 ───
            //
            // `compose` / `partial` / `curry` / `apply` / `car` / `cdr` /
            // `uncurry` 在 `builtin_signatures()` 与本文件里**一条都没有**，
            // 落到 `unwrap_or_else(|| self.fresh_type_var())` → 结果类型恒为
            // 未解算的 TypeVar，标注形同虚设：
            //     let v: String = compose(f, g)   → 修前被接受  ❌
            //
            // 之所以一直没被撞见，是因为 `print` 的形参 Union **含 `Any`** ——
            // TypeVar 能装进任何成员，于是「打不出来」这个症状被掩盖了；只有
            // **显式标注**才暴露。（与 D62 / D67 / D68 同源：TypeVar 对任何类型
            // 都兼容。）
            //
            // 参形一律 `Any`、**不限上界**：只给**结果**类型，不收紧实参 ——
            // 与 D72 的「只校验下限、不收紧上限」同源纪律。
            //
            // `compose` / `partial` 的 `Type` 变体本就存在（值域侧也有
            // `Value::Compose` / `Value::Partial`），可精确声明；
            // `curry` 缺 `Type::Curry` 变体（扩 `Type` 枚举是 v1.0 方向的
            // 设计决定），故给 `Any` —— 如实反映「值域存在、类型域没有对应物」。
            // `compose` / `partial` 已移出本函数，见上方 D80 注释。
            "curry" => {
                let f = self.fresh_type_var();
                let arity = self.fresh_type_var();
                Some(curried_arrow(vec![f, arity], Type::Any))
            }
            // `apply(fn, [args])` / `car(cell)` / `cdr(cell)` / `uncurry(fn)`
            // 的结果由**被调函数 / 容器元素**决定，运行期不固定 → fresh var。
            "apply" => {
                let f = self.fresh_type_var();
                let arglist = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(curried_arrow(vec![f, arglist], ret))
            }
            "car" | "cdr" | "uncurry" => {
                let arg = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(curried_arrow(vec![arg], ret))
            }

            // ─── v0.104.6 D79b：补上普查里**判定不确定**的那 4 个 ───
            //
            // D79 末尾诚实标注过：`swap` / `into` / `macroexpand` / `batch_chat`
            // 的普查结论**不确定**（第一轮探针实参写错，运行期就报错）。
            // 此处逐个读运行期实现补齐判据，不再留不确定项：
            //
            //   batch_chat(list)  → 逐项 `do_ai_chat`，而它 `Ok(Value::String(…))`
            //                        ⇒ List[String]（**可精确**）
            //   into(list, fn)    → 逐项调 fn，元素为 fn 的返回值；命中 List 时
            //                        会 extend（展平）⇒ List[α]（α 只能 fresh）
            //   macroexpand(n, …) → 跑宏体的 MIR，结果即宏的返回值 ⇒ fresh
            //   swap(atom, fn)    → `Ok(new_val)`，new_val 是 fn 的返回值 ⇒ fresh
            "batch_chat" => {
                let prompts = self.fresh_type_var();
                Some(curried_arrow(
                    vec![prompts],
                    Type::List(Box::new(Type::String)),
                ))
            }
            "into" => {
                let coll = self.fresh_type_var();
                let transform = self.fresh_type_var();
                let elem = self.fresh_type_var();
                Some(curried_arrow(
                    vec![coll, transform],
                    Type::List(Box::new(elem)),
                ))
            }
            // `macroexpand(name, args...)` 的实参个数由**宏定义的形参表**决定
            // （`expr_args.len() != params.len()` 才报错）——**没有固定上界**。
            //
            // D80：这里**不能**像 `compose` / `partial` 那样登记进
            // `builtin_signatures` 的变参表 —— 宏的返回值需要逐次 mint 一个
            // fresh var（静态签名表做不到）。故仍留在本函数，但把结果类型给
            // **`Type::Any`**：`Any` 是 top type，多传的实参与它合一恒成功
            // （`unify` 的 `(Any, _) => Ok` arm），于是**无需靠 slack 硬撑
            // 任何上界**。这比 D79 的「fresh var + 2 层 slack」更宽松，且
            // 不会像 slack 用尽那样误拒（见 D80 记录的那次回归）。
            "macroexpand" => Some(variadic_arrow(1, Type::Any)),
            "swap" => {
                let a = self.fresh_type_var();
                let f = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(curried_arrow(vec![a, f], ret))
            }

            // ─── v0.104.6 D79：普查里剩下**返回类型确定**的五条 ───
            //
            // 对 38 个运行期可调用的裸内建逐个实测「故意写错标注看是否被拒」
            // （结果见 CHANGELOG D79 的表）。除本组外仍无签名的那些，
            // **绝大多数属如实无法声明**，不是漏登记：
            //   · `car` / `cdr` / `uncurry` / `deref` —— 结果由容器元素 /
            //     被调函数 / 原子内容决定。`deref` 尤其无解：`Type::Atom` 是
            //     **单元变体、没有载荷**，类型域里没有位置放「这个原子装什么」。
            //   · `read` / `quote` → `Value::Code`，而 `Type` **没有 `Code` 变体**
            //     （扩枚举是 v1.0 方向的设计决定）→ 只能 `Any`
            //   · `eval` —— 结果是被求值表达式的类型，需要真正的递归推断
            //   · `swap` / `into` / `macroexpand` / `batch_chat` —— 结果由回调 /
            //     元素类型 / 宏定义决定，同样只能宽松
            //
            // 能**精确声明**的五条（逐条核对自 `builtin_impls.rs` 的 `Ok(Value::…)`）：
            //   type_of(x)         → Ok(Value::String(value_type_name(x))) ⇒ String
            //   atom(x)            → Ok(Value::Atom(…))，`Type::Atom` 存在 ⇒ Atom
            //   methods_of(x)      → Ok(Value::List(…map(Value::String))) ⇒ List[String]
            //   gensym()           → Ok(Value::String(format!("g{n}")))    ⇒ String
            //   is_instance(x, "T")→ Ok(Value::Bool(… == type_name))        ⇒ Bool
            "type_of" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::String))
            }
            "atom" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::Atom))
            }
            "methods_of" => {
                let arg = self.fresh_type_var();
                Some(curried_arrow(vec![arg], Type::List(Box::new(Type::String))))
            }
            // `gensym()` 运行期显式拒绝任何实参（`args.is_empty()` 才通过）
            "gensym" => Some(curried_arrow(vec![], Type::String)),
            "is_instance" => {
                let value = self.fresh_type_var();
                let name = self.fresh_type_var();
                Some(curried_arrow(vec![value, name], Type::Bool))
            }
            _ => None,
        }
    }

    pub(super) fn builtin_type(&mut self, op: &BuiltinOp) -> Result<Type, Vec<TypeError>> {
        match op {
            BuiltinOp::Print => {
                let arg = self.fresh_type_var();
                Ok(curried_arrow(vec![arg], Type::Nil))
            }
            BuiltinOp::Assert => Ok(curried_arrow(vec![Type::Bool], Type::Nil)),
            BuiltinOp::Not => Ok(curried_arrow(vec![Type::Bool], Type::Bool)),
            BuiltinOp::Length => {
                let arg = self.fresh_type_var();
                Ok(curried_arrow(vec![arg], Type::Int))
            }
        }
    }
}
