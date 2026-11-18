// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.
//
// Recursive descent for declarations and an iterative Pratt parser for
// infix operators. Precedence matches ParserV3::emit_pratt_w:
// or < and < equality < pipe < comparison < sum < product.
// `**` is right-associative and tighter than prefix `-`.

priv struct Parser {
  toks : Array[Tok]
  mut pos : Int
  mut depth : Int
}

fn Parser::new(toks : Array[Tok]) -> Parser {
  { toks, pos: 0, depth: 0 }
}

fn parse(src : String) -> Program raise MoraError {
  Parser::new(lex(src)).parse_program()
}

fn Parser::enter(self : Parser, span : Span) -> Unit raise MoraError {
  self.depth = self.depth + 1
  if self.depth > 512 {
    raise parse_err(span, "nesting exceeds 512")
  }
}

fn Parser::leave(self : Parser) -> Unit {
  self.depth = self.depth - 1
}

fn Parser::peek(self : Parser) -> Tok {
  if self.pos >= self.toks.length() {
    Eof(Span::make(1, 1))
  } else {
    self.toks[self.pos]
  }
}

fn Parser::advance(self : Parser) -> Tok {
  let tok = self.peek()
  if self.pos < self.toks.length() {
    self.pos = self.pos + 1
  }
  tok
}

fn Parser::skip_nl(self : Parser) -> Unit {
  while true {
    match self.peek() {
      Newline(_) => ignore(self.advance())
      _ => break
    }
  }
}

fn Parser::at(self : Parser, kind : String) -> Bool {
  match (kind, self.peek()) {
    ("end", End(_)) | ("else", Ident("else", _)) | ("newline", Newline(_)) | ("eof", Eof(_)) | ("rparen", RParen(_)) | ("rbrace", RBrace(_)) | ("rbrack", RBrack(_)) | ("comma", Comma(_)) | ("colon", Colon(_)) | ("assign", Assign(_)) | ("pipe", Pipe(_)) | ("lparen", LParen(_)) | ("lbrace", LBrace(_)) | ("lbrack", LBrack(_)) | ("arrow", Arrow(_)) | ("fat", FatArrow(_)) | ("then", Then(_)) | ("with", With(_)) | ("in", In(_)) | ("dot", Dot(_)) => true
    _ => false
  }
}

fn unsupported(span : Span, feature : String) -> MoraError {
  parse_err(span, feature + " is not in the MoonBit trial")
}

fn Parser::expect_word(self : Parser, what : String) -> (String, Span) raise MoraError {
  let tok = self.peek()
  match tok.word() {
    Some(name) => {
      ignore(self.advance())
      (name, tok.span())
    }
    None => raise parse_err(tok.span(), "expected " + what)
  }
}

fn Parser::expect_plain_ident(self : Parser, what : String) -> (String, Span) raise MoraError {
  match self.peek() {
    Ident(name, span) => {
      ignore(self.advance())
      (name, span)
    }
    other => raise parse_err(other.span(), "expected " + what)
  }
}

fn Parser::parse_program(self : Parser) -> Program raise MoraError {
  let stmts : Array[Stmt] = []
  while true {
    self.skip_nl()
    match self.peek() {
      Eof(_) => break
      _ => stmts.push(self.parse_stmt())
    }
  }
  { stmts, }
}

