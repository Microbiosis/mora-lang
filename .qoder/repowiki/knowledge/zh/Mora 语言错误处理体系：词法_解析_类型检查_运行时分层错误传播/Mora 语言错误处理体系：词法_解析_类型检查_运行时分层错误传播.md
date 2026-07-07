---
kind: error_handling
name: Mora ///
category: error_handling
scope:
    - '**'
source_files:
    - src/lexer.rs
    - src/parser_v2/mod.rs
    - src/typeck/mod.rs
    - src/mir/interp.rs
    - src/main.rs
---

## 1. 

Mora →→/MIR

-  panic TokenType::Error(String) token  parser 
- ParserV2  consume / consume_identifier  eprintln! /
-  TypeError /
- /MIR  Result<Value, String> CLI  process::exit(1)
- LSP  LSP Diagnostic 

## 2. 

- src/lexer.rs:  → TokenType::Error(msg) tokenerror_token() 
- src/parser_v2/mod.rs:  → eprintln!("Parse error: ...") + 
- src/typeck/mod.rs:  → TypeError { line, column, message, expected, actual, hint }format_error() 
- src/mir/interp.rs: MIR  → Result<Value, String> 
- src/main.rs: CLI  →  typeck/MIR/runtime  exit code (1=runtime, 2=typeck)
- src/lsp/mod.rs: LSP  →  JSON-RPC Diagnostic

## 3. 

-  panicv0.31  lexer  emit Error tokenparser 
- consume_* /None
-  TypeError  expected/actual/hint IDE 
- typeck  exit 2 exit 1 CI 
-  thiserror/anyhow  crate

## 4. 

1.  self.error_token(line, col, msg) panic!
2.  eprintln!("Parse error: ... at line {}") 
3.  TypeError::from_span_with_detail(...) //
4.  Result<T, String> ?  main 
5.  unwrap()/expect()