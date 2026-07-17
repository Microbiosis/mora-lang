# Mora v0.15 → v0.24 开发工作流

> 完整可复现的开发流程，从 v0.14 (record/replay/diff) 到 v0.24 (ParserV2 完整迁移)。

---

## 版本演进总览

```
v0.14 (record/replay/diff)
  ↓ 检查 TODO
v0.15 (AI config + record 扩展)
  ↓ 语言规范
v0.16 (模式匹配增强 - Prolog)
  ↓ 管道与流
v0.17 (管道闭包 + 数组操作 - StreamIt/APL)
  ↓ 函数式核心
v0.18 (compose/take/drop/partial - Clojure/Lisp)
  ↓ 并发与事务
v0.19 (Worker + 事务 + atom - Ballerina/Clojure)
  ↓ 反射与元编程
v0.20 (反射 + 宏 - Smalltalk/Common Lisp)
  ↓ 借用语法
v0.21 (借用 + 生命周期 - Rust)
  ↓ 性能优化
v0.22 (AI 缓存 + 管道融合 + 常量折叠 - DeepSpec)
  ↓ 强类型升级
v0.23 (类型别名 + 枚举 + 结构体)
  ↓ AST 升级
v0.24 (ParserV2 完整迁移 + 旧 Parser 删除)

## v0.24 ParserV2 完整迁移

### 状态: ✅ 完成

旧 parser.rs (2459 行) 已删除，主程序和测试全部使用 ParserV2。

### 架构

```
源码 .mora → Lexer → Token 流 → ParserV2 → ASTv2 → 反向适配器 → AST → 解释器
```

### 核心文件

| 文件 | 行数 | 说明 |
|------|------|------|
| src/parser_v2.rs | 1766 | ParserV2 主体 |
| src/ast_v2.rs | 543 | AST v2 定义 |
| src/ast_v2_to_v1.rs | 388 | 反向适配器 |
| src/interpreter.rs | - | 解释器 (使用 parse_code) |
| src/typeck.rs | - | 类型检查器 (使用 parse_code) |

### 完成的工作

1. **ParserV2 实现** (1766 行)
   - 替代旧 parser.rs (2459 行)
   - 支持所有语言特性
   - 直接输出 ast_v2 节点

2. **反向适配器** (ast_v2_to_v1.rs)
   - 将 ast_v2 转换为旧 ast
   - 支持解释器继续使用旧 AST

3. **类型检查修复**
   - let 推断: 已知类型自动推断
   - string + any: 允许字符串拼接

4. **全面迁移**
   - interpreter.rs: 使用 parse_code()
   - lsp: 直接使用 ParserV2
   - typeck 测试: 使用 parse_code()
   - 删除旧 parser.rs

5. **Bug 修复**
   - 表达式优先级: 方法调用优先于二元运算
   - trait/impl 方法循环: 添加 break guard
   - transaction rollback/commit: 正确解析
   - list/dict 字面量: 正确解析
   - match when 守卫: 支持条件匹配

### 测试状态

- 单元测试: 188 passed
- 集成测试: 5 passed (解析真实 .mora 脚本)
- CI: 全绿 (Check + Rustfmt + Clippy + Test + LSP Smoke + Record CLI)

### 已验证的示例

- container.mora ✅
- nested_generic.mora ✅
- observe_demo.mora ✅
- trait_demo.mora ✅
- trait_default_demo.mora ✅

### 语言特性覆盖

| 类别 | 特性 |
|------|------|
| 语句 | let, task, return, if, for, import, break, continue, match, with, parallel, worker, transaction, macro, route, trait, impl, type, enum, struct, save, load, read, write, append, read_bytes, write_bytes, stream, tool, observe, span, record_tokens, commit, rollback |
| 表达式 | variable, literal, binary, unary, call, method_call, index, question, closure, pipe, list, dict, match, prompt, format_string, ai_model, namespace_ref, char |
| 模式 | literal, variable, wildcard, list, dict, guard |
| 类型系统 | generic_params, type_list, type_name_recursive, where_clause, dyn trait |
   - lsp: 直接使用 ParserV2
   - typeck 测试: 使用 parse_code()
   - 删除旧 parser.rs

### 测试状态

- 单元测试: 186 passed
- 集成测试: 5 passed (解析真实 .mora 脚本)
- clippy: clean

### 已验证的示例

- container.mora ✅
- nested_generic.mora ✅
- observe_demo.mora ✅
- trait_demo.mora ✅
- trait_default_demo.mora ✅
```

