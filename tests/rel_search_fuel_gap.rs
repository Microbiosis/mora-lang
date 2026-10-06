//! v0.104.6 D397 —— `rel::search` **无搜索步数上界**：左递归规则让 `solve`
//! **永久挂死**，而 `solve N` 的 `limit` **挡不住**（否定轮 + 文档更正 + 一项待裁决）
//!
//! ## 现象（真实 CLI 实测，`solve` 无界与 `solve 2` **两者都挂死**）
//!
//! ```mora
//! rel loop2(x) loop2(x) end
//! solve 2 { loop2(?X) }
//! ```
//!
//! 20s 内不退出，**无报错、无输出**，只能手动杀进程。
//!
//! ## 根因
//!
//! [`Search::next_solution`] 是 `while let Some(..) { … }` 排空队列，**无燃料**。
//! 而 `h_solve` 的 `limit` 检查在**两次 `next_solution` 之间**
//! （`solutions.len() >= cap`）⇒ `next_solution` 永不返回 ⇒ `limit` **永不被检查**。
//!
//! ## 本轮**不修**，并说明为什么
//!
//! `src/rel/mod.rs` 明写「终止性模型与 Prolog 相同：递归关系的终止由
//! **关系作者负责**」⇒ 这是**明文的设计取舍**（D360/D368/D382 先例：只报告不擅动）。
//!
//! ⚠ 我**一度误判**：用「别处（`agent` 的 `max_steps` / `orchestrate` 的
//! `max_rounds` / `tea` 的 `max_steps`）都有上界，唯独 `rel` 没有」的不对称证据，
//! 按 D396 的规则判为「遗漏」并**真的加上了燃料上界**。随后读到
//! `rel/mod.rs` 的模块注释才发现那是**明文取舍**。
//! ⇒ **文档声明优先于「兄弟子系统一致性」启发式**（见 CHANGELOG）。
//!
//! ## 本轮**修**的是：模块 doc 里关于**既有能力**的错误声明
//!
//! 原文：「`solve N` 的界形式**可安全采样**潜在无限解流」
//! ⇒ 对「无限**解流**」成立，对「无限**搜索**」**不成立**。
//! 准确表述已写回 doc：**`solve N` 能限制「产出多少个解」，不能限制「搜索多久」**。
//!
//! ## 判据为什么**不**直接跑挂死程序
//!
//! 跑它必然挂住，判据本身就会变成 CI 事故。故分两层：
//! ① **源码级不变量**（有牙齿，将来加���界会红）；
//! ② **带硬超时的行为级**（`spawn` + `try_wait` 轮询 + 到点 `kill`），
//!    断言「6s 后仍在运行」—— 它**永不挂住测试套件**。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 剥掉整行注释与行尾注释，返回其余部分。
///
/// ⚠ 扫源码前**必须**剥注释 —— 否则判据会被自己写的说明命中
/// （D388 / D394 / D395 各踩过一次）。
fn code_only(s: &str) -> String {
    s.lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("//") {
                return "";
            }
            l.split("//").next().unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

/// 写一个探针脚本到临时目录，返回其路径。
fn write_probe(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mora_d397_{tag}"));
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    p
}

// ── ① 源码级不变量（有牙齿） ──

/// **`next_solution` 没有任何步数计数 / 上界**。
///
/// 本条是 D397 的**主判据**：若将来加了燃料，它会红，并要求把
/// 「本轮不修」重新分类为「已修」。
#[test]
fn d397_next_solution_has_no_step_bound() {
    let src = read("src/rel/search.rs");
    let start = src
        .find("pub fn next_solution(")
        .expect("应能找到 next_solution");
    let body = &src[start..];
    let body = code_only(body);
    // 只取函数体到下一个 `fn ` 定义之前
    let end = body[1..]
        .find("\n    fn ")
        .map(|i| i + 1)
        .unwrap_or(body.len());
    let body = &body[..end];
    for token in ["fuel", "steps", "max_steps", "MAX_", "budget", "上限"] {
        assert!(
            !body.contains(token),
            "`next_solution` 里出现了 `{token}` —— 搜索步数上界已被实现，\
             本文件「不修 / 只报告」的结论需重写。实得片段:\n{body}"
        );
    }
}

/// **`limit` 只在两次解之间检查** —— 它挡不住 `next_solution` 不返回。
#[test]
fn d397_limit_is_checked_between_solutions_only() {
    let src = code_only(&read("src/mir/handlers/effects.rs"));
    let start = src.find("pub fn h_solve(").expect("应能找到 h_solve");
    let body = &src[start..];
    assert!(
        body.contains("solutions.len() >= cap"),
        "`h_solve` 应按**已收集解数**检查 limit; 实得片段:\n{}",
        &body[..body.len().min(1200)]
    );
    // 该检查必须位于 `next_solution` **调用之前**（即循环内），而不是之前一次性检查
    let cap_at = body
        .find("solutions.len() >= cap")
        .expect("应有 limit 检查");
    let next_at = body
        .find("next_solution(")
        .expect("应有 next_solution 调用");
    assert!(
        cap_at < next_at,
        "limit 检查应排在 `next_solution` 调用之前（每轮一次）; \
         cap_at={cap_at} next_at={next_at}"
    );
    // 整个 h_solve 里**没有**任何步数/燃料上界
    for token in ["fuel", "step_budget", "max_steps"] {
        assert!(
            !body.contains(token),
            "`h_solve` 出现了 `{token}` —— 已有搜索步数上界，结论需重写"
        );
    }
}

// ── ② 文档更正已落地 ──

/// **模块 doc 不得再声称「`solve N` 可安全采样无限解流」**。
#[test]
fn d397_module_doc_states_the_real_limit_scope() {
    let doc = read("src/rel/mod.rs");
    assert!(
        !doc.contains("的界形式可安全采样潜在无限解流"),
        "`rel/mod.rs` 仍在声称「`solve N` 可安全采样无限解流」—— \
         该说法对无限**搜索**不成立（实测 `solve 2` 仍挂死）"
    );
    assert!(
        doc.contains("不能限制"),
        "`rel/mod.rs` 应写明「`solve N` 不能限制搜索时长」"
    );
    // 反向对照：Prolog 终止性模型这段**设计声明**要保留
    assert!(
        doc.contains("终止性模型与 Prolog 相同"),
        "「终止由关系作者负责」是**明文设计取舍**，不应被本轮改动删掉"
    );
}

// ── ③ 行为级：带硬超时地钉住「当前会挂死」 ──

/// **左递归规则 + `solve 2` 仍然挂死**（6s 后进程仍在运行）。
///
/// 用 `spawn` + `try_wait` 轮询 + 到点 `kill`：
/// **即使判据本身写错，也绝不会挂住测试套件**（kill 是无条件的）。
///
/// 本条的**反直觉点**：它断言的是「**没有**终止」——
/// 因为当前语义是「终止由关系作者负责」。若将来产品决定加上界，
/// 进程会退出 ⇒ 本条红 ⇒ 正是需要的提醒。
#[test]
fn d397_left_recursive_rule_still_hangs_even_with_limit() {
    let p = write_probe("hang", "rel loop2(x) loop2(x) end\nsolve 2 { loop2(?X) }\n");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut child = Command::new(exe)
        .arg(&p)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("跑 mora");
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut exited = false;
    while Instant::now() < deadline {
        match child.try_wait().expect("try_wait") {
            Some(_) => {
                exited = true;
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    // 无条件清理：判据挂住套件的**唯一**防线
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(p.parent().expect("应能取到父目录"));
    assert!(
        !exited,
        "左递归规则在 6s 内就退出了 —— 搜索步数上界（或左递归检测）已被实现！\
         本文件「不修 / 只报告」的结论需立即重写并改判为修复轮"
    );
}

/// **反向对照：有限规则**必须**正常退出**（否则上面那条只是在测「一切程序都挂」）。
#[test]
fn d397_finite_rule_terminates_normally() {
    let p = write_probe(
        "finite",
        "rel edge(\"a\", \"b\")\nrel edge(\"b\", \"c\")\n\
         rel path(x, y) edge(x, y) end\n\
         rel path(x, z) edge(x, y), path(y, z) end\n\
         print(solve { path(?f, ?t) })\n",
    );
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(p.parent().expect("应能取到父目录"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "有限规则（传递闭包）应正常退出; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("a") && s.contains("c"),
        "传递闭包应产出 `a` → `c`; 实得: {s}"
    );
}
