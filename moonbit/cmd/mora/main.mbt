// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

async fn main {
  let args = @env.args()
  let code = try dispatch(args) catch {
    err => {
      eprintln(err.to_string())
      1
    }
  } noraise {
    status => status
  }
  if code != 0 {
    @env.exit(code)
  }
}

async fn dispatch(args : Array[String]) -> Int raise {
  if args.length() >= 2 {
    match args[1] {
      "--version" | "-v" | "version" => {
        eprintln("Mora v" + @lib.VERSION)
        return 0
      }
      "--help" | "-h" | "help" => {
        print_help()
        return 0
      }
      _ => ()
    }
  }
  if args.length() >= 2 && args[1].has_prefix("--opt") {
    eprintln("SSA optimization (--opt) is not in the MoonBit trial. The Rust compiler at the repository root still accepts --opt.")
    return 2
  }
  print_banner()
  if args.length() < 2 {
    repl()
    return 0
  }
  match args[1] {
    "--repl" => {
      repl()
      0
    }
    "--check" => {
      if args.length() < 3 {
        eprintln("Usage: mora --check <file.mora>")
        1
      } else {
        check_file(args[2])
      }
    }
    "install" => {
      eprintln("mora install is not in the MoonBit trial.")
      2
    }
    "run" => {
      if args.length() < 3 {
        eprintln("Usage: mora run <file.mora>")
        1
      } else {
        run_file(args[2])
      }
    }
    other =>
      if other.has_prefix("-") {
        eprintln("unknown option '" + other + "'. See mora --help.")
        2
      } else {
        run_file(other)
      }
  }
}

fn print_banner() -> Unit {
  eprintln("Mora v" + @lib.VERSION)
  eprintln("  MoonBit trial: lexer, Pratt parser, Hindley-Milner, tree-walk evaluator.")
  eprintln("  Not the Rust Control SSA VM. Effects, concurrency, modules, macros, and LSP are not ported.")
  eprintln("")
}

fn print_help() -> Unit {
  println("Mora v" + @lib.VERSION + " — MoonBit trial of the functional core")
  println("")
  println("Usage:")
  println("  mora <file.mora>           Run a script")
  println("  mora run <file.mora>       Run a script (explicit form)")
  println("  mora --check <file>        Type check only")
  println("  mora --repl                Interactive REPL")
  println("  mora --version             Show version")
  println("  mora --help                Show this help")
  println("")
  println("Not in this trial: --opt, install, mora-lsp, effects, concurrency, modules, macros.")
}

async fn read_file(path : String) -> String raise {
  @fs.read_text(path)
}

fn report(err : @lib.MoraError) -> Int {
  eprintln(err.message())
  err.code()
}

async fn run_file(path : String) -> Int raise {
  let src = try read_file(path) catch {
    err => {
      eprintln(path + ": " + err.to_string())
      return 1
    }
  } noraise {
    text => text
  }
  try {
    let outcome = @lib.run_source(src)
    for line in outcome.lines {
      println(line)
    }
    0
  } catch {
    err => report(err)
  } noraise {
    status => status
  }
}

async fn check_file(path : String) -> Int raise {
  let src = try read_file(path) catch {
    err => {
      eprintln(path + ": " + err.to_string())
      return 1
    }
  } noraise {
    text => text
  }
  try {
    @lib.check_source(src)
    eprintln(path + ": ok")
    0
  } catch {
    err => report(err)
  } noraise {
    status => status
  }
}

fn repl_complete(src : String) -> Bool {
  let mut depth = 0
  let mut braces = 0
  let mut seen = false
  for line in src.split("\n") {
    let mut head = ""
    let mut started = false
    for ch in line {
      if ch == '-' {
        break
      }
      if !(started) {
        if ch == ' ' || ch == '\t' {
          continue
        }
        started = true
      }
      if (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') || ch == '_' || (ch >= '0' && ch <= '9') {
        head = head + ch.to_string()
      } else {
        break
      }
    }
    if head.length() > 0 {
      seen = true
    }
    match head {
      "task" | "fn" | "for" | "while" | "if" | "match" => depth = depth + 1
      "end" => depth = depth - 1
      _ => ()
    }
    for ch in line {
      if ch == '{' {
        braces = braces + 1
      } else if ch == '}' {
        braces = braces - 1
      }
    }
  }
  seen && depth <= 0 && braces <= 0
}

async fn repl() -> Unit raise {
  println("Mora v" + @lib.VERSION + " REPL — type 'exit' to quit")
  println("")
  let engine = @lib.Engine::new()
  let mut pending = ""
  while true {
    if pending == "" {
      println("mora>")
    } else {
      println("...>")
    }
    match @stdio.stdin.text.read_until("\n") {
      None => break
      Some(line) => {
        let trimmed = line.trim().to_owned()
        if trimmed == "" && pending == "" {
          continue
        }
        if (trimmed == "exit" || trimmed == "quit") && pending == "" {
          println("Bye!")
          break
        }
        pending = pending + line + "\n"
        if !(repl_complete(pending)) {
          continue
        }
        try {
          let outcome = engine.run(pending)
          for printed in outcome.lines {
            println(printed)
          }
          if !(outcome.is_nil) {
            println("= " + outcome.shown)
          }
        } catch {
          err => eprintln(err.message())
        } noraise {
          _ => ()
        }
        pending = ""
      }
    }
  }
}
