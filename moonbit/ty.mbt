// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.
//
// Hindley-Milner (Algorithm W) for the trial's type language:
// int, float, number (int|float), bool, string, nil, char, list, dict, arrows.
// Let-generalization follows Wright's value restriction. `number` and `int <: float`
// are subtyping constraints, not silent coercions of an int binding into float.

priv enum MType {
  TInt
  TFloat
  TNumber
  TBool
  TString
  TNil
  TChar
  TAny
  TList(MType)
  TDict(MType, MType)
  TArrow(Array[MType], MType)
  TVar(Int)
  TBuiltin(String)
}

priv struct Scheme {
  vars : Array[Int]
  numeric : Array[Int]
  body : MType
}

fn mono(ty : MType) -> Scheme {
  { vars: [], numeric: [], body: ty }
}

fn type_err(span : Span, message : String) -> MoraError {
  TypeErr("Type error at " + span.locus() + ": " + message)
}

fn type_mismatch(span : Span, expected : String, actual : String) -> MoraError {
  TypeErr(
    "Type error at " + span.locus() + ": mismatched types\n  expected: " + expected + "\n  actual:   " + actual,
  )
}

fn Engine::fresh(self : Engine) -> MType {
  let id = self.next_var
  self.next_var = id + 1
  self.subst.push(None)
  TVar(id)
}

fn Engine::resolve(self : Engine, ty : MType) -> MType {
  match ty {
    TVar(id) =>
      match self.subst[id] {
        Some(inner) => {
          let resolved = self.resolve(inner)
          self.subst[id] = Some(resolved)
          resolved
        }
        None => TVar(id)
      }
    other => other
  }
}

fn Engine::apply(self : Engine, ty : MType) -> MType {
  let ty = self.resolve(ty)
  match ty {
    TList(elem) => TList(self.apply(elem))
    TDict(key, value) => TDict(self.apply(key), self.apply(value))
    TArrow(params, ret) => {
      let next : Array[MType] = []
      for param in params {
        next.push(self.apply(param))
      }
      TArrow(next, self.apply(ret))
    }
    other => other
  }
}

fn Engine::occurs(self : Engine, id : Int, ty : MType) -> Bool {
  match self.resolve(ty) {
    TVar(other) => other == id
    TList(elem) => self.occurs(id, elem)
    TDict(key, value) => self.occurs(id, key) || self.occurs(id, value)
    TArrow(params, ret) => {
      let mut yes = self.occurs(id, ret)
      for param in params {
        if self.occurs(id, param) {
          yes = true
        }
      }
      yes
    }
    _ => false
  }
}

fn Engine::is_numeric_type(self : Engine, ty : MType) -> Bool {
  match self.resolve(ty) {
    TInt | TFloat | TNumber => true
    TVar(id) => self.numeric.get(id) is Some(true)
    _ => false
  }
}

fn Engine::mark_numeric(self : Engine, ty : MType) -> Unit raise MoraError {
  match self.resolve(ty) {
    TInt | TFloat | TNumber => ()
    TVar(id) => self.numeric.set(id, true)
    other =>
      raise TypeErr(
        "Type error: expected a numeric type (int, float, or number)\n  actual:   " + self.type_name(other),
      )
  }
}

fn Engine::bind_var(self : Engine, id : Int, ty : MType, span : Span) -> Unit raise MoraError {
  let ty = self.resolve(ty)
  match ty {
    TVar(other) if other == id => ()
    _ => {
      if self.occurs(id, ty) {
        raise type_err(span, "infinite type")
      }
      if self.numeric.get(id) is Some(true) && !(self.is_numeric_type(ty)) {
        match ty {
          TVar(other) => self.numeric.set(other, true)
          _ =>
            raise type_mismatch(span, "numeric type (int, float, or number)", self.type_name(ty))
        }
      }
      self.subst[id] = Some(ty)
    }
  }
}

