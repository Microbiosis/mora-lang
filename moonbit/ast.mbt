// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

priv enum BinOp {
  Add
  Sub
  Mul
  Div
  Mod
  Pow
  Eq
  Ne
  Gt
  Lt
  Ge
  Le
  And
  Or
  Pipe
}

priv enum UnaryOp {
  Neg
  Not
}

priv enum TyAnn {
  Int
  Float
  Number
  String
  Bool
  Nil
  Char
  Any
  List(TyAnn)
  Dict(TyAnn, TyAnn)
}

priv struct Param {
  name : String
  ann : TyAnn?
  span : Span
}

priv enum Pat {
  Wild
  Bind(String)
  LitInt(Int64)
  LitFloat(Double)
  LitStr(String)
  LitBool(Bool)
  LitNil
  LitChar(Char)
}

priv struct Arm {
  pat : Pat
  guard_expr : Expr?
  body : Expr
  span : Span
}

priv struct Block {
  stmts : Array[Stmt]
}

fn block_of(stmts : Array[Stmt]) -> Block {
  { stmts, }
}

priv enum Expr {
  Int(Int64, Span)
  Float(Double, Span)
  Str(String, Span)
  Char(Char, Span)
  Bool(Bool, Span)
  Nil(Span)
  Var(String, Span)
  Unary(UnaryOp, Expr, Span)
  Binary(BinOp, Expr, Expr, Span)
  Call(Expr, Array[Expr], Span)
  Method(Expr, String, Array[Expr], Span)
  Index(Expr, Expr, Span)
  List(Array[Expr], Span)
  Dict(Array[(String, Expr)], Span)
  If(Expr, Block, Block?, Span)
  Match(Expr, Array[Arm], Span)
  Fn(Array[Param], Block, Span)
  Assign(String, Expr, Span)
}

priv enum Stmt {
  Let(String, TyAnn?, Expr, Span)
  Task(String, Array[Param], TyAnn?, Block, Span)
  Return(Expr?, Span)
  For(String, Expr, Block, Span)
  While(Expr, Block, Span)
  Break(Span)
  Continue(Span)
  Expr(Expr)
}

priv struct Program {
  stmts : Array[Stmt]
}

fn Expr::span(self : Expr) -> Span {
  match self {
    Int(_, s) | Float(_, s) | Str(_, s) | Char(_, s) | Bool(_, s) | Nil(s) | Var(_, s) | Unary(_, _, s) | Binary(_, _, _, s) | Call(_, _, s) | Method(_, _, _, s) | Index(_, _, s) | List(_, s) | Dict(_, s) | If(_, _, _, s) | Match(_, _, s) | Fn(_, _, s) | Assign(_, _, s) => s
  }
}

fn Stmt::span(self : Stmt) -> Span {
  match self {
    Let(_, _, _, s) | Task(_, _, _, _, s) | Return(_, s) | For(_, _, _, s) | While(_, _, s) | Break(s) | Continue(s) => s
    Expr(e) => e.span()
  }
}

/// Syntactic values may be let-generalized (Wright's value restriction).
fn Expr::is_value(self : Expr) -> Bool {
  match self {
    Int(_) | Float(_) | Str(_) | Char(_) | Bool(_) | Nil(_) | Var(_) | Fn(_) => true
    List(items, _) => {
      let mut ok = true
      for item in items {
        if !(item.is_value()) {
          ok = false
        }
      }
      ok
    }
    Dict(pairs, _) => {
      let mut ok = true
      for pair in pairs {
        let (_, value) = pair
        if !(value.is_value()) {
          ok = false
        }
      }
      ok
    }
    _ => false
  }
}
