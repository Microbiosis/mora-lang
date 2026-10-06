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
                // v0.104.6 D409：**补沙箱守卫**（D408 时做不了，现在可以了）。
                //
                // `file.rs` 早有明文规则：「新增带路径的入口时，`check_path`
                // 不是「惯例」而是**义务**」—— 本入口此前**违反**了它：
                // 直接 `load_file(&path)` 读**任意**调用方给的路径。
                //
                // ## D408 为什么没能修
                //
                // D408 加过这个守卫，门禁立刻打出 1 失败（跨盘 SKILL.md 被
                // `sandbox denied`）。根因在 `permissive()`：`fs_root = "/"`
                // 会被 `canonicalize` 压成**当前盘**，`/` 这个「无限制」哨兵
                // 根本没被当哨兵 ⇒ 「不限制」实际是「只能访问当前盘」。
                // 那时「`permissive()` 该是什么」是**未定的产品语义**，
                // 所以守卫被全量回退，只报告。
                //
                // ## 现在为什么能修
                //
                // D409 已把 `/` 定为**显式哨兵**（`is_unrestricted`），
                // `permissive()` 现在真的「全路径、无限制」—— 这与它的 doc
                // 和 `docs/mora-spec.md` 17.1「当前版本**无沙箱**」都对齐了。
                //
                // ⇒ 本守卫在**默认策略下是 no-op**（不收紧任何现有功能），
                // 而在配置了**限制性** `fs_root` 时，它让 `skill.load`
                // 与 `file.*` **权限面一致**，堵掉绕过。
                self.sandbox
                    .sandbox
                    .check_path(&path_str)
                    .map_err(|e| format!("skill.load: sandbox denied '{}': {}", path_str, e))?;
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
