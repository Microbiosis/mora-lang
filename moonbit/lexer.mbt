// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

fn chars_of(src : String) -> Array[Char] {
  let out : Array[Char] = []
  for c in src {
    out.push(c)
  }
  out
}

fn is_digit(c : Char) -> Bool {
  c >= '0' && c <= '9'
}

fn is_alpha(c : Char) -> Bool {
  c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}

fn is_alnum(c : Char) -> Bool {
  is_alpha(c) || is_digit(c)
}

priv struct Lexer {
  chars : Array[Char]
  mut pos : Int
  mut line : Int
  mut col : Int
}

fn Lexer::new(src : String) -> Lexer {
  { chars: chars_of(src), pos: 0, line: 1, col: 1 }
}

fn Lexer::at_end(self : Lexer) -> Bool {
  self.pos >= self.chars.length()
}

fn Lexer::peek(self : Lexer) -> Char {
  if self.at_end() {
    '\u{0000}'
  } else {
    self.chars[self.pos]
  }
}

fn Lexer::peek_next(self : Lexer) -> Char {
  if self.pos + 1 >= self.chars.length() {
    '\u{0000}'
  } else {
    self.chars[self.pos + 1]
  }
}

fn Lexer::advance(self : Lexer) -> Char {
  let c = self.chars[self.pos]
  self.pos = self.pos + 1
  self.col = self.col + 1
  c
}

fn Lexer::match_char(self : Lexer, expected : Char) -> Bool {
  if self.at_end() || self.chars[self.pos] != expected {
    false
  } else {
    ignore(self.advance())
    true
  }
}

fn parse_err(span : Span, msg : String) -> MoraError {
  ParseErr("Parse error at " + span.locus() + ": " + msg)
}

fn Lexer::scan(self : Lexer) -> Array[Tok] raise MoraError {
  if !(self.at_end()) && self.peek().to_int() == 0xFEFF {
    ignore(self.advance())
    self.col = 1
  }
  let toks : Array[Tok] = []
  while !(self.at_end()) {
    match self.next_token() {
      Some(tok) => toks.push(tok)
      None => ()
    }
  }
  toks.push(Eof(Span::make(self.line, self.col)))
  toks
}

fn lex(src : String) -> Array[Tok] raise MoraError {
  Lexer::new(src).scan()
}

fn Lexer::next_token(self : Lexer) -> Tok? raise MoraError {
  while !(self.at_end()) {
    match self.peek() {
      ' ' | '\t' | '\r' => ignore(self.advance())
      _ => break
    }
  }
  if self.at_end() {
    return None
  }
  let line = self.line
  let col = self.col
  let span = Span::make(line, col)
  let c = self.advance()
  match c {
    '+' => Some(Plus(span))
    '-' =>
      if self.match_char('-') {
        while !(self.at_end()) && self.peek() != '\n' {
          ignore(self.advance())
        }
        self.next_token()
      } else if self.match_char('>') {
        Some(Arrow(span))
      } else {
        Some(Minus(span))
      }
    '*' =>
      if self.match_char('*') {
        Some(StarStar(span))
      } else {
        Some(Star(span))
      }
    '/' => Some(Slash(span))
    '%' => Some(Percent(span))
    '(' => Some(LParen(span))
    ')' => Some(RParen(span))
    '[' => Some(LBrack(span))
    ']' => Some(RBrack(span))
    '{' => Some(LBrace(span))
    '}' => Some(RBrace(span))
    '.' =>
      if self.peek() == '.' {
        raise parse_err(span, "list rest patterns ('..' / '...') are not in the MoonBit trial")
      } else {
        Some(Dot(span))
      }
    ',' => Some(Comma(span))
    ':' =>
      if self.match_char(':') {
        Some(ColonColon(span))
      } else {
        Some(Colon(span))
      }
    '|' =>
      if self.match_char('>') {
        Some(Pipe(span))
      } else {
        raise parse_err(span, "union types ('|') are not in the MoonBit trial")
      }
    '>' =>
      if self.match_char('=') {
        Some(Ge(span))
      } else {
        Some(Gt(span))
      }
    '<' =>
      if self.match_char('=') {
        Some(Le(span))
      } else {
        Some(Lt(span))
      }
    '=' =>
      if self.match_char('=') {
        Some(Eq(span))
      } else if self.match_char('>') {
        Some(FatArrow(span))
      } else {
        Some(Assign(span))
      }
    '!' =>
      if self.match_char('=') {
        Some(NotEq(span))
      } else {
        Some(Bang(span))
      }
    '@' => Some(At(span))
    '"' => Some(self.string_lit(span))
    '\'' => Some(self.char_lit(span))
    '\n' => {
      self.line = self.line + 1
      self.col = 1
      Some(Newline(span))
    }
    '`' => raise parse_err(span, "quasiquote is not in the MoonBit trial")
    '?' => raise parse_err(span, "the '?' operator is not in the MoonBit trial")
    '&' => raise parse_err(span, "borrow syntax is not in the MoonBit trial")
    _ =>
      if is_digit(c) {
        Some(self.number_lit(span))
      } else if is_alpha(c) {
        Some(self.ident_lit(span))
      } else {
        raise parse_err(span, "unexpected character '" + c.to_string() + "'")
      }
  }
}