fn Parser::parse_stmt(self : Parser) -> Stmt raise MoraError {
  match self.peek() {
    Let(_) => self.parse_let()
    Task(_) => self.parse_task()
    For(_) => self.parse_for()
    Return(_) => self.parse_return()
    Break(span) => {
      ignore(self.advance())
      Break(span)
    }
    Continue(span) => {
      ignore(self.advance())
      Continue(span)
    }
    If(_) => Expr(self.parse_if())
    Match(_) => Expr(self.parse_match())
    Import(span) => raise unsupported(span, "modules (import/export)")
    Macro(span) => raise unsupported(span, "macros")
    TypeKw(span) => raise unsupported(span, "type aliases")
    EnumKw(span) => raise unsupported(span, "enums")
    StructKw(span) => raise unsupported(span, "structs")
    Loop(span) => raise unsupported(span, "`loop`")
    Dyn(span) => raise unsupported(span, "dyn dispatch")
    Ident(name, span) =>
      match name {
        "while" => self.parse_while()
        "assign" => self.parse_assign_stmt()
        "handle" | "perform" => raise unsupported(span, "algebraic effects (perform/handle)")
        "spawn" | "select" | "channel" => raise unsupported(span, "concurrency (spawn/channel/select)")
        "parallel" | "worker" => raise unsupported(span, "parallel blocks")
        "transaction" | "commit" | "rollback" => raise unsupported(span, "transactions")
        "export" => raise unsupported(span, "modules (import/export)")
        "effect" => raise unsupported(span, "effect signatures")
        "quote" => raise unsupported(span, "quote")
        "eval" | "aggregate" => raise unsupported(span, "`" + name + "`")
        _ => Expr(self.parse_expr())
      }
    _ => Expr(self.parse_expr())
  }
}

fn Parser::parse_let(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  let (name, _) = self.expect_plain_ident("a variable name after 'let'")
  let ann = self.parse_optional_ann()
  match self.peek() {
    Assign(_) => ignore(self.advance())
    other => raise parse_err(other.span(), "expected '=' in let binding")
  }
  let value = self.parse_expr()
  Let(name, ann, value, span)
}

fn Parser::parse_assign_stmt(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  let (name, _) = self.expect_plain_ident("a variable name after 'assign'")
  match self.peek() {
    Assign(_) => ignore(self.advance())
    other => raise parse_err(other.span(), "expected '=' in assign statement")
  }
  Expr(Assign(name, self.parse_expr(), span))
}

fn Parser::parse_task(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  let (name, _) = self.expect_plain_ident("a task name")
  match self.peek() {
    Lt(_) => raise unsupported(self.peek().span(), "generic tasks")
    _ => ()
  }
  match self.peek() {
    LParen(_) => ignore(self.advance())
    other => raise parse_err(other.span(), "expected '(' after task name")
  }
  let params = self.parse_params()
  let ann = self.parse_optional_ann()
  let body = self.parse_task_body(span)
  Task(name, params, ann, body, span)
}

fn Parser::parse_params(self : Parser) -> Array[Param] raise MoraError {
  let params : Array[Param] = []
  self.skip_nl()
  while true {
    match self.peek() {
      RParen(_) => {
        ignore(self.advance())
        break
      }
      Eof(span) => raise parse_err(span, "expected ')' after parameters")
      _ => {
        let (name, span) = self.expect_plain_ident("a parameter name")
        let ann = self.parse_optional_ann()
        params.push({ name, ann, span })
        self.skip_nl()
        match self.peek() {
          Comma(_) => {
            ignore(self.advance())
            self.skip_nl()
          }
          _ => ()
        }
      }
    }
  }
  params
}

fn Parser::parse_optional_ann(self : Parser) -> TyAnn? raise MoraError {
  match self.peek() {
    Colon(_) => {
      ignore(self.advance())
      Some(self.parse_ty())
    }
    _ => None
  }
}

fn Parser::parse_ty(self : Parser) -> TyAnn raise MoraError {
  match self.peek() {
    Nil(_) => {
      ignore(self.advance())
      Nil
    }
    Ident(name, span) => {
      ignore(self.advance())
      let lower = name.to_lower().to_string()
      match self.peek() {
        Lt(_) => self.parse_generic_ty(lower, span)
        _ => self.simple_ty(lower, span)
      }
    }
    other => raise parse_err(other.span(), "expected a type annotation")
  }
}

fn Parser::simple_ty(self : Parser, name : String, span : Span) -> TyAnn raise MoraError {
  match name {
    "int" | "i4" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => Int
    "number" => Number
    "float" | "f16" | "bf16" | "fp8" | "f32" | "f64" => Float
    "string" => String
    "char" => Char
    "bool" => Bool
    "nil" => Nil
    "any" => Any
    _ => raise unsupported(span, "type annotation '" + name + "'")
  }
}

