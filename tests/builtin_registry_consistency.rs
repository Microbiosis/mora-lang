//! v0.104.6：内建注册表一致性（根因 A · 内建语义多点登记）。
//!
//! ## 根因
//!
//! 同一个内建名的语义在仓内**多点独立登记**，靠约定保持一致、实际漂移：
//!
//! 1. typeck 权威签名表 `typeck::dispatch::builtin_signatures()`
//! 2. typeck 本地兜底 `typeck::hm::builtin::builtin_callee_ty` 的 match
//!    （含 `"car" | "cdr" | "uncurry"` 这类多模式 arm）
//! 3. 运行期分发 `interpreter::dispatch` 的 `match`
//!
//! D4 / D5 / D9 / D12 / D16 一族缺陷皆源于此。本文件把「不许漂移」从
//! **约定**变成 **CI 强制的不变量**。
//!
//! ## 判据为什么是**行为探针**而不是「比对内部表」
//!
//! 签名可能落在上述 1 或 2 任一处，且 2 里有多模式 arm（`"car" | "cdr" |
//! "uncurry"`）—— 直接枚举内部表去比对极易算错（本轮初稿就因此把「无签名」
//! 误报成 24 条）。故此处改用**唯一可信的事实**：拿一段**故意用错**该内建
//! 的程序跑 typeck（直接调库入口，不经子进程）：
//!
//! - 返回**具体类型**的内建（[`CONCRETE_RETURN`]）—— 探针赋给不匹配标量，
//!   **必须被拒绝**，否则说明签名丢失；
//! - 返回随回调/元素变化的「宽松」内建 —— 用固定探针判不了（类型 var 会
//!   正常合一），故只要求其**能被解析**（证明内建真实可用）；
//! - 已知刻意宽松者（[`KNOWN_UNTYPABLE`]）—— 逐条验证「探针仍通过」，
//!   防白名单养僵尸。
//!
//! 这样判据与「签名放在哪张表」无关，不会因内部重排而误报。
//!
//! ## 已知刻意宽松（[`KNOWN_UNTYPABLE`]）—— 每条都有理由，非漏登记
//!
//! | 内建 | 为何无法声明 |
//! |---|---|
//! | `deref` | `Type::Atom` 是**单元变体、无载荷**，类型域里没有位置放「这个原子装什么」 |
//! | `quote` / `read` | 运行期返 `Value::Code`，而 `Type` **没有 `Code` 变体**（扩枚举是 v1.0 决策）⇒ 只能 `Any` |
//! | `eval` | 结果是被求值表达式的类型，需要真正的递归推断 |
//! | `conde` | 是 `both` 的别名（`both` 已在权威表），自身无需单列 |
//!
//! 若将来 `Type` 增补 `Code` 变体 / `Atom` 载荷等，这几条逐条从表移除 ——
//! 本测试会让「漏移除」变红。
//!
//! **结论订正（v0.104.6）**：早先一轮曾据「只看权威表」误报「24 条静默
//! `Any`」，并据此判断有一批内建零类型检查。**该结论不成立** —— 那批里
//! 绝大多数本就有签名（经本地 match），且误用均被 typeck 拦下（实测
//! `let s: string = atom(1)` 报错）。此处已改为行为探针，不再依赖可能算错的
//! 表枚举。

use mora::interpreter::RUNTIME_BUILTIN_NAMES;
use std::collections::BTreeSet;

/// 运行期有实现、但**在类型域里如实无法声明**的内建。
///
/// 这些不是「漏补签名」—— 给它们编一个窄签名会**拒绝合法程序**。故显式
/// 登记为刻意宽松，保持可见可回归。见文件头的逐条理由表。
const KNOWN_UNTYPABLE: &[(&str, &str)] = &[
    ("conde", "both 的别名，自身无需单列签名"),
    ("deref", "Type::Atom 是单元变体、无载荷，放不下元素类型"),
    ("eval", "返回值是被求值表达式的类型，需递归推断"),
    ("quote", "返回 Value::Code，Type 无 Code 变体"),
    ("read", "返回 Value::Code，Type 无 Code 变体"),
];

