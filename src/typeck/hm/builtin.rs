//! HM builtin callee signature resolution.
//!
//! Uses `crate::typeck::dispatch::lookup_builtin` as the canonical source
//! (respecting `variadic` and `min_arity`), plus higher-order / metaprogramming
//! builtins (`cons`, `car`, `cdr`, `apply`, `curry`, `uncurry`, `into`, `swap`,
//! `tail`, `atom`, `deref`, `type_of`, `methods_of`, `gensym`, `is_instance`,
//! `eval`, `quote`, `read`).

use super::*;

#[derive(Debug, Clone)]
pub(super) struct BuiltinCallSpec {
    pub params: Vec<Type>,
    pub min_arity: usize,
    pub variadic: bool,
    pub return_type: Type,
}

fn curried_arrow(params: Vec<Type>, ret: Type) -> Type {
    let mut ty = ret;
    for p in params.into_iter().rev() {
        ty = Type::Arrow(
            Box::new(p),
            Box::new(ty),
            crate::mir::effect::EffectRow::Empty,
        );
    }
    ty
}

impl HMInference {
    pub(super) fn builtin_callee_sig(&mut self, name: &str) -> Option<BuiltinCallSpec> {
        if matches!(name, "math" | "stats" | "linalg") {
            return None;
        }
        if name == "channel" {
            let elem = self.fresh_type_var();
            return Some(BuiltinCallSpec {
                params: vec![Type::Union(vec![Type::Int, Type::Float, Type::Nil])],
                min_arity: 0,
                variadic: false,
                return_type: Type::Channel(Box::new(elem)),
            });
        }
        if let Some(sig) = crate::typeck::dispatch::lookup_builtin(name) {
            let min_arity = sig.min_arity.unwrap_or(sig.params.len());
            let params: Vec<Type> = sig.params.into_iter().map(|(_, t)| t).collect();
            return Some(BuiltinCallSpec {
                params,
                min_arity,
                variadic: sig.variadic,
                return_type: sig.return_type,
            });
        }
        match name {
            "cons" => {
                let h = self.fresh_type_var();
                let t = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![h.clone(), t.clone()],
                    min_arity: 2,
                    variadic: false,
                    return_type: Type::Cons(Box::new(h), Box::new(t)),
                })
            }
            "tail" => {
                let path = self.fresh_type_var();
                let max = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![path, max],
                    min_arity: 2,
                    variadic: false,
                    return_type: Type::String,
                })
            }
            "curry" => {
                let f = self.fresh_type_var();
                let arity = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![f, arity],
                    min_arity: 1,
                    variadic: false,
                    return_type: Type::Any,
                })
            }
            "apply" => {
                let f = self.fresh_type_var();
                let arglist = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![f, arglist],
                    min_arity: 2,
                    variadic: false,
                    return_type: ret,
                })
            }
            "car" | "cdr" | "uncurry" => {
                let arg = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![arg],
                    min_arity: 1,
                    variadic: false,
                    return_type: ret,
                })
            }
            "into" => {
                let coll = self.fresh_type_var();
                let transform = self.fresh_type_var();
                let elem = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![coll, transform],
                    min_arity: 2,
                    variadic: false,
                    return_type: Type::List(Box::new(elem)),
                })
            }
            "swap" => {
                let a = self.fresh_type_var();
                let f = self.fresh_type_var();
                let ret = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![a, f],
                    min_arity: 2,
                    variadic: false,
                    return_type: ret,
                })
            }
            "type_of" => {
                let arg = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![arg],
                    min_arity: 1,
                    variadic: false,
                    return_type: Type::String,
                })
            }
            "atom" => {
                let arg = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![arg],
                    min_arity: 1,
                    variadic: false,
                    return_type: Type::Atom,
                })
            }
            "deref" => Some(BuiltinCallSpec {
                params: vec![Type::Atom],
                min_arity: 1,
                variadic: false,
                return_type: Type::Any,
            }),
            "methods_of" => {
                let arg = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![arg],
                    min_arity: 1,
                    variadic: false,
                    return_type: Type::List(Box::new(Type::String)),
                })
            }
            "gensym" => Some(BuiltinCallSpec {
                params: vec![],
                min_arity: 0,
                variadic: false,
                return_type: Type::String,
            }),
            "is_instance" => {
                let value = self.fresh_type_var();
                let name = self.fresh_type_var();
                Some(BuiltinCallSpec {
                    params: vec![value, name],
                    min_arity: 2,
                    variadic: false,
                    return_type: Type::Bool,
                })
            }
            "eval" => Some(BuiltinCallSpec {
                params: vec![Type::String],
                min_arity: 1,
                variadic: false,
                return_type: Type::Any,
            }),
            "quote" => Some(BuiltinCallSpec {
                params: vec![Type::Any],
                min_arity: 1,
                variadic: false,
                return_type: Type::Any,
            }),
            "read" => Some(BuiltinCallSpec {
                params: vec![Type::Union(vec![Type::String, Type::Any])],
                min_arity: 1,
                variadic: false,
                return_type: Type::Any,
            }),
            _ => None,
        }
    }

    pub(super) fn builtin_callee_ty(&mut self, name: &str) -> Option<Type> {
        let spec = self.builtin_callee_sig(name)?;
        Some(curried_arrow(spec.params, spec.return_type))
    }

    pub(super) fn builtin_type(&mut self, op: &BuiltinOp) -> Result<Type, Vec<TypeError>> {
        match op {
            BuiltinOp::Print => Ok(curried_arrow(vec![Type::Any], Type::Nil)),
            BuiltinOp::Assert => Ok(curried_arrow(vec![Type::Bool], Type::Nil)),
            BuiltinOp::Not => Ok(curried_arrow(vec![Type::Bool], Type::Bool)),
            BuiltinOp::Length => {
                let arg = self.fresh_type_var();
                Ok(curried_arrow(vec![arg], Type::Int))
            }
        }
    }
}
