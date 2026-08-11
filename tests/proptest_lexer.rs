//! v0.77: proptest — Lexer 对任意输入不 panic（v0.30 承诺"任何输入都能 lex 完"）。
//!
//! 加强 lexer 的 totality：scan_tokens() 对任意字符串都不 panic。
//! 真实 input fuzzing：UTF-8 边界、控制字符、零字节、Emoji 等。

use mora::lexer::Lexer;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    #[test]
    fn lexer_never_panics(src in "\\PC{0,200}") {
        // 注：\\PC = any unicode scalar value（除代理半区）
        let tokens = Lexer::new(&src).scan_tokens();
        // 必须以 EOF 结尾（Lexer 内部约定）
        prop_assert!(
            matches!(tokens.last(), Some(t) if matches!(t.token_type, mora::lexer::TokenType::EOF)),
            "tokens must end with EOF, got last={:?}",
            tokens.last().map(|t| &t.token_type)
        );
        // 至少有一个 token（EOF）
        prop_assert!(!tokens.is_empty(), "scan_tokens() returned empty vec");
    }

    /// 空字符串产生单一 EOF。
    #[test]
    fn lexer_empty_input_yields_only_eof(_unit in 0..1u8) {
        let tokens = Lexer::new("").scan_tokens();
        prop_assert_eq!(tokens.len(), 1);
        prop_assert!(matches!(tokens[0].token_type, mora::lexer::TokenType::EOF));
    }

    /// 仅 ASCII 控制字符不应 panic。
    #[test]
    fn lexer_control_chars_dont_panic(s in "[\\x00-\\x1F\\x7F]{0,50}") {
        let _ = Lexer::new(&s).scan_tokens();
    }
}