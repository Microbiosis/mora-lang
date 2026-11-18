// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

priv enum Flow {
  Val(Value)
  Ret(Value)
  Brk
  Cont
}

fn rt(message : String) -> MoraError {
  RuntimeErr("Runtime error: " + message)
}

fn Engine::push_frame(self : Engine) -> Unit {
  self.frames.push(Map([]))
}

fn Engine::pop_frame(self : Engine) -> Unit {
  ignore(self.frames.pop())
}

fn Engine::bind_value(self : Engine, name : String, value : Value) -> Unit {
  let last = self.frames.length() - 1
  self.frames[last].set(name, value)
}

fn Engine::lookup(self : Engine, name : String, span : Span) -> Value raise MoraError {
  let mut i = self.frames.length() - 1
  while i >= 0 {
    match self.frames[i].get(name) {
      Some(value) => return value
      None => i = i - 1
    }
  }
  raise rt("unbound variable '" + name + "' at " + span.locus())
}

fn Engine::assign(self : Engine, name : String, value : Value, span : Span) -> Unit raise MoraError {
  let mut i = self.frames.length() - 1
  while i >= 0 {
    if self.frames[i].contains(name) {
      self.frames[i].set(name, value)
      return
    }
    i = i - 1
  }
  raise rt("unbound variable '" + name + "' at " + span.locus())
}

fn Engine::eval_program(self : Engine, prog : Program) -> Value raise MoraError {
  let mut value = VNil
  for stmt in prog.stmts {
    match self.eval_stmt(stmt) {
      Val(next) => value = next
      Ret(_) => raise rt("'return' outside of a task or closure")
      Brk => raise rt("'break' outside of a loop")
      Cont => raise rt("'continue' outside of a loop")
    }
  }
  value
}

fn Engine::eval_block(self : Engine, block : Block, scoped : Bool) -> Flow raise MoraError {
  if scoped {
    self.push_frame()
  }
  let mut flow = Val(VNil)
  let mut failed : MoraError? = None
  for stmt in block.stmts {
    try {
      flow = self.eval_stmt(stmt)
    } catch {
      err => {
        failed = Some(err)
        break
      }
    }
    match flow {
      Val(_) => ()
      _ => break
    }
  }
  if scoped {
    self.pop_frame()
  }
  match failed {
    Some(err) => raise err
    None => flow
  }
}

fn Engine::eval_stmt(self : Engine, stmt : Stmt) -> Flow raise MoraError {
  match stmt {
    Let(name, _, value, _) =>
      match self.eval_expr(value) {
        Val(got) => {
          self.bind_value(name, got)
          Val(VNil)
        }
        other => other
      }
    Task(name, params, _, body, _) => {
      let names : Array[String] = []
      for param in params {
        names.push(param.name)
      }
      let func : Func = {
        name,
        params: names,
        body,
        env: clone_frames(self.frames),
        is_task: true,
      }
      self.bind_value(name, VFunc(func))
      Val(VNil)
    }
    Return(value, _) =>
      match value {
        None => Ret(VNil)
        Some(expr) =>
          match self.eval_expr(expr) {
            Val(got) => Ret(got)
            other => other
          }
      }
    For(name, iter, body, span) => self.eval_for(name, iter, body, span)
    While(cond, body, _) => self.eval_while(cond, body)
    Break(_) => Brk
    Continue(_) => Cont
    Expr(expr) => self.eval_expr(expr)
  }
}

fn Engine::eval_for(self : Engine, name : String, iter : Expr, body : Block, span : Span) -> Flow raise MoraError {
  match self.eval_expr(iter) {
    Val(VList(items)) => {
      for item in items {
        self.push_frame()
        self.bind_value(name, item)
        errdefer self.pop_frame()
        let flow = self.eval_block(body, false)
        self.pop_frame()
        match flow {
          Val(_) | Cont => ()
          Brk => return Val(VNil)
          Ret(value) => return Ret(value)
        }
      }
      Val(VNil)
    }
    Val(other) => raise rt("for loop at " + span.locus() + " expects a list, got " + show_value(other))
    other => other
  }
}

fn Engine::eval_while(self : Engine, cond : Expr, body : Block) -> Flow raise MoraError {
  while true {
    match self.eval_expr(cond) {
      Val(value) =>
        if !(is_truthy(value)) {
          break
        }
      other => return other
    }
    match self.eval_block(body, true) {
      Val(_) | Cont => ()
      Brk => return Val(VNil)
      Ret(value) => return Ret(value)
    }
  }
  Val(VNil)
}

