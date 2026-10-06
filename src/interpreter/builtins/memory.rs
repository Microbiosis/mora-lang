//! v0.75.54: memory.* builtin 实现 — P7 拆 domain 后补全：markdown 辅助函数
//! (markdown_memory_dir/remember/recall/list + 日期工具) 与 tests_v0431_memory_bus
//! (memory 部分) 从 builtins/mod.rs 迁入。语义与拆分前完全一致（纯搬移）。

use super::*;
use crate::value::Value;

/// 获取 markdown memory 根目录 (~/.mora/memory/)
/// v0.43.1: 优先用 Interpreter 字段 (test isolation); fallback 到 env var / home dir
fn markdown_memory_dir(override_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    if let Ok(custom) = std::env::var("MORA_MEMORY_DIR") {
        return std::path::PathBuf::from(custom);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mora").join("memory")
}

/// 当天日期 (YYYY-MM-DD)
fn today_date_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简化: 用 UNIX 秒转日期 (假设 UTC)
    // 1970-01-01 是周四, 用 Zeller 公式的一个变体
    let days = (secs / 86400) as i64;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // 从 1970-01-01 起算
    let mut year = 1970i32;
    let mut remaining = days;
    loop {
        let leap = is_leap(year);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        year += 1;
    }
    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            month = i;
            break;
        }
        remaining -= md;
    }
    (year, (month + 1) as u32, (remaining + 1) as u32)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// v0.43.1: remember(category, text) — 追加到 ~/.mora/memory/YYYY-MM-DD.md
/// 文件格式:
/// ```text
/// # YYYY-MM-DD
///
/// ## {category}
///
/// - {text 的第一行}
///   {text 的续行，每行缩进两格}
///
/// ## {other_category}
///
/// - {text}
/// ```
///
/// **续行必须缩进**（v0.104.6 D220）：多行文本的续行若原样写出，
/// `recall_markdown` 只收集以 `- ` 开头的行 ⇒ 其余各行**静默丢失**。
/// 缩进两格同时让续行既不像新 bullet 也不像新 `## ` 段，
/// 于是文本里本来就有 `- x` / `## y` 的行也不会被误判。
fn remember_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
    text: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let dir = markdown_memory_dir(override_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", today_date_string()));

    // 读取现有内容, 决定是否要新建 section
    let existing = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut new_content = if existing.is_empty() {
        format!("# {}\n\n", today_date_string())
    } else {
        existing
    };

    let section_header = format!("## {}", category);
    let bullet = format!("- {}\n", indent_continuation(text));
    // v0.104.6 D221：必须**按整行**精确匹配 `## {category}`，
    // 且追加到**该段末尾**（下一个 `## ` 行之前），而不是文件末尾。
    //
    // 修前是 `new_content.contains(&section_header)` —— 整文件**子串**匹配：
    //
    // ① 前缀冲突：`remember("notes-archive", …)` 之后
    //    `contains("## notes")` 为**真** ⇒ `remember("notes", …)` 的条目被
    //    追加到 `notes-archive` 段里，而 `## notes` 段**从未创建**：
    //        recall_markdown("notes")          = 空
    //        recall_markdown("notes-archive") = 两条都在
    //    零诊断、exit 0。
    // ② 段不在文件末尾时：追加到**文件末尾**会落进**下一个**段 ——
    //    `remember("a")` `remember("b")` `remember("a")` 三条即触发。
    match find_section(&new_content, &section_header) {
        Some(pos) => {
            let at = section_end(&new_content, pos);
            new_content.insert_str(at, &bullet);
        }
        None => {
            new_content.push_str(&format!("\n{}\n\n{}", section_header, bullet));
        }
    }

    // 写回
    let mut f = std::fs::File::create(&path)?;
    f.write_all(new_content.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// 找 `## {name}` **整行**的字节偏移（不做子串匹配）。
fn find_section(content: &str, name: &str) -> Option<usize> {
    let mut off = 0usize;
    for line in content.split_inclusive('\n') {
        if line.trim_end() == name {
            return Some(off);
        }
        off += line.len();
    }
    None
}

/// 该 section 的**结束**偏移：下一个 `## ` 行之前；没有则到文件末尾。
fn section_end(content: &str, header_pos: usize) -> usize {
    match content[header_pos..].find("\n## ") {
        Some(i) => header_pos + i + 1,
        None => content.len(),
    }
}

/// 多行文本的续行缩进两格。
fn indent_continuation(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\n  ")
}

/// v0.43.1: recall_markdown(category) — 读所有 markdown 文件, 找 ## category 段, 拼接 bullets
fn recall_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
) -> std::io::Result<String> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(String::new());
    }

    let mut out = String::new();

    // 按日期排序读所有 .md
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        // 找到 `## {category}` 段，收集到下一个 `## ` 或文件末尾。
        //
        // v0.104.6 D220/D222：条目的**续行**（缩进两格）也要收进来，
        // 且只剥**一个** `- ` —— 修前用 `trim_start_matches("- ")`
        // （剥掉**所有**前导 `- `），于是 `remember("d", "- x")`
        // 存的是 `- - x`、读回却变成 `x`：**前缀被吃掉**。
        let mut in_section = false;
        let mut cur: Option<String> = None;
        for line in content.lines() {
            if let Some(header) = line.strip_prefix("## ") {
                flush_entry(&mut cur, &mut out);
                in_section = header.trim() == category.trim();
                continue;
            }
            if !in_section {
                cur = None;
                continue;
            }
            if let Some(item) = line.strip_prefix("- ") {
                flush_entry(&mut cur, &mut out);
                cur = Some(item.to_string());
            } else if let Some(cont) = line.strip_prefix("  ") {
                // 续行：拼回同一条目
                if let Some(buf) = cur.as_mut() {
                    buf.push('\n');
                    buf.push_str(cont);
                }
            } else {
                // 空行 / 其它 → 当前条目结束
                flush_entry(&mut cur, &mut out);
            }
        }
        flush_entry(&mut cur, &mut out);
    }

    Ok(out)
}

