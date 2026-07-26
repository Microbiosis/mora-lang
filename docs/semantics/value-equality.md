# Mora  — Value 

> ****`Value`  `==` / `!=` 
> **Property tests**`src/formal_semantics.rs`  `test_value_eq_*`
> ****`src/value.rs` (Value  + PartialEq impl)

---

## 1. 

`Value`  Mora  27  `src/value.rs`

- `String`, `Char`, `Int`, `Float`, `Bool`, `Nil`
- `List`, `Dict`, `Tuple`
- `Task`, `Closure`, `Tool`, `Builtin`
- AI `Conversation`, `Stream`, `AiConfig`, `AiResult`, `Agent`
- `Router`, `HttpRequest`, `HttpResponse`, `McpServer`, `Result_`, `Macro`, `Compose`, `Partial`

---

## 2. 

###  EQ-REFLEXIVE

****`v`  `Value`

****
```
∀ v: Value.  v == v
```

###  EQ-SYMMETRIC

****`v1`, `v2`  `Value`

****
```
∀ v1, v2: Value.  (v1 == v2) ⇔ (v2 == v1)
```

###  EQ-TRANSITIVE

****`v1`, `v2`, `v3`  `Value`

****
```
∀ v1, v2, v3: Value.  (v1 == v2) ∧ (v2 == v3) ⇒ (v1 == v3)
```

###  EQ-TYPE-DISJOINT

****`v1`  `v2`  `Value` 

****
```
∀ v1, v2: Value.  variant(v1) ≠ variant(v2) ⇒ v1 ≠ v2
```

###  EQ-PRIMITIVE

****`v1`, `v2` 

****
```
∀ s1, s2: String.    String(s1) == String(s2) ⇔ s1 == s2
∀ c1, c2: Char.      Char(c1) == Char(c2) ⇔ c1 == c2
∀ i1, i2: Int.       Int(i1) == Int(i2) ⇔ i1 == i2
∀ f1, f2: Float.     Float(f1) == Float(f2) ⇔ f1 == f2
∀ b1, b2: Bool.      Bool(b1) == Bool(b2) ⇔ b1 == b2
Nil == Nil           ( true)
```

###  EQ-LIST

****`v1`, `v2`  `List`

****
```
∀ ls1, ls2: Vec<Value>.
  List(ls1) == List(ls2) ⇔ (|ls1| == |ls2|) ∧ (∀i < |ls1|. ls1[i] == ls2[i])
```

###  EQ-DICT

****`v1`, `v2`  `Dict`

****
```
∀ d1, d2: HashMap<String, Value>.
  Dict(d1) == Dict(d2) ⇔ (∀k ∈ keys(d1) ∪ keys(d2). d1[k] == d2[k])
   k  ≠ 
```

###  EQ-NANNaN 

****`v`  `Float`  NaN

****
```
Float(NaN) != Float(NaN)  NaN 
```

###  EQ-TASKTask 

****`t1`, `t2`  `Task`

****
```
Task(t1) == Task(t2) ⇔ t1.name == t2.name

```

###  EQ-CLOSUREClosure 

****`c1`, `c2`  `Closure`

****
```
Closure(c1) == Closure(c2) ⇔ c1  c2  Arc 
```

---

## 3. Property Tests

 `src/formal_semantics.rs`  proptest 

| Property |  |
|----------|------|
| `P-EQ-REFLEXIVE` | `∀ v: Value. v == v` |
| `P-EQ-SYMMETRIC` | `∀ v1, v2: Value. (v1 == v2) ⇔ (v2 == v1)` |
| `P-EQ-TRANSITIVE` | `∀ v1, v2, v3: Value. (v1 == v2) ∧ (v2 == v3) ⇒ (v1 == v3)` |
| `P-EQ-TYPE-DISJOINT` | `∀ v1, v2: Value. variant(v1) ≠ variant(v2) ⇒ v1 ≠ v2` |
| `P-EQ-LIST-LENGTH` | `∀ ls1, ls2: Vec<Value>. len(ls1) ≠ len(ls2) ⇒ List(ls1) ≠ List(ls2)` |
| `P-EQ-DICT-SUPERSET` | `∀ d1, d2: Dict. (∀k ∈ keys(d1). d2[k] == d1[k]) ⇒ (d1 == d2 ∧ keys(d1) ⊆ keys(d2))` |
| `P-EQ-NAN` | `Float(NaN) != Float(NaN)` |
| `P-EQ-NIL-REFLEXIVE` | `Nil == Nil` |

---

## 4. 



1.  `proptest`  `Value` 
2.  property
3.  `#[test] fn test_value_eq_reflexive() { ... }` 
