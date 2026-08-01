//! v0.53: Type Environment for HM Inference
//!
//! Manages variable-to-type mappings with let-generalization support

use std::collections::HashMap;

use crate::typeck::Type;

///  Type environment: maps variable names to their types (including polymorphic types)
#[derive(Debug, Clone)]
pub struct TypeEnv {
    /// Binding map: name → type (could be ∀-quantified)
    bindings: HashMap<String, Type>,

    /// Current level for scoping (higher in nested scopes)
    depth: usize,
}

impl TypeEnv {
    /// Create a new empty type environment
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            depth: 0,
        }
    }

    /// Add a binding to the environment
    pub fn add(&mut self, name: String, ty: Type) {
        self.bindings.insert(name, ty);
    }

    /// Get the type of a variable by name
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.bindings.get(name)
    }

    /// Check if a variable is bound
    pub fn contains(&self, name: &str) -> bool {
        self.bindings.contains_key(name)
    }

    /// Return free variables in the current environment
    pub fn free_variables(&self) -> Vec<char> {
        // Collect all type variable identifiers from all types
        let mut free_vars = Vec::new();

        for ty in self.bindings.values() {
            free_vars.extend(collect_type_vars(ty));
        }

        free_vars.sort();
        free_vars.dedup();
        free_vars
    }

    /// Enter a new scope (e.g., start of function body)
    pub fn enter_scope(&mut self) {
        self.depth += 1;
    }

    /// Exit the current scope
    pub fn exit_scope(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
        }
    }
}

impl Default for TypeEnv {
    fn default() -> Self {
        Self::new()
    }
}

///  Recursively collect all type variable identifiers from a type
fn collect_type_vars(ty: &Type) -> Vec<char> {
    match ty {
        Type::TypeVar(c) => vec![*c],
        Type::List(elem_ty) => collect_type_vars(elem_ty),
        Type::Dict(key_ty, value_ty) => {
            let mut vars = collect_type_vars(key_ty);
            vars.extend(collect_type_vars(value_ty));
            vars
        }
        Type::Result_(ok_ty, err_ty) => {
            let mut vars = Vec::new();
            vars.extend(collect_type_vars(ok_ty));
            vars.extend(collect_type_vars(err_ty));
            vars
        }
        // v0.75.17: ForAll 内层仍是自由变量的来源（量化的变量自身是绑定，
        // 但内层嵌套的自由变量要计入 FV(Γ) 用于外层 let-generalization）。
        Type::ForAll(_, inner_ty) => collect_type_vars(inner_ty),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_add_get() {
        let mut env = TypeEnv::new();
        env.add("x".to_string(), Type::Int);
        env.add("y".to_string(), Type::String);

        assert_eq!(env.get("x"), Some(&Type::Int));
        assert_eq!(env.get("y"), Some(&Type::String));
        assert_eq!(env.get("z"), None);
    }

    #[test]
    fn test_free_variables_empty() {
        let env = TypeEnv::new();
        assert!(env.free_variables().is_empty());
    }

    #[test]
    fn test_free_variables_with_typevars() {
        let mut env = TypeEnv::new();
        env.add("f".to_string(), Type::Closure);

        let vars = env.free_variables();
        assert!(vars.is_empty());
    }
}
