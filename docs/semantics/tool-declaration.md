# Mora 形式化语义 — Tool 声明语法

> **构造**：`tool` 声明 + `with tools:` 绑定 + `ai.chat` 工具调用
> **Property tests**：`src/semantics_tests.rs` 中 `test_tool_*`
> **相关代码**：`src/interpreter/execute.rs` (execute_tool_def, build_tool_json_schema)
>   `src/parser_v2/statements.rs` (tool_statement)
>   `src/typeck/check.rs` (check_tool_def_stmt)
>   `src/runtime/core.rs` (tool_registry: Arc<HashMap<String, ToolDef>>)

---

## 1. 定义

`tool` 是 Mora 中的命名工具函数，可被 AI（`ai.chat`）在对话中自动调用。
与 `task` 的区别：

| 特性 | task | tool |
|------|------|------|
| 调用方 | Mora 代码 | AI LLM |
| 注册位置 | environment | environment + tool_registry |
| 类型校验 | 编译期 type hint | 编译期 type hint + 运行时 JSON Schema |
| 调用语法 | `f(args...)` | `ai.chat(prompt, tools=["f"])` |

---

## 2. 语法（BNF）

```
tool-def       ::= 'tool' ident [string-lit] '(' param-list ')' [':' type-name] 'do' stmt* 'end'
param-list     ::= param {',' param}
param          ::= ident [':' type-name]
type-name      ::= ident | 'list<' type-name '>' | 'dict<' type-name ',' type-name '>'
```

示例：
```mora
tool read_file "Read a file from disk"
    (path: string, encoding: string) : string
do
    let content = file.read(path, encoding)
    return content
end
```

---

## 3. 操作语义规则

### 规则 TOOL-DEFINE（定义）

**前提**：
1. `name` 是标识符
2. `description` 是字符串字面量（可选，默认空）
3. `params` 是 `(name, type_hint?)` 元组列表
4. `return_type` 是 type hint（可选，默认 `any`）
5. `body` 是语句列表
6. `exported` 为 false（tool 不支持 export）

**规则**：
```
Γ, R ⊢ tool name "desc" (p₁:t₁, ..., pₙ:tₙ) → T body end
    ⇒ (Γ, R, body)
─────────────────────────────────────────────────────────────────────
Γ, R ⊢ tool-def ⇒ Γ', R'
  其中：
  Γ' = Γ + {name ↦ Tool(name, desc, [p₁,...,pₙ], T, body_ids)}
  R' = R + {name ↦ ToolDef(name, desc, schema(name, [p₁:t₁,...,pₙ:tₙ], T), body_closure)}
```

### 规则 TOOL-SCHEMA-GENERATE（JSON Schema 生成）

**前提**：`params` 是 `(pname, hint?)` 元组列表，`return_type` 是 type hint

**规则**：
```
schema(name, [p₁:h₁, ..., pₙ:hₙ], T)
  = {"type":"object",
     "properties": {p₁: {type: hint2json(h₁)}, ..., pₙ: {type: hint2json(hₙ)}},
     "required": [p₁, ..., pₙ]}
  ∪ 如果 T 存在:
     {"returnType": {type: hint2json(T), description: desc_or_default}}
```

**映射表** `hint2json`：

| Mora type hint | JSON Schema type |
|----------------|------------------|
| `string` | `string` |
| `float` / `number` | `number` |
| `int` | `integer` |
| `bool` | `boolean` |
| `list` / `list<any>` | `array` |
| `dict` | `object` |
| `any` / 空 / 未知 | `string`（默认 fallback） |

### 规则 TOOL-CALL-AI（AI 调用工具）

**前提**：
1. `Γ ⊢ ai.chat ⇒ RealAiChat`（当前无 tools）
2. `Γ ⊢ ai.chat + with tools:[t₁,...,tₖ] ⇒ RealAiChatWithTools([t₁,...,tₖ])`

**规则**：
```
with tools: [t₁, ..., tₖ] {
  ai.chat(prompt)
}
─────────────────────────────────────────────────────
Γ ⊢ prompt ⇒ p
∀ i ∈ [1,k]. R ⊢ tᵢ ⇒ ToolDef(tᵢ, descᵢ, schemaᵢ, closureᵢ)
─────────────────────────────────────────────────────────────
Γ ⊢ with-block ⇒ RealAiChatWithTools([t₁,...,tₖ])(p)
```

**执行语义**：
- `RealAiChatWithTools` 将 tools 序列化为 JSON `tools` 字段发给 LLM
- LLM 返回 `tool_calls` 时，按名称 lookup `ToolDef.handler`（闭包）并执行
- 执行结果作为 `ChatMessage::Tool` 追加到对话历史
- 最多 10 轮 tool call 循环

### 规则 TOOL-JSON-ESCAPE（JSON 转义）

**前提**：`name` 或 `description` 包含特殊字符

**规则**：
```
JSON 中 name/description 的转义：
  '\"' → '\\'
  '"'  → '\"'
  其他字符原样保留
```

---

## 4. 类型系统形式化

### 规则 TOOL-TYPE-HINT（参数类型校验）

**前提**：tool 声明中有 `params: [(pname, hint?)]`

**规则**：
```
∀ (pname, hint?) ∈ params.
  if hint exists:
    Type::from_hint(hint) 必须成功（否则 type error）
  else:
    hint 默认 Type::Union(vec![])（any）
```

### 规则 TOOL-RETURN-TYPE（返回类型校验）

**前提**：tool 声明中有 `return_type: hint?`

**规则**：
```
if return_type exists:
  Type::from_hint(return_type) 必须成功（否则 type error）
else:
  return_type 默认 Type::Union(vec![])（any）
```

### 规则 TOOL-SIGNATURE-REGISTER（签名注册）

**前提**：tool 通过类型检查

**规则**：
```
Γ ⊢ tool-def ⇒ ToolDef(name, desc, params, return_type, body)
─────────────────────────────────────────────────────────────
signatures ← {name ↦ Signature(
  params: [(pname, Type::from_hint(hint))],
  return_type: Type::from_hint(return_type),
)}
```

---

## 5. 后验性质（Property Tests）

以下性质应在 `src/semantics_tests.rs` 中用 proptest 验证：

| Property | 断言 |
|----------|------|
| `P-TOOL-SCHEMA-VALID` | `∀ params, RT. build_tool_json_schema(params, RT)` 输出是有效 JSON |
| `P-TOOL-TYPE-MAP` | `∀ h ∈ KnownHints. type_hint_to_json_type(h)` 映射正确 |
| `P-TOOL-SCHEMA-REQ` | `∀ params. schema.required == [p₁.name, ..., pₙ.name]` |
| `P-TOOL-SCHEMA-PROPS` | `∀ params. schema.properties 包含所有 pᵢ` |
| `P-TOOL-SCHEMA-RET` | `RT = Some → schema.returnType 存在` |
| `P-TOOL-SCHEMA-EMPTY` | `params = [] → schema.properties == {}` |
| `P-TOOL-SCHEMA-ESCAPE` | `desc` 含 `"` 时，JSON 中转义正确 |
| `P-TOOL-REGISTRY` | `tool` 声明执行后，`tool_registry` 包含该 tool |

---

## 6. 实现验证

验证以上性质的方法：

1. 构造 `proptest` 策略生成随机 `params` 和 `return_type`
2. 对每个 property，调用 `build_tool_json_schema` / `type_hint_to_json_type` 断言性质成立
3. 用 `#[test] fn test_tool_schema_valid()` 等函数包装
4. 对 `tool_registry` 测试，使用 `Interpreter` 执行 `tool` 声明后检查注册结果
