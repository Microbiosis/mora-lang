//! Interactive REPL runner backed directly by `CoreRuntime` and the Control SSA VM.

use crate::runtime::CoreRuntime;
use crate::value::Value;

/// Decide whether accumulated REPL input forms a complete block/statement before parsing.
///
/// Tracks two balances simultaneously so incomplete blocks continue reading lines instead of
/// failing early on the opening header line:
/// 1. Line-leading block keywords balanced by `end` (`task`, `for`, `while`, `if`, `match`,
///    `worker`, `transaction`, `macro`, `parallel`).
/// 2. Brace balance (`{` / `}`) for inline handler syntax such as `handle E { body } { handler }`.
pub(crate) fn repl_input_is_complete(src: &str) -> bool {
    const OPENERS: &[&str] = &[
        "task",
        "for",
        "while",
        "if",
        "match",
        "select",
        "worker",
        "transaction",
        "macro",
        "parallel",
    ];
    let mut end_depth: i64 = 0;
    let mut brace_depth: i64 = 0;
    for raw in src.lines() {
        let line = strip_repl_literals(raw);
        let line = match line.find("--") {
            Some(i) => line[..i].to_string(),
            None => line,
        };
        let head = line.trim_start();
        let head = head.split_whitespace().next().unwrap_or("");
        let head: String = head
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if head == "end" {
            end_depth -= 1;
        } else if head == "else" {
            // `else` / `else if` keep the enclosing block depth unchanged.
        } else if OPENERS.contains(&head.as_str())
            || (head == "spawn" && line.split_whitespace().last() == Some("do"))
        {
            end_depth += 1;
        }
        for c in line.chars() {
            match c {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                _ => {}
            }
        }
    }
    end_depth <= 0 && brace_depth <= 0
}

fn strip_repl_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quote = c;
                out.push(c);
                while let Some(&n) = chars.peek() {
                    chars.next();
                    out.push(n);
                    if n == '\\' {
                        if let Some(&e) = chars.peek() {
                            chars.next();
                            out.push(e);
                        }
                        continue;
                    }
                    if n == quote {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Start an interactive REPL session with a fresh [`CoreRuntime`].
pub fn run_repl() {
    let mut core = CoreRuntime::default();
    run_repl_with(&mut core);
}

/// Run the interactive REPL loop against an existing [`CoreRuntime`].
#[allow(clippy::too_many_lines)]
pub fn run_repl_with(core: &mut CoreRuntime) {
    use std::io::{self, BufRead, Write};

    println!("Mora v{} REPL — type 'exit' to quit", crate::VERSION);
    println!();

    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();
    let mut accepted_source = String::new();
    let mut stripped_first_bom = false;
    let mut pending = String::new();

    loop {
        if pending.is_empty() {
            print!("mora> ");
        } else {
            print!("  ...> ");
        }
        let _ = io::stdout().flush();
        line.clear();
        match handle.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if !stripped_first_bom {
            stripped_first_bom = true;
            line = crate::cli::strip_bom(&line).to_string();
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !pending.is_empty() {
                eprintln!("parse error: 放弃未完成的输入");
                pending.clear();
            }
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            println!("Bye!");
            break;
        }

        pending.push_str(line.as_str());
        if !repl_input_is_complete(&pending) {
            continue;
        }
        let func_witnesses = match crate::parser_v3::ParserV3::parse_witnesses(&pending) {
            Ok(ws) => ws,
            Err(_) => {
                eprintln!("parse error: 语法错误，已放弃当前输入");
                pending.clear();
                continue;
            }
        };

        let mut candidate = accepted_source.clone();
        candidate.push_str(&pending);
        let type_errs = match crate::parser_v3::ParserV3::parse_witnesses(&candidate) {
            Ok(ws) => crate::typeck::check_mir::check_program_witnesses(&ws),
            Err(e) => {
                eprintln!("parse error: {}", e);
                pending.clear();
                continue;
            }
        };
        if !type_errs.is_empty() {
            for e in &type_errs {
                eprintln!("type error: {}", e.message);
            }
            pending.clear();
            continue;
        }
        accepted_source.push_str(&pending);
        pending.clear();

        match crate::runtime::vm::execute_repl_chunk(
            &func_witnesses,
            crate::mir::control::OptLevel::default(),
            core,
        ) {
            Ok(value) => {
                if !matches!(value, Value::Nil) {
                    println!("= {}", value);
                }
            }
            Err(e) => eprintln!("Error: {}", e),
        }
    }
}
