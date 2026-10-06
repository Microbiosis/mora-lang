//! v0.104.6 D216：`solve { … }` 合取里**只有最后一个 goal 的绑定活到结果** ——
//! join 查询静默返回错值（已修）。
//!
//! ## 缺陷
//!
//! `parser_v3/rel.rs::emit_solve_w` 把多个 goal 的 witness 收进
//! `WitnessKind::Sequence(body_wits)` —— 一个**顺序块**，而块的产出值
//! 只是**最后一个**子表达式的值。于是运行期 `h_solve` 拿到的 `Value::Goal`
//! 里**只有最后一个 goal**，前面 goal 建立的绑定被静默丢弃。
//!
//! 真实 `mora run` 实测（修前，facts: `p("a","b")` / `q("b","c")`）：
//!
//! ```text
//! solve { p(?X, ?Y) }                      → [[a, b]]      ✅
//! solve { p(?X, ?Y), q(?Y, ?Z) }           → [[_.0, b, c]] ❌ 期望 [[a, b, c]]
//! solve { p(?X,?Y), q(?Y,?Z), q(?Z,?W) }    → [[_.0,_.0,b,c]] ❌
//! solve { p("a", ?Y), p(?X, ?Y) }          → [[b, a]]      ✅（巧合：末个 goal 重新推出同样绑定）
//! ```
//!
//! 规则精确到一句：**合取中只有最后一个 goal 的绑定到达结果**。
//! Datalog 的核心用法就是 join，而结果是**零诊断、exit 0、看起来合理**
//! （`b`、`c` 都在），未绑定的 `_.N` 按 `reify.rs` 的约定还是**合法输出**。
//!
//! ## 修法（两处，同一个缺陷面的两半）
//!
//! ① `emit_solve_w`：多 goal 改为对各子 goal 调 **`both(...)`** ——
//!    它就是运行期的 `Goal::Conj` 构造器
//!    （`interpreter/builtins/rel.rs::call_builtin_both`），复用既有 builtin。
//! ② `typeck/dispatch.rs`：`both` / `either` 的签名改为**变参**。
//!    运行期 `call_builtin_both` 接受**任意个数**（≥1）的 goal，
//!    而签名表此前只声明**恰好 2 个** ⇒ 三个及以上 goal 直接被类型检查拒绝：
//!    `Type error: expected goal, got fn (…) -> …`。
//!    不改这里，修复就只对**恰好 2 个** goal 有效 —— 而三表 join 才是常见情形。
//!
//! ## 判据
//!
//! 写在**真实 CLI** 层（`mora run`）而不是引擎层：`rel::search` 的单测
//! （`conjunction_threads_bindings` 等 39 条）**本来就全过** —— 引擎是对的，
//! 错的是「目标体怎么被构造成一个 Goal」。故判据必须走脚本这条路才能有牙齿。
//!
//! 覆盖：单 goal 不回归、2-goal join、**3-goal join**（它同时是 `both`
//! **变参**声明的证明 —— `emit_solve_w` 对多个 goal 生成的正是
//! `both(g1, g2, g3)`，修前签名表只声明**恰好 2 个**参数，
//! 三个 goal 会直接被类型检查拒绝）、无解合取、重复 goal、末位 ground。
//!
//! ⚠ 判据里**没有**直接调 `both(a, b, c)` 的用例：`both` / `either`
//! 在当前语法里**无法手写**（实测
//! `let g = both(p(?X), q(?Y), s(?Z))` 报
//! `Parse error: Expected ')' after relation arguments`）—— 它们是 parser
//! **内部发出**的构造。判据必须走用户可表达的那条路。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d216_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 跑一份脚本，返回「程序自己的输出」（剥掉横幅），失败时带上 stderr。
fn run(dir: &WorkDir, tag: &str, body: &str) -> (String, i32) {
    let p = dir.0.join(format!("{tag}.mora"));
    std::fs::write(&p, body).expect("写脚本");
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(&dir.0)
    .arg(&p)
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    let code = out.status.code().unwrap_or(-1);
    let program_out = s
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("Mora v")
                && !t.starts_with("AI:")
                && !t.starts_with("AI 原语")
                && !t.starts_with("显式 API")
                && !t.starts_with("Trait 系统")
                && !t.starts_with("Built-in")
                && !t.starts_with("v0.15 CLI")
                && !t.starts_with('⚠')
                && !t.starts_with("[9layer]")
        })
        .collect::<Vec<_>>()
        .join("\n");
    (program_out, code)
}

const FACTS_PQ: &str = "rel p(\"a\", \"b\")\nrel q(\"b\", \"c\")\nrel r(\"c\", \"d\")\n";
/// 一元关系（给「末位 goal 只用到部分变量」的场景用）。
const FACTS_P: &str = "rel p(\"a\", \"b\")\nrel f(\"b\")\n";

