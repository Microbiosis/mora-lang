//! v0.104.6 D293：模块方法表由「名字表 + 组号」两张手写表合成**一张**。
//!
//! ## 重构前（D171 形态）的失守方式
//!
//! ```text
//! const MATH_GROUPS = &[ &["PI","E",…], &["sin",…], &["pow",…], … ];  // 名字
//! "math" => match group_of(MATH_GROUPS, method) {
//!              Some(0) => params_variadic(0, Type::Float),            // 签名
//!              Some(1) => params_variadic(1, Type::Float),
//! ```
//!
//! 两张表之间**没有任何编译期约束**。把 `Some(1)` 抄成 `Some(2)` 不会编译
//! 报错，只会让 17 个一元函数静默拿到二元签名 —— 而 `module_method_names`
//! 照旧报出真名：**自省说它有、typeck 却不查**。D171 注释里「二者不可能
//! 漂移」那句只对**方法名**成立，对**组号↔签名**不成立。
//!
//! ## 修法
//!
//! `MethodGroup { names, min_arity, ret }` —— 名字与签名同处一项，组号这个
//! 中间层整个消失，上面的失守形态**结构上不可能**。
//!
//! D293 只改表述方式、不改任何签名，故本文件的前三条判据全部在**新结构**
//! 上重建护栏（而不是复述重构前已有的那些）。
//!
//! ## 等价性怎么验的
//!
//! 重构**前后**各跑一次全量 dump（26 个模块名 × 全部方法名的完整
//! `Signature` Debug），SHA256 逐字节比对：30286 bytes，hash 相同、0 行差异。
//! 本文件是那次比对留下的**长期**不变量，dump 本身已随临时 oracle 删除。

use mora::typeck::Type;
use mora::typeck::dispatch::{module_method_names, module_method_signature};

/// 走模块分组表的 20 个模块（`ai` / `agent` / `random` 三个**不走**，
/// 见 `module_level_signature_is_absent_for_the_three_typed_modules`）。
const GROUPED: &[&str] = &[
    "math", "json", "file", "exec", "stats", "linalg", "document", "mora", "bus", "mock", "ccr",
    "plan", "tea", "tool", "skill", "xform", "schedule", "memory", "sandbox", "web",
];

/// ① `module_method_names` 列出的**每个**名字都必须解析出签名。
///
/// 这是 D171 结构的直接后果：列出的名字来自方法组表，查签名的也是同一张表，
/// 「列了却没有签名」在旧结构里是可能的（组表加了名字、忘了加 `Some(N)` 臂），
/// 现在两者同处一项 ⇒ 不可能再分叉。
#[test]
fn every_listed_method_resolves_to_a_signature() {
    for m in GROUPED {
        let names = module_method_names(m);
        assert!(!names.is_empty(), "[{m}] 分组表不应为空");
        for meth in &names {
            assert!(
                module_method_signature(m, meth).is_some(),
                "[{m}.{meth}] 被 `module_method_names` 列出，却没有签名 —— \
                 这会让自省报出一个 typeck 不检查的方法（D171 的失守形态）"
            );
        }
    }
}

/// ② 同一模块内**不得有重名**。
///
/// `find` 只返回**第一个**命中项 ⇒ 重名会让后一个组被静默吞掉：它既不在
/// 任何有效签名路径上，又在 `module_method_names` 里出现一次。D171 的
/// `group_of`（`position`）有同样的行为，所以这不是新引入的风险，但旧结构
/// 没有任何东西能发现它 —— 组号是手写的，重名同样是手写的。
#[test]
fn no_duplicate_method_names_within_a_module() {
    for m in GROUPED {
        let mut seen: Vec<&str> = Vec::new();
        let mut dups: Vec<&str> = Vec::new();
        for n in module_method_names(m) {
            if !seen.contains(&n) {
                seen.push(n);
            } else {
                dups.push(n);
            }
        }
        assert!(
            dups.is_empty(),
            "[{m}] 方法名重复 {dups:?} —— 重复项里靠后的那个组永远不会被查到，\
             且它让 `module_method_names` 报出一个实际无签名的方法"
        );
    }
}

/// ③ 未列出的名字**不得**有签名。
///
/// 与 ① 一起构成双向闭合：**名字集合 ≡ 有签名的方法集合**。
/// D170 记档时把这一条列为将来重构的验收判据（其原文是「两张手写表互为
/// 交叉验证」）；D293 把两张表合成一张后，它从「需要人工维护的约定」
/// 变成了「由同一张表保证的不变量」，本条把它钉住。
#[test]
fn unlisted_names_have_no_signature() {
    for m in GROUPED {
        let names = module_method_names(m);
        for probe in ["__nope__", "definitely_not_a_method", "", "MATH"] {
            if names.contains(&probe) {
                continue;
            }
            assert!(
                module_method_signature(m, probe).is_none(),
                "[{m}.{probe}] 不在方法名表里，却查出了签名 —— \
                 自省与 typeck 对不上"
            );
        }
    }
    // 整张表之外的模块名
    for m in ["nope", "mathx", "MATH", "Math", ""] {
        assert!(
            module_method_signature(m, "anything").is_none(),
            "[{m}] 不是已登记的模块名，不应有签名"
        );
    }
}

