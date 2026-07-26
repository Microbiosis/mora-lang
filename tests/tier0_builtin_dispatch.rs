//! Tier 0 builtin dispatch
//!
//! dispatch.rs  `call_function` builtin `match name {`
//!  `self.evaluate` / `self.execute` / `self.call_value_inner` /
//! `self.call_task_inner`MIR  (`mir_call_function`)  `Vec<Value>`
//!  dispatchbuiltin
//!
//!  builtin  Tier 0 AST

use std::fs;

fn dispatch_source() -> String {
    fs::read_to_string("src/interpreter/dispatch.rs")
        .expect("dispatch.rs must be readable from tests/")
}

///  `match name { ... "builtin" => { ... } ... }`  builtin
///  `// builtin-marker:`  builtin
///  AST ——builtin
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

/// dispatch.rs  `pub(super) fn call_function(...)`  match
///  builtin  AST evaluate / execute / call_value_inner / call_task_inner
#[test]
fn dispatch_call_function_builtins_do_not_invoke_tier0() {
    let src = dispatch_source();

    //  `pub(super) fn call_function(`  `pub(super) fn`
    let start = src
        .find("pub(super) fn call_function(")
        .expect("dispatch.rs must contain call_function");
    let rest = &src[start..];
    let end = rest.find("\n    pub(super) fn ").unwrap_or(rest.len());
    let block = &rest[..end];

    //  builtin  dispatch.rs  `match name { ... }`
    //  call_function  forbidden token
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

/// dispatch.rs  `pub(super) fn call_method(...)`  builtin
/// List/Dict/String  receiver  method  AST evaluate
///  `self.call_value(&user_fn, ...)` partial/compose
///  `self.evaluate(`  Tier 0
#[test]
fn dispatch_call_method_builtins_do_not_invoke_tier0_evaluate() {
    let src = dispatch_source();

    let start = src
        .find("pub(super) fn call_method(")
        .expect("dispatch.rs must contain call_method");
    let rest = &src[start..];
    //  `pub(super) fn`  `pub(crate) fn`
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

///  Tier 0 → Tier 1 builtin
///  dispatch.rs  call_function/call_method
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
    // MIR
    assert!(
        src.contains("name: &str,") && src.contains("args: Vec<Value>,"),
        "dispatch::call_function args shape changed"
    );
}

/// MIR  (interpreter/mod.rs)  forward  dispatch
///  AST
#[test]
fn mir_bridge_does_not_introduce_ast_eval() {
    let bridge_src =
        fs::read_to_string("src/interpreter/mod.rs").expect("interpreter/mod.rs must be readable");
    //  mir_call_function  `pub(crate)` / `///`  fn
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
    //  forward  call_function
    assert!(
        block.contains("self.call_function("),
        "MIR bridge must forward to dispatch::call_function"
    );
}

//
#[allow(dead_code)]
fn _keep_block_lines_helper_signature(block_lines: &[&str]) {
    assert_builtin_block_has_no_tier0_calls(block_lines, "");
}