/// 每个运行期自由函数对应的「故意用错」探针源码（仅对**具体返回类型**者
/// 有效，见 [`CONCRETE_RETURN`]）。
///
/// 探针必须能被 parser 接受、且若该名返回**具体类型**则会被 typeck 拒绝
///（赋给一个明显不匹配的标量）。覆盖 `RUNTIME_BUILTIN_NAMES` 的全集；
/// 缺席即测试漏项（见 [`every_runtime_builtin_has_a_probe`]）。
fn probe_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "type_of" => "let v: bool = type_of(1)\n",
        "atom" => "let v: string = atom(1)\n",
        "methods_of" => "let v: bool = methods_of(1)\n",
        "gensym" => "let v: bool = gensym()\n",
        "is_instance" => "let v: string = is_instance(1, \"Int\")\n",
        "bool" => "let v: string = bool(1)\n",
        "int" => "let v: string = int(1)\n",
        "float" => "let v: string = float(1)\n",
        "str" => "let v: bool = str(1)\n",
        "len" => "let v: string = len([1])\n",
        "range" => "let v: string = range(0, 2)\n",
        "print" => "let v: string = print(1)\n",
        "batch_chat" => "let v: bool = batch_chat([\"a\"])\n",
        // 返回类型随回调/元素变化 ⇒ 无法用固定探针判「被拒」，见 LOOSE。
        "car" | "cdr" | "cons" | "uncurry" | "apply" | "into" | "swap" | "compose" | "partial"
        | "both" | "either" | "fail" | "succeed" | "project" | "unify" | "merge_with"
        | "compose_prompt" | "compress" | "crush_json" | "tail" | "macroexpand" | "curry" => {
            "let v = NAME_PLACEHOLDER\n"
        }
        // 已知刻意宽松 —— 探针必然「通过」，登记在 KNOWN_UNTYPABLE。
        "deref" | "quote" | "read" | "eval" | "conde" => "let v: bool = NAME_PLACEHOLDER\n",
        _ => return None,
    })
}

/// 这些内建返回**具体类型** ⇒ 探针（赋给不匹配标量）**必须被 typeck 拒绝**。
/// 它们若哪天不再拒绝，说明签名被移除或放宽了。
const CONCRETE_RETURN: &[&str] = &[
    "type_of",
    "atom",
    "methods_of",
    "gensym",
    "is_instance",
    "bool",
    "int",
    "float",
    "str",
    "len",
    "range",
    "print",
    "batch_chat",
];

/// 把探针里的占位符换成真实调用。
fn render(name: &str, template: &str) -> String {
    template
        .replace("NAME_PLACEHOLDER", &call_expr(name))
        .replace("NAME", &call_expr(name))
}

fn call_expr(name: &str) -> String {
    match name {
        "deref" => "deref(atom(1))".to_string(),
        "quote" => "quote(\"x\")".to_string(),
        "read" => "read(\"x\")".to_string(),
        "eval" => "eval(\"x\")".to_string(),
        "conde" => "conde(fn(x) x)".to_string(),
        "car" | "cdr" => format!("{name}([1, 2, 3])"),
        "cons" => "cons(1, [2])".to_string(),
        "uncurry" => "uncurry(fn(x) x)".to_string(),
        "apply" => "apply(fn(x) x, [1])".to_string(),
        "into" => "into([1], fn(x) x)".to_string(),
        "swap" => "swap(atom(1), fn(x) x)".to_string(),
        "curry" => "curry(fn(x) x, 1)".to_string(),
        "compose" => "compose(fn(x) x, fn(y) y)".to_string(),
        "partial" => "partial(fn(x) x, 1)".to_string(),
        "both" | "either" => format!("{name}(fn(x) x)"),
        "fail" => "fail()".to_string(),
        "succeed" => "succeed(fn(x) x)".to_string(),
        "project" => "project(fn(x) x, 1, 2)".to_string(),
        "unify" => "unify(1, 2)".to_string(),
        "merge_with" => "merge_with(\"k\", \"s\")".to_string(),
        "compose_prompt" => "compose_prompt(\"a\", \"b\")".to_string(),
        "compress" => "compress(\"x\", \"s\")".to_string(),
        "crush_json" => "crush_json(\"{}\", 1)".to_string(),
        "tail" => "tail(\"p\", 1)".to_string(),
        "macroexpand" => "macroexpand(\"m\")".to_string(),
        other => other.to_string(),
    }
}

