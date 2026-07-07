//! v0.75.70: HM 类型推断 builtin 类型 — 自 hm/mod.rs 拆出（D6 单文件惯例）。
//! builtin_callee_ty（按名字查 callee 类型）+ builtin_type（op 类型）。

use super::*;

impl HMInference {
    pub(super) fn builtin_callee_ty(&mut self, name: &str) -> Option<Type> {
        // v0.55: prefer the canonical dispatch registry for the
        // canonical arity / return type, but mint fresh type variables
        // for every parameter so the HM unifier can still infer
        // concrete argument types instead of being pinned to a Union
        // annotation.
        if let Some(sig) = crate::typeck::dispatch::lookup_builtin(name) {
            let param_count = sig.params.len();
            let param_types: Vec<Type> = (0..param_count).map(|_| self.fresh_type_var()).collect();
            return Some(self.fresh_closure(param_types, sig.return_type.clone()));
        }
        match name {
            "print" => {
                let arg = self.fresh_type_var();
                let ret = Type::Nil;
                Some(self.fresh_closure(vec![arg], ret))
            }
            "len" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Int))
            }
            "str" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::String))
            }
            "int" => Some(self.fresh_closure(vec![Type::String], Type::Int)),
            "float" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Float))
            }
            "bool" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Bool))
            }
            "range" => {
                let a = self.fresh_type_var();
                let b = self.fresh_type_var();
                let c = self.fresh_type_var();
                let elem = self.fresh_type_var();
                Some(self.fresh_closure(vec![a, b, c], Type::List(Box::new(elem))))
            }
            _ => None,
        }
    }

    pub(super) fn builtin_type(&mut self, op: &BuiltinOp) -> Result<Type, Vec<TypeError>> {
        match op {
            BuiltinOp::Print => {
                let arg = self.fresh_type_var();
                Ok(self.fresh_closure(vec![arg], Type::Nil))
            }
            BuiltinOp::Assert => Ok(self.fresh_closure(vec![Type::Bool], Type::Nil)),
            BuiltinOp::Not => Ok(self.fresh_closure(vec![Type::Bool], Type::Bool)),
            BuiltinOp::Length => {
                let arg = self.fresh_type_var();
                Ok(self.fresh_closure(vec![arg], Type::Int))
            }
        }
    }
}