fn Engine::unify(self : Engine, left : MType, right : MType, span : Span) -> Unit raise MoraError {
  let left = self.resolve(left)
  let right = self.resolve(right)
  match (left, right) {
    (TVar(id), other) | (other, TVar(id)) => self.bind_var(id, other, span)
    (TInt, TInt) | (TFloat, TFloat) | (TNumber, TNumber) | (TBool, TBool) | (TString, TString) | (TNil, TNil) | (TChar, TChar) | (TAny, TAny) => ()
    (TList(a), TList(b)) => self.unify(a, b, span)
    (TDict(k1, v1), TDict(k2, v2)) => {
      self.unify(k1, k2, span)
      self.unify(v1, v2, span)
    }
    (TArrow(p1, r1), TArrow(p2, r2)) => {
      if p1.length() != p2.length() {
        raise type_mismatch(span, self.type_name(left), self.type_name(right))
      }
      for i in 0..<p1.length() {
        self.unify(p1[i], p2[i], span)
      }
      self.unify(r1, r2, span)
    }
    (TBuiltin(a), TBuiltin(b)) if a == b => ()
    _ => raise type_mismatch(span, self.type_name(left), self.type_name(right))
  }
}

fn Engine::subtype(self : Engine, sub : MType, sup : MType) -> Bool raise MoraError {
  let sub = self.resolve(sub)
  let sup = self.resolve(sup)
  match (sub, sup) {
    (_, TAny) | (TAny, _) => true
    (TVar(id), _) => {
      self.bind_var(id, sup, Span::make(0, 1))
      true
    }
    (_, TVar(_)) => false
    (TInt, TInt) | (TFloat, TFloat) | (TNumber, TNumber) | (TBool, TBool) | (TString, TString) | (TNil, TNil) | (TChar, TChar) => true
    (TInt, TFloat) | (TInt, TNumber) | (TFloat, TNumber) => true
    (TList(a), TList(b)) => self.subtype(a, b)
    (TDict(k1, v1), TDict(k2, v2)) => self.subtype(k1, k2) && self.subtype(v1, v2)
    (TArrow(p1, r1), TArrow(p2, r2)) => {
      if p1.length() != p2.length() {
        false
      } else {
        let mut ok = self.subtype(r1, r2)
        for i in 0..<p1.length() {
          if !(self.subtype(p2[i], p1[i])) {
            ok = false
          }
        }
        ok
      }
    }
    _ => false
  }
}

fn Engine::expect_subtype(self : Engine, actual : MType, expected : MType, span : Span) -> Unit raise MoraError {
  if !(self.subtype(actual, expected)) {
    raise type_mismatch(span, self.type_name(expected), self.type_name(actual))
  }
}

fn Engine::type_name(self : Engine, ty : MType) -> String {
  match self.apply(ty) {
    TInt => "int"
    TFloat => "float"
    TNumber => "number"
    TBool => "bool"
    TString => "string"
    TNil => "nil"
    TChar => "char"
    TAny => "any"
    TList(elem) => "list<" + self.type_name(elem) + ">"
    TDict(key, value) => "dict<" + self.type_name(key) + ", " + self.type_name(value) + ">"
    TArrow(params, ret) => {
      let mut text = "("
      for i, param in params {
        if i > 0 {
          text = text + ", "
        }
        text = text + self.type_name(param)
      }
      text + ") -> " + self.type_name(ret)
    }
    TVar(id) =>
      if self.numeric.get(id) is Some(true) {
        "'n" + id.to_string()
      } else {
        "'t" + id.to_string()
      }
    TBuiltin(name) => name
  }
}

fn ann_type(ann : TyAnn) -> MType {
  match ann {
    Int => TInt
    Float => TFloat
    Number => TNumber
    String => TString
    Bool => TBool
    Nil => TNil
    Char => TChar
    Any => TAny
    List(elem) => TList(ann_type(elem))
    Dict(key, value) => TDict(ann_type(key), ann_type(value))
  }
}

fn Engine::lookup_scheme(self : Engine, name : String) -> Scheme? {
  let mut i = self.ty_frames.length() - 1
  while i >= 0 {
    match self.ty_frames[i].get(name) {
      Some(scheme) => return Some(scheme)
      None => i = i - 1
    }
  }
  None
}

fn Engine::bind_scheme(self : Engine, name : String, scheme : Scheme) -> Unit {
  let last = self.ty_frames.length() - 1
  self.ty_frames[last].set(name, scheme)
}

fn Engine::push_ty(self : Engine) -> Unit {
  self.ty_frames.push(Map([]))
}

fn Engine::pop_ty(self : Engine) -> Unit {
  ignore(self.ty_frames.pop())
}

