//! v0.77: proptest — ParserV3::compile 随机输入鲁棒性测试。
//!
//! 验证 compile 对任意输入不 panic（成功或返回 Err）。
//! 旧 parse_code_v3→lower_mir_exprs 双路径等价测试已删除
//! （AGENTS.md §6 禁止兼容桥）。

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,  // CI 友好
        .. ProptestConfig::default()
    })]

    /// compile 对任意输入不 panic（成功或返回 Err）。
    #[test]
    fn compile_never_panics(src in proptest::string::string_regex(".*").unwrap()) {
        let _ = mora::parser_v3::ParserV3::compile(&src);
        // 不 panic 即通过
    }

    /// 成功编译的程序 body 不应为空（除非输入为空）。
    #[test]
    fn successful_compile_has_nonempty_body(src in proptest::string::string_regex("[a-z0-9+\\-*/() ]+").unwrap()) {
        if let Ok((func, _)) = mora::parser_v3::ParserV3::compile(&src) {
            prop_assert!(!func.body.is_empty() || src.trim().is_empty(),
                "successful compile should have non-empty body for: {:?}", src);
        }
    }
}
