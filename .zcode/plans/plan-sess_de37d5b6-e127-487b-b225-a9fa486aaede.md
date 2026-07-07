## Step 1：ParserV3 加 `dyn:` 语法（先做）

### 背景
调研确认：
- `dyn` 是 lexer 关键字（src/lexer.rs:73, 697）
- `TokenType::Dyn` 存在但 parser_v3 唯一调用点（src/parser_v3/mod.rs:2762-2794）在 `parse_unary` 后缀——CLI 走 `ParserV3::compile` 路径不走
- 用户实际写法：`let x: dyn Foo = …`（type annotation）——但 `parse_type_annotation`（:3086）match arm 没有 `TokenType::Dyn` 分支，实测 `let a: dyn Foo = 1` 报 `expected type annotation`
- `Type::Trait` 已有 `from_hint("dyn:Foo")` 路径（src/typeck/mod.rs:228-247）作为临时 workaround

### 实现（按调研方案 A：零公共 API 破坏）

**唯一改动**：`src/parser_v3/mod.rs:3089` `parse_type_annotation` match arm 加 `TokenType::Dyn` 分支：

```rust
TokenType::Dyn => {
    self.advance();  // 吃 'dyn'
    let name = self.consume_identifier("Expected trait name after 'dyn'")?;
    let generics = if self.match_token(&[TokenType::Less]) {
        let mut g = Vec::new();
        loop {
            g.push(self.parse_type_annotation()?);
            if !self.match_token(&[TokenType::Comma]) { break; }
        }
        self.consume(TokenType::Greater, "Expected '>' in dyn trait generics")?;
        g
    } else { Vec::new() };
    Some(Type::Trait { name, generics })
}
```

### 行为
- `let x: dyn Foo = ...` → `type_hint: Type::Trait{"Foo", []}`（与 `from_hint("dyn:Foo")` 同构）
- `let x: dyn Container<number> = ...` → `Type::Trait{"Container", [Int]}`（复用 :3104-3118 的泛型解析）
- `parse_type_annotation` 不变：与 `from_hint` 路径产出同一 `Type::Trait`——`flow.rs:312` 运行时检查与静态检查不再分叉

### 影响范围
- `src/parser_v3/mod.rs`：仅 `parse_type_annotation` match 新增 1 个 arm
- `Type`、`MirExpr`、`MirWitness` 公共 API：**零改动**
- HM 推断：零改动（`Type::Trait` 已有）
- `BidirectionalChecker` Phase C（`bidirectional.rs:213`）：零改动（`let_with_type_hint` 已自动 check）
- 运行时 flow.rs:312：零改动
- 公共 `Type::TraitObject`：**保持 unit variant**（用户调研结论"无任何构造点"——保留死代码以备未来）

### 测试
- `tests/parser_v3_coverage.rs:27-61` 两个 `#[ignore]` 测试**可能自动跑通**——移除 `#[ignore]` 标记
- 新增 1 个测试：`let x: dyn Foo = 1` 应能 parse（typecheck 单独失败，验证 parser 通过）

### 风险（按 AGENTS.md §6 最小修改控制）
1. **`Less` 歧义**：`parse_type_annotation` 是 type 位置，不是 expression——`dyn Foo < x` 不会在这出现。`Less` 解析在 type 位置是泛型开界符，无二义。
2. **`dyn Typo` 拼错**：静默产出 `Type::Trait{"Typo",[]}`，HM 不报错——pre-existing 问题（无 trait_registry 校验），不在本次范围。
3. **`from_hint` 双轨**：新路径与 `from_hint("dyn:Foo")` 都产出 `Type::Trait`——不冲突，但冗余。后续可清理 `from_hint` 路径，本次不动。

### 估时 0.5 天

---

## Step 2：Match exhaustiveness 检查（再做）

### 背景
调研确认：
- `infer_match`（src/typeck/hm/infer.rs:324）当前**不做** exhaustiveness——任意 arm（包括 Wildcard）都参与 typed unification，但不分析覆盖
- 任何 arm 是 `Wildcard/Variable(_)` 时**应**视为 exhaustive（catch-all）——W1 简化方案
- `Type::TraitObject` 在 `subtype_of` 显式返 `false`——`TraitObject` 真正没被构造，不参与判定
- 公共 `TypeError` 是 struct（含 `line/column/message/expected/actual/hint`），**非 enum**——新增 exhaustiveness 不需新 variant
- `TypeError::from_span_with_detail`（src/typeck/mod.rs:700）已存在，直接复用

### 实现（按调研方案 B：W2 中等 + 公共 TypeError 路径）