fn Parser::parse_generic_ty(self : Parser, name : String, span : Span) -> TyAnn raise MoraError {
  ignore(self.advance())
  let args : Array[TyAnn] = []
  self.skip_nl()
  while true {
    match self.peek() {
      Gt(_) => {
        ignore(self.advance())
        break
      }
      Eof(_) => raise parse_err(span, "expected '>' after generic type arguments")
      _ => {
        args.push(self.parse_ty())
        self.skip_nl()
        match self.peek() {
          Comma(_) => {
            ignore(self.advance())
            self.skip_nl()
          }
          _ => ()
        }
      }
    }
  }
  match name {
    "list" =>
      match args {
        [elem, ..] => List(elem)
        _ => raise parse_err(span, "list<T> needs one type argument")
      }
    "dict" =>
      match args {
        [key, value, ..] => Dict(key, value)
        _ => raise parse_err(span, "dict<K, V> needs two type arguments")
      }
    _ => raise unsupported(span, "generic type '" + name + "'")
  }
}

fn Parser::parse_task_body(self : Parser, span : Span) -> Block raise MoraError {
  self.enter(span)
  defer self.leave()
  match self.peek() {
    Newline(_) => {
      self.skip_nl()
      let stmts : Array[Stmt] = []
      while true {
        match self.peek() {
          End(_) => {
            ignore(self.advance())
            break
          }
          Eof(_) => raise parse_err(span, "expected 'end' after task body")
          _ => {
            stmts.push(self.parse_stmt())
            self.skip_nl()
          }
        }
      }
      { stmts, }
    }
    End(_) => {
      ignore(self.advance())
      block_of([])
    }
    _ => {
      let expr = self.parse_expr()
      block_of([Expr(expr)])
    }
  }
}

fn Parser::parse_for(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  let (name, _) = self.expect_plain_ident("a loop variable")
  match self.peek() {
    In(_) => ignore(self.advance())
    other => raise parse_err(other.span(), "expected 'in' in for loop")
  }
  let iter = self.parse_expr()
  let body = self.parse_end_block(span)
  For(name, iter, body, span)
}

fn Parser::parse_while(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  let cond = self.parse_expr()
  let body = self.parse_end_block(span)
  While(cond, body, span)
}

fn Parser::parse_return(self : Parser) -> Stmt raise MoraError {
  let span = self.advance().span()
  if self.stmt_ends() {
    Return(None, span)
  } else {
    Return(Some(self.parse_expr()), span)
  }
}

fn Parser::stmt_ends(self : Parser) -> Bool {
  match self.peek() {
    Newline(_) | End(_) | Eof(_) | RParen(_) | RBrace(_) => true
    Ident("else", _) => true
    _ => false
  }
}

/// Block that runs until `end`, `else`, or a closing delimiter.
/// `end` is consumed. `else` is left for the caller.
fn Parser::parse_end_block(self : Parser, span : Span) -> Block raise MoraError {
  self.enter(span)
  defer self.leave()
  match self.peek() {
    LBrace(_) => self.parse_brace_block()
    _ => {
      self.skip_nl()
      let stmts : Array[Stmt] = []
      while !(self.block_end()) {
        stmts.push(self.parse_stmt())
        self.skip_nl()
      }
      match self.peek() {
        End(_) => ignore(self.advance())
        _ =>
          if stmts.length() > 1 {
            match self.peek() {
              Eof(_) => raise parse_err(span, "expected 'end' to close this block")
              _ => ()
            }
          }
      }
      { stmts, }
    }
  }
}

fn Parser::block_end(self : Parser) -> Bool {
  match self.peek() {
    End(_) | RParen(_) | RBrace(_) | Comma(_) | Eof(_) => true
    Ident("else", _) => true
    _ => false
  }
}