fn Engine::eval_expr(self : Engine, expr : Expr) -> Flow raise MoraError {
  match expr {
    Int(n, _) => Val(VInt(n))
    Float(n, _) => Val(VFloat(n))
    Str(text, _) => Val(VStr(text))
    Char(c, _) => Val(VChar(c))
    Bool(flag, _) => Val(VBool(flag))
    Nil(_) => Val(VNil)
    Var(name, span) => Val(self.lookup(name, span))
    Unary(Neg, inner, span) =>
      match self.eval_expr(inner) {
        Val(value) => Val(self.eval_neg(value, span))
        other => other
      }
    Unary(Not, inner, span) =>
      match self.eval_expr(inner) {
        Val(VBool(flag)) => Val(VBool(!(flag)))
        Val(other) => raise rt("unary 'not' at " + span.locus() + " expects bool, got " + show_value(other))
        other => other
      }
    Binary(op, left, right, span) => self.eval_binary(op, left, right, span)
    Call(callee, args, span) => self.eval_call(callee, args, span)
    Method(recv, name, args, span) => self.eval_method(recv, name, args, span)
    Index(recv, index, span) => self.eval_index_expr(recv, index, span)
    List(items, _) => {
      let values : Array[Value] = []
      for item in items {
        match self.eval_expr(item) {
          Val(value) => values.push(value)
          other => return other
        }
      }
      Val(VList(values))
    }
    Dict(pairs, _) => {
      let map : Map[String, Value] = Map([])
      for pair in pairs {
        let (key, value) = pair
        match self.eval_expr(value) {
          Val(got) => map.set(key, got)
          other => return other
        }
      }
      Val(VDict(map))
    }
    If(cond, then_block, else_block, _) => self.eval_if(cond, then_block, else_block)
    Match(scrut, arms, span) =>
      match self.eval_expr(scrut) {
        Val(value) => self.eval_match(value, arms, span)
        other => other
      }
    Fn(params, body, _) => {
      let names : Array[String] = []
      for param in params {
        names.push(param.name)
      }
      Val(VFunc({
        name: "",
        params: names,
        body,
        env: clone_frames(self.frames),
        is_task: false,
      }))
    }
    Assign(name, value, span) =>
      match self.eval_expr(value) {
        Val(got) => {
          self.assign(name, got, span)
          Val(VNil)
        }
        other => other
      }
  }
}

fn Engine::eval_if(self : Engine, cond : Expr, then_block : Block, else_block : Block?) -> Flow raise MoraError {
  match self.eval_expr(cond) {
    Val(value) =>
      if is_truthy(value) {
        match self.eval_block(then_block, true) {
          Val(value) =>
            match else_block {
              None => Val(VNil)
              Some(_) => Val(value)
            }
          other => other
        }
      } else {
        match else_block {
          Some(body) => self.eval_block(body, true)
          None => Val(VNil)
        }
      }
    other => other
  }
}

fn Engine::eval_neg(self : Engine, value : Value, span : Span) -> Value raise MoraError {
  match value {
    VInt(n) => VInt(checked_sub(0L, n))
    VFloat(n) => VFloat(0.0 - n)
    _ => raise rt("unary '-' at " + span.locus() + " expects a number, got " + show_value(value))
  }
}

fn Engine::eval_binary(self : Engine, op : BinOp, left : Expr, right : Expr, span : Span) -> Flow raise MoraError {
  match op {
    And =>
      match self.eval_expr(left) {
        Val(value) =>
          if !(is_truthy(value)) {
            Val(VBool(false))
          } else {
            match self.eval_expr(right) {
              Val(next) => Val(VBool(is_truthy(next)))
              other => other
            }
          }
        other => other
      }
    Or =>
      match self.eval_expr(left) {
        Val(value) =>
          if is_truthy(value) {
            Val(VBool(true))
          } else {
            match self.eval_expr(right) {
              Val(next) => Val(VBool(is_truthy(next)))
              other => other
            }
          }
        other => other
      }
    Pipe =>
      match right {
        Call(Var(name, _), args, call_span) => self.eval_method(left, name, args, call_span)
        _ =>
          match self.eval_expr(left) {
            Val(arg) =>
              match self.eval_expr(right) {
                Val(func) => Val(self.call_value(func, [arg], span))
                other => other
              }
            other => other
          }
      }
    _ =>
      match self.eval_expr(left) {
        Val(lhs) =>
          match self.eval_expr(right) {
            Val(rhs) => Val(self.apply_op(op, lhs, rhs, span))
            other => other
          }
        other => other
      }
  }
}

