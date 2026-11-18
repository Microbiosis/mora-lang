// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

/// Result of type-checking and running one Mora chunk.
pub struct Outcome {
  /// Lines produced by `print`, without a trailing newline on the last line.
  lines : Array[String]
  /// Display of the chunk's value. `nil` when the chunk yields nil.
  shown : String
  is_nil : Bool
}

/// Join printed lines with newlines, the way a script's stdout looks
/// before the final newline a terminal adds.
pub fn Outcome::text(self : Outcome) -> String {
  let buf = StringBuilder()
  for i in 0..<self.lines.length() {
    if i > 0 {
      buf.write_string("\n")
    }
    buf.write_string(self.lines[i])
  }
  buf.to_string()
}

/// One Mora session: a lexical environment plus its Hindley-Milner scheme.
///
/// File runs use a fresh engine. The REPL keeps one engine so later lines
/// see earlier bindings.
pub struct Engine {
  priv mut frames : Array[Map[String, Value]]
  priv ty_frames : Array[Map[String, Scheme]]
  priv prints : Array[String]
  priv mut next_var : Int
  priv subst : Array[MType?]
  priv numeric : Map[Int, Bool]
}

fn builtin_values() -> Map[String, Value] {
  let env : Map[String, Value] = Map([])
  env.set("print", VBuiltin("print"))
  env.set("str", VBuiltin("str"))
  env.set("len", VBuiltin("len"))
  env.set("range", VBuiltin("range"))
  env
}

fn builtin_schemes() -> Map[String, Scheme] {
  let env : Map[String, Scheme] = Map([])
  env.set("print", mono(TBuiltin("print")))
  env.set("str", mono(TBuiltin("str")))
  env.set("len", mono(TBuiltin("len")))
  env.set("range", mono(TBuiltin("range")))
  env
}

pub fn Engine::new() -> Engine {
  {
    frames: [builtin_values()],
    ty_frames: [builtin_schemes()],
    prints: [],
    next_var: 0,
    subst: [],
    numeric: {},
  }
}

fn Engine::take_lines(self : Engine, start : Int) -> Array[String] {
  let out : Array[String] = []
  for i in start..<self.prints.length() {
    out.push(self.prints[i])
  }
  out
}

/// Type-check `src` and, on success, evaluate it.
/// Bindings stay in this engine.
pub fn Engine::run(self : Engine, src : String) -> Outcome raise MoraError {
  let prog = parse(src)
  self.infer_program(prog)
  let start = self.prints.length()
  let value = self.eval_program(prog)
  {
    lines: self.take_lines(start),
    shown: show_value(value),
    is_nil: match value {
      VNil => true
      _ => false
    },
  }
}

/// Type-check `src` without evaluating it.
/// Successful bindings stay in this engine, matching a later `run` of the
/// same source on a fresh engine.
pub fn Engine::check(self : Engine, src : String) -> Unit raise MoraError {
  let prog = parse(src)
  self.infer_program(prog)
}

/// Type-check and run `src` on a fresh engine.
pub fn run_source(src : String) -> Outcome raise MoraError {
  Engine::new().run(src)
}

/// Type-check `src` on a fresh engine.
pub fn check_source(src : String) -> Unit raise MoraError {
  Engine::new().check(src)
}

/// Trial identity. The Rust compiler remains `0.110.0`.
pub const VERSION : String = "0.110.0-moonbit"
