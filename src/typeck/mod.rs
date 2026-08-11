//! v11 静态类型检查
//!
//! 设计原则：
//! - **多错误收集**：一次跑完所有检查，统一报告（不首个错误终止）
//! - **位置精确**：每条 TypeError 带行号（line, col），IDE 友好
//! - **可选类型注解**：Mora 是渐进式静态类型语言（spec §3.1）——无注解时走 HM Hindley-Milner 推断；推断不出来视为 Any
//! - **不破坏现有行为**：未标注类型注解的代码继续走默认推断路径（仅在 main.rs 入口可选启用严格 typeck）
//!
//! 检查范围：
//! - let 初始化值 vs 类型 hint
//! - task / closure 参数 vs 实参类型
//! - task / closure 返回类型 vs return 表达式
//! - binary 操作数类型（+ - * / % + 比较）
//! - 索引操作类型（list→number, dict→string）
//! - if 条件类型（任何值视为 truthy，不报）
//! - method call 接收者类型 + 方法存在性
//! - 变量引用 vs 作用域
//!
//! 不做：
//! - 列表/字典元素类型推断（Mora 列表是异构容器）——注：v0.75.16 M1 已做
//!   方法签名级元素类型保留，此处指字面量级不再扩展
//! - 控制流敏感的类型缩窄

// v0.55: typeck V2 模块 (mod check, mod pregel_check) 已删除。
// 类型检查的唯一入口: `check_program_mir` (mod check_mir)。

pub mod bidirectional; // v0.75.86: 双向类型检查骨架入口（Phase A）
pub mod check_mir;
pub mod dispatch;
pub mod hm;
/// v0.75.18: 跨模块 import 符号表（typeck 阶段预扫描合并）
pub mod imports;

use std::collections::HashMap;

// v1 AST types no longer imported — all v2 paths use ast_v2 / common
use crate::common::Span;

// ===================================================================
// 公共类型
// ===================================================================

