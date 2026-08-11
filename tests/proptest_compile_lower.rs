//! v0.77: proptest — ParserV3::compile 与 parse_code_v3→lower_mir_exprs 双路径等价。
//!
//! 推广 tests/compile_differential.rs 的 22 个手写 case 到任意随机输入。
//!
//! 不变量：对于任意 Mora 源码 src：
//!   ParserV3::compile(src).0.body == lower_mir_exprs(parse_code_v3(src)).body
//!   ParserV3::compile(src).0.n_regs == lower_mir_exprs(parse_code_v3(src)).n_regs

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,  // CI 友好：默认 256 减半
        .. ProptestConfig::default()
    })]

    /// 关键 invariant：compile vs parse+lower 双路径产物等价。
    /// 这条规则防止 parser→MirInst 单遍编译（v0.75.40）和
    /// parse→MirExpr→lower→MirInst 双阶段路径分叉。
    #[test]
    fn compile_equals_lower_mir_exprs(src in proptest::string::string_regex(".*").unwrap()) {
        // 任意字符串都可能 parse 失败或 typecheck 失败 — 我们只比较两条路径的产物，
        // 当两条路径都成功时产物必须相同。
        let compile_func: Option<mora::mir::MirFunction> = mora::parser_v3::ParserV3::compile(&src)
            .ok()
            .map(|(f, _)| f);
        let parse_func: Option<mora::mir::MirFunction> = mora::interpreter::parse_code_v3(&src)
            .ok()
            .and_then(|exprs| mora::mir::lower::lower_mir_exprs(&exprs).ok());

        match (compile_func, parse_func) {
            (Some(c), Some(p)) => {
                prop_assert_eq!(c.n_regs, p.n_regs, "n_regs mismatch: compile={}, lower={}", c.n_regs, p.n_regs);
                prop_assert_eq!(c.body.len(), p.body.len(), "body length mismatch: compile={}, lower={}", c.body.len(), p.body.len());
                for (i, (a, b)) in c.body.iter().zip(p.body.iter()).enumerate() {
                    prop_assert_eq!(a, b, "body[{}] mismatch: compile={:?}, lower={:?}", i, a, b);
                }
            }
            // 一条路径失败另一条路径成功 → 反例（这是真正想要捕获的 regression）
            (Some(_), None) => prop_assert!(false, "compile succeeded but lower failed for: {:?}", src),
            (None, Some(_)) => prop_assert!(false, "lower succeeded but compile failed for: {:?}", src),
            // 两条路径都失败（无效输入）— OK
            (None, None) => {}
        }
    }
}