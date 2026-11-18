// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

priv struct Func {
  name : String
  params : Array[String]
  body : Block
  env : Array[Map[String, Value]]
  is_task : Bool
}

priv enum Value {
  VInt(Int64)
  VFloat(Double)
  VBool(Bool)
  VStr(String)
  VChar(Char)
  VNil
  VList(Array[Value])
  VDict(Map[String, Value])
  VFunc(Func)
  VBuiltin(String)
}

fn clone_frames(frames : Array[Map[String, Value]]) -> Array[Map[String, Value]] {
  let out : Array[Map[String, Value]] = []
  for frame in frames {
    out.push(frame)
  }
  out
}

fn fits_f64(n : Int64) -> Bool {
  if n >= -9007199254740992L && n <= 9007199254740992L {
    true
  } else {
    let d = n.to_double()
    d.is_finite() && d == d.trunc() && d.to_int64() == n
  }
}

fn overflow(op : String, a : Int64, b : Int64) -> MoraError {
  RuntimeErr(
    "integer overflow in " + op + ": " + a.to_string() + " " + op + " " + b.to_string() + " (exceeds i64). std/math/bigint is not ported",
  )
}

fn checked_add(a : Int64, b : Int64) -> Int64 raise MoraError {
  if b > 0L && a > @int64.MAX_VALUE - b {
    raise overflow("Add", a, b)
  }
  if b < 0L && a < @int64.MIN_VALUE - b {
    raise overflow("Add", a, b)
  }
  a + b
}

fn checked_sub(a : Int64, b : Int64) -> Int64 raise MoraError {
  if b > 0L && a < @int64.MIN_VALUE + b {
    raise overflow("Sub", a, b)
  }
  if b < 0L && a > @int64.MAX_VALUE + b {
    raise overflow("Sub", a, b)
  }
  a - b
}

fn checked_mul(a : Int64, b : Int64) -> Int64 raise MoraError {
  if a == 0L || b == 0L {
    return 0L
  }
  if (a == @int64.MIN_VALUE && b == -1L) || (b == @int64.MIN_VALUE && a == -1L) {
    raise overflow("Mul", a, b)
  }
  if a > 0L && b > 0L && a > @int64.MAX_VALUE / b {
    raise overflow("Mul", a, b)
  }
  if a > 0L && b < 0L && b < @int64.MIN_VALUE / a {
    raise overflow("Mul", a, b)
  }
  if a < 0L && b > 0L && a < @int64.MIN_VALUE / b {
    raise overflow("Mul", a, b)
  }
  if a < 0L && b < 0L && a < @int64.MAX_VALUE / b {
    raise overflow("Mul", a, b)
  }
  a * b
}

fn checked_div(a : Int64, b : Int64) -> Int64 raise MoraError {
  if b == 0L {
    raise RuntimeErr("division by zero")
  }
  if a == @int64.MIN_VALUE && b == -1L {
    raise overflow("Div", a, b)
  }
  a / b
}

fn checked_rem(a : Int64, b : Int64) -> Int64 raise MoraError {
  if b == 0L {
    raise RuntimeErr("modulo by zero")
  }
  if a == @int64.MIN_VALUE && b == -1L {
    raise overflow("Mod", a, b)
  }
  a % b
}

fn checked_pow(a : Int64, b : Int64) -> Int64 raise MoraError {
  if b < 0L {
    raise RuntimeErr(
      "negative integer exponent `" + b.to_string() + "` is invalid for int ** int; use a float base (e.g. `2.0 ** " + b.to_string() + "`)",
    )
  }
  let mut result = 1L
  let mut base = a
  let mut exp = b
  while exp > 0L {
    if exp % 2L == 1L {
      result = checked_mul(result, base)
    }
    exp = exp / 2L
    if exp > 0L {
      base = checked_mul(base, base)
    }
  }
  result
}

fn show_float(n : Double) -> String {
  if n.is_nan() {
    "nan"
  } else if n.is_pos_inf() {
    "inf"
  } else if n.is_neg_inf() {
    "-inf"
  } else {
    let text = n.to_string()
    if text.contains(".") || text.contains("e") || text.contains("E") {
      text
    } else {
      text + ".0"
    }
  }
}

