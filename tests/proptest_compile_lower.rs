//! v0.77 / v0.110.0: Zero-dependency deterministic property tests — `ParserV3::compile` robustness.
//!
//! Verifies that `ParserV3::compile` never panics on arbitrary token/character combinations and
//! that every successful compilation produces a verifiable, non-empty `ControlProgram`.

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
fn compile_never_panics() {
    let mut rng = SplitMix64(0x2026_1120_d200_0001);
    let fragments = [
        "let ",
        "x",
        "y",
        " = ",
        "1",
        "2.5",
        " + ",
        " - ",
        " * ",
        " / ",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        "\n",
        "if ",
        " then ",
        " else ",
        " end",
        "fn(a) ",
        "match ",
        " with ",
        " -> ",
        "::",
        "..",
        ":",
        "true",
        "false",
        "nil",
        "\"s\"",
        "-- comment\n",
    ];
    for _case in 0..64 {
        let count = rng.gen_range(24);
        let mut src = String::new();
        for _ in 0..count {
            src.push_str(fragments[rng.gen_range(fragments.len())]);
        }
        let _ = mora::parser_v3::ParserV3::compile(&src);
    }
}

#[test]
fn successful_compile_has_nonempty_body() {
    let mut rng = SplitMix64(0x2026_1120_d200_0002);
    let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789+-*/() ";
    for _case in 0..64 {
        let len = 1 + rng.gen_range(40);
        let mut src = String::with_capacity(len);
        for _ in 0..len {
            src.push(alphabet[rng.gen_range(alphabet.len())] as char);
        }
        if let Ok((prog, _)) = mora::parser_v3::ParserV3::compile(&src) {
            assert!(mora::mir::control::verify_program(&prog).is_ok());
            let entry = prog.func(prog.entry);
            let has_code =
                entry.blocks.iter().any(|b| !b.insts.is_empty()) || prog.functions.len() > 1;
            assert!(
                has_code || src.trim().is_empty(),
                "successful compile should have non-empty body for: {:?}",
                src
            );
        }
    }
}
