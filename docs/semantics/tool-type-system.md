# Mora 形式化语义 — Tool 类型系统

> **构造**：tool 声明的编译期类型校验 + 运行时 JSON Schema 生成
> **Property tests**：`src/semantics_tests.rs` 中 `test_tool_*`
> **相关代码**：`src/typeck/check.rs` (check_tool_def_stmt)
>   `src/typeck/mod.rs` (Type::from_hint)
>   `src/interpreter/execute.rs` (build_tool_json_schema, type_hint_to_json_type)
>   `src/interpreter/mod.rs` (ToolDef)

---

## 1. 定义

Tool 类型系统涵盖两个阶段：

1. **编译期**（typeck）：校验参数/返回类型 hint 的合法性，注册签名到 `signatures` 表
2. **运行时**（interpreter）：将 type hint 映射为 JSON Schema type 字符串，生成供 AI 消费的 schema

两个阶段共享同一组类型映射规则，但表达形式不同：

| 阶段 | 输入 | 输出 |
|------|------|------|
| 编译期 typeck | `hint: &str` | `Type` 枚举变体 |
| 运行时 interpreter | `hint: Option<String>` | JSON Schema type `&'static str` |

---

## 2. 编译期类型规则（typeck）

### 规则 TOOL-PARAM-TYPE（参数类型校验）

**前提**：`params: [(pname, hint?)]` 是 tool 声明的参数列表

**规则**：
```
∀ (pname, hint?) ∈ params.
  if hint exists:
    from_hint(hint) 必须成功
    symbols.define(pname, from_hint(hint))
  else:
    symbols.define(pname, Type::Union(vec![]))    // default: any
```

**形式化**：
```
Γ ⊢ tool-declare name(p₁:h₁?, ..., pₙ:hₙ?)
  其中 ∀i. hint(hᵢ) → True ∨ hint(hᵢ) = None
─────────────────────────────────────────────
Γ ⊢ params ⇒ [(pnameᵢ, Tᵢ)]
  其中 Tᵢ = from_hint(hᵢ) if hint(hᵢ) exists
       Tᵢ = Type::Union([])   if hint(hᵢ) = None
```

### 规则 TOOL-RETURN-TYPE（返回类型校验）

**前提**：`return_type: Option<&str>` 是 tool 声明的返回类型 hint

**规则**：
```
if return_type exists:
  from_hint(return_type) 必须成功
  current_return_hint = Some(from_hint(return_type))
else:
  current_return_hint = Some(Type::Union(vec![]))    // default: any
```

**形式化**：
```
Γ ⊢ tool-declare name(...) → R?
  其中 hint(R) → True ∨ R = None
─────────────────────────────────────
Γ ⊢ return ⇒ Rₜᵧₚₑ
  其中 Rₜᵧₚₑ = from_hint(R) if R exists
       Rₜᵧₚₑ = Type::Union([])   if R = None
```

### 规则 TOOL-SIGNATURE-REGISTER（签名注册）

**前提**：tool 通过类型检查

**规则**：
```
signatures.insert(name, Signature {
  params: [(pnameᵢ, from_hint(hᵢ) ∨ Union([]))],
  raw_params: [hᵢ],
  return_type: from_hint(R) ∨ Union([]),
  raw_return_type: R,
})
```

**形式化**：
```
Γ ⊢ tool-def ⇒ ToolDef(name, desc, params, return_type, body)
──────────────────────────────────────────────────────────────
Γ ⊢ signatures ⇒ Σ ∪ {name ↦ Signature(params_typed, return_type_typed)}
```

---

## 3. Type::from_hint 映射规则

### 规则 FROM-HINT-BASIC（基本类型映射）

| hint 字符串 | Type 变体 |
|------------|-----------|
| `string` | `Type::String` |
| `char` | `Type::Char` |
| `float` / `number` | `Type::Float` |
| `bool` | `Type::Bool` |
| `nil` | `Type::Nil` |
| `any` | `Type::Union(vec![])` |
| `list` | `Type::List(Box::Type::Union(vec![]))` |
| `dict` | `Type::Dict(Union([]), Union([]))` |
| `task` | `Type::Task` |
| `closure` | `Type::Closure` |
| `conversation` | `Type::Conversation` |
| `stream` | `Type::Stream` |
| `ai_config` / `ai_result` / `ai_error` | 对应 Type 变体 |

### 规则 FROM-HINT-GENERIC（泛型映射）

| hint 模式 | Type 变体 |
|----------|-----------|
| `list<T>` | `Type::List(Box::from_hint(T))` |
| `dict<K, V>` | `Type::Dict(Box::from_hint(K), Box::from_hint(V))` |
| `string<char>` | `Type::Char` |
| `dyn:Foo` | `Type::Trait { name: "Foo", generics: [] }` |
| `dyn:Foo<T>` | `Type::Trait { name: "Foo", generics: [from_hint(T)] }` |

### 规则 FROM-HINT-UNKNOWN（未知类型）

**前提**：hint 字符串不在已知类型表中

**规则**：
```
from_hint(unknown) → Type::Trait { name: unknown, generics: [] }
```

**注意**：未知类型被解析为 Trait 而非报错——这是设计选择，允许用户定义自定义类型名。

---

## 4. 运行时 JSON Schema 映射规则（hint2json）

### 规则 HINT2JSON（type hint → JSON Schema type）

运行时 `type_hint_to_json_type` 将 Mora type hint 映射为 JSON Schema type 字符串：

