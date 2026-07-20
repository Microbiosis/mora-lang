//! Tier 0 builtin dispatch 静态合约测试
//!
//! 合约：dispatch.rs 的 `call_function` builtin 分支（`match name {`）不得
//! 调用 `self.evaluate` / `self.execute` / `self.call_value_inner` /
//! `self.call_task_inner`。MIR 桥 (`mir_call_function`) 把已求值好的 `Vec<Value>`
//! 透传给 dispatch；builtin 必须纯值处理。
//!
//! 失败即意味着某个 builtin 偷偷掉回 Tier 0 AST 解释路径。

use std::fs;

fn dispatch_source() -> String {
    fs::read_to_string("src/interpreter/dispatch.rs")
        .expect("dispatch.rs must be readable from tests/")
}

/// 提取 `match name { ... "builtin" => { ... } ... }` 中某个 builtin 名称对应
/// 的分支文本。简化版：用 `// builtin-marker:` 注释行显式标记 builtin 边界。
/// 但避免对源码做 AST 解析——按行扫描足够（builtin 块不会跨很远）。
fn assert_builtin_block_has_no_tier0_calls(block_lines: &[&str], builtin_name: &str) {
    let forbidden = [
        "self.evaluate(",
        "self.evaluate_call(",
        "self.execute(&kind",
        "self.call_value_inner(",
        "self.call_task_inner(",
    ];
    let joined = block_lines.join("\n");
    for f in forbidden {
        assert!(
            !joined.contains(f),
            "builtin `{}` must not invoke Tier 0 AST path `{}`\n--- block ---\n{}",
            builtin_name,
            f,
            joined
        );
    }
}

/// 静态合约：dispatch.rs 顶层 `pub(super) fn call_function(...)` 的 match 块内
/// 任意 builtin 分支都禁止调用 AST evaluate / execute / call_value_inner / call_task_inner。
#[test]
fn dispatch_call_function_builtins_do_not_invoke_tier0() {
    let src = dispatch_source();

    // 提取 `pub(super) fn call_function(` 起点到下一个 `pub(super) fn` 之间的内容
    let start = src
        .find("pub(super) fn call_function(")
        .expect("dispatch.rs must contain call_function");
    let rest = &src[start..];
    let end = rest.find("\n    pub(super) fn ").unwrap_or(rest.len());
    let block = &rest[..end];

    // 收集常见 builtin 名称（与 dispatch.rs 中的 `match name { ... }` 分支对齐）
    // 不依赖枚举：直接扫描整个 call_function 块是否包含 forbidden token。
    let forbidden = [
        "self.evaluate(",
        "self.execute(&kind",
        "self.call_value_inner(",
        "self.call_task_inner(",
    ];
    for f in forbidden {
        assert!(
            !block.contains(f),
            "dispatch::call_function builtin block must not call `{}` (Tier 0 AST path)\n\
             block excerpt:\n{}",
            f,
            &block[..block.len().min(2000)]
        );
    }
}

/// 静态合约：dispatch.rs 顶层 `pub(super) fn call_method(...)` 的 builtin 分支
/// （List/Dict/String 等 receiver 的 method 分支）同样不得调用 AST evaluate。
/// 允许 `self.call_value(&user_fn, ...)` 用于用户传值回调（partial/compose），
/// 但不允许 `self.evaluate(` 这种 Tier 0 表达式求值。
#[test]
fn dispatch_call_method_builtins_do_not_invoke_tier0_evaluate() {
    let src = dispatch_source();

    let start = src
        .find("pub(super) fn call_method(")
        .expect("dispatch.rs must contain call_method");
    let rest = &src[start..];
    // 终止于下一个 `pub(super) fn` 或 `pub(crate) fn`
    let end = rest
        .find("\n    pub(super) fn ")
        .or_else(|| rest.find("\n    pub(crate) fn "))
        .unwrap_or(rest.len());
    let block = &rest[..end];

    let forbidden_evaluate = [
        "self.evaluate(",
        "self.evaluate_call(",
        "self.execute(&kind",
    ];
    for f in forbidden_evaluate {
        assert!(
            !block.contains(f),
            "dispatch::call_method must not call Tier 0 evaluate `{}`\n\
             block excerpt:\n{}",
            f,
            &block[..block.len().min(2000)]
        );
    }
}

/// 守门：上述合约保护的是 Tier 0 → Tier 1 builtin 切换；
/// 若 dispatch.rs 改了入口签名或删除了 call_function/call_method，本测试失败。
#[test]
fn dispatch_entry_signatures_remain_stable() {
    let src = dispatch_source();
    assert!(
        src.contains("pub(super) fn call_function("),
        "dispatch::call_function signature changed — MIR bridge mir_call_function will break"
    );
    assert!(
        src.contains("pub(super) fn call_method("),
        "dispatch::call_method signature changed — MIR bridge mir_call_method will break"
    );
    // MIR 桥固定签名
    assert!(
        src.contains("name: &str,") && src.contains("args: Vec<Value>,"),
        "dispatch::call_function args shape changed"
    );
}

/// 守门：MIR 桥 (interpreter/mod.rs) 必须直接 forward 到 dispatch，
/// 不在中间插入 AST 求值逻辑。
#[test]
fn mir_bridge_does_not_introduce_ast_eval() {
    let bridge_src =
        fs::read_to_string("src/interpreter/mod.rs").expect("interpreter/mod.rs must be readable");
    // 取 mir_call_function 体（到下一个 `pub(crate)` / `///` 注释或 fn 头）
    let start = bridge_src
        .find("pub(crate) fn mir_call_function(")
        .expect("mir_call_function must exist");
    let rest = &bridge_src[start..];
    let end = rest
        .find("\n    pub(crate) fn ")
        .or_else(|| rest.find("\n    /// "))
        .unwrap_or(rest.len());
    let block = &rest[..end];
    let forbidden = ["self.evaluate(", "self.execute(", "self.call_value("];
    for f in forbidden {
        assert!(
            !block.contains(f),
            "MIR bridge mir_call_function must not invoke Tier 0 `{}`\nblock:\n{}",
            f,
            block
        );
    }
    // 必须 forward 到 call_function
    assert!(
        block.contains("self.call_function("),
        "MIR bridge must forward to dispatch::call_function"
    );
}

// 为编译期防止误删
#[allow(dead_code)]
fn _keep_block_lines_helper_signature(block_lines: &[&str]) {
    assert_builtin_block_has_no_tier0_calls(block_lines, "");
}
