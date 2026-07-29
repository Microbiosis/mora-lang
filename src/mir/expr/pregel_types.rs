//! Phase 2: Pregel Orchestration Native Types
//! 
//! 这是 MIR-native Pregel 引擎所需的类型定义。
//! Batch D3: 将这些类型移到 src/mir/expr.rs 中

use std::collections::HashMap;
use crate::common::Span;
use crate::value::Value;

// Import types from parent module
use super::{MirExpr, MirFunction, TypedMirExpr};

/// Pregel orchestration configuration (native MirExpr types)
#[derive(Debug, Clone)]
pub struct MirPregelConfig {
    pub agents: Vec<MirAgentDef>,
    pub edges: Vec<MirEdgeDef>,
    pub state_schema: Vec<MirStateChannel>,
    pub checkpoint: Option<String>,
    pub interrupt_points: Vec<MirInterruptPoint>,
    pub adjacency: HashMap<String, Vec<String>>,
}

/// Agent definition in a Pregel graph
#[derive(Debug, Clone)]
pub struct MirAgentDef {
    pub name: String,
    pub task_expr: MirExpr, // The expression that defines the agent's logic
    pub verify_expr: Option<MirExpr>, // Optional verification condition
    pub with_config: Option<MirExpr>, // Optional AI config expression
    pub task_body: MirFunction, // Pre-lowered task body
    pub task_mir_expr: Option<MirExpr>, // Alternative representation
}

/// Edge definition between agents
#[derive(Debug, Clone)]
pub struct MirEdgeDef {
    pub from: String, // Source agent name or "@start"
    pub to: String, // Target agent name or "@exit"
    pub condition_expr: Option<MirExpr>, // Optional edge condition
    pub condition_body: Option<MirFunction>, // Optional condition evaluation body
}

/// State channel in Pregel computation
#[derive(Debug, Clone)]
pub struct MirStateChannel {
    pub name: String,
    pub ty: String, // Type annotation (e.g., "Int", "List<Int>")
    pub reducer: MirReducerKind,
}

/// Interruption point configuration
#[derive(Debug, Clone, PartialEq)]
pub enum MirInterruptWhen {
    BeforeStep, // Interrupt before each step
    AfterStep, // Interrupt after each step
}

#[derive(Debug, Clone)]
pub struct MirInterruptPoint {
    pub when: MirInterruptWhen,
    pub label: usize, // Instruction index
}

/// Reducer kinds for Pregel state updates
#[derive(Debug, Clone, PartialEq)]
pub enum MirReducerKind {
    Last, // Last value wins
    Append, // Append to list
    Add, // Numeric addition
    Merge(MirExpr), // Custom merge expression (TODO: evaluate with run_mir)
    Sum, // Sum of all values (not yet implemented)
    Product, // Product of all values (not yet implemented)
    Concat, // String concatenation (not yet implemented)
    Custom(String), // User-defined custom reducer
}

// Note: MirExpr is imported from parent module - see imports below
