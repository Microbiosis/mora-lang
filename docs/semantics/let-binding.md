# Mora  — Let 

> ****`let x: T = expr` 
> **Property tests**`src/formal_semantics.rs`  `test_let_*`
> ****`src/interpreter/execute.rs` (execute_let), `src/value.rs` (Environment)

---

## 1. 

`let` 

****
```
let <name>[: <type_hint>] = <expr>
```

- `name`
- `type_hint`/
- `expr`

---

## 2. 

###  LET-BIND

****
1. `Γ` 
2. `expr`  `Γ`  `v`
3. `name` 

****
```
Γ ⊢ expr ⇒ v

Γ, let name = v ⊢ body ⇒ result    body 
```

 `Γ, name ↦ v`  `name`  `v` 

###  LET-SCOPE

****`name`  `Γ` 

****
```
Γ ⊢ name ⇒ v_old
Γ' = Γ + {name ↦ v_new}

Γ' ⊢ name ⇒ v_new    
```

###  LET-READ-ONLY

****`x`  `let` 

****
```
Γ ⊢ let x = v ⊢ assign x = v' ⇒ Error("x is immutable")
```

`let`  `assign`  `assign`  Mora  `let` 

###  LET-ORDER

**** `let` 

****
```
Γ ⊢ let x = v1 ⊢ body1 ⇒ result1
Γ ⊢ let y = v2 ⊢ body2 ⇒ result2

Γ ⊢ let x = v1; let y = v2 ⊢ body ⇒ body  {x↦v1, y↦v2} 
```

`let`  `let`  `let` 

---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-LET-READ` | `let x = v` `x`  `v` |
| `P-LET-SHADOW` |  `let x = v2`  `let x = v1` `x`  `v2` |
| `P-LET-ORDER` |  |
| `P-LET-IMMUTABLE` | `let x = v`  `assign x = v'`  |

---

## 4. 

Property tests 

1.  Mora  `let` 
2.  +  + 
3. 
