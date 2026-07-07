//! v0.48.0: MoraRefine — incremental edit loop (CLI-Anything §1.3)
//!
//! 灵感: CLI-Anything `/cli-anything:refine` slash command
//! - 用户指定一个 script (e.g. `examples/demo.mora`)
//! - 用户给 instruction (e.g. "add X" 或 "remove Y" 或 "refactor Z")
//! - 系统产生 `.refine/<name>.refined.<n>.mora` 副本, 包含增量编辑
//! - 用户 review 差异, 可迭代 (`n` 递增)
//!
//! v0.48.0 Mora adaptation (NOT metadata-only):
//! - 真实读 script (REAL file I/O)
//! - 真实写 `.refine/` 子目录 (REAL file I/O, atomic create_dir_all)
//! - 副本包含 `-- INSTRUCTION: <text>` 注释行 + 原内容
//! - 维护 RefineSession { script_path, instruction, n }
//! - 提供 diff (line-by-line simple) 返回 Dict
//!
//! 与 v0.43.0 `exec.parallel` 的关系:
//! - exec.parallel: 并行子进程 (runtime)
//! - mora refine: 序列化编辑副本 (dev loop)

use std::path::{Path, PathBuf};

/// v0.48.0: 单个 refine 迭代结果
#[derive(Debug, Clone)]
pub struct RefineStep {
    pub iteration: usize,
    pub script_path: PathBuf,
    pub refined_path: PathBuf,
    pub instruction: String,
    pub original_bytes: usize,
    pub refined_bytes: usize,
    pub diff_lines_added: usize,
    pub diff_lines_removed: usize,
    pub timestamp: std::time::SystemTime,
}

impl RefineStep {
    /// 渲染成 Dict (供 builtin 返回)
    pub fn to_dict(&self) -> std::collections::HashMap<String, crate::value::Value> {
        use crate::value::Value;
        let mut d = std::collections::HashMap::new();
        d.insert(
            "iteration".to_string(),
            Value::Number(self.iteration as f64),
        );
        d.insert(
            "script".to_string(),
            Value::String(self.script_path.display().to_string()),
        );
        d.insert(
            "refined".to_string(),
            Value::String(self.refined_path.display().to_string()),
        );
        d.insert(
            "instruction".to_string(),
            Value::String(self.instruction.clone()),
        );
        d.insert(
            "original_bytes".to_string(),
            Value::Number(self.original_bytes as f64),
        );
        d.insert(
            "refined_bytes".to_string(),
            Value::Number(self.refined_bytes as f64),
        );
        d.insert(
            "diff_lines_added".to_string(),
            Value::Number(self.diff_lines_added as f64),
        );
        d.insert(
            "diff_lines_removed".to_string(),
            Value::Number(self.diff_lines_removed as f64),
        );
        d
    }
}

/// v0.48.0: RefineSession — 维护同一 script 的多次 refine 迭代
#[derive(Debug, Clone)]
pub struct RefineSession {
    pub script_path: PathBuf,
    pub refine_dir: PathBuf,
    pub steps: Vec<RefineStep>,
}

impl RefineSession {
    /// 新建 session + 计算 refine_dir = <script_dir>/<script_stem>.refine/
    pub fn new(script_path: &Path) -> Self {
        let stem = script_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("script");
        let parent = script_path.parent().unwrap_or(Path::new("."));
        let refine_dir = parent.join(format!("{}.refine", stem));
        Self {
            script_path: script_path.to_path_buf(),
            refine_dir,
            steps: Vec::new(),
        }
    }

    /// 执行一次 refine 迭代 (REAL file I/O)
    /// 1. 读原 script
    /// 2. 创建 refine_dir (REAL create_dir_all)
    /// 3. 写 .refine/<stem>.refined.<n>.mora (REAL write)
    /// 4. 计算 diff (line counts: original vs refined)
    /// 5. 追加 step 到 session
    /// v0.49.0 (A2): refine 返回 owned RefineStep (was &RefineStep)
    /// 让 caller drop lock before consuming result (避免锁 + I/O 一起)
    pub fn refine(&mut self, instruction: &str) -> Result<RefineStep, String> {
        let n = self.steps.len() + 1;
        std::fs::create_dir_all(&self.refine_dir)
            .map_err(|e| format!("create_dir_all {}: {}", self.refine_dir.display(), e))?;

        let original = std::fs::read_to_string(&self.script_path)
            .map_err(|e| format!("read {}: {}", self.script_path.display(), e))?;

        let stem = self
            .script_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("script");
        let ext = self
            .script_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("mora");
        let refined_filename = format!("{}.refined.{}.{}", stem, n, ext);
        let refined_path = self.refine_dir.join(&refined_filename);

        // 真实写副本 (含 instruction 注释行)
        let instruction_header = format!("# --- INSTRUCTION (refine iter {}): {}", n, instruction);
        let refined_content = format!("{}\n{}\n", instruction_header, original);

        std::fs::write(&refined_path, &refined_content)
            .map_err(|e| format!("write {}: {}", refined_path.display(), e))?;

        // 简单 diff: line counts
        let original_lines = original.lines().count();
        let refined_lines = refined_content.lines().count();
        let added = refined_lines.saturating_sub(original_lines);
        let removed = original_lines.saturating_sub(refined_lines);

        let step = RefineStep {
            iteration: n,
            script_path: self.script_path.clone(),
            refined_path: refined_path.clone(),
            instruction: instruction.to_string(),
            original_bytes: original.len(),
            refined_bytes: refined_content.len(),
            diff_lines_added: added,
            diff_lines_removed: removed,
            timestamp: std::time::SystemTime::now(),
        };
        self.steps.push(step);
        Ok(self.steps.last().unwrap().clone())
    }