fn Parser::parse_brace_block(self : Parser) -> Block raise MoraError {
  ignore(self.advance())
  self.skip_nl()
  let stmts : Array[Stmt] = []
  while true {
    match self.peek() {
      RBrace(_) => {
        ignore(self.advance())
        break
      }
      Eof(span) => raise parse_err(span, "expected '}' after block")
      _ => {
        stmts.push(self.parse_stmt())
        self.skip_nl()
      }
    }
  }
  { stmts, }
}

fn Parser::parse_if(self : Parser) -> Expr raise MoraError {
  let span = self.advance().span()
  self.enter(span)
  defer self.leave()
  let cond = self.parse_expr()
  let then_block = match self.peek() {
    Then(_) => {
      ignore(self.advance())
      match self.peek() {
        Newline(_) | LBrace(_) => self.parse_end_block(span)
        _ => {
          let stmt = self.parse_stmt()
          match self.peek() {
            End(_) => ignore(self.advance())
            _ => ()
          }
          block_of([stmt])
        }
      }
    }
    _ => self.parse_end_block(span)
  }
  let else_block = match self.peek() {
    Ident("else", _) => {
      ignore(self.advance())
      match self.peek() {
        If(_) => {
          let nested = self.parse_if()
          Some(block_of([Expr(nested)]))
        }
        _ => Some(self.parse_end_block(span))
      }
    }
    _ => None
  }
  If(cond, then_block, else_block, span)
}

fn Parser::parse_match(self : Parser) -> Expr raise MoraError {
  let span = self.advance().span()
  self.enter(span)
  defer self.leave()
  let scrut = self.parse_expr()
  let with_form = match self.peek() {
    With(_) => {
      ignore(self.advance())
      true
    }
    LBrace(_) => {
      ignore(self.advance())
      false
    }
    other => raise parse_err(other.span(), "expected 'with' or '{' after match subject")
  }
  let arms : Array[Arm] = []
  if with_form {
    self.skip_nl()
    while true {
      match self.peek() {
        End(_) => {
          ignore(self.advance())
          break
        }
        Eof(_) => raise parse_err(span, "expected 'end' after match")
        _ => {
          arms.push(self.parse_arm())
          self.skip_nl()
        }
      }
    }
  } else {
    self.skip_nl()
    while true {
      match self.peek() {
        RBrace(_) => {
          ignore(self.advance())
          break
        }
        Eof(_) => raise parse_err(span, "expected '}' after match")
        _ => {
          arms.push(self.parse_arm())
          self.skip_nl()
          match self.peek() {
            Comma(_) => {
              ignore(self.advance())
              self.skip_nl()
            }
            _ => ()
          }
        }
      }
    }
  }
  Match(scrut, arms, span)
}

fn Parser::parse_arm(self : Parser) -> Arm raise MoraError {
  let span = self.peek().span()
  let pat = self.parse_pat()
  let guard_expr = match self.peek() {
    Ident("when", _) => {
      ignore(self.advance())
      Some(self.parse_expr())
    }
    _ => None
  }
  match self.peek() {
    Arrow(_) | FatArrow(_) => ignore(self.advance())
    other =>
      raise parse_err(
        other.span(),
        "malformed match arm — an arm body must be an expression: `pattern [when <expr>] -> <expr>`",
      )
  }
  let body = self.parse_expr()
  { pat, guard_expr, body, span }
}

fn Parser::parse_pat(self : Parser) -> Pat raise MoraError {
  match self.peek() {
    Ident("_", _) => {
      ignore(self.advance())
      Wild
    }
    Ident(name, _) => {
      ignore(self.advance())
      Bind(name)
    }
    Int(n, _) => {
      ignore(self.advance())
      LitInt(n)
    }
    Float(n, _) => {
      ignore(self.advance())
      LitFloat(n)
    }
    Str(s, _) => {
      ignore(self.advance())
      LitStr(s)
    }
    True(_) => {
      ignore(self.advance())
      LitBool(true)
    }
    False(_) => {
      ignore(self.advance())
      LitBool(false)
    }
    Nil(_) => {
      ignore(self.advance())
      LitNil
    }
    Char(c, _) => {
      ignore(self.advance())
      LitChar(c)
    }
    Minus(span) => {
      ignore(self.advance())
      match self.peek() {
        Int(n, _) => {
          ignore(self.advance())
          LitInt(0L - n)
        }
        Float(n, _) => {
          ignore(self.advance())
          LitFloat(0.0 - n)
        }
        _ => raise parse_err(span, "expected a literal after '-' in a pattern")
      }
    }
    LBrack(span) | LBrace(span) =>
      raise unsupported(span, "list and dict patterns")
    other => raise parse_err(other.span(), "expected a pattern")
  }
}