fn Engine::instantiate(self : Engine, scheme : Scheme) -> MType raise MoraError {
  let mut body = scheme.body
  for id in scheme.vars {
    let fresh = self.fresh()
    if scheme.numeric.contains(id) {
      match fresh {
        TVar(fid) => self.numeric.set(fid, true)
        _ => ()
      }
    }
    body = subst_var(body, id, fresh)
  }
  body
}

fn subst_var(ty : MType, id : Int, replacement : MType) -> MType {
  match ty {
    TVar(other) if other == id => replacement
    TList(elem) => TList(subst_var(elem, id, replacement))
    TDict(key, value) => TDict(subst_var(key, id, replacement), subst_var(value, id, replacement))
    TArrow(params, ret) => {
      let next : Array[MType] = []
      for param in params {
        next.push(subst_var(param, id, replacement))
      }
      TArrow(next, subst_var(ret, id, replacement))
    }
    other => other
  }
}

fn Engine::free_vars(self : Engine, ty : MType, acc : Map[Int, Bool]) -> Unit {
  match self.apply(ty) {
    TVar(id) => acc.set(id, true)
    TList(elem) => self.free_vars(elem, acc)
    TDict(key, value) => {
      self.free_vars(key, acc)
      self.free_vars(value, acc)
    }
    TArrow(params, ret) => {
      for param in params {
        self.free_vars(param, acc)
      }
      self.free_vars(ret, acc)
    }
    _ => ()
  }
}

fn Engine::env_free(self : Engine) -> Map[Int, Bool] {
  let acc : Map[Int, Bool] = Map([])
  for frame in self.ty_frames {
    for scheme in frame.values() {
      let body_free : Map[Int, Bool] = Map([])
      self.free_vars(scheme.body, body_free)
      for id in body_free.keys() {
        if !(scheme.vars.contains(id)) {
          acc.set(id, true)
        }
      }
    }
  }
  acc
}

fn Engine::generalize(self : Engine, ty : MType) -> Scheme {
  let resolved = self.apply(ty)
  let env_free = self.env_free()
  let own : Map[Int, Bool] = Map([])
  self.free_vars(resolved, own)
  let vars : Array[Int] = []
  let numeric : Array[Int] = []
  for id in own.keys() {
    if env_free.get(id) is None {
      vars.push(id)
      if self.numeric.get(id) is Some(true) {
        numeric.push(id)
      }
    }
  }
  { vars, numeric, body: resolved }
}

fn Engine::arith(self : Engine, op : BinOp, left : MType, right : MType, span : Span) -> MType raise MoraError {
  let left = self.resolve(left)
  let right = self.resolve(right)
  if op is Add && (is_stringish(left) || is_stringish(right)) {
    return TString
  }
  self.mark_numeric(left) catch {
    _ => raise type_err(span, "operands must be numbers, or strings for '+'\n  actual:   " + self.type_name(left) + " and " + self.type_name(right))
  } noraise {
    _ => ()
  }
  self.mark_numeric(right) catch {
    _ => raise type_err(span, "operands must be numbers\n  actual:   " + self.type_name(left) + " and " + self.type_name(right))
  } noraise {
    _ => ()
  }
  let left = self.resolve(left)
  let right = self.resolve(right)
  match (left, right) {
    (TInt, TInt) => TInt
    (TVar(id), TInt) | (TInt, TVar(id)) => {
      self.bind_var(id, TInt, span)
      TInt
    }
    (TVar(id), TFloat) | (TFloat, TVar(id)) => {
      self.bind_var(id, TFloat, span)
      TFloat
    }
    (TVar(id), TNumber) | (TNumber, TVar(id)) => {
      self.bind_var(id, TNumber, span)
      TNumber
    }
    (TVar(a), TVar(b)) => {
      self.unify(TVar(a), TVar(b), span)
      self.resolve(TVar(a))
    }
    (TFloat, _) | (_, TFloat) => TFloat
    (TNumber, TInt) | (TInt, TNumber) | (TNumber, TNumber) => TNumber
    _ => raise type_err(span, "operands must be numbers\n  actual:   " + self.type_name(left) + " and " + self.type_name(right))
  }
}

fn is_stringish(ty : MType) -> Bool {
  match ty {
    TString => true
    _ => false
  }
}

fn Engine::infer_program(self : Engine, prog : Program) -> Unit raise MoraError {
  for stmt in prog.stmts {
    ignore(self.infer_stmt(stmt, None))
  }
}