fn Lexer::string_lit(self : Lexer, span : Span) -> Tok raise MoraError {
  let buf = StringBuilder()
  while !(self.at_end()) && self.peek() != '"' {
    let c = self.advance()
    if c == '\n' {
      self.line = self.line + 1
      self.col = 1
    }
    if c == '\\' {
      if self.at_end() {
        break
      }
      match self.advance() {
        '"' => buf.write_char('"')
        '\\' => buf.write_char('\\')
        'n' => buf.write_char('\n')
        't' => buf.write_char('\t')
        'r' => buf.write_char('\r')
        '0' => buf.write_char('\u{0000}')
        other => {
          buf.write_char('\\')
          buf.write_char(other)
        }
      }
    } else {
      let code = c.to_int()
      let legit = c == '\t' || c == '\n' || c == '\r'
      if !(legit) && (code < 0x20 || c == '\u{007F}') {
        raise parse_err(span, "control character in string literal")
      }
      buf.write_char(c)
    }
  }
  if self.at_end() {
    raise parse_err(span, "unterminated string")
  }
  ignore(self.advance())
  Str(buf.to_string(), span)
}

fn Lexer::char_lit(self : Lexer, span : Span) -> Tok raise MoraError {
  if self.at_end() {
    raise parse_err(span, "unterminated char literal")
  }
  let ch = if self.peek() == '\\' {
    ignore(self.advance())
    if self.at_end() {
      raise parse_err(span, "unterminated char escape")
    }
    match self.advance() {
      '\'' => '\''
      '\\' => '\\'
      'n' => '\n'
      't' => '\t'
      'r' => '\r'
      '0' => '\u{0000}'
      other => raise parse_err(span, "unsupported char escape '\\" + other.to_string() + "'")
    }
  } else {
    self.advance()
  }
  if self.at_end() || self.peek() != '\'' {
    raise parse_err(span, "char literal must contain exactly one character")
  }
  ignore(self.advance())
  Char(ch, span)
}

fn Lexer::slice_from(self : Lexer, start : Int) -> String {
  let buf = StringBuilder()
  for i in start..<self.pos {
    buf.write_char(self.chars[i])
  }
  buf.to_string()
}

fn Lexer::number_lit(self : Lexer, span : Span) -> Tok raise MoraError {
  let start = self.pos - 1
  while is_digit(self.peek()) || (self.peek() == '_' && is_digit(self.peek_next())) {
    ignore(self.advance())
  }
  let mut has_dot = false
  if self.peek() == '.' && is_digit(self.peek_next()) {
    has_dot = true
    ignore(self.advance())
    while is_digit(self.peek()) || (self.peek() == '_' && is_digit(self.peek_next())) {
      ignore(self.advance())
    }
  }
  let mut has_exp = false
  if self.peek() == 'e' || self.peek() == 'E' {
    has_exp = true
    ignore(self.advance())
    if self.peek() == '+' || self.peek() == '-' {
      ignore(self.advance())
    }
    if !(is_digit(self.peek())) {
      while is_alnum(self.peek()) {
        ignore(self.advance())
      }
      raise parse_err(span, "invalid exponent in float literal: " + self.slice_from(start))
    }
    while is_digit(self.peek()) || (self.peek() == '_' && is_digit(self.peek_next())) {
      ignore(self.advance())
    }
  }
  let raw = self.slice_from(start)
  let mut suffix = ""
  if is_alpha(self.peek()) {
    let suf_start = self.pos
    while is_alnum(self.peek()) {
      ignore(self.advance())
    }
    suffix = self.slice_from(suf_start).to_lower().to_string()
  }
  let clean = strip_underscores(raw)
  let full = raw + suffix
  if suffix == "" {
    if has_dot || has_exp {
      Float(parse_float_token(clean, full, span), span)
    } else {
      Int(parse_int_token(clean, span), span)
    }
  } else {
    match suffix {
      "f" | "f16" | "f32" | "f64" | "bf16" | "fp8" =>
        Float(parse_float_token(clean, full, span), span)
      "i" | "i4" | "i8" | "i16" | "i32" | "i64" | "u" | "u8" | "u16" | "u32" | "u64" =>
        if has_dot || has_exp {
          raise parse_err(span, "float literal cannot have integer suffix: " + full)
        } else {
          let n = parse_int_token(clean, span)
          if !(width_ok(suffix, n)) {
            raise parse_err(span, "integer literal out of range for suffix " + suffix + ": " + full)
          }
          Int(n, span)
        }
      "n" =>
        raise parse_err(span, "bigint suffix 'n' is not in the MoonBit trial (std/math/bigint is not ported)")
      _ => raise parse_err(span, "unknown numeric suffix '" + suffix + "' in literal: " + full)
    }
  }
}