/// **主判据（有牙齿）**：合取里的**每个** goal 建立的绑定都必须到达结果。
///
/// 修前：2-goal 得 `[[_.0, b, c]]`、3-goal 得 `[[_.0, _.0, b, c]]` —— 只有
/// 最后一个 goal 的绑定活着，前面的静默变成未绑定符号。
#[test]
fn d216_conjunction_threads_every_goals_bindings() {
    let dir = WorkDir::new("join");
    for (tag, query, want) in [
        ("join2", "p(?X, ?Y), q(?Y, ?Z)", "[[a, b, c]]"),
        ("join3", "p(?X, ?Y), q(?Y, ?Z), r(?Z, ?W)", "[[a, b, c, d]]"),
        ("partial", "p(?X, ?Y), f(?Y)", "[[a, b]]"),
    ] {
        let facts = if tag == "partial" { FACTS_P } else { FACTS_PQ };
        let (out, code) = run(
            &dir,
            tag,
            &format!("{facts}let r = solve {{ {query} }}\nprint(r)\n"),
        );
        assert_eq!(
            code, 0,
            "[{tag}] 查询 `{query}` 应当正常执行（三个及以上 goal 需要 `both` 的\
             变参签名）:\n{out}"
        );
        assert_eq!(
            out, want,
            "[{tag}] 查询 `{query}` 的绑定**没有全部到达结果** —— \
             修前只有最后一个 goal 的绑定活着，前面的变成未绑定符号 `_.N`。\n输出:\n{out}"
        );
    }
}

/// **不回归**：单 goal、重复 goal、末位 ground 的行为必须不变。
#[test]
fn d216_single_and_repeated_goals_are_unchanged() {
    let dir = WorkDir::new("single");
    for (tag, facts, query, want) in [
        ("one", "rel p(\"a\", \"b\")\n", "p(?X, ?Y)", "[[a, b]]"),
        (
            "self",
            "rel p(\"a\", \"b\")\n",
            "p(?X, ?Y), p(?X, ?Y)",
            "[[a, b]]",
        ),
        (
            "ground_first",
            "rel p(\"a\", \"b\")\n",
            "p(\"a\", ?Y), p(?X, ?Y)",
            "[[b, a]]",
        ),
    ] {
        let (out, code) = run(
            &dir,
            tag,
            &format!("{facts}let r = solve {{ {query} }}\nprint(r)\n"),
        );
        assert_eq!(code, 0, "[{tag}] 应正常执行:\n{out}");
        assert_eq!(out, want, "[{tag}] 结果变了:\n{out}");
    }
}

/// **不回归**：**无解**的合取必须仍返回空列表（不能因为合取接线而「变成有解」）。
#[test]
fn d216_unsatisfiable_conjunction_still_yields_nothing() {
    let dir = WorkDir::new("nosol");
    let (out, code) = run(
        &dir,
        "nosol",
        "rel p(\"a\", \"b\")\nlet r = solve { p(?X, ?Y), p(\"z\", ?K) }\nprint(r)\n",
    );
    assert_eq!(code, 0, "应正常执行:\n{out}");
    assert_eq!(
        out, "[]",
        "第二个 goal 要求 ?X = \"z\"，与 p(\"a\",\"b\") 矛盾 → 应无解。\
         修前这个反例恰好「通过」是因为绑定根本没串起来。\n输出:\n{out}"
    );
}
// ===================================================================
// D216 不回归：`rel` **规则**路径未被改动波及
// ===================================================================

/// D216 改的是 `emit_solve_w`（**查询**侧的目标体降级），而**规则**侧的
/// 合取走另一条路 —— 编译期就构造好 `Goal::Conj` 塞进 `Clause::rule(...)`
/// （`rel.rs:126`）。本条把那侧**已验证正确**的行为钉住：递归闭包、
/// 子句体内 3 个 goal 的合取、单目标规则。
#[test]
fn d216_rel_rule_path_is_unaffected() {
    let dir = WorkDir::new("rule");
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "transitive_closure",
            concat!(
                "rel edge(\"a\", \"b\")\n",
                "rel edge(\"b\", \"c\")\n",
                "rel edge(\"c\", \"d\")\n",
                // 经典 path/2 传递闭包：基础 edge + 递归规则
                "rel path(x, y) edge(x, y) end\n",
                "rel path(x, z) edge(x, y), path(y, z) end\n",
                "let r = solve { path(?S, ?E) }\nprint(r)\n",
            )
            .to_string(),
            "[[a, b], [b, c], [c, d], [a, c], [b, d], [a, d]]",
        ),
        (
            "clause_body_3_goals",
            concat!(
                "rel p(\"a\", \"x\", \"y\")\n",
                "rel q(\"x\", \"y\", \"z\")\n",
                "rel r(\"y\", \"z\", \"w\")\n",
                // 子句体内 3 个 goal 的合取（编译期 Conj 路径）
                "rel chain(a, b, c, d) p(a, b, c), q(b, c, d), r(c, d, e) end\n",
                "let r2 = solve { chain(?P, ?Q, ?S, ?T) }\nprint(r2)\n",
            )
            .to_string(),
            "[[a, x, y, z]]",
        ),
        (
            "single_goal_rule",
            concat!(
                "rel parent(\"a\", \"b\")\n",
                "rel child(x, y) parent(x, y) end\n",
                "let r = solve { child(?X, ?Y) }\nprint(r)\n",
            )
            .to_string(),
            "[[a, b]]",
        ),
    ];
    for (tag, body, want) in cases {
        let (out, code) = run(&dir, tag, &body);
        assert_eq!(code, 0, "[{tag}] 应正常执行:\n{out}");
        assert_eq!(
            out, want,
            "[{tag}] `rel` **规则**侧的结果变了 —— D216 只应影响**查询**侧的目标体降级，\
             规则侧的合取走编译期 `Goal::Conj`，不应被波及。\n输出:\n{out}"
        );
    }
}
