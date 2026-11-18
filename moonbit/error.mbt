// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

/// Diagnostic raised by the MoonBit Mora trial.
///
/// `ParseErr` and `TypeErr` are compile failures (process exit 2).
/// `RuntimeErr` is an evaluation or I/O failure (process exit 1).
pub suberror MoraError {
  ParseErr(String)
  TypeErr(String)
  RuntimeErr(String)
}

/// The user-facing diagnostic text.
pub fn MoraError::message(self : MoraError) -> String {
  match self {
    ParseErr(s) | TypeErr(s) | RuntimeErr(s) => s
  }
}

/// Process status used by the `mora` CLI.
pub fn MoraError::code(self : MoraError) -> Int {
  match self {
    ParseErr(_) | TypeErr(_) => 2
    RuntimeErr(_) => 1
  }
}
