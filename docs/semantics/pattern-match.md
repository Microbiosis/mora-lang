# Mora  — 

> ****`match <expr> with <pattern> => <body> ... end` 
> **Property tests**`src/semantics_tests.rs`  `test_match_*`
> ****`src/interpreter/execute.rs` (execute_match), `src/interpreter/evaluate.rs` (match_pattern)

---

## 1. 

Mora  body

****
```
match <expr>
  | <pattern1> => <body1>
  | <pattern2> => <body2>
  ...
end
```

****
- `Wildcard` (`_`)
- `Variable(name)` `name`
- `Literal(lit)` `lit` 
- `List { prefix, rest }` N `rest` 
- `Dict([(k₁, p₁), ...])`
- `Guard { pattern, condition }` `pattern`  `condition` 

---

## 2. 

###  MATCH-WILDCARD

**** `v`

****
```
∀ v: Value.  v matches _    
```

###  MATCH-VARIABLE

****`name` `v` 

****
```
∀ v: Value, ∀ name: Ident.
  v matches name      bindings = {name ↦ v}
```

###  MATCH-LITERAL

****`lit_val`  `Value`

****
```
v matches Literal(lit) ⇔ v == lit_val

```

###  MATCH-LIST

****`v = List([e₁, ..., eₙ])``pats = [p₁, ..., pₖ]`

**** rest
```
n = k    ∀ i. eᵢ matches pᵢ
⇐⇒ List([e₁,...,eₙ]) matches List([p₁,...,pₖ])
```

**** rest
```
n ≥ k    ∀ i ∈ [1,k]. eᵢ matches pᵢ
⇐⇒ List([e₁,...,eₙ]) matches List([p₁,...,pₖ], rest=r)
      r = List([eₖ₊₁,...,eₙ])
```

###  MATCH-DICT

****`v = Dict(map)``pats = [(k₁, p₁), ..., (kₙ, pₙ)]`

****
```
∀ i ∈ [1,n]. (kᵢ ∈ keys(map))   map[kᵢ] matches pᵢ
⇐⇒ Dict(map) matches Dict([(k₁,p₁), ..., (kₙ,pₙ)])
```

###  MATCH-GUARD

****`v matches pattern``Γ ⊢ condition ⇒ Bool(true)`

****
```
v matches pattern    Γ ⊢ condition ⇒ Bool(true)
⇐⇒ v matches Guard{pattern, condition}
```

****
```
v matches pattern    Γ ⊢ condition ⇒ Bool(false)
⇐⇒ v  Guard{pattern, condition}
```

###  MATCH-FIRST-WINS

****`v matches pᵢ`  `∀ j < i. v  p`

****
```
v matches p₁ => body₁
...
v matches pₖ => bodyₖ

v matches arms[i]     body[i] body[i+1..k]
```

###  MATCH-NO-MATCH

****`∀ i. v  pᵢ`

****
```
∀ i. v  pᵢ

v matches arms ⇒ Nil     Nil
```

---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-MATCH-WILDCARD` | `_`  |
| `P-MATCH-VARIABLE` | `x`  |
| `P-MATCH-LITERAL` | `Literal(l)`  `l`  |
| `P-MATCH-LIST-EXACT` | `[a, b]`  2  |
| `P-MATCH-LIST-REST` | `[a, ...rest]`  ≥ 1 `rest`  |
| `P-MATCH-DICT` | `{k: p}`  `k`  `p` |
| `P-MATCH-FIRST-WINS` |  body |
| `P-MATCH-NO-MATCH` |  `Nil` |
| `P-MATCH-GUARD-TRUE` |  |
| `P-MATCH-GUARD-FALSE` |  |

---

## 4. 

Property tests 

1.  Mora  match 
2.  body  `let marker = "arm1"`
3.  +  + 
4.  arm 