---

## 阶段 1: 检查与准备 (v0.15)

### 1.1 检查构建状态
```bash
cargo build          # 编译检查
cargo test           # 运行测试
cargo clippy         # 代码质量检查
cargo fmt -- --check # 格式检查
```

### 1.2 安装工具
```bash
rustup component add rustfmt clippy
```

### 1.3 检查 TODO
```bash
grep -rn "TODO\|FIXME" src/ --include="*.rs"
```

### 1.4 接入遗留 TODO
```rust
// 示例: TokenBudget.per_call
// 1. 找到相关代码
// 2. 实现功能
// 3. 删除 TODO 注释
// 4. 添加测试
```

---

## 阶段 2: 功能开发 (v0.16-v0.20)

### 2.1 确定版本目标
```bash
# 从学习计划中选择
cat docs/learning-plan.md
```

### 2.2 修改 AST
```rust
// src/ast.rs
pub enum Stmt {
    // 添加新语句类型
    NewFeature { ... },
}

pub enum Expr {
    // 添加新表达式类型
}
```

### 2.3 修改 Lexer
```rust
// src/lexer.rs
pub enum TokenType {
    // 添加新关键字
    NewKeyword,
}

// 在 identifier_from 中添加
"new_keyword" => TokenType::NewKeyword,
```

### 2.4 修改 Parser
```rust
// src/parser.rs
fn new_feature_statement(&mut self) -> Stmt {
    // 解析新语法
}
```

### 2.5 修改 Interpreter
```rust
// src/interpreter.rs
// 在 execute 函数中添加
Stmt::NewFeature { .. } => {
    // 执行新功能
}

// 在 call_method 中添加
"new_method" => {
    // 新方法实现
}
```

### 2.6 修改 TypeChecker
```rust
// src/typeck.rs
// 添加类型检查
```

### 2.7 更新 LSP
```rust
// src/lsp/providers.rs
// 添加新语句的行号处理
```

### 2.8 添加测试
```rust
#[test]
fn test_new_feature() {
    let src = r#"
task main()
  // 测试代码
end
"#;
    run(src).expect("should work");
}
```

### 2.9 验证
```bash
cargo build && cargo test && cargo clippy
```

---

## 阶段 3: 性能优化 (v0.22)

### 3.1 AI 调用优化
```rust
// AI 调用内联缓存
let cache_key = format!("{}:{:?}", model, messages);
if let Some(cached) = self.ai_cache.get(&cache_key) {
    return Ok(Value::String(cached.clone()));
}

// 投机执行
with speculative = true, draft_model = "gpt-4o-mini"
  let result = ai.chat("question")
end

// 批量 AI 调用
let results = batch_chat(["q1", "q2", "q3"])
```

### 3.2 管道优化
```rust
// 管道融合 - 连续操作合并执行
fn is_fusable_method(method: &str) -> bool {
    matches!(method, "map" | "filter" | "take" | "drop")
}
```

### 3.3 常量折叠
```rust
// 编译期计算常量表达式
fn try_fold_binary(left: &Value, op: &BinaryOp, right: &Value) -> Option<Value> {
    match (left, op, right) {
        (Value::Number(l), BinaryOp::Add, Value::Number(r)) => Some(Value::Number(l + r)),
        // ...
    }
}
```

### 3.4 字符串驻留
```rust
// 相同字符串只存储一次
fn intern_string(&mut self, s: String) -> Value {
    if let Some(interned) = self.string_interner.get(&s) {
        return interned.clone();
    }
    let val = Value::String(s.clone());
    self.string_interner.insert(s, val.clone());
    val
}
```

---

## 阶段 4: 强类型升级 (v0.23)