fn Parser::parse_expr(self : Parser) -> Expr raise MoraError {
  match self.peek() {
    Ident(_, _) => {
      let saved = self.pos
      let (name, span) = self.expect_plain_ident("an expression")
      match self.peek() {
        Assign(_) => {
          ignore(self.advance())
          Assign(name, self.parse_expr(), span)
        }
        _ => {
          self.pos = saved
          self.parse_pratt()
        }
      }
    }
    If(_) => self.parse_if()
    _ => self.parse_pratt()
  }
}

fn prec(op : BinOp) -> Int {
  match op {
    Or => 1
    And => 2
    Eq | Ne => 3
    Pipe => 4
    Gt | Lt | Ge | Le => 5
    Add | Sub => 6
    Mul | Div | Mod => 7
    Pow => 8
  }
}

fn Parser::parse_pratt(self : Parser) -> Expr raise MoraError {
  let first = self.parse_unary()
  let operands : Array[Expr] = [first]
  let ops : Array[BinOp] = []
  while true {
    match self.peek_infix() {
      None => break
      Some(op) => {
        let p = prec(op)
        while ops.length() > 0 {
          let top = ops[ops.length() - 1]
          if prec(top) >= p {
            ignore(ops.pop())
            let rhs = operands.pop()
            let lhs = operands.pop()
            match (lhs, rhs) {
              (Some(left), Some(right)) => operands.push(self.fold_infix(left, top, right))
              _ => abort("pratt stack underflow")
            }
          } else {
            break
          }
        }
        ops.push(op)
        operands.push(self.parse_unary())
      }
    }
  }
  while ops.length() > 0 {
    match ops.pop() {
      Some(top) => {
        let rhs = operands.pop()
        let lhs = operands.pop()
        match (lhs, rhs) {
          (Some(left), Some(right)) => operands.push(self.fold_infix(left, top, right))
          _ => abort("pratt stack underflow")
        }
      }
      None => break
    }
  }
  match operands.pop() {
    Some(expr) => expr
    None => abort("pratt produced no expression")
  }
}

fn Parser::fold_infix(self : Parser, lhs : Expr, op : BinOp, rhs : Expr) -> Expr {
  let span = lhs.span()
  match op {
    Pipe =>
      match rhs {
        Call(Var(name, _), args, _) => Method(lhs, name, args, span)
        _ => Binary(Pipe, lhs, rhs, span)
      }
    _ => Binary(op, lhs, rhs, span)
  }
}

fn Parser::peek_infix(self : Parser) -> BinOp? raise MoraError {
  match self.peek() {
    Ident("or", _) => {
      ignore(self.advance())
      Some(Or)
    }
    Ident("and", _) => {
      ignore(self.advance())
      Some(And)
    }
    Eq(_) => {
      ignore(self.advance())
      Some(Eq)
    }
    NotEq(_) => {
      ignore(self.advance())
      Some(Ne)
    }
    Lt(_) => {
      ignore(self.advance())
      Some(Lt)
    }
    Gt(_) => {
      ignore(self.advance())
      Some(Gt)
    }
    Le(_) => {
      ignore(self.advance())
      Some(Le)
    }
    Ge(_) => {
      ignore(self.advance())
      Some(Ge)
    }
    Plus(_) => {
      ignore(self.advance())
      Some(Add)
    }
    Minus(_) => {
      ignore(self.advance())
      Some(Sub)
    }
    Star(_) => {
      ignore(self.advance())
      Some(Mul)
    }
    Slash(_) => {
      ignore(self.advance())
      Some(Div)
    }
    Percent(_) => {
      ignore(self.advance())
      Some(Mod)
    }
    At(span) => raise unsupported(span, "matrix multiplication ('@')")
    Pipe(_) => {
      ignore(self.advance())
      Some(Pipe)
    }
    Newline(_) => {
      let saved = self.pos
      self.skip_nl()
      match self.peek() {
        Pipe(_) => {
          ignore(self.advance())
          Some(Pipe)
        }
        _ => {
          self.pos = saved
          None
        }
      }
    }
    _ => None
  }
}

