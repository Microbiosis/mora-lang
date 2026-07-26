# Mora  — If-Then-Else 

> ****`if <condition> then <then_body> [else <else_body>] end`
> **Property tests**`src/formal_semantics.rs`  `test_if_*`
> ****`src/interpreter/execute.rs` (execute_if)

---

## 1. 

`if` 

****
```
if <condition> then
  <then_body>
[else
  <else_body>]
end
```

---

## 2. 

###  IF-THEN

****
1. `Γ ⊢ condition ⇒ Bool(true)`
2. `Γ ⊢ then_body ⇒ result`

****
```
Γ ⊢ condition ⇒ Bool(true)
Γ ⊢ then_body ⇒ result

Γ ⊢ if condition then then_body end ⇒ result
```

###  IF-ELSE-TRUE else

****
1. `Γ ⊢ condition ⇒ Bool(true)`
2. `Γ ⊢ then_body ⇒ result`

****
```
Γ ⊢ condition ⇒ Bool(true)
Γ ⊢ then_body ⇒ result

Γ ⊢ if condition then then_body else else_body end ⇒ result
```

###  IF-ELSE-FALSE else

****
1. `Γ ⊢ condition ⇒ Bool(false)`
2. `Γ ⊢ else_body ⇒ result`

****
```
Γ ⊢ condition ⇒ Bool(false)
Γ ⊢ else_body ⇒ result

Γ ⊢ if condition then then_body else else_body end ⇒ result
```

###  IF-NO-ELSE-FALSE else

****`Γ ⊢ condition ⇒ Bool(false)`

****
```
Γ ⊢ condition ⇒ Bool(false)

Γ ⊢ if condition then then_body end ⇒ Nil
```

###  IF-CONDITION-TYPE-ERROR

****`Γ ⊢ condition ⇒ v` `v`  `Bool`

****
```
Γ ⊢ condition ⇒ v    v ∉ {Bool(true), Bool(false)}

Γ ⊢ if condition then ... end ⇒ Error("if condition must be bool")
```

---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-IF-THEN-EXECUTES` | `then_body` `else_body`  |
| `P-IF-ELSE-EXECUTES` |  else `else_body`  |
| `P-IF-NO-ELSE-NIL` |  else  `Nil` |
| `P-IF-COMP-STRONG` |  `then_body`  |
| `P-IF-COMP-FALSE` |  `else_body`  else `Nil` else |
| `P-IF-NON-BOOL-ERROR` |  |

---

## 4. 

Property tests 

1.  Mora  if 
2.  +  + 
3.  then/else  `let marker = "then"` / `let marker = "else"`
4. 
