//! v0.48.0: Plan — real-time checklist with state transitions (pi-agent §1.11)
//!
//! 灵感: pi-agent `packages/coding-agent/src/tools/planning.ts` update_plan
//! - 工具支持 ⬜/🔄/✅ emoji 状态
//! - "When you start working on a step, mark it as in_progress (🔄)"
//! - "When you complete a step, mark it as completed (✅)"
//!
//! v0.48.0 Mora adaptation:
//! - `Plan` struct: ordered list of `PlanStep { id, text, status }`
//! - `StepStatus` enum: `Pending` (⬜) / `InProgress` (🔄) / `Done` (✅)
//! - `update([{id, status}])` — 增量更新 (id match, status patch)
//! - `complete_count()` / `pending_count()` helpers

use std::collections::HashMap;

/// v0.48.0: Step status (pi-agent emoji → Mora enum)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// ⬜ pending
    Pending,
    /// 🔄 in_progress
    InProgress,
    /// ✅ done
    Done,
}

impl StepStatus {
    /// 解析字符串 (支持 emoji + 英文)
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        match s {
            "pending" | "⬜" | "todo" => Some(Self::Pending),
            "in_progress" | "in-progress" | "🔄" | "doing" => Some(Self::InProgress),
            "done" | "✅" | "completed" | "finish" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Pending => "⬜",
            Self::InProgress => "🔄",
            Self::Done => "✅",
        }
    }
}

/// v0.48.0: Plan step
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: StepStatus,
}

impl PlanStep {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            status: StepStatus::Pending,
        }
    }

    pub fn with_status(mut self, status: StepStatus) -> Self {
        self.status = status;
        self
    }
}

/// v0.48.0: Plan (real-time checklist)
#[derive(Debug, Default, Clone)]
pub struct Plan {
    steps: Vec<PlanStep>,
    by_id: HashMap<String, usize>,
}

impl Plan {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个 step
    pub fn add_step(&mut self, step: PlanStep) -> Result<(), String> {
        if self.by_id.contains_key(&step.id) {
            return Err(format!("step id '{}' already exists", step.id));
        }
        let idx = self.steps.len();
        self.steps.push(step);
        self.by_id.insert(self.steps[idx].id.clone(), idx);
        Ok(())
    }

    /// 增量更新 steps (id match, status patch)
    /// Input: `&[PlanStepUpdate { id, status }]`
    /// 找不到的 id 返回 error
    pub fn update(&mut self, updates: &[(String, StepStatus)]) -> Result<(), String> {
        for (id, new_status) in updates {
            let idx = self
                .by_id
                .get(id)
                .ok_or_else(|| format!("step id '{}' not found", id))?;
            self.steps[*idx].status = *new_status;
        }
        Ok(())
    }

    /// 删除一个 step
    pub fn remove_step(&mut self, id: &str) -> Option<PlanStep> {
        let idx = self.by_id.remove(id)?;
        let step = self.steps.remove(idx);
        // 重建 by_id (idx 偏移)
        self.by_id.clear();
        for (i, s) in self.steps.iter().enumerate() {
            self.by_id.insert(s.id.clone(), i);
        }
        Some(step)
    }

    pub fn get(&self, id: &str) -> Option<&PlanStep> {
        let idx = self.by_id.get(id)?;
        self.steps.get(*idx)
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn complete_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count()
    }

    pub fn in_progress_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::InProgress)
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Pending)
            .count()
    }

    pub fn completion_ratio(&self) -> f64 {
        if self.steps.is_empty() {
            1.0
        } else {
            self.complete_count() as f64 / self.steps.len() as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_status_parses_emoji_and_text() {
        assert_eq!(StepStatus::parse("⬜"), Some(StepStatus::Pending));
        assert_eq!(StepStatus::parse("🔄"), Some(StepStatus::InProgress));
        assert_eq!(StepStatus::parse("✅"), Some(StepStatus::Done));
        assert_eq!(StepStatus::parse("pending"), Some(StepStatus::Pending));
        assert_eq!(StepStatus::parse("done"), Some(StepStatus::Done));
        assert_eq!(StepStatus::parse("garbage"), None);
    }

    #[test]
    fn step_status_emoji_roundtrip() {
        for s in [
            StepStatus::Pending,
            StepStatus::InProgress,
            StepStatus::Done,
        ] {
            assert_eq!(StepStatus::parse(s.emoji()), Some(s));
        }
    }

    #[test]
    fn plan_add_step() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("1", "First step")).unwrap();
        plan.add_step(PlanStep::new("2", "Second step")).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.get("1").unwrap().text, "First step");
    }

    #[test]
    fn plan_add_duplicate_id_errors() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("1", "A")).unwrap();
        let err = plan.add_step(PlanStep::new("1", "B")).unwrap_err();
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn plan_update_status() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("a", "A")).unwrap();
        plan.add_step(PlanStep::new("b", "B")).unwrap();
        plan.update(&[
            ("a".to_string(), StepStatus::Done),
            ("b".to_string(), StepStatus::InProgress),
        ])
        .unwrap();
        assert_eq!(plan.get("a").unwrap().status, StepStatus::Done);
        assert_eq!(plan.get("b").unwrap().status, StepStatus::InProgress);
        assert_eq!(plan.complete_count(), 1);
        assert_eq!(plan.in_progress_count(), 1);
        assert_eq!(plan.pending_count(), 0);
    }

    #[test]
    fn plan_update_unknown_id_errors() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("a", "A")).unwrap();
        let err = plan
            .update(&[("ghost".to_string(), StepStatus::Done)])
            .unwrap_err();
        assert!(err.contains("not found"), "got: {}", err);
    }

    #[test]
    fn plan_remove_step_reindexes() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("1", "A")).unwrap();
        plan.add_step(PlanStep::new("2", "B")).unwrap();
        plan.add_step(PlanStep::new("3", "C")).unwrap();
        plan.remove_step("2");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.get("1").unwrap().text, "A");
        assert_eq!(plan.get("3").unwrap().text, "C");
    }

    #[test]
    fn plan_completion_ratio() {
        let mut plan = Plan::new();
        plan.add_step(PlanStep::new("a", "A")).unwrap();
        plan.add_step(PlanStep::new("b", "B")).unwrap();
        plan.add_step(PlanStep::new("c", "C")).unwrap();
        plan.add_step(PlanStep::new("d", "D")).unwrap();
        plan.update(&[
            ("a".to_string(), StepStatus::Done),
            ("b".to_string(), StepStatus::Done),
        ])
        .unwrap();
        assert_eq!(plan.completion_ratio(), 0.5);
    }

    #[test]
    fn empty_plan_completion_ratio_is_one() {
        let plan = Plan::new();
        assert_eq!(plan.completion_ratio(), 1.0);
    }
}