priv enum Prefix {
  NotP(Span)
  NegP(Span)
}

fn Parser::parse_unary(self : Parser) -> Expr raise MoraError {
  let prefixes : Array[Prefix] = []
  while true {
    match self.peek() {
      Ident("not", span) | Bang(span) => {
        ignore(self.advance())
        prefixes.push(NotP(span))
      }
      Minus(span) => {
        ignore(self.advance())
        prefixes.push(NegP(span))
      }
      _ => break
    }
  }
  let mut expr = self.parse_power()
  let mut i = prefixes.length() - 1
  while i >= 0 {
    match prefixes[i] {
      NotP(span) => expr = Unary(Not, expr, span)
      NegP(span) => expr = Unary(Neg, expr, span)
    }
    i = i - 1
  }
  expr
}

fn Parser::parse_power(self : Parser) -> Expr raise MoraError {
  let lhs = self.parse_postfix()
  match self.peek() {
    StarStar(span) => {
      ignore(self.advance())
      let rhs = self.parse_unary()
      Binary(Pow, lhs, rhs, span)
    }
    _ => lhs
  }
}

fn Parser::parse_postfix(self : Parser) -> Expr raise MoraError {
  let mut expr = self.parse_primary()
  while true {
    match self.peek() {
      Dot(_) => {
        ignore(self.advance())
        let (name, span) = self.expect_word("a method name")
        let args = match self.peek() {
          LParen(_) => self.parse_args()
          _ => []
        }
        expr = Method(expr, name, args, span)
      }
      LBrack(span) => {
        ignore(self.advance())
        self.skip_nl()
        let index = self.parse_expr()
        self.skip_nl()
        match self.peek() {
          RBrack(_) => ignore(self.advance())
          other => raise parse_err(other.span(), "expected ']' after index")
        }
        expr = Index(expr, index, span)
      }
      LParen(_) => {
        let args = self.parse_args()
        expr = Call(expr, args, expr.span())
      }
      ColonColon(span) => raise unsupported(span, "associated paths ('::')")
      _ => break
    }
  }
  expr
}

fn Parser::parse_args(self : Parser) -> Array[Expr] raise MoraError {
  ignore(self.advance())
  let args : Array[Expr] = []
  self.skip_nl()
  while true {
    match self.peek() {
      RParen(_) => {
        ignore(self.advance())
        break
      }
      Eof(span) => raise parse_err(span, "expected ')'")
      _ => {
        args.push(self.parse_expr())
        self.skip_nl()
        match self.peek() {
          Comma(_) => {
            ignore(self.advance())
            self.skip_nl()
          }
          _ => ()
        }
      }
    }
  }
  args
}