fn Engine::infer_block(self : Engine, block : Block, ret : MType?, scoped : Bool) -> MType raise MoraError {
  if scoped {
    self.push_ty()
  }
  let mut last = TNil
  let mut failed : MoraError? = None
  for stmt in block.stmts {
    try {
      last = self.infer_stmt(stmt, ret)
    } catch {
      err => {
        failed = Some(err)
        break
      }
    }
  }
  if scoped {
    self.pop_ty()
  }
  match failed {
    Some(err) => raise err
    None => last
  }
}

fn Engine::infer_stmt(self : Engine, stmt : Stmt, ret : MType?) -> MType raise MoraError {
  match stmt {
    Let(name, ann, value, span) => {
      let inferred = self.infer_expr(value, ret)
      match ann {
        Some(annotation) => {
          let expected = ann_type(annotation)
          self.expect_subtype(inferred, expected, span)
          self.bind_scheme(name, mono(expected))
        }
        None =>
          if value.is_value() {
            self.bind_scheme(name, self.generalize(inferred))
          } else {
            self.bind_scheme(name, mono(self.apply(inferred)))
          }
      }
      TNil
    }
    Task(name, params, ann, body, span) => {
      self.infer_task(name, params, ann, body, span)
      TNil
    }
    Return(value, span) =>
      match ret {
        None => raise type_err(span, "'return' outside of a task or closure")
        Some(expected) => {
          let actual = match value {
            Some(expr) => self.infer_expr(expr, ret)
            None => TNil
          }
          self.check_arg(actual, expected, span)
          expected
        }
      }
    For(name, iter, body, span) => {
      let iter_ty = self.infer_expr(iter, ret)
      let elem = match self.resolve(iter_ty) {
        TList(elem) => elem
        TVar(id) => {
          let elem = self.fresh()
          self.bind_var(id, TList(elem), span)
          elem
        }
        other => raise type_mismatch(span, "list<_>", self.type_name(other))
      }
      self.push_ty()
      self.bind_scheme(name, mono(elem))
      ignore(self.infer_block(body, ret, false))
      self.pop_ty()
      TNil
    }
    While(cond, body, span) => {
      let cond_ty = self.infer_expr(cond, ret)
      self.expect_subtype(cond_ty, TBool, span)
      ignore(self.infer_block(body, ret, true))
      TNil
    }
    Break(span) | Continue(span) => {
      ignore(span)
      TNil
    }
    Expr(If(cond, then_block, None, span)) => {
      let cond_ty = self.infer_expr(cond, ret)
      self.expect_subtype(cond_ty, TBool, span)
      ignore(self.infer_block(then_block, ret, true))
      TNil
    }
    Expr(expr) => self.infer_expr(expr, ret)
  }
}

fn Engine::infer_task(
  self : Engine,
  name : String,
  params : Array[Param],
  ann : TyAnn?,
  body : Block,
  span : Span,
) -> Unit raise MoraError {
  let param_tys : Array[MType] = []
  for param in params {
    let ty = match param.ann {
      Some(annotation) => ann_type(annotation)
      None => self.fresh()
    }
    param_tys.push(ty)
  }
  let ret = match ann {
    Some(annotation) => ann_type(annotation)
    None => self.fresh()
  }
  self.bind_scheme(name, mono(TArrow(param_tys, ret)))
  self.push_ty()
  for i, param in params {
    self.bind_scheme(param.name, mono(param_tys[i]))
  }
  let body_ty = self.infer_block(body, Some(ret), false)
  self.pop_ty()
  match ann {
    Some(_) => self.expect_subtype(body_ty, ret, span)
    None => self.unify(body_ty, ret, span)
  }
  let arrow = TArrow(param_tys, self.apply(ret))
  let last = self.ty_frames.length() - 1
  self.ty_frames[last].remove(name)
  self.bind_scheme(name, self.generalize(arrow))
}

