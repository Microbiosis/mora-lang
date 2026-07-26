# Mora  — Task

> ****`task`  `f(args...)`
> **Property tests**`src/semantics_tests.rs`  `test_fn_*`
> ****`src/interpreter/dispatch.rs` (call_function, call_task_inner)

---

## 1. 

`task`  Mora  task body 

****
```
task <name>(<params>) [: <return_type>]
  <body>
end
```


```
<name>(<arg1>, <arg2>, ...)
```

---

## 2. 

###  FN-DEFINE

****
1. `params`  `[p₁, ..., pₙ]`
2. `body`  `[s₁, ..., sₖ]`
3. `name` 

****
```

Γ ⊢ task name(p₁, ..., pₙ) body end ⇒ Γ, name ↦ Task(name, [p₁,...,pₙ], body)
```

###  FN-CALL-OK

****
1. `Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)`
2. `Γ ⊢ args[i] ⇒ aᵢ``i = 1..n`
3. `Γ' = Γ + {p₁↦a₁, ..., pₙ↦aₙ}`
4. `Γ' ⊢ body ⇒ r`body 

****
```
Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)
∀ i ∈ [1,n]. Γ ⊢ args[i] ⇒ aᵢ
Γ, p₁↦a₁, ..., pₙ↦aₙ ⊢ body ⇒ r

Γ ⊢ name(args[1], ..., args[n]) ⇒ r
```

###  FN-CALL-ARITY-ERROR

****`Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)``args`  `m ≠ n`

****
```
Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)
m = len(args), m ≠ n

Γ ⊢ name(args[1], ..., args[m]) ⇒ Error("task expects n args, got m")
```

###  FN-RETURN

****body  `return <expr>`  body 

****
```
body = [s₁, ..., sₖ₋₁, return e]
Γ ⊢ e ⇒ r

Γ ⊢ body ⇒ r
```

###  FN-RETURN-IMPLICIT

****body  `return` 

****
```
body = [s₁, ..., sₖ]   return
sₖ  r

Γ ⊢ body ⇒ r
```

###  FN-ARG-EVAL-ORDER

****`args = [a₁, ..., aₙ]`

****
```

Γ ⊢ args[1] ⇒ v₁, Γ ⊢ args[2] ⇒ v₂, ..., Γ ⊢ args[n] ⇒ vₙ
```

###  FN-CALL-VALUE

****`name` `print`, `len`, `range` 

****
```
Γ ⊢ print(v₁, ..., vₙ) ⇒ Nil    
Γ ⊢ len(v) ⇒ Int(len(v))         v  list/dict/string
Γ ⊢ range(start, end, step) ⇒ List(start, start+step, ..., <end)
```

---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-FN-RETURN` | `task f() return v end; f()`  `v` |
| `P-FN-PARAM-BIND` | `task f(x) return x end; f(a)`  `a` |
| `P-FN-ARITY-ERROR` | `task f(x, y) ... end; f(a)`  |
| `P-FN-ARG-EVAL-LEFT-TO-RIGHT` |  |
| `P-FN-IMPLICIT-RETURN` |  `return`  task  body  |
| `P-FN-NIL-RETURN` |  body  task  `Nil` |
| `P-FN-BUILTIN-LEN` | `len([1,2,3])` → `Int(3)` |
| `P-FN-BUILTIN-RANGE` | `range(0, 3)` → `[0, 1, 2]` |

---

## 4. 

Property tests 

1.  Mora  task 
2.  +  + 
3. 