/// Mora 类型系统：基础类型 + Any（推断不出时退路）
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    /// v0.x: 单字符类型（`string[number]` 索引结果）
    Char,
    // v0.38: numeric tower — Int(i64) and Float(f64) as distinct types.
    Int,
    Float,
    Bool,
    Nil,
    /// 任意类型（推断不出时的退路，或显式 `any` 标注）
    Any,
    /// v0.75.91: 未知类型逃逸标签 — HM 推断 / parser / import 解析失败时的
    /// 兜底；与 `Any` 区别：`Any` 是 strict top type（仅在 subtype_of /
    /// unify 路径与所有类型兼容），`Unknown` 在所有路径都是「不可判定」标
    /// 记。详见 `docs/decisions/any-vs-unknown.md`（v0.75.91）。
    Unknown,
    /// v0.x: 列表类型携带元素类型（`list<T>`）
    List(Box<Type>),
    /// v0.x: 字典类型携带键值类型（`dict<K, V>`）
    Dict(Box<Type>, Box<Type>),
    Task,
    Closure,
    Conversation,
    Stream,
    Builtin,
    /// v0.06: AI 配置类型（ai.chat 的接收者 / AiConfig::new() 构造）
    AiConfig,
    /// v0.06: AI 调用结果类型（ai.chat 的成功返回）
    AiResult,
    /// v0.06: AI 调用错误类型（ai.chat 的失败返回，v0.06.2 起被 Result<T,E> 包裹）
    AiError,
    /// v0.06: AI 模块类型（`ai` 内建变量的接收者类型）
    AiModule,
    /// v0.06.2: 类型化错误处理 Result<T, E>
    Result_(Box<Type>, Box<Type>),
    /// v0.06.3: HTTP 路由构建器
    Router,
    /// v0.06.3: HTTP 请求对象
    HttpRequest,
    /// v0.06.3: HTTP 响应对象（handler 返回值）
    HttpResponse,
    /// v0.06.6: MCP 服务器构建器
    McpServer,
    /// v0.08: dyn trait 类型（名称）
    /// v0.09: 携带泛型参数列表（如 `dyn Container<number>`）
    Trait {
        name: String,
        generics: Vec<Type>,
    },
    /// v0.09: 具体类型（替代 v0.08.5 删的 Type::Struct）
    ///   携带泛型参数 + 实现的 trait 列表
    Concrete {
        name: String,
        generics: Vec<Type>,
        traits: Vec<Type>,
    },
    /// v0.13: Union 类型（多种类型的合集，e.g. `string | number | bool`）
    ///   用于 builtin 多类型签名（print 等）
    ///   兼容规则: A 兼容 B 当 A 是 B 的成员, 或 B 是 A 的成员, 或递归嵌套
    Union(Vec<Type>),
    // v0.36 (Permanent #3): 8 new Type variants for v0.17–v0.27 Value kinds.
    // The v0.34 audit's claim that "16 Value variants lack Type variants" was
    // solvable in one commit; the previous deferral to v1.0 was a cop-out.
    /// v0.03: Agent (name + tool_names + model_route + max_steps + system)
    Agent,
    /// v0.08.5: Trait object carrier (for_type + trait_name + generics + data)
    /// v0.75.86: 扩展 trait_name + generics 字段——subtype_of 升级为
    /// trait_name + generics 同构判断（之前 unit variant 返 false 是 stub）。
    /// 对应运行时 [`Value::TraitObject`] 包含 dyn dispatch 信息。
    TraitObject {
        trait_name: String,
        generics: Vec<Type>,
    },
    /// v0.17: Compose pipeline (arity = number of functions)
    Compose,
    /// v0.18: Partial application (boxed origin + how many args applied)
    Partial,
    /// v0.19: Atom (mutable reference cell)
    Atom,
    /// v0.20: Macro definition (name + params shape)
    Macro,
    /// v0.26: Prompt section (named system-prompt segment)
    PromptSection,
    /// v0.27: Document unified IR (Arc<dyn DocumentBackend>)
    Document,
    /// v0.55: HM type variable (char key, fresh in inference, unified during solve)
    TypeVar(char),
    /// v0.75.17: 泛型量化 ∀α₁...αₙ. τ — let-generalization 的产物。
    /// 命中 env 时由 instantiate 替换为 fresh TypeVar（标准 HM 规则）。
    ForAll(Vec<char>, Box<Type>),
    /// v0.80: 函数类型带 effect row（Stage 2/4 algebraic effects 的类型基础）。
    /// `fn (T) -> U ! {Ai, Fs}` — input type → output type with effect row。
    /// Koka 风格：`Arrow(input, output, EffectRow)`。
    /// 与 ForAll 的区别：ForAll 跨函数泛型量化；Arrow 标记具体函数类型的 effect。
    /// 老 `Closure` / `Task` 类型视为 `Arrow(_, _, Empty)`。
    Arrow(Box<Type>, Box<Type>, crate::mir::effect::EffectRow),
} // ← close pub enum Type