### 4.1 类型别名
```rust
// src/ast.rs
pub enum Stmt {
    TypeAlias {
        name: String,
 generics: Vec<String>,
        target: String,
        span: Span,
    },
}
```

### 4.2 枚举类型
```rust
pub enum Stmt {
    EnumDef {
        name: String,
        generics: Vec<String>,
        variants: Vec<EnumVariant>,
        span: Span,
    },
}

pub struct EnumVariant {
    pub name: String,
    pub data: Option<String>,
}
```

### 4.3 结构体类型
```rust
pub enum Stmt {
    StructDef {
        name: String,
        generics: Vec<String>,
        fields: 文档更新

### 5.1 更新语言规范
```bash
# docs/mora-spec.md
# 添加新特性的文档
```

### 5.2 更新 CHANGELOG
```bash
# CHANGELOG.md
## [v0.XX] - YYYY-MM-DD
### 新特性
- 特性描述
```

### 5.3 更新学习计划
```bash
# docs/learning-plan.md
# 标记已完成的特性
```

---

## 阶段 7: CI/CD 更新

### 7.1 更新工作流
```yaml
# .github/workflows/ci.yml
jobs:
  check: cargo check --all-targets
  test:  # 跨平台测试
  fmt:   cargo fmt --all -- --check
  clippy: cargo clippy --all-targets --all-features -- -D warnings
```

### 7.2 更新版本号
```toml
# Cargo.toml
version = "0.0.XX"
```

---

## 阶段 8: 配套文件更新

### 8.1 README.md
```markdown
# 添加新特性说明
### v0.XX-v0.YY 新特性

| 版本 | 特性 | 来源 | 语法 |
|------|------|------|------|
| v0.XX | 特性名 | 来源 | `语法示例` |
```

### 8.2 AGENTS.md
```markdown
## 代码质量
- clippy: 0 warnings
- 测试: N passed
```

### 8.3 CLAUDE.md
```markdown
## 代码结构
src/
├── value.rs      # 核心类型
├── flow.rs       # 自由函数
├── interpreter.rs # 解释器
```

### 8.4 配置文件
```bash
# Cargo.toml: 更新版本号
# Dockerfile: 更新版本注释
# docker-compose.yml: 更新服务
# .gitignore: 更新忽略规则
```

---

## 阶段 9: 最终验证

### 9.1 完整检查
```bash
cargo build          # ✅ 0 errors
cargo test           # ✅ N passed
cargo clippy         # ✅ 0 warnings
cargo fmt -- --check # ✅ formatted
```

### 9.2 提交
```bash
git add -A
git commit -m "feat(v0.XX): 版本描述"
```

### 9.3 统计
```bash
git log --oneline origin/main..HEAD | wc -l
git diff --stat origin/main
```

---

## 关键命令速查

```bash
# 构建检查
cargo build && cargo test && cargo clippy

# 格式化
cargo fmt

# 运行单个测试
cargo test test_name

# 查看测试数量
cargo test 2>&1 | grep "test result"

# 查看 clippy 警告
cargo clippy 2>&1 | grep "warning:"

# 提交
git add -A && git commit -m "feat: description"

# 查看提交历史
git log --oneline -10

# 查看文件统计
wc -l src/*.rs
```

---

## 版本发布检查清单

- [ ] 所有测试通过
- [ ] 无 clippy 警告
- [ ] 代码已格式化
- [ ] CHANGELOG 已更新
- [ ] 语言规范已更新
- Mora 的不可变优先在 Mora 中的具体体现是什么？
    - 不可变优先在 Mora 中的具体体现是 `let` 默认不可变，`assign` 显式修改。
- Mora 的管道操作符 | 和 |> 有什么区别？
    - Mora 的管道操作符是 `|>` 而不是 `|`。
- Mora 的 task 返回类型用什么符号？
    - Mora 的 task 返回类型用 `:` 而不是 `->`。
- Mora 的 match 语法是什么？
    - Mora 的 match 语法是 `match expr with pattern -> result end`。
- Mora 的闭包语法是什么？
    - Mora 的闭包语法是 `fn(x) x * 2 end` 或 `fn(x) return x * 2 end`。

---

*v0.15 → v0.24 完整工作流 — 2026-06-29*