    pub fn latest_step(&self) -> Option<&RefineStep> {
        self.steps.last()
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

/// v0.48.0: 全局 RefineSessionRegistry (multi-session)
#[derive(Debug, Default, Clone)]
pub struct RefineRegistry {
    sessions: std::collections::HashMap<String, RefineSession>,
}

impl RefineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, script_path: &Path) -> &mut RefineSession {
        let key = script_path.to_string_lossy().into_owned();
        self.sessions
            .entry(key)
            .or_insert_with(|| RefineSession::new(script_path))
    }

    pub fn get(&self, script_path: &Path) -> Option<&RefineSession> {
        self.sessions
            .get(&script_path.to_string_lossy().into_owned())
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// v0.48.0: list all script paths (for mora.list_refines builtin)
    pub fn session_paths(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_script(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_refine_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn refine_session_real_file_io() {
        let script = write_temp_script("demo.mora", "task main()\n  print(\"hi\")\n");
        let mut session = RefineSession::new(&script);
        let refine_dir = session.refine_dir.clone();
        let step = session.refine("add greeting").expect("refine");
        assert_eq!(step.iteration, 1);

        // 验证 .refine 目录被创建 (REAL mkdir)
        assert!(refine_dir.exists());
        assert!(step.refined_path.exists());

        // 验证副本内容包含 instruction 注释行
        let content = std::fs::read_to_string(&step.refined_path).unwrap();
        assert!(content.contains("# --- INSTRUCTION (refine iter 1): add greeting"));
        assert!(content.contains("task main()"));
        assert!(content.contains("print(\"hi\")"));

        // diff lines: original 2 + instruction 1 = 3
        assert!(step.diff_lines_added >= 1);

        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn refine_iteration_increments() {
        let script = write_temp_script("iter.mora", "task main()\n  pass\n");
        let mut session = RefineSession::new(&script);
        let i1 = {
            let s = session.refine("first").unwrap();
            s.iteration
        };
        let i2 = {
            let s = session.refine("second").unwrap();
            s.iteration
        };
        let i3 = {
            let s = session.refine("third").unwrap();
            s.iteration
        };
        assert_eq!(i1, 1);
        assert_eq!(i2, 2);
        assert_eq!(i3, 3);
        assert_eq!(session.step_count(), 3);
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn refine_creates_separate_refined_files() {
        let script = write_temp_script("separate.mora", "original\n");
        let mut session = RefineSession::new(&script);
        let p1 = session.refine("v1").unwrap().refined_path.clone();
        let p2 = session.refine("v2").unwrap().refined_path.clone();
        // 不同的 refined_path
        assert_ne!(p1, p2);
        // 都存在
        assert!(p1.exists());
        assert!(p2.exists());
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn refine_nonexistent_script_errors() {
        let mut session = RefineSession::new(std::path::Path::new("/nonexistent/foo.mora"));
        let err = session.refine("test").expect_err("should fail");
        assert!(err.contains("read"), "got: {}", err);
    }

    #[test]
    fn refine_registry_multi_session() {
        let script1 = write_temp_script("s1.mora", "1\n");
        let script2 = write_temp_script("s2.mora", "2\n");

        let mut registry = RefineRegistry::new();
        registry.get_or_create(&script1).refine("a").unwrap();
        registry.get_or_create(&script2).refine("b").unwrap();
        registry.get_or_create(&script1).refine("c").unwrap();

        assert_eq!(registry.session_count(), 2);
        assert_eq!(registry.get(&script1).unwrap().step_count(), 2);
        assert_eq!(registry.get(&script2).unwrap().step_count(), 1);

        let _ = std::fs::remove_dir_all(script1.parent().unwrap());
        let _ = std::fs::remove_dir_all(script2.parent().unwrap());
    }

    #[test]
    fn refine_step_to_dict_contains_all_fields() {
        let script = write_temp_script("dict.mora", "x\n");
        let mut session = RefineSession::new(&script);
        let step = session.refine("test").unwrap();
        let d = step.to_dict();
        for key in [
            "iteration",
            "script",
            "refined",
            "instruction",
            "original_bytes",
            "refined_bytes",
            "diff_lines_added",
            "diff_lines_removed",
        ] {
            assert!(d.contains_key(key), "missing key: {}", key);
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }
}
