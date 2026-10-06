//! v0.104.6 D396 —— `Subst::walk_star` 的 **List 分支用浅层判据**，
//! 嵌套容器的解析结果被整份丢弃（修复轮；当前脚本**不可达**，如实记录）
//!
//! ## 缺陷
//!
//! `walk_star` 是**递归**的，但 List 分支的 `changed` 判据是**浅层**的：
//! `items.iter().any(|it| matches!(it, Value::LogicVar(_)))` —— 只看**直接子项**
//! 是不是 `LogicVar`。
//!
//! 于是「直接子项是**容器**、变量藏在容器里面」时：容器已被递归解析，
//! 但 `changed` 仍为 `false` ⇒ 函数返回**原始** `walked`，把解析结果**整份丢弃**。
//!
//! 实测（`s.bind(0, Int(42))`）：
//!
//! ```text
//! walk_star([LogicVar(0)])        → List([Int(42)])               ✅
//! walk_star([[LogicVar(0)]])      → List([List([LogicVar(0)])])   ❌ 原样未解析
//! walk_star(Dict{k: LogicVar(0)}) → Dict({"k": Int(42)})         ✅
//! walk_star(Cons{car: LogicVar})  → Cons { car: Int(42) }        ✅
//! ```
//!
//! ## 同一个函数里三种写法，**只有 List 错** —— 这是「笔误」而非设计的证据
//!
//! | 分支 | `changed` 怎么算 | 结果 |
//! |---|---|---|
//! | `Dict` | **深比较** `&resolved != val` | ✅ 正确 |
//! | `Cons` | **没有** `changed`，总是重建 | ✅ 正确 |
//! | `List` | **浅层** `matches!(it, LogicVar)` | ❌ **错** |
//!
//! 修法：与 `Dict` 分支对齐，改用**深比较**（保留「无变化就不重建」的优化）。
//!
//! ## 后果会走**用户可见**路径（若可达）
//!
//! `walk_star` 的**唯一**生产调用方是 `reify`（`reify.rs:15`），
//! 而 `reify` 是 `solve` 的答案投影路径（`project_solution` → `h_solve`）。
//! 残留变量会被 `rename_unbound` 改名成 **`String("_.0")`** ——
//! 用户拿到的是**字符串占位符**而不是真值。
//!
//! 实测：`reify([[LogicVar(0)]], s)` 修前 = `List([List([String("_.0")])])`。
//!
//! ## ⚠ 当前**脚本不可达**（如实记录，不夸大）
//!
//! rel 的**项语法**（`parser_v3/rel.rs`）只接受：原子、标量字面量、
//! `cons(...)` / `nil`、`?name`、`_` —— **没有列表 / 字典字面量**。
//! 而查询变量只能绑定到 `Cons` 链或标量，两者分支都正确。
//!
//! ⇒ 本条是 **`pub` 库 API 上的潜伏缺陷**：
//! `Subst` / `walk_star` 都是 `pub`，且 `Dict` 分支证明本意就是深比较。
//! 判据 `d396_rel_term_grammar_has_no_list_literal` 把「不可达」这一事实钉住 ——
//! 若将来 rel 项语法支持列表字面量，本条会红并提醒同步复核。

use mora::rel::reify::reify;
use mora::rel::subst::Subst;
use mora::value::Value;
use mora::value::list::List;

fn v(id: u64) -> Value {
    Value::LogicVar(id)
}

fn list(xs: Vec<Value>) -> Value {
    Value::List(List::from(xs))
}

// ── ① 核心：嵌套容器必须被完整解析 ──

/// **嵌套 List**：变量藏在**内层**列表里，外层必须被解析。
#[test]
fn d396_nested_list_is_fully_resolved() {
    let s = Subst::new().bind(0, Value::Int(42));
    let term = list(vec![list(vec![v(0)])]);
    assert_eq!(
        s.walk_star(&term),
        list(vec![list(vec![Value::Int(42)])]),
        "嵌套列表未被解析 —— List 分支的 `changed` 是浅层判据"
    );
}

/// **三层嵌套**同样成立。
#[test]
fn d396_deeply_nested_list_is_fully_resolved() {
    let s = Subst::new().bind(0, Value::Int(7));
    let term = list(vec![list(vec![list(vec![v(0)])])]);
    assert_eq!(
        s.walk_star(&term),
        list(vec![list(vec![list(vec![Value::Int(7)])])])
    );
}

/// **顶层 LogicVar 绑定到嵌套列表**（`project_solution` 的真实形态）。
#[test]
fn d396_query_var_bound_to_nested_list() {
    let s = Subst::new().bind(0, list(vec![list(vec![Value::Int(1), v(1)])]));
    let s = s.bind(1, Value::String("a".into()));
    // 对 `reify(LogicVar(0), s)` 而言这正是「查询变量绑定到嵌套列表」
    assert_eq!(
        s.walk_star(&v(0)),
        list(vec![list(vec![Value::Int(1), Value::String("a".into())])])
    );
}

// ── ② 后果：`reify` 不得把已绑定的变量改名成 `"_.N"` 字符串 ──

/// **`reify` 不得产出 `String("_.0")` 占位符**。
///
/// 这是走用户可见路径的断言：残留变量会被 `rename_unbound` 改名成字符串。
#[test]
fn d396_reify_resolves_nested_instead_of_renaming() {
    let s = Subst::new().bind(0, Value::Int(42));
    let term = list(vec![list(vec![v(0)])]);
    let out = reify(&term, &s);
    assert_eq!(
        out,
        list(vec![list(vec![Value::Int(42)])]),
        "reify 把已绑定的变量改名成了 `_.N` 字符串"
    );
    // 反向对照：真正**未绑定**的变量**仍应**被改名（不得过度解析）
    let out2 = reify(&list(vec![list(vec![v(9)])]), &s);
    assert_eq!(
        out2,
        list(vec![list(vec![Value::String("_.0".into())])]),
        "未绑定变量仍应被 rename_unbound 改名成 `_.0`; 实得 {out2:?}"
    );
}