fn width_ok(suffix : String, n : Int64) -> Bool {
  match suffix {
    "u8" => n >= 0L && n <= 255L
    "u16" => n >= 0L && n <= 65535L
    "u32" => n >= 0L && n <= 4294967295L
    _ => true
  }
}

fn strip_underscores(text : String) -> String {
  let buf = StringBuilder()
  for c in text {
    if c != '_' {
      buf.write_char(c)
    }
  }
  buf.to_string()
}

fn parse_int_token(text : String, span : Span) -> Int64 raise MoraError {
  if text.length() == 0 {
    raise parse_err(span, "invalid integer literal")
  }
  let mut n = 0L
  for c in text {
    if !(is_digit(c)) {
      raise parse_err(span, "invalid integer literal: " + text)
    }
    let d = (c.to_int() - '0'.to_int()).to_int64()
    if n > (@int64.MAX_VALUE - d) / 10L {
      let shown = if text.length() > 24 {
        text[0:24].to_owned() + "… (" + text.length().to_string() + " digits)"
      } else {
        text
      }
      raise parse_err(
        span,
        "integer literal out of range: " + shown + " exceeds i64 max (9223372036854775807). Use a float literal or std/math/bigint",
      )
    }
    n = n * 10L + d
  }
  n
}

fn has_nonzero_digit(text : String) -> Bool {
  let mut yes = false
  for c in text {
    if c >= '1' && c <= '9' {
      yes = true
    }
  }
  yes
}

fn parse_float_token(clean : String, full : String, span : Span) -> Double raise MoraError {
  let parsed = try @json.parse(clean) catch {
    _ => raise parse_err(span, "invalid float literal: " + full)
  } noraise {
    value => value
  }
  match parsed {
    Number(n, ..) => {
      if n.is_nan() || n.is_inf() {
        raise parse_err(
          span,
          "float literal out of range: " + full + " exceeds f64 max (~1.797e308)",
        )
      }
      if n == 0.0 && has_nonzero_digit(clean) {
        raise parse_err(
          span,
          "float literal out of range: " + full + " underflows to 0 (f64 min normal ~2.2e-308)",
        )
      }
      n
    }
    _ => raise parse_err(span, "invalid float literal: " + full)
  }
}

fn Lexer::ident_lit(self : Lexer, span : Span) -> Tok {
  let start = self.pos - 1
  while is_alnum(self.peek()) {
    ignore(self.advance())
  }
  let text = self.slice_from(start)
  match text {
    "let" => Let(span)
    "task" => Task(span)
    "if" => If(span)
    "then" => Then(span)
    "end" => End(span)
    "return" => Return(span)
    "true" => True(span)
    "false" => False(span)
    "nil" => Nil(span)
    "for" => For(span)
    "in" => In(span)
    "match" => Match(span)
    "fn" => Fn(span)
    "as" => As(span)
    "break" => Break(span)
    "continue" => Continue(span)
    "macro" => Macro(span)
    "quote" => Ident("quote", span)
    "dyn" => Dyn(span)
    "type" => TypeKw(span)
    "enum" => EnumKw(span)
    "struct" => StructKw(span)
    "loop" => Loop(span)
    "with" => With(span)
    "import" => Import(span)
    _ => Ident(text, span)
  }
}
