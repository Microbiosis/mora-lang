// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

priv struct Span {
  line : Int
  col : Int
}

fn Span::make(line : Int, col : Int) -> Span {
  { line, col }
}

fn Span::locus(self : Span) -> String {
  "line " + self.line.to_string() + ":" + self.col.to_string()
}