fn show_value(value : Value) -> String {
  let buf = StringBuilder()
  write_value(buf, value, 0)
  buf.to_string()
}

fn write_value(buf : StringBuilder, value : Value, depth : Int) -> Unit {
  if depth > 16 {
    buf.write_string("…")
    return
  }
  match value {
    VInt(n) => buf.write_string(n.to_string())
    VFloat(n) => buf.write_string(show_float(n))
    VBool(true) => buf.write_string("true")
    VBool(false) => buf.write_string("false")
    VStr(s) => buf.write_string(s)
    VChar(c) => buf.write_string(c.to_string())
    VNil => buf.write_string("nil")
    VList(items) => {
      buf.write_string("[")
      for i, item in items {
        if i > 0 {
          buf.write_string(", ")
        }
        write_value(buf, item, depth + 1)
      }
      buf.write_string("]")
    }
    VDict(map) => {
      buf.write_string("{")
      let keys : Array[String] = []
      for key in map.keys() {
        keys.push(key)
      }
      sort_strings(keys)
      for i, key in keys {
        if i > 0 {
          buf.write_string(", ")
        }
        buf.write_string(key)
        buf.write_string(": ")
        match map.get(key) {
          Some(child) => write_value(buf, child, depth + 1)
          None => buf.write_string("nil")
        }
      }
      buf.write_string("}")
    }
    VFunc(func) =>
      if func.is_task {
        buf.write_string("<task " + func.name + ">")
      } else {
        buf.write_string("<closure>")
      }
    VBuiltin(name) => buf.write_string("<builtin " + name + ">")
  }
}

fn sort_strings(keys : Array[String]) -> Unit {
  for i in 1..<keys.length() {
    let key = keys[i]
    let mut j = i
    while j > 0 && keys[j - 1] > key {
      keys[j] = keys[j - 1]
      j = j - 1
    }
    keys[j] = key
  }
}

fn values_equal(left : Value, right : Value) -> Bool {
  match (left, right) {
    (VNil, VNil) => true
    (VInt(a), VInt(b)) => a == b
    (VFloat(a), VFloat(b)) => a == b
    (VInt(a), VFloat(b)) => fits_f64(a) && a.to_double() == b
    (VFloat(a), VInt(b)) => fits_f64(b) && a == b.to_double()
    (VStr(a), VStr(b)) => a == b
    (VChar(a), VChar(b)) => a == b
    (VBool(a), VBool(b)) => a == b
    (VList(a), VList(b)) => {
      if a.length() != b.length() {
        false
      } else {
        let mut ok = true
        for i in 0..<a.length() {
          if !(values_equal(a[i], b[i])) {
            ok = false
          }
        }
        ok
      }
    }
    (VDict(a), VDict(b)) => {
      if a.length() != b.length() {
        false
      } else {
        let mut ok = true
        for key in a.keys() {
          match (a.get(key), b.get(key)) {
            (Some(x), Some(y)) =>
              if !(values_equal(x, y)) {
                ok = false
              }
            _ => ok = false
          }
        }
        ok
      }
    }
    (VBuiltin(a), VBuiltin(b)) => a == b
    _ => false
  }
}

fn is_truthy(value : Value) -> Bool {
  match value {
    VNil => false
    VBool(flag) => flag
    VInt(n) => n != 0L
    VFloat(n) => n != 0.0
    VStr(s) => s.length() != 0
    VList(items) => items.length() != 0
    VDict(map) => !(map.is_empty())
    _ => true
  }
}

fn char_len(text : String) -> Int64 {
  let mut n = 0L
  for _ in text {
    n = n + 1L
  }
  n
}

fn char_at(text : String, index : Int) -> Char? {
  let mut i = 0
  for c in text {
    if i == index {
      return Some(c)
    }
    i = i + 1
  }
  None
}

fn as_int(value : Value, what : String) -> Int64 raise MoraError {
  match value {
    VInt(n) => n
    VFloat(n) =>
      if !(n.is_finite()) {
        raise RuntimeErr(what + " must be finite, got " + show_float(n))
      } else {
        n.to_int64()
      }
    other => raise RuntimeErr(what + " must be a number, got " + show_value(other))
  }
}
