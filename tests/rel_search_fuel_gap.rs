//! v0.104.6 D397 —— `rel::search` 的**搜索步数上界**（**已修**：挂死 → 明确报错）
//!
//! ## 现象（修前）
//!
//! ```mora
//! rel loop2(x) loop2(x) end
//! solve 2 { loop2(?X) }
//! ```
//!
//! 20s 内不退出，**无报错、无输出**，只能手动杀进程。
//! `solve N` 的 `limit` **挡不住** —— 它只在两次解**之间**检查，
//! 而 `next_solution` 永不返回。
//!
//! ## 修法：方案 ① 搜索步数上界（燃料）
//!
//! `Search` 增加 `steps` / `max_steps` 字段，`next_solution` 的 while 循环
//! 每轮 `steps += 1`，超过上界即返回 `Err`。
//!
//! | 决定 | 值 |
//! |---|---|
//! | 默认上界 | `DEFAULT_MAX_STEPS = 200_000` |
//! | 自定义 | `Search::with_max_steps(n)`，`0` = 回到修前的无限语义 |
//! | 计数范围 | **跨多次 `next_solution` 累计**（`h_solve` 循环里是同一个 `Search`）|
//!
//! 默认值标定：左递归在 1 000 000 步下要 **8.7 秒**才报错（每步会克隆越堆越长的
//! chain，见 `Disj` 臂），20 万步把最坏等待压到 **~1.8 秒**；
//! 而合法搜索量级低得多（`rel_basic.mora` 的传递闭包全解只有几十步）。
//!
//! ## 代价（明写，不藏）
//!
//! 「终止由关系作者负责」这条 Prolog 式约定，对**产不出解**的规则不再成立：
//! 超长但合法的搜索需要 `with_max_steps` 调大。这正是选方案①时
//! 已知的代价（另两个方案：② 左递归检测、实现复杂且拦不住「非左递归但无限失败」；
//! ③ 让 `limit` 同时约束工作量，语义变化最明显）。
//!
//! ## 判据设计沿用原文件的正确做法
//!
//! 行为级那条**仍然**用 `spawn` + `try_wait` + 无条件 `kill` —
//! 哪怕燃料机制本身坏掉（没报错、也没退出），判据也**绝不会挂住测试套件**。
//! 只是断言方向从「仍在运行」翻转为「已退出且报 fuel exhausted」。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 剥掉整行注释与行尾注释，返回其余部分。
///
/// ⚠ 扫源码前**必须**剥注释 —— 否则判据会被自己写的说明命中
/// （D388 / D394 / D395 各踩过一次；本文件当初写「不修」时也踩过）。
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

/// **`next_solution` 必须有步数计数与上界检查**。
///
/// 本条是 D397 的**主判据**：修前断言「一个 token 都别出现」，
/// 现在反过来 —— 把 `max_steps` 检查去掉它就红。
#[test]
fn d397_next_solution_has_a_fuel_bound() {
    let src = read("src/rel/search.rs");
    let start = src
        .find("pub fn next_solution(")
        .expect("应能找到 next_solution");
    let body = code_only(&src[start..]);
    let end = body[1..]
        .find("\n    fn ")
        .map(|i| i + 1)
        .unwrap_or(body.len());
    let body = &body[..end];
    assert!(
        body.contains("self.steps += 1"),
        "`next_solution` 必须逐步计数; 实得片段:\n{body}"
    );
    assert!(
        body.contains("max_steps"),
        "`next_solution` 必须拿 `max_steps` 做上界检查; 实得片段:\n{body}"
    );
    assert!(
        body.contains("Err("),
        "耗尽燃料必须**返回 Err**（明确报错），而不是别的出路; 实得片段:\n{body}"
    );
}

/// **计数必须是跨调用累计的字段**，不是局部变量。
///
/// 若退回「每次 `next_solution` 从 0 数」，那么 `h_solve` 的循环里
/// 每轮都能白拿一整份预算 —— 「每次只跑几千步」的合法深搜索会被误杀，
/// 而左递归也未必在单次调用内撞上限。这条钉住该结构。
#[test]
fn d397_fuel_is_cumulative_across_calls() {
    let src = read("src/rel/search.rs");
    let at = src.find("pub struct Search").expect("有 Search");
    // ⚠ 必须**从结构体位置**往后找闭合括号：从文件开头 find 会命中更早的
    // 某个 `\n}`，切出来的片段随机得很（本条第一版就栽在这）。
    let end = at + src[at..].find("\n}").expect("Search 结构体应闭合");
    let decl = code_only(&src[at..end]);
    assert!(
        decl.contains("steps: u64"),
        "`Search` 必须持有跨调用累计的 `steps` 字段; 实得:\n{decl}"
    );
    assert!(
        decl.contains("max_steps: u64"),
        "`Search` 必须持有 `max_steps` 字段; 实得:\n{decl}"
    );
}

/// **`limit` 仍只在两次解之间检查** —— 本条没变，但结论要重写：
/// 燃料在 `Search` 内部，`limit` 在 `h_solve` 外层，两者**互补**。
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
}