impl Type {
    /// 返回类型的字符串表示。v0.x 起支持泛型：`list<number>` / `dict<string, any>` / `result<T, E>`
    pub fn name(&self) -> String {
        match self {
            Type::String => "string".to_string(),
            Type::Char => "char".to_string(),
            // v0.38: Int and Float distinct.
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Nil => "nil".to_string(),
            Type::Any => "any".to_string(),
            Type::Unknown => "unknown".to_string(),
            Type::List(elem) => format!("list<{}>", elem.name()),
            Type::Dict(k, v) => format!("dict<{}, {}>", k.name(), v.name()),
            Type::Task => "task".to_string(),
            Type::Closure => "closure".to_string(),
            Type::Conversation => "conversation".to_string(),
            Type::Stream => "stream".to_string(),
            Type::Builtin => "builtin".to_string(),
            Type::AiConfig => "ai_config".to_string(),
            Type::AiResult => "ai_result".to_string(),
            Type::AiError => "ai_error".to_string(),
            Type::AiModule => "ai".to_string(),
            Type::Result_(ok, err) => format!("result<{}, {}>", ok.name(), err.name()),
            Type::Router => "router".to_string(),
            Type::HttpRequest => "http_request".to_string(),
            Type::HttpResponse => "http_response".to_string(),
            Type::McpServer => "mcp_server".to_string(),
            Type::Trait { .. } => "trait".to_string(),
            Type::Concrete { .. } => "concrete".to_string(),
            // v0.36: 8 new variants
            Type::Agent => "agent".to_string(),
            Type::TraitObject {
                trait_name,
                generics,
            } => {
                if generics.is_empty() {
                    format!("dyn {}", trait_name)
                } else {
                    format!(
                        "dyn {}<{}>",
                        trait_name,
                        generics
                            .iter()
                            .map(|t| t.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Compose => "compose".to_string(),
            Type::Partial => "partial".to_string(),
            Type::Atom => "atom".to_string(),
            Type::Macro => "macro".to_string(),
            Type::PromptSection => "prompt_section".to_string(),
            Type::Document => "document".to_string(),
            Type::TypeVar(c) => format!("'{}", c),
            // v0.75.17: 泛型量化显示为 "forall<'a, 'b>. τ"（let-generalization 产物）
            Type::ForAll(vars, inner) => {
                let names: Vec<String> = vars.iter().map(|v| format!("'{}", v)).collect();
                format!("forall<{}>. {}", names.join(", "), inner.name())
            }
            // v0.80: 函数类型带 effect row —— `fn (T) -> U ! {Ai, Fs}` 形式。
            Type::Arrow(input, output, row) => {
                let row_str = row.to_string();
                if row_str == "pure" {
                    format!("fn ({}) -> {}", input.name(), output.name())
                } else {
                    format!("fn ({}) -> {} ! {{ {} }}", input.name(), output.name(), row_str)
                }
            }
            // v0.13: Union 类型显示为 "T1 | T2 | T3"
            Type::Union(members) => {
                if members.is_empty() {
                    return "any".to_string();
                }
                let parts: Vec<String> = members.iter().map(|m| m.name()).collect();
                parts.join(" | ")
            }
        }
    }

    /// 从用户写的类型 hint 字符串解析
    pub fn from_hint(hint: &str) -> Type {
        match hint {
            "string" => Type::String,
            "char" => Type::Char,
            "float" | "number" => Type::Float, // v0.x: "number" kept for backwards compatibility
            "bool" => Type::Bool,
            "nil" => Type::Nil,
            // v0.50: "any" 应解析为 Union(vec![])（兼容任何类型）
            "any" => Type::Union(vec![]),
            "list" => Type::List(Box::new(Type::Union(vec![]))),
            "dict" => Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
            "task" => Type::Task,
            "closure" => Type::Closure,
            "conversation" => Type::Conversation,
            "stream" => Type::Stream,
            "ai_config" => Type::AiConfig,
            "ai_result" => Type::AiResult,
            "ai_error" => Type::AiError,
            "router" => Type::Router,
            "http_request" => Type::HttpRequest,
            "http_response" => Type::HttpResponse,
            "mcp_server" => Type::McpServer,
            // v0.x: list<T> 泛型语法
            s if s.starts_with("list<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                Type::List(Box::new(Type::from_hint(inner.trim())))
            }
            // v0.x: dict<K, V> 泛型语法（顶层 split，保留嵌套）
            s if s.starts_with("dict<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                match split_top_level_comma(inner) {
                    Some((k_str, v_str)) => Type::Dict(
                        Box::new(Type::from_hint(k_str.trim())),
                        Box::new(Type::from_hint(v_str.trim())),
                    ),
                    None => {
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![])))
                    }
                }
            }
            // v0.x: string<char> 单字符
            "string<char>" => Type::Char,
            // v0.08: dyn: 前缀 → Trait 类型
            // v0.09: dyn:Foo<number> → Trait { name: "Foo", generics: [Number] }
            // v0.10: 泛型嵌套如 Boxed<number> → Type::Trait { name: "Boxed", generics: [Number] }
            s if s.starts_with("dyn:") => {
                let rest = &s[4..];
                if let Some(lt) = rest.find('<') {
                    let name = rest[..lt].to_string();
                    let generics_str = &rest[lt + 1..rest.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Trait {
                        name: rest.to_string(),
                        generics: vec![],
                    }
                }
            }
            // v0.10 修复: 嵌套泛型 `Foo<Bar<number>>` 解析为 Type::Trait
            s if s.contains('<') && s.ends_with('>') => {
                if let Some(lt) = s.find('<') {
                    let name = s[..lt].to_string();
                    let generics_str = &s[lt + 1..s.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Union(vec![])
                }
            }
            // v0.12: 未知类型名 fallback → 改用 Type::Trait 占位
            //   这样调用方可以查 trait_registry 判断是否合法
            //   （之前是 Any, 丢失了 hint 信息）
            _ => Type::Trait {
                name: hint.to_string(),
                generics: vec![],
            },
        }
    }

    /// v0.12: 判断类型名是否是合法 builtin / 已知类型
    pub fn is_builtin_type_name(name: &str) -> bool {
        matches!(
            name,
            "string"
                | "char"
                | "float"
                | "bool"
                | "nil"
                | "list"
                | "dict"
                | "task"
                | "closure"
                | "conversation"
                | "stream"
                | "ai_config"
                | "ai_result"
                | "ai_error"
                | "ai_module"
                | "router"
                | "http_request"
                | "http_response"
                | "mcp_server"
                | "any"
        )
    }

    /// 类型兼容：Any 总兼容；Result<T,E> 与 Ok/Err 兼容
    /// v0.13: Union 类型支持 —— A ∈ union(expected) 或 expected ∈ union(self)
    pub fn compatible_with(&self, expected: &Type) -> bool {
        // v0.75.92: Unknown fail-fast — 不参与任何兼容判断（与 Any 不同，
        // Any 是 top type；Unknown 是「无法判定」标记）。调用方应通过 env/closure_sigs
        // TypeVar 推断得到精确类型，或在 builtin/import 兜底处显式产出 Unknown。
        if matches!(self, Type::Unknown) || matches!(expected, Type::Unknown) {
            return false;
        }
        // v0.13: Union 兼容 —— self 是 union, expected 是 union 任一成员
        if let Type::Union(members) = expected {
            // 空 Union = "any element type" (兼容任何)
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| self.compatible_with(m));
        }
        if let Type::Union(members) = self {
            // 空 Union = "any element type" (兼容任何)
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| m.compatible_with(expected));
        }
        // v0.75.17: ForAll 类型 — 与内层类型同兼容性判断（命中 env 时已实例化，
        // 此处仅作防御：泛型值兼容任意实例）。
        if let Type::ForAll(_, inner) = self {
            return inner.compatible_with(expected);
        }
        if let Type::ForAll(_, inner) = expected {
            return self.compatible_with(inner);
        }
        // v0.13: Result<T1, E1> 兼容 Result<T2, E2> 当 T1==T2 且 E1==E2 (真正同构)
        if let (Type::Result_(t1, e1), Type::Result_(t2, e2)) = (self, expected) {
            return t1.compatible_with(t2) && e1.compatible_with(e2);
        }
        // v0.x: List<T1> 兼容 List<T2> 当 T1 兼容 T2
        if let (Type::List(a), Type::List(b)) = (self, expected) {
            return a.compatible_with(b);
        }
        // v0.x: Dict<K1, V1> 兼容 Dict<K2, V2> 当 K 兼容且 V 兼容
        if let (Type::Dict(k1, v1), Type::Dict(k2, v2)) = (self, expected) {
            return k1.compatible_with(k2) && v1.compatible_with(v2);
        }
        // v0.08.1: Nil 兼容所有 trait（用于 dyn Trait = nil 占位）
        // v0.12: 后门 2 关闭 —— Nil 仅兼容 Nil, 不再豁免 trait 赋值
        //   若需要 dyn Trait = nil, 显式使用 Option<T> 或 T? 语法
        if matches!(self, Type::Nil) && matches!(expected, Type::Nil) {
            return true;
        }
        if matches!(self, Type::Nil) || matches!(expected, Type::Nil) {
            return false;
        }
        // v0.08.5: Trait 兼容
        // v0.09: 含泛型比较（name 一致 + generics 个数一致 + 元素兼容）
        if let (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) = (self, expected)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            for (x, y) in ga.iter().zip(gb.iter()) {
                if !x.compatible_with(y) {
                    return false;
                }
            }
            return true;
        }
        // v0.08.5: Type::Struct 已删除，统一为 Type::Trait 注册
        self == expected
    }

    /// v0.75.86: 非对称 subtype 关系 `self <: super`。
    ///
    /// 与 [`compatible_with`](Self::compatible_with) 的关键区别：
    /// `compatible_with` 是**对称**关系（任一边匹配即 true），
    /// `subtype_of` 是**单向**子类型——`A <: B` 不蕴含 `B <: A`。
    ///
    /// 实现策略：
    ///   1. 所有 `compatible_with` 的现有 arm 复用（`Any` top type、
    ///      `ForAll` 内层、容器递归、Trait 同构、Union 任一成员、`Result`
    ///      同构、`Nil` 自反）
    ///   2. 新增 `Concrete <: Trait` ——`Concrete { traits, .. }` 的 traits
    ///      列表中任一与 super 同构即 subtype（实现 trait 即 subtype）
    ///   3. 新增 `TraitObject <: Trait` ——dyn trait object 视为 trait 的
    ///      运行时载体（`TraitObject { trait_name, generics, .. }` 与 super
    ///      name 一致 + generics 兼容即 subtype）
    ///   4. 末尾 `self == expected` 兜底（同构严格相等是 subtype）
    ///
    /// 应用：双向定型 check 模式核心 — `actual <: expected ?`。
    /// `compatible_with` 保留对称语义给 HM 合一（双向算法需要任一
    /// 方向兼容即可 unify），`subtype_of` 给定向检查用。
    pub fn subtype_of(&self, super_ty: &Type) -> bool {
        // === 复用 compatible_with 全部 arm ===
        // Any 是 top type —— 与任何类型兼容（自然 subtype 任何）
        if matches!(self, Type::Any) || matches!(super_ty, Type::Any) {
            return true;
        }
        // v0.75.92: Unknown fail-fast — 不参与任何 subtype 判断
        // （与 Any 区分：Any top type 任意 subtype；Unknown 未知 type 拒绝）
        if matches!(self, Type::Unknown) || matches!(super_ty, Type::Unknown) {
            return false;
        }
        // v0.75.17: ForAll 类型——泛型值命中 env 时已实例化，此处防御
        if let Type::ForAll(_, inner) = self {
            return inner.subtype_of(super_ty);
        }
        if let Type::ForAll(_, inner) = super_ty {
            return self.subtype_of(inner);
        }
        // v0.13: Union subtype——任一成员 subtype super_ty 即可（保守）
        if let Type::Union(members) = self {
            if members.is_empty() {
                // 空 Union = "any element type" 占位，subtype 任何
                return true;
            }
            return members.iter().any(|m| m.subtype_of(super_ty));
        }
        // super 是 Union 时，self 必须 subtype 任一成员（与
        // compatible_with 一致——但保守方向）
        if let Type::Union(members) = super_ty {
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| self.subtype_of(m));
        }
        // Result<T1, E1> subtype Result<T2, E2> 当 T1<:T2 && E1<:E2
        if let (Type::Result_(t1, e1), Type::Result_(t2, e2)) = (self, super_ty) {
            return t1.subtype_of(t2) && e1.subtype_of(e2);
        }
        // List<T1> subtype List<T2> 当 T1<:T2
        if let (Type::List(a), Type::List(b)) = (self, super_ty) {
            return a.subtype_of(b);
        }
        // Dict<K1, V1> subtype Dict<K2, V2>
        if let (Type::Dict(k1, v1), Type::Dict(k2, v2)) = (self, super_ty) {
            return k1.subtype_of(k2) && v1.subtype_of(v2);
        }
        // Nil 仅 subtype Nil（v0.12 后门 2 关闭）
        match (self, super_ty) {
            (Type::Nil, Type::Nil) => return true,
            (Type::Nil, _) | (_, Type::Nil) => return false,
            _ => {}
        }
        // === subtype 新增 arm ===
        // Concrete subtype Trait：实现 trait 即 subtype trait
        if let (Type::Concrete { traits, .. }, Type::Trait { .. }) = (self, super_ty) {
            return traits.iter().any(|t| t.subtype_of(super_ty));
        }
        // TraitObject subtype Trait：dyn object 视为 trait 的运行时载体
        //
        // 当前 TraitObject 是 unit variant（无 trait_name/generics 字段——
        // trait 信息存在 `Value::TraitObject` 运行时值里，类型系统层面
        // 不可达）。真正的 dyn: 语法未实现（parser_v3/mod.rs 无 dyn: 解析），
        // 此处兜底：TraitObject 不 subtype 任何具体 Trait。后续若解析器
        // 加 dyn: 语法、扩展 `Type::TraitObject { trait_name, generics }`，
        // TraitObject subtype Trait：v0.75.86 升级为 trait_name + generics
        // 同构判断（之前 unit variant 返 false 是 stub）。
        if let (
            Type::TraitObject {
                trait_name: tn1,
                generics: g1,
            },
            Type::Trait {
                name: n2,
                generics: g2,
            },
        ) = (self, super_ty)
        {
            if tn1 != n2 || g1.len() != g2.len() {
                return false;
            }
            return g1.iter().zip(g2.iter()).all(|(a, b)| a.subtype_of(b));
        }
        // === 现有 compatible_with 的 Trait 同构 arm ===
        if let (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) = (self, super_ty)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            // 严格 subtype：ga 必须逐元素 <: gb（保守方向）
            return ga.iter().zip(gb.iter()).all(|(x, y)| x.subtype_of(y));
        }
        // 兜底：同构严格相等
        self == super_ty
    }
}