fn Engine::infer_expr(self : Engine, expr : Expr, ret : MType?) -> MType raise MoraError {
  match expr {
    Int(_, _) => TInt
    Float(_, _) => TFloat
    Str(_, _) => TString
    Char(_, _) => TChar
    Bool(_, _) => TBool
    Nil(_) => TNil
    Var(name, span) =>
      match self.lookup_scheme(name) {
        Some(scheme) => self.instantiate(scheme)
        None => raise type_err(span, "unbound variable '" + name + "'")
      }
    Unary(Neg, inner, span) => {
      let ty = self.infer_expr(inner, ret)
      self.mark_numeric(ty) catch {
        _ => raise type_err(span, "unary '-' expects a number\n  actual:   " + self.type_name(ty))
      } noraise {
        _ => ()
      }
      self.resolve(ty)
    }
    Unary(Not, inner, span) => {
      let ty = self.infer_expr(inner, ret)
      self.expect_subtype(ty, TBool, span)
      TBool
    }
    Binary(op, left, right, span) => self.infer_binary(op, left, right, span, ret)
    Call(callee, args, span) => self.infer_call(callee, args, span, ret)
    Method(recv, name, args, span) => self.infer_method(recv, name, args, span, ret)
    Index(recv, index, span) => self.infer_index(recv, index, span, ret)
    List(items, _) => {
      if items.length() == 0 {
        TList(self.fresh())
      } else {
        let mut elem = self.infer_expr(items[0], ret)
        for i in 1..<items.length() {
          let next = self.infer_expr(items[i], ret)
          elem = self.unify_elem(elem, next, items[i].span())
        }
        TList(self.apply(elem))
      }
    }
    Dict(pairs, _) => {
      let value_ty = if pairs.length() == 0 {
        self.fresh()
      } else {
        let (_, first) = pairs[0]
        let mut elem = self.infer_expr(first, ret)
        for i in 1..<pairs.length() {
          let (_, value) = pairs[i]
          let next = self.infer_expr(value, ret)
          elem = self.unify_elem(elem, next, value.span())
        }
        self.apply(elem)
      }
      TDict(TString, value_ty)
    }
    If(cond, then_block, else_block, span) => {
      let cond_ty = self.infer_expr(cond, ret)
      self.expect_subtype(cond_ty, TBool, span)
      match else_block {
        None =>
          raise type_err(span, "if without else cannot be used as a value in the MoonBit trial; add an else branch")
        Some(else_body) => {
          let then_ty = self.infer_block(then_block, ret, true)
          let else_ty = self.infer_block(else_body, ret, true)
          self.unify_elem(then_ty, else_ty, span)
        }
      }
    }
    Match(scrut, arms, span) => self.infer_match(scrut, arms, span, ret)
    Fn(params, body, _) => self.infer_fn(params, body, ret)
    Assign(name, value, span) => {
      match self.lookup_scheme(name) {
        None => raise type_err(span, "unbound variable '" + name + "'")
        Some(scheme) => {
          let actual = self.infer_expr(value, ret)
          self.expect_subtype(actual, self.instantiate(scheme), span)
          TNil
        }
      }
    }
  }
}

fn Engine::unify_elem(self : Engine, left : MType, right : MType, span : Span) -> MType raise MoraError {
  let left = self.resolve(left)
  let right = self.resolve(right)
  if self.is_numeric_type(left) && self.is_numeric_type(right) {
    self.arith(Add, left, right, span)
  } else {
    self.unify(left, right, span)
    self.resolve(left)
  }
}

fn Engine::infer_binary(
  self : Engine,
  op : BinOp,
  left : Expr,
  right : Expr,
  span : Span,
  ret : MType?,
) -> MType raise MoraError {
  match op {
    And | Or => {
      let left_ty = self.infer_expr(left, ret)
      self.expect_subtype(left_ty, TBool, span)
      let right_ty = self.infer_expr(right, ret)
      self.expect_subtype(right_ty, TBool, span)
      TBool
    }
    Eq | Ne => {
      let left_ty = self.infer_expr(left, ret)
      let right_ty = self.infer_expr(right, ret)
      if self.is_numeric_type(left_ty) && self.is_numeric_type(right_ty) {
        ignore(self.arith(Add, left_ty, right_ty, span))
      } else {
        self.unify(left_ty, right_ty, span)
      }
      TBool
    }
    Gt | Lt | Ge | Le => {
      let left_ty = self.infer_expr(left, ret)
      let right_ty = self.infer_expr(right, ret)
      ignore(self.arith(Add, left_ty, right_ty, span))
      TBool
    }
    Pipe => self.infer_pipe(left, right, span, ret)
    other => {
      let left_ty = self.infer_expr(left, ret)
      let right_ty = self.infer_expr(right, ret)
      if is_list(self.resolve(left_ty)) || is_list(self.resolve(right_ty)) {
        self.infer_broadcast(other, left_ty, right_ty, span)
      } else {
        self.arith(other, left_ty, right_ty, span)
      }
    }
  }
}