/// **现状判据**：`ai` / `agent` / `random` 列进了 `module_method_names`，
/// 却**故意没有**模块级签名 —— 它们有精确的 `Type` 变体
/// （`Type::AiModule` / `Type::Agent` / `Type::RandomModule`），签名走
/// `method_signature` 的 `Type` 级 match（D175 的设计决定）。
///
/// 本条**钉住现状**。若将来有人给它们补了模块级签名，本条会红并提示改写；
/// 若有人从 `module_method_names` 里删掉它们，`d170_census_methods_of_module_objects`
/// 也会红。两侧都有护栏。
#[test]
fn module_level_signature_is_absent_for_the_three_typed_modules() {
    for m in ["ai", "agent", "random"] {
        let names = module_method_names(m);
        assert!(!names.is_empty(), "[{m}] 仍应列出方法名（D175）");
        for meth in &names {
            assert!(
                module_method_signature(m, meth).is_none(),
                "[{m}.{meth}] 突然有了模块级签名 —— 与 D175 的设计决定冲突，\
                 请先确认 `Type` 级 match 那条路是否成了重复声明"
            );
        }
    }
}

/// 分组表覆盖 `MODULE_OBJECTS` 的每个注册名（除上面那三个）。
///
/// 防止「新增模块忘了建表」—— 那会让它的方法**静默地**既无签名也无自省，
/// 且因为「查不到表」与「表里没有」在行为上完全一样，**不会有任何测试失败**。
/// 这正是 D74 把 `BuiltinKind::Toolplane` 建成 `"toolplane"` 时的形态。
#[test]
fn every_registered_module_has_a_group_table_or_is_documented_as_exempt() {
    for (name, _kind) in mora::value::MODULE_OBJECTS {
        let in_table = GROUPED.contains(name);
        assert!(
            in_table || matches!(*name, "ai" | "agent" | "random"),
            "模块 `{name}` 既没有分组表，也不在三个豁免模块里 —— \
             它的方法会静默地既无签名也无自省。建表，或把它加进豁免名单并写明理由。"
        );
    }
}

/// 抽样钉住几个**跨类型形态**的签名 —— 覆盖 `Ret` 的每种构造方式，
/// 防止 `materialize()` 写错时只在小众类型上暴露。
#[test]
fn ret_shapes_materialize_correctly() {
    let cases: &[(&str, &str, usize, &Type)] = &[
        // (module, method, 最小元数含 self, 返回类型)
        ("math", "sqrt", 2, &Type::Float), // Float
        (
            "math",
            "floor",
            2,
            &Type::Union(vec![Type::Int, Type::Float]),
        ), // Union
        ("math", "is_nan", 2, &Type::Bool), // Bool
        ("math", "PI", 1, &Type::Float),   // 零参
        ("ccr", "len", 1, &Type::Int),     // Int
        ("file", "list", 2, &Type::List(Box::new(Type::String))), // List
        ("json", "parse", 2, &Type::Any),  // Any
        ("document", "parse", 2, &Type::Document), // Document
        ("bus", "emit", 2, &Type::Nil),    // Nil
        (
            "tool",
            "info",
            2,
            &Type::Union(vec![
                Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                Type::Nil,
            ]),
        ), // Dict + Union
        (
            "stats",
            "histogram",
            3,
            &Type::List(Box::new(Type::Dict(
                Box::new(Type::String),
                Box::new(Type::Float),
            ))),
        ), // List[Dict]（嵌套两层）
        (
            "linalg",
            "transpose",
            2,
            &Type::List(Box::new(Type::List(Box::new(Type::Float)))),
        ), // List[List]
    ];
    for (m, meth, want_arity, want_ret) in cases {
        let sig =
            module_method_signature(m, meth).unwrap_or_else(|| panic!("[{m}.{meth}] 应当有签名"));
        assert_eq!(
            sig.params.len(),
            *want_arity,
            "[{m}.{meth}] 元数（含 self）变了: {:?}",
            sig.params.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        assert_eq!(&sig.return_type, *want_ret, "[{m}.{meth}] 返回类型变了");
        assert!(sig.variadic, "[{m}.{meth}] 模块方法一律变参（只校验下限）");
        assert_eq!(
            sig.raw_params.len(),
            sig.params.len(),
            "[{m}.{meth}] raw_params 必须与 params 等长"
        );
        assert!(
            sig.raw_params.iter().all(|r| r.is_none()),
            "[{m}.{meth}] 模块方法不登记 raw hint"
        );
    }
}

/// `bus` 那一组在 D293 被**拆成两组**（`emit`/`off` 返 `Nil`，
/// `publish`/`subscribe` 返 `Float`）—— 它是 D171 结构下「同组不同签名」
/// 的唯一破口，拆分是本次重构唯一的数据变更，故单独钉住。
#[test]
fn bus_group_split_preserves_every_return_type() {
    for (meth, want_arity, want) in [
        ("emit", 2, Type::Nil),
        ("off", 2, Type::Nil),
        ("publish", 2, Type::Float),
        ("subscribe", 2, Type::Float),
        ("count", 1, Type::Float), // 零参（含 self 共 1 个形参）
    ] {
        let sig = module_method_signature("bus", meth).expect("bus 方法必须有签名");
        assert_eq!(sig.return_type, want, "[bus.{meth}] 返回类型变了");
        assert_eq!(sig.params.len(), want_arity, "[bus.{meth}] 最小元数变了");
    }
}