/// v0.13: 判断类型是否是空 Union (即原 Any 占位)
pub fn is_empty_union(ty: &Type) -> bool {
    matches!(ty, Type::Union(m) if m.is_empty())
}

/// v0.75.86 (Phase D + Phase E)：跨节点 Union merge 工具。
///
/// 把多个 `Type` 合并成 `Type::Union`，自动平展嵌套 Union + `Any` 短路。
///
/// **Phase E 签名变更**：参数从 `&[Type]` 改为 `&[(Span, Type)]`——
/// 每个 arm 携带自身 span，让错误诊断能精确定位到出错 arm（之前
/// `span: Span` 参数在 `join_types` 内部 unused 丢弃；现在 span 来源
/// 于每个 arm 的实际位置）。
///
/// 行为：
///   - 空切片 → `Type::Union(vec![])`（"any element type"占位）
///   - 单个类型 → 该类型本身
///   - 多类型 → `Type::Union(vec![t1, t2, ...])`（去重：单成员时退化为成员）
///   - 嵌套 `Union(a, Union(b, c))` → `Union(a, b, c)`（平展）
///   - 含 `Any` → `Type::Union(vec![])`（Any 是 top type，合并结果即"any"）
///
/// 设计参考：
///   - HM `infer_if` 无 else 时 `Union(vec![then, Nil])`（唯一手工构造点）
///   - Phase D 把这种「手工 Union 构造」抽象为 helper，给双向
///     `pre_check_witness` 的 Match/If 分支 join 用。
///   - Phase E 加 arm span 支持错误定位（之前 span 整体作为 outer 参数）。
///
/// 应用：
///   - Match arm body join → `join_types(&arm_pairs, outer_span)`
///   - 后续 If-else result join（独立 commit）→ `join_types(&[then_pair, else_pair], span)`
///
/// 参数语义：
///   - `arms`：每个元素 `(arm_span, arm_body_inferred_type)`。
///     arm_span 来自 `MirWitness::span`（Phase E 错误定位用）
///   - `outer_span`：整个 Match 节点的 span（fallback——当无 arm span 时用）
///
/// v0.75.96: 抽离到 [`crate::typeck::hm::util::join_types`] 与
/// [`crate::typeck::hm::util::check_union`]——双向定型内部 helper。
/// 检查是否是已知的内置类型名（大小写不敏感）
pub fn is_known_type(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "string"
            | "char"
            | "float"
            | "bool"
            | "nil"
            | "list"
            | "dict"
            | "task"
            | "closure"
            | "conversation"
            | "stream"
            | "ai_config"
            | "ai_result"
            | "ai_error"
            | "ai_module"
            | "router"
            | "http_request"
            | "http_response"
            | "mcp_server"
            | "result"
            | "atom"
            | "compose"
            | "partial"
            | "macro"
            | "any"
    ) || lower.starts_with("list<")
        || lower.starts_with("dict<")
        || lower.starts_with("result<")
        || lower.starts_with("dyn ")
}