// ── ③ 四种容器形态必须**一致**（回归护栏） ──

/// **List / Dict / Cons 三条分支给出一致答案**。
///
/// 修前只有 List 不一致；本条是「同一性质用同一断言」的收敛钉。
#[test]
fn d396_three_container_arms_agree() {
    let s = Subst::new().bind(0, Value::Int(42));
    let resolved = Value::Int(42);
    // 直接子项就是变量
    assert_eq!(s.walk_star(&list(vec![v(0)])), list(vec![resolved.clone()]));
    // Dict（一直正确）
    let d = Value::Dict([("k".to_string(), v(0))].into_iter().collect());
    assert_eq!(
        s.walk_star(&d),
        Value::Dict([("k".to_string(), resolved.clone())].into_iter().collect())
    );
    // Cons（一直正确）
    let c = Value::Cons {
        car: Box::new(v(0)),
        cdr: Box::new(Value::Nil),
    };
    assert!(matches!(&s.walk_star(&c), Value::Cons { car, .. } if **car == resolved));
    // 嵌套在 List 里的 Dict / Cons（修前**部分**失效）
    let mixed = list(vec![
        Value::Dict([("k".to_string(), v(0))].into_iter().collect()),
        Value::Cons {
            car: Box::new(v(0)),
            cdr: Box::new(Value::Nil),
        },
    ]);
    assert_eq!(
        s.walk_star(&mixed),
        list(vec![
            Value::Dict([("k".to_string(), resolved.clone())].into_iter().collect()),
            Value::Cons {
                car: Box::new(resolved.clone()),
                cdr: Box::new(Value::Nil),
            },
        ]),
        "List 内嵌 Dict / Cons 时也必须被解析"
    );
}

// ── ④ 不可达性现状钉（防止夸大影响，也防止将来被忽略） ──

/// **rel 项语法没有列表 / 字典字面量** ⇒ 缺陷当前**脚本不可达**。
///
/// `parser_v3/rel.rs` 的项解析只接受：原子（标识符 / 字符串）、
/// 标量字面量（Int/Float/String/Char/BigInt/Bool/Nil）、
/// `cons(...)` / `nil`、`?name`、`_`。
#[test]
fn d396_rel_term_grammar_has_no_list_literal() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/parser_v3/rel.rs"),
    )
    .expect("读 src/parser_v3/rel.rs");
    // 项解析里不应出现「构造 Value::List」的分支
    let constructs_list = src
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("Term::Val(") || l.contains("Term::Param("))
        .any(|l| l.contains("Value::List"));
    assert!(
        !constructs_list,
        "rel 项语法已能构造 `Value::List` ⇒ 嵌套列表缺陷**变为可达**，\
         需补端到端判据（`solve` 返回值是否正确）"
    );
    // 反向对照：确实支持 cons —— 但它产生的是 `Term::Cons`
    // （由 `rename_term` 再转成 `Value::Cons`），不是 `Value::Cons`。
    assert!(
        src.contains("Term::Cons("),
        "rel 项语法应仍支持 `cons(...)`（产生 Term::Cons）"
    );
}

/// **`walk_star` 的唯一生产调用方是 `reify`** —— 后果路径的现状钉。
#[test]
fn d396_walk_star_has_single_production_caller() {
    let mut callers = Vec::new();
    for rel_path in [
        "src/rel/reify.rs",
        "src/rel/unify.rs",
        "src/rel/search.rs",
        "src/rel/goal.rs",
        "src/rel/mod.rs",
        "src/mir/handlers/effects.rs",
    ] {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
        let Ok(src) = std::fs::read_to_string(&p) else {
            continue;
        };
        for (i, line) in src.lines().enumerate() {
            let t = line.trim();
            if t.contains("walk_star(") && !t.contains("pub fn walk_star") && !t.starts_with("//") {
                callers.push(format!("{rel_path}:{}: {t}", i + 1));
            }
        }
    }
    assert_eq!(
        callers.len(),
        1,
        "`walk_star` 应恰好 1 处生产调用（reify）; 实得 {callers:?} —— \
         若新增调用方，需为它补嵌套解析的判据"
    );
    assert!(callers[0].contains("reify.rs"), "实得 {}", callers[0]);
}

/// **`rename_unbound` 确实把残留变量改名成 `String("_.N")`** —— 后果的机制钉。
#[test]
fn d396_unbound_renaming_produces_string_placeholder() {
    let s = Subst::new();
    let out = reify(&v(5), &s);
    assert_eq!(
        out,
        Value::String("_.0".into()),
        "未绑定变量应被改名成 `_.0` 字符串"
    );
    // 同一 id 在多处出现时复用同名（嵌套层同样算「出现」）
    let term = Value::List(List::from(vec![v(9), v(4), v(9), list(vec![v(4)])]));
    let out2 = reify(&term, &s);
    assert_eq!(
        out2,
        Value::List(List::from(vec![
            Value::String("_.0".into()),
            Value::String("_.1".into()),
            Value::String("_.0".into()),
            list(vec![Value::String("_.1".into())]),
        ])),
        "同名复用 + 嵌套改名都应成立; 实得 {out2:?}"
    );
}