fn Engine::apply_op(self : Engine, op : BinOp, left : Value, right : Value, span : Span) -> Value raise MoraError {
  match op {
    Add => self.apply_add(left, right, span)
    Sub | Mul | Div | Mod | Pow => self.apply_num(op, left, right, span)
    Eq => VBool(values_equal(left, right))
    Ne => VBool(!(values_equal(left, right)))
    Gt | Lt | Ge | Le => VBool(self.apply_cmp(op, left, right, span))
    And | Or | Pipe => raise rt("internal: operator dispatched twice")
  }
}

fn Engine::apply_add(self : Engine, left : Value, right : Value, span : Span) -> Value raise MoraError {
  match (left, right) {
    (VStr(a), _) => VStr(a + show_value(right))
    (_, VStr(b)) => VStr(show_value(left) + b)
    (VList(a), VList(b)) =>
      if a.length() == b.length() {
        let items : Array[Value] = []
        for i in 0..<a.length() {
          items.push(try self.apply_add(a[i], b[i], span) catch {
            _ => VNil
          } noraise {
            value => value
          })
        }
        VList(items)
      } else {
        let items : Array[Value] = []
        for item in a {
          items.push(item)
        }
        for item in b {
          items.push(item)
        }
        VList(items)
      }
    (VList(items), scalar) => self.broadcast(Add, items, scalar, false, span)
    (scalar, VList(items)) => self.broadcast(Add, items, scalar, true, span)
    _ => self.num_scalar(Add, left, right, span)
  }
}

fn Engine::apply_num(self : Engine, op : BinOp, left : Value, right : Value, span : Span) -> Value raise MoraError {
  match (left, right) {
    (VList(a), VList(b)) => {
      if a.length() != b.length() {
        raise rt("list length mismatch: " + a.length().to_string() + " vs " + b.length().to_string())
      }
      let items : Array[Value] = []
      for i in 0..<a.length() {
        items.push(try self.apply_num(op, a[i], b[i], span) catch {
          _ => VNil
        } noraise {
          value => value
        })
      }
      VList(items)
    }
    (VList(items), scalar) => self.broadcast(op, items, scalar, false, span)
    (scalar, VList(items)) => self.broadcast(op, items, scalar, true, span)
    _ => self.num_scalar(op, left, right, span)
  }
}

fn Engine::broadcast(
  self : Engine,
  op : BinOp,
  items : Array[Value],
  scalar : Value,
  scalar_left : Bool,
  span : Span,
) -> Value raise MoraError {
  let out : Array[Value] = []
  for item in items {
    let got = try {
      if scalar_left {
        self.num_scalar(op, scalar, item, span)
      } else {
        self.num_scalar(op, item, scalar, span)
      }
    } catch {
      _ => VNil
    } noraise {
      value => value
    }
    out.push(got)
  }
  VList(out)
}

fn Engine::num_scalar(self : Engine, op : BinOp, left : Value, right : Value, span : Span) -> Value raise MoraError {
  match (left, right) {
    (VInt(a), VInt(b)) => VInt(self.int_op(op, a, b))
    (VFloat(a), VFloat(b)) => VFloat(self.float_op(op, a, b))
    (VInt(a), VFloat(b)) => {
      if !(fits_f64(a)) {
        raise rt("integer " + a.to_string() + " is not exactly representable as float")
      }
      VFloat(self.float_op(op, a.to_double(), b))
    }
    (VFloat(a), VInt(b)) => {
      if !(fits_f64(b)) {
        raise rt("integer " + b.to_string() + " is not exactly representable as float")
      }
      VFloat(self.float_op(op, a, b.to_double()))
    }
    _ => raise rt("operands must be numbers at " + span.locus() + ", got " + show_value(left) + " and " + show_value(right))
  }
}

fn Engine::int_op(self : Engine, op : BinOp, a : Int64, b : Int64) -> Int64 raise MoraError {
  match op {
    Add => checked_add(a, b)
    Sub => checked_sub(a, b)
    Mul => checked_mul(a, b)
    Div => checked_div(a, b)
    Mod => checked_rem(a, b)
    Pow => checked_pow(a, b)
    _ => raise rt("not an integer arithmetic operator")
  }
}