/// v0.x: 在顶层（不进入嵌套 `<...>`）按 ',' 分割字符串。
/// 返回 `Some((head, tail))`；若找不到顶层 ',' 则 `None`。
/// 例：`"string, list<int>"` → `Some(("string", " list<int>"))`
fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

/// 类型错误 + 位置 + 修复建议（v0.05）
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// 期望的类型（可选）
    pub expected: Option<String>,
    /// 实际的类型（可选）
    pub actual: Option<String>,
    /// 修复建议（可选）
    pub hint: Option<String>,
}

impl TypeError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span 构造 (line + column)
    pub fn from_span(span: &Span, message: impl Into<String>) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span + 详情构造
    pub fn from_span_with_detail(
        span: &Span,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 完整构造：定位 + 期望 + 实际 + 修复建议
    pub fn with_detail(
        line: usize,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 加修复建议
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 格式化错误信息（含修复建议）
pub fn format_error(err: &TypeError) -> String {
    let mut s = if err.column > 0 {
        format!("Type error at line {}:{}", err.line, err.column)
    } else {
        format!("Type error at line {}", err.line)
    };
    s.push_str(&format!(": {}", err.message));
    if let (Some(exp), Some(act)) = (&err.expected, &err.actual) {
        s.push_str(&format!("\n  expected: {}", exp));
        s.push_str(&format!("\n  actual:   {}", act));
    }
    if let Some(hint) = &err.hint {
        s.push_str(&format!("\n  hint:     {}", hint));
    }
    s
}

// ===================================================================
// 符号表
// ===================================================================

/// 多 scope 嵌套的变量类型表
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    scopes: Vec<HashMap<String, Type>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 当前 scope 定义变量
    pub fn define(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    /// 沿作用域链查找；找不到返回 Any
    pub fn lookup(&self, name: &str) -> Type {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
        Type::Union(vec![])
    }
}

// ===================================================================
// v0.55: MirExpr-based type checking (V3 pipeline)
// ===================================================================

// Re-exported from `check_mir` — the single entry point for HM inference.
pub use check_mir::check_program_mir;
pub use check_mir::check_program_mir_with_types;