/// 把累积中的条目刷进输出（每条一行）。
fn flush_entry(cur: &mut Option<String>, out: &mut String) {
    if let Some(s) = cur.take() {
        out.push_str(&s);
        out.push('\n');
    }
}

/// v0.43.1: list_markdown_categories() — 列出所有 markdown 文件中出现过的 ## section 标题
fn list_markdown_categories(
    override_dir: Option<&std::path::Path>,
) -> std::io::Result<Vec<String>> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut seen = std::collections::BTreeSet::new();
    let entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("## ") {
                // 跳过子标题 (### 等), 只取 ## level
                seen.insert(rest.trim().to_string());
            }
        }
    }

    Ok(seen.into_iter().collect())
}

impl Interpreter {
    pub fn call_memory_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "store" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.store: requires key")?;
                // v0.104.6 D51：value 此前静默兜底成 `Nil`（`unwrap_or`），
                // 而**同一函数**里 key 那一侧是 `.ok_or(..)` 报错 —— 验证
                // 不对称。实测 `memory.store("k1")` 随后 `memory.recall("k1")`
                // 返回 `nil`，exit 0、零提示：用户以为存了，实际存进去的是空。
                //
                // 该 namespace 在 spec 里**零记载**、typeck 也**零签名**，
                // 故没有编译期 arity 兜底，只能在此处拦。
                let value = args
                    .get(1)
                    .cloned()
                    .ok_or("memory.store: requires a value")?;
                self.registry.memory_store.insert(key, value);
                Ok(Value::Nil)
            }
            "recall" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.recall: requires key")?;
                Ok(self
                    .registry
                    .memory_store
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Nil))
            }
            "search" => {
                let query = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.search: requires query")?;
                let query_lower = query.to_lowercase();
                let results: Vec<Value> = self
                    .registry
                    .memory_store
                    .iter()
                    .filter(|(k, _)| k.to_lowercase().contains(&query_lower))
                    .map(|(k, v)| {
                        let mut m = HashMap::new();
                        m.insert("key".to_string(), Value::String(k.clone()));
                        m.insert("value".to_string(), v.clone());
                        Value::Dict(m)
                    })
                    .collect();
                Ok(Value::List(results.into()))
            }
            "forget" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.forget: requires key")?;
                self.registry.memory_store.remove(&key);
                Ok(Value::Nil)
            }
            "clear" => {
                self.registry.memory_store.clear();
                Ok(Value::Nil)
            }
            "size" => Ok(Value::Float(self.registry.memory_store.len() as f64)),
            // v0.43.1: memory.remember(category, text) — markdown-backed persistent memory
            // Appends `text` under `## {category}` in ~/.mora/memory/YYYY-MM-DD.md
            // Returns: Bool(true) on success
            "remember" => {
                let category = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.remember: requires category")?;
                let text = args
                    .get(1)
                    .map(|v| v.to_string())
                    .ok_or("memory.remember: requires text")?;
                remember_markdown(
                    self.persist.markdown_memory_dir.as_deref(),
                    &category,
                    &text,
                )
                .map_err(|e| format!("memory.remember: {}", e))?;
                // 也写到 memory_store (key=category, value=text) 让 recall 能查到
                self.registry
                    .memory_store
                    .insert(format!("md:{}", category), Value::String(text));
                Ok(Value::Bool(true))
            }
            // v0.43.1: memory.recall_markdown(category) — read markdown entries for category
            // Returns: String with concatenated entries (empty if none)
            "recall_markdown" => {
                let category = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.recall_markdown: requires category")?;
                recall_markdown(self.persist.markdown_memory_dir.as_deref(), &category)
                    .map(Value::String)
                    .map_err(|e| format!("memory.recall_markdown: {}", e))
            }
            // v0.43.1: memory.list_markdown() — list all categories
            // Returns: List[String] of category names
            "list_markdown" => {
                list_markdown_categories(self.persist.markdown_memory_dir.as_deref())
                    .map(|cats| Value::List(cats.into_iter().map(Value::String).collect()))
                    .map_err(|e| format!("memory.list_markdown: {}", e))
            }
            "keys" => {
                // v0.104.6 D406：**按 key 排序**返回。
                //
                // 修前直接收集 `HashMap::keys()` ⇒ 迭代序由 `RandomState`
                // （**逐进程随机**）决定。实测同一脚本连跑 5 次：
                // ```text
                // [bravo, delta, echo, alpha, charlie]
                // [echo, alpha, bravo, delta, charlie]
                // [delta, alpha, charlie, echo, bravo]   …（5 次全不同）
                // ```
                // 而 `memory.keys()[0]`（「第一个键」）每次拿到**不同的键**。
                //
                // 这与 `method_dispatch.rs` 里 `dict.keys()` 的注释描述的是
                // **同一个危害**（「用户按 keys()[0] 取第一个键会拿到随机结果
                // —— 且**不报错**」）—— `dict` 侧已修并有
                // `tests/dict_determinism.rs` 守护，`memory` 侧**漏了**。
                // 本条把它补齐，口径与 `dict.keys()` / `mock.names()` 一致。
                let mut keys: Vec<String> = self.registry.memory_store.keys().cloned().collect();
                keys.sort();
                Ok(Value::List(keys.into_iter().map(Value::String).collect()))
            }
            "save" => {
                let path = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.save: requires path")?;
                let json = value_to_json(&Value::Dict(self.registry.memory_store.clone()));
                fs::write(&path, json).map_err(|e| format!("memory.save: {}", e))?;
                Ok(Value::Bool(true))
            }
            "load" => {
                let path = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.load: requires path")?;
                let content =
                    fs::read_to_string(&path).map_err(|e| format!("memory.load: {}", e))?;
                match json_to_value(&content) {
                    Ok(Value::Dict(map)) => {
                        self.registry.memory_store = map;
                        Ok(Value::Bool(true))
                    }
                    Ok(_) => Err("memory.load: file must contain a JSON object".to_string()),
                    Err(e) => Err(format!("memory.load: {}", e)),
                }
            }
            _ => Err(format!("memory has no method: {}", method)),
        }
    }
}