fn is_list(ty : MType) -> Bool {
  match ty {
    TList(_) => true
    _ => false
  }
}

fn Engine::infer_broadcast(self : Engine, op : BinOp, left : MType, right : MType, span : Span) -> MType raise MoraError {
  match (self.resolve(left), self.resolve(right)) {
    (TList(a), TList(b)) => TList(self.arith(op, a, b, span))
    (TList(a), scalar) => TList(self.arith(op, a, scalar, span))
    (scalar, TList(b)) => TList(self.arith(op, scalar, b, span))
    _ => self.arith(op, left, right, span)
  }
}

fn Engine::infer_pipe(self : Engine, left : Expr, right : Expr, span : Span, ret : MType?) -> MType raise MoraError {
  match right {
    Call(Var(name, _), args, _) => self.infer_method(left, name, args, span, ret)
    _ => {
      let arg = self.infer_expr(left, ret)
      let callee = self.infer_expr(right, ret)
      self.apply_arrow(callee, [arg], span)
    }
  }
}

fn Engine::infer_call(self : Engine, callee : Expr, args : Array[Expr], span : Span, ret : MType?) -> MType raise MoraError {
  match callee {
    Var(name, _) =>
      match self.lookup_scheme(name) {
        Some(scheme) =>
          match scheme.body {
            TBuiltin(builtin) => self.infer_builtin(builtin, args, span, ret)
            _ => {
              let callee_ty = self.instantiate(scheme)
              let arg_tys : Array[MType] = []
              for arg in args {
                arg_tys.push(self.infer_expr(arg, ret))
              }
              self.apply_arrow(callee_ty, arg_tys, span)
            }
          }
        None => {
          let callee_ty = self.infer_expr(callee, ret)
          let arg_tys : Array[MType] = []
          for arg in args {
            arg_tys.push(self.infer_expr(arg, ret))
          }
          self.apply_arrow(callee_ty, arg_tys, span)
        }
      }
    _ => {
      let callee_ty = self.infer_expr(callee, ret)
      let arg_tys : Array[MType] = []
      for arg in args {
        arg_tys.push(self.infer_expr(arg, ret))
      }
      self.apply_arrow(callee_ty, arg_tys, span)
    }
  }
}

fn Engine::apply_arrow(self : Engine, callee : MType, args : Array[MType], span : Span) -> MType raise MoraError {
  match self.resolve(callee) {
    TArrow(params, ret) => {
      if params.length() != args.length() {
        raise type_err(
          span,
          "arity mismatch\n  expected: " + params.length().to_string() + " argument(s)\n  actual:   " + args.length().to_string(),
        )
      }
      for i in 0..<params.length() {
        self.check_arg(args[i], params[i], span)
      }
      self.apply(ret)
    }
    TVar(id) => {
      let result = self.fresh()
      self.bind_var(id, TArrow(args, result), span)
      result
    }
    other => raise type_mismatch(span, "function", self.type_name(other))
  }
}

fn Engine::contains_var(self : Engine, ty : MType) -> Bool {
  match self.resolve(ty) {
    TVar(_) => true
    TList(elem) => self.contains_var(elem)
    TDict(key, value) => self.contains_var(key) || self.contains_var(value)
    TArrow(params, ret) => {
      let mut yes = self.contains_var(ret)
      for param in params {
        if self.contains_var(param) {
          yes = true
        }
      }
      yes
    }
    _ => false
  }
}

fn Engine::check_arg(self : Engine, actual : MType, expected : MType, span : Span) -> Unit raise MoraError {
  if self.contains_var(expected) || self.contains_var(actual) {
    self.unify(actual, expected, span)
  } else {
    self.expect_subtype(actual, expected, span)
  }
}

