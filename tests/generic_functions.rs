//! v0.104.6：泛型函数声明 `task f<T>(x: T): T` / `fn<T>(x: T)`。
//!
//! 此前 `task first<T>(x)` 直接 Parse error（`Expected '(' after task name`）。
//! 本刀让显式泛型函数可用：类型参数在**声明处**引入、在**调用点**按 fresh 变量
//! 单态化（复用既有 `ForAll`/`generalize`/`instantiate` 通路）。
//!
//! 本文件锁住五条性质：
//! 1. 泛型声明可解析；同一泛型函数能被**多种类型**调用（各次独立单态化）；
//! 2. 体内一致性：同一类型参数约束多个形参时，混型调用被拒；
//! 3. 多个类型参数彼此独立；
//! 4. 泛型容器（`list<T>`）可用；
//! 5. 作用域严格：泛型名不泄漏；首字母重复报错；裸参零回归。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 编译 + typecheck，返回类型错误数（解析失败会 panic，便于定位）。
fn type_errors(src: &str) -> usize {
    let (_, witnesses) = mora::parser_v3::ParserV3::compile(src)
        .unwrap_or_else(|e| panic!("应能解析（{src:?}）: {e}"));
    check_program_witnesses_bidirectional(&witnesses).len()
}

/// 泛型函数可声明，且**按调用点各自单形化**：不同类型实参都通过。
#[test]
fn generic_id_accepts_multiple_types() {
    assert_eq!(
        type_errors("task id<T>(x: T): T\n  return x\nend\nprint(id(1))\nprint(id(\"s\"))\n"),
        0,
        "`id<T>` 应能被两种类型分别实例化（这是泛型存在的意义）"
    );
}

/// 体内一致性：`<T>` 同时约束两个形参时，混型实参被拒。
#[test]
fn same_type_param_enforces_consistency() {
    let errs = type_errors("task same<T>(x: T, y: T)\n  return x\nend\nprint(same(1, \"s\"))\n");
    assert!(
        errs > 0,
        "`same<T>(x: T, y: T)` 传 `(1, \"s\")` 必须被拒 —— 同一 T 须一致"
    );
}

/// 多个类型参数彼此独立：`<T, U>` 允许混合类型实参。
#[test]
fn multiple_type_params_are_independent() {
    assert_eq!(
        type_errors(
            "task pair<T, U>(a: T, b: U)\n  return a\nend\n\
             print(pair(1, \"s\"))\nprint(pair(1, 2))\n"
        ),
        0,
        "`<T, U>` 的 T 与 U 应彼此独立"
    );
}

/// 泛型容器：`head<T>(xs: list<T>): T`。
#[test]
fn generic_container_works() {
    assert_eq!(
        type_errors("task head<T>(xs: list<T>): T\n  return xs[0]\nend\nprint(head([1, 2]))\n"),
        0
    );
}

/// 泛型名**不泄漏**到函数外：`T` 只在该泛型声明内是类型变量。
#[test]
fn generic_param_does_not_leak() {
    // 函数体外的 `T` 仍应报「未知类型注解」⇒ 解析失败。
    assert!(
        mora::parser_v3::ParserV3::compile(
            "task f<T>(x: T)\n  return x\nend\nlet v: T = 1\nprint(v)\n"
        )
        .is_err(),
        "泛型作用域必须严格配对：`T` 不得泄漏到函数外被当作类型变量"
    );
}

/// 类型参数首字母重复时报错（`Type::TypeVar` 是单 char，重复会静默合并）。
#[test]
fn duplicate_type_param_first_char_errors() {
    assert!(
        mora::parser_v3::ParserV3::compile("task bad<TT, Tx>(a: TT)\n  return a\nend\nprint(1)\n")
            .is_err(),
        "两个类型参数首字母相同必须报错，不得静默合并成同一个类型变量"
    );
}

/// **零回归**：非泛型的 `task f(a: T→number)` 标注仍照旧工作，裸参不受影响。
#[test]
fn non_generic_annotations_unaffected() {
    // 普通标注：错实参仍被拒（上一刀能力不回归）。
    let errs = type_errors("task f(a: number)\n  return a\nend\nprint(f(\"x\"))\n");
    assert!(errs > 0, "非泛型标注仍须在调用点检查实参");
    // 裸参：不同类型都通过。
    assert_eq!(
        type_errors("task g(a)\n  return a\nend\nprint(g(1))\nprint(g(\"s\"))\n"),
        0,
        "裸形参行为不变"
    );
}

/// 泛型**闭包** `fn<T>(x: T)`：可声明、可多态、且体内一致（同一 T）。
#[test]
fn generic_closure_works() {
    // 多态：两种类型分别实例化都通过。
    assert_eq!(
        type_errors("let id = fn<T>(x: T) x end\nprint(id(1))\nprint(id(\"s\"))\n"),
        0
    );
    // 同一 T 约束两个形参时，混型被拒。
    let errs = type_errors("let same = fn<T>(x: T, y: T) x end\nprint(same(1, \"s\"))\n");
    assert!(errs > 0, "`fn<T>(x: T, y: T)` 传混型实参必须被拒");
}