**位置**：`src/typeck/bidirectional.rs:294` Phase D 之后插入 Phase G。

**算法伪代码**：
```rust
fn phase_g_match_exhaustiveness(arms, scrutinee_ty, span) {
    // 1. 任意 arm 是 Wildcard（不含 Variable 绑定）—— 即 catch-all，跳过
    if arms.iter().any(|a| matches!(a.pattern.kind, Wildcard) && a.guard.is_none()) {
        return;
    }
    // 2. 收集覆盖的 Literal Bool（scrutinee 是 Bool 时）
    if let Type::Bool = scrutinee_ty {
        let covered: HashSet<bool> = arms.iter()
            .filter_map(|a| match &a.pattern.kind {
                Literal(Literal::Bool(b, _)) => Some(*b),
                _ => None,
            })
            .collect();
        if !(covered.contains(&true) && covered.contains(&false)) {
            emit_non_exhaustive(span, missing=["true","false"]);
        }
    }
    // 3. 其他 scrutinee 类型（Int/Float/String/enum/Any）W1 简化方案
    //    覆盖：当前不报（保守）—— 后续 W3 算法扩展
}
```

**错误报告**：
```rust
let mut e = TypeError::new(span.line, "non-exhaustive match patterns");
e.column = span.column;
e.expected = Some("exhaustive match".to_string());
e.actual = Some(format!("arms: {:?}", arms.iter().map(|a| ...).collect::<Vec<_>>()));
e.hint = Some("add a wildcard arm: _ => <default-value>".to_string());
self.errors.push(e);
```

**守卫条件**：
1. `arms[i].guard.is_none()`（有 guard 的 arm 不计入覆盖——条件可能永假）
2. `scrutinee_ty` 不是 `Any`（保守跳过）
3. `BidirectionalChecker.errors` 是 `Vec<TypeError>`——直接 push

### 影响范围
- `src/typeck/bidirectional.rs`：仅 1 处加 ~40 行（Phase G 块）
- 公共 `TypeError`：零改动（复用 `from_span_with_detail`）
- `infer_match`：零改动（不报错路径，与 d7f35f9+ 模式一致）
- `hm_to_external`：零改动（7-arm match 不变）
- 公共 API：零改动

### 测试（4 个）
- `phase_g_match_with_wildcard_passes` —— `match x { _ => 1 }` 不报
- `phase_g_match_bool_missing_true_reports` —— `match x: bool { false => 0 }` 报 missing true
- `phase_g_match_bool_complete_passes` —— `match x: bool { true => 1, false => 0 }` 不报
- `phase_g_match_with_guard_ignores_coverage` —— `match x { 1 if c => "a" }` 不报（guard 可能永假）

### 风险（按 §6）
1. **Variable arm 当 catch-all**：W1 简化用 `Wildcard` 专用，不包 Variable——更安全。
2. **List/Dict/Tuple pattern**：W2 不分析，跳过即可。
3. **scrutinee 是 Any**：保守跳过（HM 兜底）。
4. **新检查 vs HM 现有 Eq 一致错误**：双向报错是「Branch mismatch」——HM Eq 报「if branches type inconsistent」——两者**独立两条信息通道**，`hm.diagnosed` 按 line+column 过滤消重。

### 估时 0.5 天

---

## 总计

| Step | 估时 | 风险 |
|---|---|---|
| 1. ParserV3 `dyn:` 语法 | 0.5 天 | `Less` 歧义（已分析无问题）；`dyn Typo` 静默（pre-existing） |
| 2. Match exhaustiveness 检查 | 0.5 天 | 守卫条件覆盖；HM 错误独立通道 |

**总计 1 天**。

### 不在本次范围
- W3 严格 Maranget 决策树算法（v0.75.x 增量过大）
- B 方案 `Type::TraitObject` struct 变更（破坏性）
- `from_hint` 路径清理
- C 方案 `as dyn` cast 复活

### 顺序
Step 1 → Step 2（Step 1 完成后 type annotation 完整，让 Step 2 测试构造 `let x: bool = ...` 更直接）

### 验证
- cargo fmt + clippy 0 warning
- 既有 656 测试全过
- 新增 5-6 个测试（dyn: 2 + match 4）
- 端到端 exe 跑 `test_v0_75_85_bidirectional.mora` 正常

### 后续
- 严格 exhaustiveness（W3）：Maranget decision tree，留独立会话
- trait 定义（trait Foo do…end）解析：parser_v3 完整 trait 系统
- Type::TraitObject 真正构造点：object-safety 检查

---

完整按 AGENTS.md §6 最小修改原则——不重构任何已生产化算法，仅增量。