fn Engine::infer_builtin(self : Engine, name : String, args : Array[Expr], span : Span, ret : MType?) -> MType raise MoraError {
  match name {
    "print" => {
      for arg in args {
        ignore(self.infer_expr(arg, ret))
      }
      TNil
    }
    "str" => {
      if args.length() != 1 {
        raise type_err(span, "str() expects 1 argument")
      }
      ignore(self.infer_expr(args[0], ret))
      TString
    }
    "len" => {
      if args.length() != 1 {
        raise type_err(span, "len() expects 1 argument")
      }
      let ty = self.resolve(self.infer_expr(args[0], ret))
      match ty {
        TString | TList(_) | TDict(_, _) => ()
        TVar(_) => ()
        other => raise type_mismatch(span, "string, list, or dict", self.type_name(other))
      }
      TInt
    }
    "range" => {
      if args.length() < 1 || args.length() > 3 {
        raise type_err(span, "range() expects 1, 2, or 3 numeric arguments")
      }
      for arg in args {
        let ty = self.infer_expr(arg, ret)
        self.mark_numeric(ty) catch {
          _ => raise type_err(arg.span(), "range() arguments must be numbers")
        } noraise {
          _ => ()
        }
      }
      TList(TInt)
    }
    _ => raise type_err(span, "unknown builtin '" + name + "'")
  }
}

fn Engine::infer_method(
  self : Engine,
  recv : Expr,
  name : String,
  args : Array[Expr],
  span : Span,
  ret : MType?,
) -> MType raise MoraError {
  let recv_ty = self.resolve(self.infer_expr(recv, ret))
  let arg_tys : Array[MType] = []
  for arg in args {
    arg_tys.push(self.infer_expr(arg, ret))
  }
  match recv_ty {
    TVar(id) =>
      match name {
        "map" | "filter" | "reduce" | "get" | "push" | "len" => {
          let elem = self.fresh()
          self.bind_var(id, TList(elem), span)
          self.type_list_method(elem, name, arg_tys, span)
        }
        "upper" | "lower" | "trim" | "split" | "contains" | "replace" | "starts_with" | "ends_with" => {
          self.bind_var(id, TString, span)
          self.type_string_method(name, arg_tys, span)
        }
        _ => raise type_err(span, "method '" + name + "' needs a known receiver type")
      }
    TList(elem) => self.type_list_method(elem, name, arg_tys, span)
    TString => self.type_string_method(name, arg_tys, span)
    TDict(_, value) => self.type_dict_method(value, name, arg_tys, span)
    other => raise type_err(span, "no method '" + name + "' on " + self.type_name(other))
  }
}

fn Engine::type_list_method(self : Engine, elem : MType, name : String, args : Array[MType], span : Span) -> MType raise MoraError {
  match name {
    "map" => {
      self.expect_arity(span, "map", 1, args.length())
      let result = self.fresh()
      self.check_arg(args[0], TArrow([elem], result), span)
      TList(self.apply(result))
    }
    "filter" => {
      self.expect_arity(span, "filter", 1, args.length())
      self.check_arg(args[0], TArrow([elem], TBool), span)
      TList(elem)
    }
    "reduce" => {
      self.expect_arity(span, "reduce", 2, args.length())
      let acc = args[1]
      self.check_arg(args[0], TArrow([acc, elem], acc), span)
      self.apply(acc)
    }
    "len" => {
      self.expect_arity(span, "len", 0, args.length())
      TInt
    }
    "get" => {
      self.expect_arity(span, "get", 1, args.length())
      self.expect_subtype(args[0], TInt, span)
      elem
    }
    "push" => {
      self.expect_arity(span, "push", 1, args.length())
      self.expect_subtype(args[0], elem, span)
      TList(elem)
    }
    _ => raise type_err(span, "list has no method '" + name + "' in the MoonBit trial")
  }
}

fn Engine::type_string_method(self : Engine, name : String, args : Array[MType], span : Span) -> MType raise MoraError {
  match name {
    "len" => {
      self.expect_arity(span, "len", 0, args.length())
      TInt
    }
    "upper" | "lower" | "trim" => {
      self.expect_arity(span, name, 0, args.length())
      TString
    }
    "contains" | "starts_with" | "ends_with" => {
      self.expect_arity(span, name, 1, args.length())
      self.expect_subtype(args[0], TString, span)
      TBool
    }
    "split" => {
      self.expect_arity(span, "split", 1, args.length())
      self.expect_subtype(args[0], TString, span)
      TList(TString)
    }
    "replace" => {
      self.expect_arity(span, "replace", 2, args.length())
      self.expect_subtype(args[0], TString, span)
      self.expect_subtype(args[1], TString, span)
      TString
    }
    _ => raise type_err(span, "string has no method '" + name + "' in the MoonBit trial")
  }
}