fn Engine::float_op(self : Engine, op : BinOp, a : Double, b : Double) -> Double raise MoraError {
  match op {
    Add => a + b
    Sub => a - b
    Mul => a * b
    Div => a / b
    Mod =>
      if b == 0.0 {
        0.0 / 0.0
      } else {
        a - (a / b).trunc() * b
      }
    Pow => @math.pow(a, b)
    _ => raise rt("not a float arithmetic operator")
  }
}

fn Engine::apply_cmp(self : Engine, op : BinOp, left : Value, right : Value, span : Span) -> Bool raise MoraError {
  let ord = match (left, right) {
    (VInt(a), VInt(b)) => Some(cmp_int(a, b))
    (VFloat(a), VFloat(b)) => cmp_float(a, b)
    (VInt(a), VFloat(b)) => {
      if !(fits_f64(a)) {
        raise rt("integer " + a.to_string() + " is not exactly representable as float")
      }
      cmp_float(a.to_double(), b)
    }
    (VFloat(a), VInt(b)) => {
      if !(fits_f64(b)) {
        raise rt("integer " + b.to_string() + " is not exactly representable as float")
      }
      cmp_float(a, b.to_double())
    }
    _ => raise rt("operands must be numbers at " + span.locus())
  }
  match ord {
    None => false
    Some(order) =>
      match op {
        Gt => order > 0
        Lt => order < 0
        Ge => order >= 0
        Le => order <= 0
        _ => false
      }
  }
}

fn cmp_int(a : Int64, b : Int64) -> Int {
  if a < b {
    -1
  } else if a > b {
    1
  } else {
    0
  }
}

fn cmp_float(a : Double, b : Double) -> Int? {
  if a.is_nan() || b.is_nan() {
    None
  } else if a < b {
    Some(-1)
  } else if a > b {
    Some(1)
  } else {
    Some(0)
  }
}

fn Engine::eval_call(self : Engine, callee : Expr, args : Array[Expr], span : Span) -> Flow raise MoraError {
  match self.eval_expr(callee) {
    Val(func) => {
      let values : Array[Value] = []
      for arg in args {
        match self.eval_expr(arg) {
          Val(value) => values.push(value)
          other => return other
        }
      }
      Val(self.call_value(func, values, span))
    }
    other => other
  }
}

fn Engine::call_value(self : Engine, func : Value, args : Array[Value], span : Span) -> Value raise MoraError {
  match func {
    VBuiltin(name) => self.call_builtin(name, args, span)
    VFunc(closure) => {
      if closure.params.length() != args.length() {
        raise rt(
          "arity mismatch at " + span.locus() + ": expected " + closure.params.length().to_string() + ", got " + args.length().to_string(),
        )
      }
      let saved = self.frames
      let frames = clone_frames(closure.env)
      let params_frame : Map[String, Value] = Map([])
      for i in 0..<closure.params.length() {
        params_frame.set(closure.params[i], args[i])
      }
      frames.push(params_frame)
      errdefer { self.frames = saved }
      self.frames = frames
      let flow = self.eval_block(closure.body, false)
      self.frames = saved
      match flow {
        Val(value) | Ret(value) => value
        Brk => raise rt("'break' outside of a loop")
        Cont => raise rt("'continue' outside of a loop")
      }
    }
    other => raise rt("cannot call " + show_value(other) + " at " + span.locus())
  }
}

fn Engine::call_builtin(self : Engine, name : String, args : Array[Value], span : Span) -> Value raise MoraError {
  match name {
    "print" => {
      let buf = StringBuilder()
      for i, arg in args {
        if i > 0 {
          buf.write_string("\t")
        }
        buf.write_string(show_value(arg))
      }
      self.prints.push(buf.to_string())
      VNil
    }
    "str" => {
      if args.length() != 1 {
        raise rt("str() expects 1 argument at " + span.locus())
      }
      VStr(show_value(args[0]))
    }
    "len" => {
      if args.length() != 1 {
        raise rt("len() expects 1 argument at " + span.locus())
      }
      match args[0] {
        VStr(text) => VInt(char_len(text))
        VList(items) => VInt(items.length().to_int64())
        VDict(map) => VInt(map.length().to_int64())
        other => raise rt("len() expects a string, list, or dict, got " + show_value(other))
      }
    }
    "range" => self.builtin_range(args, span)
    _ => raise rt("unknown builtin '" + name + "'")
  }
}

