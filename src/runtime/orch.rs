//! v0.52 ADR-001: OrchRuntime — BC4 (agent orchestration registry)
//!
//! 从 Interpreter god object 抽出的 orchestration 状态容器，3 字段。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::plan::Plan;
use crate::refine::RefineRegistry;
use crate::skill::SkillRegistry;

#[derive(Debug, Clone)]
pub struct OrchRuntime {
    /// v0.48.0: Plans (multi-plan, keyed by name) for plan.update (pi-agent)
    pub plans: Arc<Mutex<HashMap<String, Plan>>>,
    /// v0.48.0: Refine session registry (multi-script, CLI-Anything /refine)
    pub refine_registry: Arc<Mutex<RefineRegistry>>,
    /// v0.46.0: Skill registry (MoraSkillSpec + dual registry, CLI-Anything pattern)
    /// Loaded from `~/.mora/skills/` or via builtin skill.load / skill.install
    pub skill_registry: Arc<Mutex<SkillRegistry>>,
}

impl Default for OrchRuntime {
    fn default() -> Self {
        Self {
            plans: Arc::new(Mutex::new(HashMap::new())),
            refine_registry: Arc::new(Mutex::new(RefineRegistry::new())),
            skill_registry: Arc::new(Mutex::new(SkillRegistry::new())),
        }
    }
}

impl OrchRuntime {
    /// 创建新 plan
    pub fn plan_create(&self, name: String) -> bool {
        let mut plans = self.plans.lock().expect("OrchRuntime plans poisoned");
        plans.insert(name, Plan::new());
        true
    }

    /// 添加 skill 到 registry，返回 skill 名称
    pub fn skill_register(&self, skill: crate::skill::MoraSkillSpec) -> String {
        let mut registry = self
            .skill_registry
            .lock()
            .expect("OrchRuntime skill_registry poisoned");
        let name = skill.name.clone();
        registry.register(skill);
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::MoraSkillSpec;

    #[test]
    fn plans_default_empty() {
        let orch = OrchRuntime::default();
        let plans = orch.plans.lock().expect("plans poisoned");
        assert!(plans.is_empty());
    }

    #[test]
    fn refine_registry_default() {
        let orch = OrchRuntime::default();
        let registry = orch
            .refine_registry
            .lock()
            .expect("refine_registry poisoned");
        let _ = &*registry; // 不 panic 即可
    }

    #[test]
    fn skill_registry_default() {
        let orch = OrchRuntime::default();
        let registry = orch.skill_registry.lock().expect("skill_registry poisoned");
        let _ = &*registry;
    }

    #[test]
    fn plan_create_inserts_entry() {
        let orch = OrchRuntime::default();
        assert!(orch.plan_create("test".to_string()));
        let plans = orch.plans.lock().expect("plans poisoned");
        assert!(plans.contains_key("test"));
    }

    #[test]
    fn skill_register_inserts_entry() {
        let orch = OrchRuntime::default();
        let skill = MoraSkillSpec {
            name: "s1".to_string(),
            description: "test skill".to_string(),
            trigger: None,
            body: String::new(),
            source: None,
        };
        let name = orch.skill_register(skill);
        assert_eq!(name, "s1");
        let registry = orch.skill_registry.lock().expect("skill_registry poisoned");
        assert!(registry.get("s1").is_some());
    }

    #[test]
    fn clone_shares_arc_registries() {
        let orch1 = OrchRuntime::default();
        let orch2 = orch1.clone();
        // Arc 共享：改一个应能影响另一个
        orch1
            .plans
            .lock()
            .expect("plans poisoned")
            .insert("shared".to_string(), Plan::new());
        let plans2 = orch2.plans.lock().expect("plans poisoned");
        assert!(plans2.contains_key("shared"));
    }
}
