# Mora  — Tool 

> ****`tool`  + `with tools:`  + `ai.chat` 
> **Property tests**`src/semantics_tests.rs`  `test_tool_*`
> ****`src/interpreter/execute.rs` (execute_tool_def, build_tool_json_schema)
>   `src/parser_v2/statements.rs` (tool_statement)
>   `src/typeck/check.rs` (check_tool_def_stmt)
>   `src/runtime/core.rs` (tool_registry: Arc<HashMap<String, ToolDef>>)

---

## 1. 

`tool`  Mora  AI`ai.chat`
 `task` 

|  | task | tool |
|------|------|------|
|  | Mora  | AI LLM |
|  | environment | environment + tool_registry |
|  |  type hint |  type hint +  JSON Schema |
|  | `f(args...)` | `ai.chat(prompt, tools=["f"])` |

---

## 2. BNF

```
tool-def       ::= 'tool' ident [string-lit] '(' param-list ')' [':' type-name] 'do' stmt* 'end'
param-list     ::= param {',' param}
param          ::= ident [':' type-name]
type-name      ::= ident | 'list<' type-name '>' | 'dict<' type-name ',' type-name '>'
```


```mora
tool read_file "Read a file from disk"
    (path: string, encoding: string) : string
do
    let content = file.read(path, encoding)
    return content
end
```

---

## 3. 

###  TOOL-DEFINE

****
1. `name` 
2. `description` 
3. `params`  `(name, type_hint?)` 
4. `return_type`  type hint `any`
5. `body` 
6. `exported`  falsetool  export

****
```
Γ, R ⊢ tool name "desc" (p₁:t₁, ..., pₙ:tₙ) → T body end
    ⇒ (Γ, R, body)

Γ, R ⊢ tool-def ⇒ Γ', R'
  
  Γ' = Γ + {name ↦ Tool(name, desc, [p₁,...,pₙ], T, body_ids)}
  R' = R + {name ↦ ToolDef(name, desc, schema(name, [p₁:t₁,...,pₙ:tₙ], T), body_closure)}
```

###  TOOL-SCHEMA-GENERATEJSON Schema 

****`params`  `(pname, hint?)` `return_type`  type hint

****
```
schema(name, [p₁:h₁, ..., pₙ:hₙ], T)
  = {"type":"object",
     "properties": {p₁: {type: hint2json(h₁)}, ..., pₙ: {type: hint2json(hₙ)}},
     "required": [p₁, ..., pₙ]}
  ∪  T :
     {"returnType": {type: hint2json(T), description: desc_or_default}}
```

**** `hint2json`

| Mora type hint | JSON Schema type |
|----------------|------------------|
| `string` | `string` |
| `float` / `number` | `number` |
| `int` | `integer` |
| `bool` | `boolean` |
| `list` / `list<any>` | `array` |
| `dict` | `object` |
| `any` /  /  | `string` fallback |

###  TOOL-CALL-AIAI 

****
1. `Γ ⊢ ai.chat ⇒ RealAiChat` tools
2. `Γ ⊢ ai.chat + with tools:[t₁,...,tₖ] ⇒ RealAiChatWithTools([t₁,...,tₖ])`

****
```
with tools: [t₁, ..., tₖ] {
  ai.chat(prompt)
}

Γ ⊢ prompt ⇒ p
∀ i ∈ [1,k]. R ⊢ tᵢ ⇒ ToolDef(tᵢ, descᵢ, schemaᵢ, closureᵢ)

Γ ⊢ with-block ⇒ RealAiChatWithTools([t₁,...,tₖ])(p)
```

****
- `RealAiChatWithTools`  tools  JSON `tools`  LLM
- LLM  `tool_calls`  lookup `ToolDef.handler`
-  `ChatMessage::Tool` 
-  10  tool call 

###  TOOL-JSON-ESCAPEJSON 

****`name`  `description` 

****
```
JSON  name/description 
  '\"' → '\\'
  '"'  → '\"'
  
```

---

## 4. 

###  TOOL-TYPE-HINT

****tool  `params: [(pname, hint?)]`

****
```
∀ (pname, hint?) ∈ params.
  if hint exists:
    Type::from_hint(hint)  type error
  else:
    hint  Type::Union(vec![])any
```

###  TOOL-RETURN-TYPE

****tool  `return_type: hint?`

****
```
if return_type exists:
  Type::from_hint(return_type)  type error
else:
  return_type  Type::Union(vec![])any
```

###  TOOL-SIGNATURE-REGISTER

****tool 

****
```
Γ ⊢ tool-def ⇒ ToolDef(name, desc, params, return_type, body)

signatures ← {name ↦ Signature(
  params: [(pname, Type::from_hint(hint))],
  return_type: Type::from_hint(return_type),
)}
```

---

## 5. Property Tests

 `src/semantics_tests.rs`  proptest 

| Property |  |
|----------|------|
| `P-TOOL-SCHEMA-VALID` | `∀ params, RT. build_tool_json_schema(params, RT)`  JSON |
| `P-TOOL-TYPE-MAP` | `∀ h ∈ KnownHints. type_hint_to_json_type(h)`  |
| `P-TOOL-SCHEMA-REQ` | `∀ params. schema.required == [p₁.name, ..., pₙ.name]` |
| `P-TOOL-SCHEMA-PROPS` | `∀ params. schema.properties  pᵢ` |
| `P-TOOL-SCHEMA-RET` | `RT = Some → schema.returnType ` |
| `P-TOOL-SCHEMA-EMPTY` | `params = [] → schema.properties == {}` |
| `P-TOOL-SCHEMA-ESCAPE` | `desc`  `"` JSON  |
| `P-TOOL-REGISTRY` | `tool` `tool_registry`  tool |

---

## 6. 



1.  `proptest`  `params`  `return_type`
2.  property `build_tool_json_schema` / `type_hint_to_json_type` 
3.  `#[test] fn test_tool_schema_valid()` 
4.  `tool_registry`  `Interpreter`  `tool` 