fn Engine::builtin_range(self : Engine, args : Array[Value], span : Span) -> Value raise MoraError {
  let (start, end, step) = match args.length() {
    1 => (0L, as_int(args[0], "range() end"), 1L)
    2 => (as_int(args[0], "range() start"), as_int(args[1], "range() end"), 1L)
    3 => (as_int(args[0], "range() start"), as_int(args[1], "range() end"), as_int(args[2], "range() step"))
    _ => raise rt("range() expects 1, 2, or 3 arguments at " + span.locus())
  }
  if step == 0L {
    raise rt("range() step must not be 0 (would never terminate)")
  }
  let items : Array[Value] = []
  if step > 0L {
    let mut i = start
    while i < end {
      items.push(VInt(i))
      i = checked_add(i, step)
    }
  } else {
    let mut i = start
    while i > end {
      items.push(VInt(i))
      i = checked_add(i, step)
    }
  }
  VList(items)
}

fn Engine::eval_method(self : Engine, recv : Expr, name : String, args : Array[Expr], span : Span) -> Flow raise MoraError {
  match self.eval_expr(recv) {
    Val(target) => {
      let values : Array[Value] = []
      for arg in args {
        match self.eval_expr(arg) {
          Val(value) => values.push(value)
          other => return other
        }
      }
      Val(self.call_method(target, name, values, span))
    }
    other => other
  }
}

fn Engine::call_method(self : Engine, target : Value, name : String, args : Array[Value], span : Span) -> Value raise MoraError {
  match target {
    VList(items) => self.method_list(items, name, args, span)
    VStr(text) => self.method_string(text, name, args, span)
    VDict(map) => self.method_dict(map, name, args, span)
    other => raise rt("cannot call method '" + name + "' on " + show_value(other))
  }
}

fn Engine::method_list(self : Engine, items : Array[Value], name : String, args : Array[Value], span : Span) -> Value raise MoraError {
  match name {
    "map" => {
      let mapper = expect_arg(args, 0, "map() requires a function")
      let out : Array[Value] = []
      for item in items {
        out.push(self.call_value(mapper, [item], span))
      }
      VList(out)
    }
    "filter" => {
      let pred = expect_arg(args, 0, "filter() requires a function")
      let out : Array[Value] = []
      for item in items {
        match self.call_value(pred, [item], span) {
          VBool(true) => out.push(item)
          VBool(false) => ()
          other => raise rt("filter predicate must return bool, got " + show_value(other))
        }
      }
      VList(out)
    }
    "reduce" => {
      let reducer = expect_arg(args, 0, "reduce() requires a function")
      if args.length() < 2 {
        raise rt("reduce() requires an initial value as the second argument")
      }
      let mut acc = args[1]
      for item in items {
        acc = self.call_value(reducer, [acc, item], span)
      }
      acc
    }
    "len" => VInt(items.length().to_int64())
    "get" => {
      let index = as_int(expect_arg(args, 0, "List.get requires an index"), "List.get index")
      if index < 0L {
        raise rt("List.get: index must be non-negative")
      }
      let i = index.to_int()
      if i < 0 || i >= items.length() {
        raise rt("index " + index.to_string() + " out of bounds (len " + items.length().to_string() + ")")
      }
      items[i]
    }
    "push" => {
      let item = if args.length() == 0 { VNil } else { args[0] }
      let out : Array[Value] = []
      for existing in items {
        out.push(existing)
      }
      out.push(item)
      VList(out)
    }
    _ => raise rt("list has no method '" + name + "' at " + span.locus())
  }
}

fn Engine::method_string(self : Engine, text : String, name : String, args : Array[Value], span : Span) -> Value raise MoraError {
  match name {
    "len" => VInt(char_len(text))
    "upper" => VStr(text.to_upper().to_string())
    "lower" => VStr(text.to_lower().to_string())
    "trim" => VStr(text.trim().to_owned())
    "contains" => VBool(text.contains(arg_text(args, 0)))
    "starts_with" => VBool(text.has_prefix(arg_text(args, 0)))
    "ends_with" => VBool(text.has_suffix(arg_text(args, 0)))
    "split" => {
      let sep = arg_text(args, 0)
      let parts : Array[Value] = []
      for part in text.split(sep) {
        parts.push(VStr(part.to_owned()))
      }
      VList(parts)
    }
    "replace" => {
      let from = arg_text(args, 0)
      let to = arg_text(args, 1)
      VStr(text.replace_all(old=from, new=to))
    }
    _ => raise rt("string has no method '" + name + "' at " + span.locus())
  }
}

