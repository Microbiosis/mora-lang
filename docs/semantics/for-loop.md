# Mora  — For 

> ****`for <var> in <iterable> ... end`
> **Property tests**`src/formal_semantics.rs`  `test_for_*`
> ****`src/interpreter/execute.rs` (execute_for)

---

## 1. 

`for` 

****
```
for <var>[: <type>] in <iterable>
  <body>
end
```

---

## 2. 

###  FOR-LIST

****
1. `Γ ⊢ iterable ⇒ List([e₀, e₁, ..., eₙ₋₁])`
2.  `i``Γ ⊢ body[i] ⇒ rᵢ` `body[i]`  `Γ, var ↦ eᵢ`  body

****
```
Γ ⊢ iterable ⇒ List([e₀, ..., eₙ₋₁])
∀ i < n. Γ, var ↦ eᵢ ⊢ body ⇒ rᵢ

Γ ⊢ for var in iterable body end ⇒ [r₀, r₁, ..., rₙ₋₁]
```

###  FOR-EMPTY

****`Γ ⊢ iterable ⇒ List([])`

****
```
Γ ⊢ iterable ⇒ List([])

Γ ⊢ for var in iterable body end ⇒ []
```

###  FOR-BREAK

****
1.  `i``body[i]`  `break`
2.  `j < i` 

****
```
∀ j < i. Γ, var ↦ e ⊢ body ⇒ r
Γ, var ↦ eᵢ ⊢ body ⇒ Break

Γ ⊢ for var in iterable body end ⇒ [r₀, ..., rᵢ₋₁]
```

###  FOR-CONTINUE

****
1.  `i``body[i]`  `continue`
2. 

****
```
∀ j < i. Γ, var ↦ e ⊢ body ⇒ r
Γ, var ↦ eᵢ ⊢ body ⇒ Continue
∀ j > i. Γ, var ↦ e ⊢ body ⇒ r

Γ ⊢ for var in iterable body end ⇒ [r₀, ..., rᵢ₋₁, rᵢ₊₁, ..., rₙ₋₁]
 i
```

###  FOR-VAR-SCOPE

****`var` 

****
```
Γ ⊢ var ⇒ v_old
Γ, var ↦ e ⊢ body ⇒ r

Γ ⊢ for var in iterable body end ⇒ result
 result  Γ  var  v_old
```



###  FOR-ITEM-VALUE

****`Γ ⊢ iterable ⇒ List([e₀, ..., eₙ₋₁])`

****
```
 i Γ ⊢ var ⇒ eᵢ
```



---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-FOR-ITEMS` | `for x in [a, b, c]`  body 3 `x`  `a`, `b`, `c` |
| `P-FOR-EMPTY` | `for x in [] body end`  body 0  |
| `P-FOR-BREAK` | `break`  |
| `P-FOR-CONTINUE` | `continue`  body  |
| `P-FOR-SCOPE` |  `var`  |
| `P-FOR-LEN` | `len(for x in lst collect x end)` == `len(lst)` |

---

## 4. 

Property tests 

1.  Mora  for 
2.  body  `var` 
3.  +  + 
4. 
