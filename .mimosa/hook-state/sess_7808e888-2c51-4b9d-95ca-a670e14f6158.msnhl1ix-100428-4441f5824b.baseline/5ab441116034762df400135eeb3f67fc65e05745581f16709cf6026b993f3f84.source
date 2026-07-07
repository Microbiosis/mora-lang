//! v0.75.51: skill.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_skill_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut reg = self
            .orch
            .skill_registry
            .lock()
            .expect("skill_registry poisoned");

        match method {
            "list" => {
                let names: Vec<String> = reg.list().into_iter().map(|s| s.name.clone()).collect();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            "find" => {
                let name = args.first().ok_or("skill.find: requires name")?.to_string();
                match reg.get(&name) {
                    Some(spec) => {
                        let mut d = std::collections::HashMap::new();
                        d.insert("name".to_string(), Value::String(spec.name.clone()));
                        d.insert(
                            "description".to_string(),
                            Value::String(spec.description.clone()),
                        );
                        d.insert(
                            "trigger".to_string(),
                            match &spec.trigger {
                                Some(t) => Value::String(t.clone()),
                                None => Value::Nil,
                            },
                        );
                        d.insert("body".to_string(), Value::String(spec.body.clone()));
                        d.insert(
                            "source".to_string(),
                            match &spec.source {
                                Some(p) => Value::String(p.display().to_string()),
                                None => Value::Nil,
                            },
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            "load" => {
                // 真正从文件加载 SKILL.md (REAL file I/O)
                let path_str = args.first().ok_or("skill.load: requires path")?.to_string();
                let path = std::path::PathBuf::from(&path_str);
                let spec = crate::skill::MoraSkillSpec::load_file(&path)
                    .map_err(|e| format!("skill.load: {}", e))?;
                reg.register(spec);
                Ok(Value::Bool(true))
            }
            "install" => {
                // 从 content 字符串合成 skill
                if args.len() < 2 {
                    return Err("skill.install: requires 2 args (name, content)".to_string());
                }
                let name = args[0].to_string();
                let content = args[1].to_string();
                let mut spec = crate::skill::MoraSkillSpec::parse(&content, None)
                    .map_err(|e| format!("skill.install: {}", e))?;
                // 强制 name 覆盖 (allows `skill.install("alias", content)` 模式)
                spec.name = name.clone();
                reg.register(spec);
                Ok(Value::Bool(true))
            }
            "uninstall" => {
                let name = args
                    .first()
                    .ok_or("skill.uninstall: requires name")?
                    .to_string();
                let removed = reg.unregister(&name);
                Ok(Value::Bool(removed.is_some()))
            }
            "set_hub" => {
                let path = args
                    .first()
                    .ok_or("skill.set_hub: requires path")?
                    .to_string();
                reg.set_public_registry(std::path::PathBuf::from(&path));
                Ok(Value::Bool(true))
            }
            "refresh_hub" => {
                // 真正从 mora-public.json 重读
                let count = reg
                    .load_public_registry()
                    .map_err(|e| format!("skill.refresh_hub: {}", e))?;
                Ok(Value::Float(count as f64))
            }
            _ => Err(format!("skill.{}: unknown method", method)),
        }
    }
}