// ── ② 文档必须说清「两层界限」 ──

/// **`rel/mod.rs` 的模块文档必须与代码一致**（v0.104.6 D397）。
///
/// ⚠ **判据演进记录**：本条第一版只要求
/// `doc.contains("max_steps") || doc.contains("步数上界")` —— **无判别力**。
/// 因为 `步数上界` 恰好**只出现一次**，而且就在那句**已经过时**的
/// 「是否加搜索步数上界……属产品策略决定」里 —— 也就是说：
/// **文档越陈旧，这条越容易过。**
///
/// 下面改成断言**具体的新事实**，每条都能被「回退成修前状态」精确打红。
#[test]
fn d397_module_doc_states_the_real_limit_scope() {
    let doc = read("src/rel/mod.rs");

    // 旧的不准确表述必须消失
    assert!(
        !doc.contains("的界形式可安全采样潜在无限解流"),
        "`rel/mod.rs` 仍在声称「`solve N` 可安全采样无限解流」—— \
         该说法对无限**搜索**不成立"
    );
    assert!(
        !doc.contains("本轮不擅改"),
        "`rel/mod.rs` 仍写着「本轮不擅改」—— D397 已修，该措辞已过期"
    );
    assert!(
        !doc.contains("已列入待裁决"),
        "`rel/mod.rs` 仍写着「已列入待裁决」—— D397 已按方案①修完"
    );
    assert!(
        !doc.contains("20s 不退出"),
        "`rel/mod.rs` 仍把「20s 不退出」写成**当前实测行为**—— \
         D397 修完后左递归是 1.8 秒明确报错"
    );
    assert!(
        !doc.contains("两者都挂死"),
        "`rel/mod.rs` 仍称挂死是当前行为"
    );

    // 新的准确表述必须在位
    assert!(
        doc.contains("DEFAULT_MAX_STEPS"),
        "`rel/mod.rs` 应写明步数上界的常量名 `DEFAULT_MAX_STEPS`"
    );
    assert!(
        doc.contains("with_max_steps"),
        "`rel/mod.rs` 应写明可调 `Search::with_max_steps`（含传 `0` 回到无界）"
    );
    assert!(
        doc.contains("互补"),
        "`rel/mod.rs` 应写明 `limit` 与步数上界是**互补**的两层"
    );

    // ⚠ 这一条才是「文档没跟着代码改」的护栏：
    // 一旦 fuel 机制被撤掉，文档却还在宣称它有上界，本条会红。
    let search_src = read("src/rel/search.rs");
    let has_fuel = search_src.contains("max_steps");
    assert!(
        has_fuel,
        "`search.rs` 里已无 `max_steps` —— 若 `rel/mod.rs` 仍宣称有步数上界，\
         那份文档就是**过时的**，必须一并更新"
    );
}

// ── ③ 行为级：左递归从「挂死」翻转为「明确报错」 ──

/// **左递归规则必须**在有限时间内**退出并报 fuel exhausted**。
///
/// 仍用 `spawn` + `try_wait` + **无条件 `kill`**：万一燃料机制本身
/// 坏掉（既不报错也不退出），本判据也**绝不会挂住测试套件**。
#[test]
fn d397_left_recursive_rule_now_errors_instead_of_hanging() {
    let p = write_probe("hang", "rel loop2(x) loop2(x) end\nsolve 2 { loop2(?X) }\n");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut child = Command::new(exe)
        .arg(&p)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("跑 mora");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut status = None;
    while Instant::now() < deadline {
        match child.try_wait().expect("try_wait") {
            Some(s) => {
                status = Some(s);
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    // 无条件清理：判据挂住套件的**唯一**防线
    let still_running = status.is_none();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(p.parent().expect("应能取到父目录"));

    assert!(
        !still_running,
        "左递归规则 30s 内仍未退出 —— 搜索步数上界**没有生效**（或上界过大）"
    );
}

/// **反向对照 1：有限规则必须正常退出且产出解**。
///
/// 防止上面那条只是在测「一切程序都报错」。
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

/// **反向对照 2：同一份左递归脚本必须报出 fuel exhausted 文案**。
///
/// 光断言「退出了」不够 —— 它也可能是别的错误（例如解析失败）导致的退出。
#[test]
fn d397_left_recursion_reports_fuel_exhausted() {
    let p = write_probe(
        "fuelmsg",
        "rel loop2(x) loop2(x) end\nsolve 2 { loop2(?X) }\n",
    );
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(p.parent().expect("应能取到父目录"));
    assert_ne!(out.status.code(), Some(0), "左递归必须非零退出");
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.contains("max_steps"),
        "报错应点名 `max_steps`（让用户知道是燃料耗尽而非别的错）; 实得: {s}"
    );
    assert!(
        s.contains("left-recursive") || s.contains("左递归"),
        "报错应提示**左递归**这一最常见成因; 实得: {s}"
    );
}
