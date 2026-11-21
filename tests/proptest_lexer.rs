//! v0.77 / v0.110.0: Zero-dependency deterministic property tests — Lexer totality on arbitrary input.
//!
//! Verifies that `Lexer::new(&src).scan_tokens()` never panics and always terminates with `TokenType::EOF`
//! across arbitrary Unicode scalar strings, empty input, and ASCII control byte sequences.

use mora::lexer::{Lexer, TokenType};

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn gen_range(&mut self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            (self.next_u64() as usize) % len
        }
    }
}

#[test]
fn lexer_never_panics() {
    let mut rng = SplitMix64(0x2026_1120_d100_0001);
    let unicode_pool: &[char] = &[
        'a', 'Z', '0', '9', '_', '+', '-', '*', '/', '%', '=', '!', '<', '>', '|', ':', '.', ',',
        '(', ')', '[', ']', '{', '}', '"', '\'', '`', '@', '\n', '\r', '\t', ' ', '\0', '中', '文',
        'λ', 'τ', '🚀', 'é', 'ß',
    ];
    for _case in 0..128 {
        let len = rng.gen_range(201);
        let mut src = String::new();
        for _ in 0..len {
            if rng.gen_range(4) == 0 {
                let code = (rng.next_u64() as u32) % 0x110000;
                if let Some(ch) = char::from_u32(code) {
                    src.push(ch);
                }
            } else {
                src.push(unicode_pool[rng.gen_range(unicode_pool.len())]);
            }
        }
        let tokens = Lexer::new(&src).scan_tokens();
        assert!(
            matches!(tokens.last(), Some(t) if matches!(t.token_type, TokenType::EOF)),
            "tokens must end with EOF, got last={:?}",
            tokens.last().map(|t| &t.token_type)
        );
        assert!(!tokens.is_empty(), "scan_tokens() returned empty vec");
    }
}

#[test]
fn lexer_empty_input_yields_only_eof() {
    let tokens = Lexer::new("").scan_tokens();
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0].token_type, TokenType::EOF));
}

#[test]
fn lexer_control_chars_dont_panic() {
    let mut rng = SplitMix64(0x2026_1120_d100_0002);
    for _case in 0..64 {
        let len = rng.gen_range(51);
        let mut s = String::with_capacity(len);
        for _ in 0..len {
            let b = match rng.gen_range(33) {
                32 => 0x7Fu8,
                v => v as u8,
            };
            s.push(b as char);
        }
        let tokens = Lexer::new(&s).scan_tokens();
        assert!(matches!(
            tokens.last().map(|t| &t.token_type),
            Some(TokenType::EOF)
        ));
    }
}