fn Parser::parse_primary(self : Parser) -> Expr raise MoraError {
  let tok = self.peek()
  let span = tok.span()
  self.enter(span)
  defer self.leave()
  match tok {
    Int(n, s) => {
      ignore(self.advance())
      Int(n, s)
    }
    Float(n, s) => {
      ignore(self.advance())
      Float(n, s)
    }
    Str(text, s) => {
      ignore(self.advance())
      Str(text, s)
    }
    Char(c, s) => {
      ignore(self.advance())
      Char(c, s)
    }
    True(s) => {
      ignore(self.advance())
      Bool(true, s)
    }
    False(s) => {
      ignore(self.advance())
      Bool(false, s)
    }
    Nil(s) => {
      ignore(self.advance())
      Nil(s)
    }
    Ident(name, s) => {
      ignore(self.advance())
      match name {
        "handle" | "perform" => raise unsupported(s, "algebraic effects (perform/handle)")
        "spawn" | "select" => raise unsupported(s, "concurrency (spawn/channel/select)")
        "quote" => raise unsupported(s, "quote")
        _ => Var(name, s)
      }
    }
    LParen(_) => {
      ignore(self.advance())
      self.skip_nl()
      let expr = self.parse_expr()
      self.skip_nl()
      match self.peek() {
        RParen(_) => ignore(self.advance())
        other => raise parse_err(other.span(), "expected ')'")
      }
      expr
    }
    LBrack(_) => self.parse_list()
    LBrace(_) => self.parse_dict()
    Fn(_) => self.parse_fn()
    Match(_) => self.parse_match()
    If(_) => self.parse_if()
    other => raise parse_err(other.span(), "expected an expression")
  }
}

fn Parser::parse_list(self : Parser) -> Expr raise MoraError {
  let span = self.advance().span()
  let items : Array[Expr] = []
  self.skip_nl()
  while true {
    match self.peek() {
      RBrack(_) => {
        ignore(self.advance())
        break
      }
      Eof(_) => raise parse_err(span, "expected ']'")
      _ => {
        items.push(self.parse_expr())
        self.skip_nl()
        match self.peek() {
          Comma(_) => {
            ignore(self.advance())
            self.skip_nl()
          }
          _ => ()
        }
      }
    }
  }
  List(items, span)
}

fn Parser::parse_dict(self : Parser) -> Expr raise MoraError {
  let span = self.advance().span()
  let pairs : Array[(String, Expr)] = []
  self.skip_nl()
  while true {
    match self.peek() {
      RBrace(_) => {
        ignore(self.advance())
        break
      }
      Eof(_) => raise parse_err(span, "expected '}'")
      _ => {
        let key = match self.peek() {
          Str(text, _) => {
            ignore(self.advance())
            text
          }
          other =>
            match other.word() {
              Some(name) => {
                ignore(self.advance())
                name
              }
              None => raise parse_err(other.span(), "expected a dict key")
            }
        }
        match self.peek() {
          Colon(_) => ignore(self.advance())
          other => raise parse_err(other.span(), "expected ':' after dict key")
        }
        pairs.push((key, self.parse_expr()))
        self.skip_nl()
        match self.peek() {
          Comma(_) => {
            ignore(self.advance())
            self.skip_nl()
          }
          _ => ()
        }
      }
    }
  }
  Dict(pairs, span)
}

fn Parser::parse_fn(self : Parser) -> Expr raise MoraError {
  let span = self.advance().span()
  self.enter(span)
  defer self.leave()
  match self.peek() {
    Lt(_) => raise unsupported(self.peek().span(), "generic closures")
    _ => ()
  }
  match self.peek() {
    LParen(_) => ignore(self.advance())
    other => raise parse_err(other.span(), "expected '(' after 'fn'")
  }
  let params = self.parse_params()
  let body = match self.peek() {
    FatArrow(_) => {
      ignore(self.advance())
      let expr = self.parse_expr()
      match self.peek() {
        End(_) => ignore(self.advance())
        _ => ()
      }
      block_of([Expr(expr)])
    }
    Newline(_) | End(_) | LBrace(_) => self.parse_end_block(span)
    _ =>
      if self.inline_fn_body() {
        let expr = self.parse_expr()
        match self.peek() {
          End(_) => ignore(self.advance())
          _ => ()
        }
        block_of([Expr(expr)])
      } else {
        self.parse_end_block(span)
      }
  }
  Fn(params, body, span)
}

fn Parser::inline_fn_body(self : Parser) -> Bool {
  match self.peek() {
    Ident(name, _) =>
      !(name == "handle" || name == "perform" || name == "assign")
    Str(_) | Char(_) | Int(_) | Float(_) | True(_) | False(_) | Nil(_) | LParen(_) | Minus(_) | Bang(_) => true
    _ => false
  }
}
