//! v0.110.0: Trait 注册条目与方法签名（位于 `common/` 基础层，供 `parser_v3`、`mir`、`typeck` 与 `runtime` 共享）。
//!
//! 分层关系：
//!   `parser_v3` / `mir` / `typeck` / `runtime` → `common` (`TraitInfo` / `TraitMethodSig`)
//!
//! 类型：
//! - `TraitInfo` — trait 注册条目（name + parents + methods）
//! - `TraitMethodSig` — trait 方法签名（name + params + return_type + has_self）
//! - `impl_method_key` / `default_impl_method_key` — impl 注册时的 key 构造

#[derive(Clone, Debug)]
pub struct TraitInfo {
    pub name: String,
    pub type_params: Vec<String>,
    pub parents: Vec<String>,
    pub methods: Vec<TraitMethodSig>,
}

#[derive(Clone, Debug)]
pub struct TraitMethodSig {
    pub name: String,
    /// v0.08.5: 参数名 + 可选类型标注 `(name, Some("int"))`
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    /// 第一个参数是否为 `self`（决定 dispatch 时是否传 receiver.clone()）
    pub has_self: bool,
    /// Stage 3C: 该方法是否在 trait 定义中提供默认实现体
    pub has_default: bool,
}

/// v0.09: 注册 impl method 时用的 key（含泛型签名）
/// 格式: __impl_`<Trait>`_`<TraitGen>`_`<ForType>`_`<ForGen>`_`<method>`
pub fn impl_method_key(
    trait_name: &str,
    trait_generics: &[String],
    for_type: &str,
    for_generics: &[String],
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    let fg = for_generics.join(",");
    format!(
        "__impl_{}_{}_{}_{}_{}",
        trait_name, tg, for_type, fg, method
    )
}

/// v0.09: 默认实现的 key（self 类型 = trait 名）
/// 格式: __impl_`<Trait>`_`<TraitGen>`_`<method>`
pub fn default_impl_method_key(
    trait_name: &str,
    trait_generics: &[String],
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    format!("__impl_{}_{}_{}", trait_name, tg, method)
}