| Mora type hint | JSON Schema type | 规则 |
|----------------|------------------|------|
| `string` | `string` | HINT2JSON-STRING |
| `float` / `number` | `number` | HINT2JSON-FLOAT |
| `int` | `integer` | HINT2JSON-INT |
| `bool` | `boolean` | HINT2JSON-BOOL |
| `list` / `list<any>` | `array` | HINT2JSON-LIST |
| `dict` | `object` | HINT2JSON-DICT |
| `any` / None / 未知 | `string` | HINT2JSON-FALLBACK |

**形式化**：
```
hint2json(h) =
  case h of
    "string"         → "string"
    "float" | "number" → "number"
    "int"            → "integer"
    "bool"           → "boolean"
    "list" | "list<any>" → "array"
    "dict"           → "object"
    _                → "string"    // fallback: any/None/unknown
```

### 规则 SCHEMA-STRUCTURE（JSON Schema 结构）

**前提**：`params: [(pname, hint?)]`，`return_type: hint?`

**规则**：
```
schema = {
  "type": "object",
  "properties": {
    p₁: { "type": hint2json(h₁) },
    ...
    pₙ: { "type": hint2json(hₙ) }
  },
  "required": [p₁, ..., pₙ]
}
∪ if return_type exists:
  {
    "returnType": {
      "type": hint2json(R),
      "description": desc_or("Tool return value")
    }
  }
```

**形式化**：
```
Schema(name, params, R?, desc) =
  { type: "object",
    properties: Π,
    required: [p₁,...,pₙ]
  } ∪ returnType
  其中 Π = { pᵢ: { type: hint2json(hᵢ) } | i ∈ [1,n] }
        returnType = Some({ type: hint2json(R), description: desc })
                   if R exists
        returnType = None
                   if R = None
```

### 规则 SCHEMA-ESCAPE（JSON 转义）

**前提**：pname 或 description 包含 JSON 特殊字符

**规则**：
```
escape(s) =
  s 中的每个字符 c:
    if c = '"'  → '\"'
    if c = '\'  → '\\'
    else → c
```

**注意**：当前实现中，`pname` 通过 Rust `format!` 直接插入 JSON 字符串，若 pname 含 `"` 或 `\` 则产生无效 JSON。这是一个**已知限制**。

---

## 5. 编译期 ↔ 运行时的对应关系

编译期 typeck 和运行时 interpreter 使用**不同的映射表**：

| hint | typeck (Type) | interpreter (JSON Schema) |
|------|---------------|---------------------------|
| `string` | `Type::String` | `string` |
| `int` | `Type::Int` | `integer` |
| `float` | `Type::Float` | `number` |
| `number` | `Type::Float`（向后兼容） | `number` |
| `bool` | `Type::Bool` | `boolean` |
| `list` | `Type::List(Union([]))` | `array` |
| `dict` | `Type::Dict(Union([]), Union([]))` | `object` |
| `any` | `Type::Union([])` | `string`（fallback） |
| 未知 | `Type::Trait{...}` | `string`（fallback） |

**关键差异**：
1. typeck 将 `int` 映射为 `Type::Int`，interpreter 映射为 JSON Schema `integer`
2. typeck 将未知类型解析为 Trait，interpreter 将未知类型 fallback 为 `string`
3. 两个映射表在语义上保持一致：都是"已知类型→精确映射，未知→保守默认"

---

## 6. 后验性质（Property Tests）

以下性质应在 `src/semantics_tests.rs` 中用 proptest 验证：

| Property | 阶段 | 断言 |
|----------|------|------|
| `P-TOOL-TYPE-MAP` | interpreter | `∀ h ∈ KnownHints. type_hint_to_json_type(h) == expected` |
| `P-TOOL-SCHEMA-VALID` | interpreter | `∀ params, RT. parse_json(build_tool_json_schema(params, RT))` 成功 |
| `P-TOOL-SCHEMA-REQ` | interpreter | `∀ params. schema.required.length == params.length` |
| `P-TOOL-SCHEMA-PROPS` | interpreter | `∀ params. schema.properties 包含所有 pname` |
| `P-TOOL-SCHEMA-RET` | interpreter | `RT = Some → schema.returnType 存在` |
| `P-TOOL-SCHEMA-EMPTY` | interpreter | `params = [] → schema.properties == {}` |
| `P-TOOL-SCHEMA-ESCAPE` | interpreter | desc 含 `"` 时 schema 仍为有效 JSON |
| `P-TOOL-REGISTRY` | interpreter | `tool` 声明执行后 tool_registry 包含该 tool |
| `P-TOOL-FROM-HINT-ALL` | typeck | `∀ h ∈ AllKnownHints. from_hint(h) → success` |
| `P-TOOL-FROM-HINT-UNKNOWN` | typeck | `∀ h ∉ KnownHints. from_hint(h) → Trait{h}`（不报错） |
| `P-TOOL-FROM-HINT-GENERIC` | typeck | `from_hint("list<string>") → List(String)` |
| `P-TOOL-FROM-HINT-DICT` | typeck | `from_hint("dict<string,int>") → Dict(String, Int)` |

---

## 7. 实现验证

验证以上性质的方法：

1. **编译期性质**：直接调用 `Type::from_hint` 验证映射正确性
2. **运行时 JSON 性质**：构造 `Interpreter`，调用 `build_tool_json_schema`（通过 Mora 源码执行），用 `serde_json` 或正则验证 JSON 合法性
3. **registry 性质**：执行 `tool` 声明后检查 `tool_registry` 内容
4. **proptest 策略**：随机生成 `(pname, hint?)` 元组列表，覆盖所有已知 hint 组合