fn Engine::method_dict(self : Engine, map : Map[String, Value], name : String, args : Array[Value], span : Span) -> Value raise MoraError {
  match name {
    "get" => {
      let key = arg_text(args, 0)
      match map.get(key) {
        Some(value) => value
        None => VNil
      }
    }
    "set" => {
      let key = arg_text(args, 0)
      let value = if args.length() < 2 { VNil } else { args[1] }
      let next = map.copy()
      next.set(key, value)
      VDict(next)
    }
    "keys" => {
      let keys : Array[String] = []
      for key in map.keys() {
        keys.push(key)
      }
      sort_strings(keys)
      let items : Array[Value] = []
      for key in keys {
        items.push(VStr(key))
      }
      VList(items)
    }
    "values" => {
      let keys : Array[String] = []
      for key in map.keys() {
        keys.push(key)
      }
      sort_strings(keys)
      let items : Array[Value] = []
      for key in keys {
        match map.get(key) {
          Some(value) => items.push(value)
          None => ()
        }
      }
      VList(items)
    }
    "len" => VInt(map.length().to_int64())
    _ => raise rt("dict has no method '" + name + "' at " + span.locus())
  }
}

fn expect_arg(args : Array[Value], index : Int, message : String) -> Value raise MoraError {
  if index < 0 || index >= args.length() {
    raise rt(message)
  }
  args[index]
}

fn arg_text(args : Array[Value], index : Int) -> String {
  if index < 0 || index >= args.length() {
    ""
  } else {
    show_value(args[index])
  }
}

fn Engine::eval_index_expr(self : Engine, recv : Expr, index : Expr, span : Span) -> Flow raise MoraError {
  match self.eval_expr(recv) {
    Val(target) =>
      match self.eval_expr(index) {
        Val(idx) => Val(self.eval_index(target, idx, span))
        other => other
      }
    other => other
  }
}

fn Engine::eval_index(self : Engine, target : Value, index : Value, span : Span) -> Value raise MoraError {
  match target {
    VList(items) => self.method_list(items, "get", [index], span)
    VDict(map) => self.method_dict(map, "get", [index], span)
    VStr(text) => {
      let at = as_int(index, "string index")
      if at < 0L {
        raise rt("string index must be non-negative")
      }
      match char_at(text, at.to_int()) {
        Some(ch) => VChar(ch)
        None => raise rt("string index " + at.to_string() + " out of bounds (len " + char_len(text).to_string() + ")")
      }
    }
    other => raise rt("cannot index " + show_value(other) + " at " + span.locus())
  }
}

fn Engine::eval_match(self : Engine, scrut : Value, arms : Array[Arm], span : Span) -> Flow raise MoraError {
  for arm in arms {
    if self.pat_matches(arm.pat, scrut) {
      self.push_frame()
      self.bind_pat_value(arm.pat, scrut)
      let guard_ok = match arm.guard_expr {
        None => true
        Some(guard_expr) =>
          match self.eval_expr(guard_expr) {
            Val(value) => is_truthy(value)
            other => {
              self.pop_frame()
              return other
            }
          }
      }
      if guard_ok {
        errdefer self.pop_frame()
        let flow = self.eval_expr(arm.body)
        self.pop_frame()
        return flow
      }
      self.pop_frame()
    }
  }
  raise rt("non-exhaustive match at " + span.locus())
}

fn Engine::pat_matches(self : Engine, pat : Pat, value : Value) -> Bool {
  match pat {
    Wild | Bind(_) => true
    LitInt(n) =>
      match value {
        VInt(got) => got == n
        _ => false
      }
    LitFloat(n) =>
      match value {
        VFloat(got) => got == n
        _ => false
      }
    LitStr(text) =>
      match value {
        VStr(got) => got == text
        _ => false
      }
    LitBool(flag) =>
      match value {
        VBool(got) => got == flag
        _ => false
      }
    LitNil =>
      match value {
        VNil => true
        _ => false
      }
    LitChar(ch) =>
      match value {
        VChar(got) => got == ch
        _ => false
      }
  }
}

fn Engine::bind_pat_value(self : Engine, pat : Pat, value : Value) -> Unit {
  match pat {
    Bind(name) => self.bind_value(name, value)
    _ => ()
  }
}
