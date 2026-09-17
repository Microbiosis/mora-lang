//! v0.84 Phase 5: Trait 注册条目与方法签名。
//!
//! 原先在 `runtime/types.rs` 中定义，但 `mir/host.rs` 和 `mir/handlers.rs`
//! 同时引用——造成 `mir/` → `runtime/` 的架构耦合。下沉到 `common/` 后，
//! `mir/` 和 `runtime/` 各自依赖 `common/`，互不依赖。
//!
//! 依赖关系（修复前）:
//!   mir/ → runtime/ (仅因 TraitInfo/TraitMethodSig)
//!
//! 依赖关系（修复后）:
//!   mir/ → common/ (TraitInfo/TraitMethodSig)
//!   runtime/ → common/ (TraitInfo/TraitMethodSig)
//!   mir/ 与 runtime/ 互不依赖
//!
//! 类型：
//! - `TraitInfo` — trait 注册条目（name + parents + methods）
//! - `TraitMethodSig` — trait 方法签名（name + params + return_type + has_self）
//! - `impl_method_key` / `default_impl_method_key` — impl 注册时的 key 构造

#[derive(Clone, Debug)]
pub struct TraitInfo {
    pub name: String,
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
}

/// v0.09: 注册 impl method 时用的 key（含泛型签名）
/// 格式: __impl_<Trait>_<TraitGen>_<ForType>_<ForGen>_<method>
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
/// 格式: __impl_<Trait>_<TraitGen>_<method>
pub fn default_impl_method_key(
    trait_name: &str,
    trait_generics: &[String],
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    format!("__impl_{}_{}_{}", trait_name, tg, method)
}
