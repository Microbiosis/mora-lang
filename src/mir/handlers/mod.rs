//! v0.75.56: MirInst handler functions — split across submodules.
//!
//! Each submodule owns one logical family of handlers:
//! - values:       pure value instructions
//! - effects:      side-effect / algebraic-effects instructions
//! - definitions:   type/trait/impl/skill/prompt/document definitions
//! - runtime:      orchestrate / file I/O / transaction / private helpers
//! - control:      control flow + match / quasiquote

mod control;
mod definitions;
mod effects;
mod runtime;
mod values;

use crate::value::Value;

/// What the linear interpreter should do after a handler runs.
#[derive(Debug)]
pub enum Flow {
    /// Advance pc by 1 (normal).
    Continue,
    /// Jump to the given label.
    Jump(usize),
    /// Return from the function with the given value.
    Return(Value),
    /// v0.70: Vote to halt. In Pregel context, the current agent signals
    /// "I'm done — don't reschedule me unless someone sends me a message."
    /// In linear context, behaves like Return.
    Halt(Option<Value>),
}

pub type HandlerResult = Result<Flow, String>;

// Re-export all public handler functions so callers can continue using
// `handlers::h_xxx` and `handlers::Flow` / `handlers::HandlerResult`.
pub use self::control::{
    h_break, h_continue, h_halt, h_jump, h_jump_if, h_jump_if_not, h_match_expr, h_quasiquote,
    h_return,
};
pub use self::definitions::{h_impl_def, h_skill_def, h_trait_def};
pub use self::effects::{
    h_app_def, AppDefArgs, h_assign, h_define, h_enum_def, h_handle, h_import,
    h_macro_def, h_model_def, h_msg_def, h_perform, h_struct_def, h_type_alias,
    h_update_def, h_with_config,
};
pub use self::runtime::{
    h_aggregate, h_append_file, h_document_section, h_eval, h_load, h_observe,
    h_orchestrate, h_prompt_section, h_read_bytes_file, h_read_file, h_save, h_send, h_span,
    h_transaction, h_worker, h_write_bytes_file, h_write_file,
};
pub use self::values::{
    h_binary_op, h_call, h_closure, h_const, h_dict_lit, h_dyn_trait, h_index, h_index_assign,
    h_list_lit, h_method_call, h_pipe, h_prompt, h_var,
};

// v0.75.56: MirInst metadata + dispatch — kept in inst.rs, re-exported here
// so callers can continue using `handlers::dispatch`.
pub use crate::mir::inst::dispatch;