/// 跑一次 typeck，返回是否**报**类型错。
fn typecheck_rejects(src: &str) -> bool {
    let (_, witnesses) = match mora::parser_v3::ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => panic!("探针源码应能解析（{src:?}）：{e}"),
    };
    !mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses).is_empty()
}

/// **主判据**：每个**具体返回类型**的运行期内建都必须**有签名** ——
/// 用故意用错的探针证明「typeck 会拒绝」。签名被移除/放宽 ⇒ 红。
#[test]
fn every_concrete_return_builtin_is_actually_typechecked() {
    let mut not_rejected: Vec<String> = Vec::new();
    for name in CONCRETE_RETURN {
        let Some(t) = probe_source(name) else {
            not_rejected.push(format!("{name}: 缺探针"));
            continue;
        };
        let src = render(name, t);
        if !typecheck_rejects(&src) {
            not_rejected.push(name.to_string());
        }
    }
    assert!(
        not_rejected.is_empty(),
        "这些**具体返回类型**的内建未被 typeck 拦下（⇒ 签名丢失或被放宽，\
         静默失去类型保障）：{not_rejected:?}\n\
         修法：恢复/补上其真实签名。"
    );
}

/// **宽松组**（返回随回调/元素变化者）至少必须**能被 parser 接受** ——
/// 保证它们确实是可用内建，而非拼写错误的探针蒙混过关。
#[test]
fn loose_builtins_at_least_parse() {
    let untypable: BTreeSet<&str> = KNOWN_UNTYPABLE.iter().map(|(n, _)| *n).collect();
    let concrete: BTreeSet<&str> = CONCRETE_RETURN.iter().copied().collect();
    for name in RUNTIME_BUILTIN_NAMES {
        if untypable.contains(name) || concrete.contains(name) {
            continue;
        }
        let Some(t) = probe_source(name) else {
            continue;
        };
        let src = render(name, t);
        // 解析必须成功（证明内建名真实可用、探针非笔误）。
        let res = mora::parser_v3::ParserV3::compile(&src);
        assert!(
            res.is_ok(),
            "宽松内建 `{name}` 的探针无法解析（探针有笔误或内建名拼错）：\
             源={src:?} err={:?}",
            res.err()
        );
    }
}

/// [`KNOWN_UNTYPABLE`] 里每条都必须**真的**无法声明（探针通过），
/// 且**真的**在运行期清单里 —— 防白名单养僵尸条目。
#[test]
fn known_untypable_entries_are_accurate() {
    let rt: BTreeSet<&str> = RUNTIME_BUILTIN_NAMES.iter().copied().collect();
    for (name, reason) in KNOWN_UNTYPABLE {
        assert!(
            rt.contains(name),
            "KNOWN_UNTYPABLE 的 `{name}` 不在运行期清单里（应移除）"
        );
        assert!(!reason.is_empty(), "`{name}` 缺少「为何无法声明」的理由");
        let t = probe_source(name).expect("已知宽松项也应备有探针");
        assert!(
            !typecheck_rejects(&render(name, t)),
            "`{name}` 已被登记为 KNOWN_UNTYPABLE，但探针现在**被 typeck 拒绝** \
             —— 说明它已获得签名，请从 KNOWN_UNTYPABLE 移除并补文档。"
        );
    }
}

/// 覆盖面自检：运行期清单**每一条**都必须备有探针，防止新增内建绕过检查。
#[test]
fn every_runtime_builtin_has_a_probe() {
    let missing: Vec<&&str> = RUNTIME_BUILTIN_NAMES
        .iter()
        .filter(|n| probe_source(n).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "这些运行期内建没有探针，其类型保障状态**未被本文件检查**：{missing:?}\n\
         修法：为每条补一个「故意用错」的探针源码。"
    );
}

/// 运行期清单自身不得有重复（`match` 多模式抄错时能抓到）。
#[test]
fn runtime_builtin_names_have_no_duplicates() {
    let mut seen = BTreeSet::new();
    let mut dup = Vec::new();
    for n in RUNTIME_BUILTIN_NAMES {
        if !seen.insert(*n) {
            dup.push(*n);
        }
    }
    assert!(dup.is_empty(), "RUNTIME_BUILTIN_NAMES 有重复：{dup:?}");
}