#[cfg(test)]
mod tests_v0431_memory_bus {
    use super::*;
    use crate::value::Value;

    /// v0.43.1: memory.remember / recall_markdown / list_markdown
    fn setup_temp_memory_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_md_mem_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn teardown_memory_dir(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn memory_remember_appends_to_markdown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("user_prefs".to_string()),
                    Value::String("likes Rust".to_string()),
                ],
            )
            .expect("remember");
        assert_eq!(result, Value::Bool(true));

        // 验证文件存在并包含内容
        let date = today_date_string();
        let md_path = dir.join(format!("{}.md", date));
        let content = std::fs::read_to_string(&md_path).unwrap();
        assert!(content.contains("# "));
        assert!(content.contains("## user_prefs"));
        assert!(content.contains("- likes Rust"));

        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_remember_appends_to_existing_section() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("first entry".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("second entry".to_string()),
                ],
            )
            .unwrap();

        let date = today_date_string();
        let content = std::fs::read_to_string(dir.join(format!("{}.md", date))).unwrap();
        // 只有一个 ## cat section
        assert_eq!(content.matches("## cat").count(), 1);
        assert!(content.contains("- first entry"));
        assert!(content.contains("- second entry"));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_text() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("notes".to_string()),
                    Value::String("remember this".to_string()),
                ],
            )
            .unwrap();
        let recalled = interp
            .call_memory_method("recall_markdown", &[Value::String("notes".to_string())])
            .expect("recall 调用应成功");
        match recalled {
            Value::String(s) => assert!(s.contains("remember this"), "got: {}", s),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_empty_for_unknown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method("recall_markdown", &[Value::String("nope".to_string())])
            .expect("recall 调用应成功");
        assert_eq!(result, Value::String(String::new()));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_list_markdown_lists_categories() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("a".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("b".to_string()),
                    Value::String("y".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_memory_method("list_markdown", &[])
            .expect("list 调用应成功");
        match list {
            Value::List(items) => {
                let cats: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(cats.contains(&"a".to_string()));
                assert!(cats.contains(&"b".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_after_remember_syncs_to_memory_store() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.persist.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("k".to_string()),
                    Value::String("v".to_string()),
                ],
            )
            .unwrap();
        // 通过现有 recall (HashMap-backed) 应能查到
        let recalled = interp
            .call_memory_method("recall", &[Value::String("md:k".to_string())])
            .expect("recall 调用应成功");
        match recalled {
            Value::String(s) => assert_eq!(s, "v"),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }
}