fn Engine::type_dict_method(self : Engine, value : MType, name : String, args : Array[MType], span : Span) -> MType raise MoraError {
  match name {
    "get" => {
      self.expect_arity(span, "get", 1, args.length())
      self.expect_subtype(args[0], TString, span)
      value
    }
    "set" => {
      self.expect_arity(span, "set", 2, args.length())
      self.expect_subtype(args[0], TString, span)
      self.expect_subtype(args[1], value, span)
      TDict(TString, value)
    }
    "keys" => {
      self.expect_arity(span, "keys", 0, args.length())
      TList(TString)
    }
    "values" => {
      self.expect_arity(span, "values", 0, args.length())
      TList(value)
    }
    "len" => {
      self.expect_arity(span, "len", 0, args.length())
      TInt
    }
    _ => raise type_err(span, "dict has no method '" + name + "' in the MoonBit trial")
  }
}

fn Engine::expect_arity(self : Engine, span : Span, name : String, expected : Int, actual : Int) -> Unit raise MoraError {
  if expected != actual {
    raise type_err(
      span,
      name + "() arity mismatch\n  expected: " + expected.to_string() + "\n  actual:   " + actual.to_string(),
    )
  }
}

fn Engine::infer_index(self : Engine, recv : Expr, index : Expr, span : Span, ret : MType?) -> MType raise MoraError {
  let recv_ty = self.resolve(self.infer_expr(recv, ret))
  let index_ty = self.infer_expr(index, ret)
  match recv_ty {
    TList(elem) => {
      self.expect_subtype(index_ty, TInt, span)
      elem
    }
    TString => {
      self.expect_subtype(index_ty, TInt, span)
      TChar
    }
    TDict(key, value) => {
      self.expect_subtype(index_ty, key, span)
      value
    }
    TVar(id) => {
      let elem = self.fresh()
      self.bind_var(id, TList(elem), span)
      self.expect_subtype(index_ty, TInt, span)
      elem
    }
    other => raise type_mismatch(span, "list, string, or dict", self.type_name(other))
  }
}

fn Engine::infer_fn(self : Engine, params : Array[Param], body : Block, _ret : MType?) -> MType raise MoraError {
  let param_tys : Array[MType] = []
  self.push_ty()
  for param in params {
    let ty = match param.ann {
      Some(annotation) => ann_type(annotation)
      None => self.fresh()
    }
    param_tys.push(ty)
    self.bind_scheme(param.name, mono(ty))
  }
  let result = self.fresh()
  let body_ty = self.infer_block(body, Some(result), false)
  self.pop_ty()
  self.unify(body_ty, result, Span::make(1, 1))
  TArrow(param_tys, self.apply(result))
}

fn Engine::infer_match(self : Engine, scrut : Expr, arms : Array[Arm], span : Span, ret : MType?) -> MType raise MoraError {
  if arms.length() == 0 {
    raise type_err(span, "match needs at least one arm")
  }
  let scrut_ty = self.infer_expr(scrut, ret)
  let mut result : MType? = None
  for arm in arms {
    self.push_ty()
    self.bind_pat(arm.pat, scrut_ty, arm.span)
    match arm.guard_expr {
      Some(guard_expr) => {
        let guard_ty = self.infer_expr(guard_expr, ret)
        self.expect_subtype(guard_ty, TBool, guard_expr.span())
      }
      None => ()
    }
    let body_ty = self.infer_expr(arm.body, ret)
    self.pop_ty()
    result = Some(match result {
      None => body_ty
      Some(prev) => self.unify_elem(prev, body_ty, arm.span)
    })
  }
  match result {
    Some(ty) => self.apply(ty)
    None => TNil
  }
}

fn Engine::bind_pat(self : Engine, pat : Pat, scrut : MType, span : Span) -> Unit raise MoraError {
  match pat {
    Wild => ()
    Bind(name) => self.bind_scheme(name, mono(self.apply(scrut)))
    LitInt(_) => self.expect_subtype(TInt, scrut, span) catch {
      _ => self.unify(scrut, TInt, span)
    } noraise {
      _ => ()
    }
    LitFloat(_) => self.unify(scrut, TFloat, span) catch {
      _ =>
        if !(self.subtype(TFloat, scrut)) {
          raise type_mismatch(span, self.type_name(scrut), "float")
        }
    } noraise {
      _ => ()
    }
    LitStr(_) => self.unify(scrut, TString, span)
    LitBool(_) => self.unify(scrut, TBool, span)
    LitNil => self.unify(scrut, TNil, span)
    LitChar(_) => self.unify(scrut, TChar, span)
  }
}
