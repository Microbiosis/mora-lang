//! v0.104.6 D474：**9 层管线的行为契约表** —— 逐条钉住期望输出。
//!
//! ## 为什么是「期望输出」而不是「两条路径一致」
//!
//! D473 之前，这张表的判据是 `run_pipeline` 的 `differential_ok`（9 层管线
//! vs `emit.rs` 参考实现的**指令形状**逐条比对）。**D474 把差分机制整个退役** ——
//! parser 不再发射 MirInst（只产 witness），MIR 全部由 9 层产出 ⇒「参考实现」
//! 这一侧**已经不存在**，那次比对会变成**自己跟自己比**（恒真）。
//!
//! ⇒ 判据换成**期望输出**：每条构造必须产出**具体且正确**的值。
//!
//! **这比原判据更强**：
//!
//! | 判据 | 能发现 | 发现不了 |
//! |---|---|---|
//! | 两条路径一致 | 两条路径**不一致** | 两条路径**一起错**到同一个错值 |
//! | 期望输出 | 值**不对** | —— |
//!
//! ## 这张表已经抓到过一个真缺陷
//!
//! `or` / `and` 的短路降级此前**从不给结果寄存器赋值** ⇒ 依赖它的后续 Effect
//! 永远不就绪、整条不执行（真值表 8 例错 4 例，`print(true or false)` 无任何
//! 输出）。该缺陷**两边一样错** ⇒ 逐条差分**结构上抓不到**它 —— 正是「差分
//! 只比形状、不比语义」的盲区。见 `src/mir/fcfg_lower.rs` 的 D474 注释。
//!
//! ## 期望值的来源
//!
//! 每条期望值都来自**语义推导 + 真实 CLI 实测**双向确认，不是「把当前输出
//! 抄下来」—— 凡与推导不符的，以推导为准并修实现。
//!
//! ⚠ 浮点打印格式：Mora 的 `print` 把数值渲染成 `1.0`（带 `.0`），
//! 而 `p"…{x}"` 模板插值走 `mir::vm::value_to_string`（Rust `f64::to_string`
//! ⇒ 整数值渲染成 `1`）。两条路径的差异是**既有事实**，本表按实测钉住。

use mora::test_support::TempDir;
use std::process::Command;

/// 一条构造的期望结果。
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 编译成功、运行成功（退出码 0），stdout **逐行**等于给定值。
    Out(&'static str),
    /// 编译期**类型错误**（退出码 2），stderr 含给定子串。
    TypeErr(&'static str),
    /// 运行期错误（退出码 1），stderr 含给定子串。
    RuntimeErr(&'static str),
}

/// 构造清单 —— 覆盖 spec §14.2 的 statement 产生式 + TEA 独立声明 +
/// 字面量 / 表达式形态。
///
/// 形态取自 spec §14.2，注意两处**与 spec EBNF 不一致**的实现事实：
/// - `handle` 实为块形态 `handle E { body } { handler }`，
///   而 spec §14.2 写的是 `handle IDENT "on" IDENT "->" {stmt} "end"`；
/// - `model` / `msg` / `update` 是**无花括号**的 `name … end` 形态。
const CONSTRUCTS: &[(&str, &str, Expect)] = &[
    // ── 块形态（observe / span / parallel / section）──
    (
        "observe",
        "observe trace \"t\" do\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "span",
        "span \"s\" do\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "parallel",
        "parallel\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "prompt_section",
        "prompt \"p\" do\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "document_section",
        "document \"d\" do\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    // ── 效果 / 事务 ──
    (
        "worker",
        "worker w do\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "transaction",
        "transaction\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "transaction_comp",
        "transaction\n  print(1)\ncompensation\n  print(2)\nend\n",
        // 补偿段只在**回滚**时执行；此处事务正常提交 ⇒ 只看到 `1.0`。
        Expect::Out("1.0"),
    ),
    (
        "perform_bare",
        "let g = \"i\"\ng = perform Ai(\"x\")\nprint(g)\n",
        // 裸 `perform` 没有匹配的 `handle` ⇒ **typeck 拦截**（效果行不匹配）。
        // v0.104.6 D472 的注释曾说 typeck 拦不住，那是当时的实测；现已拦得住。
        Expect::TypeErr("Effect row mismatch"),
    ),
    (
        "handle",
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
        Expect::Out("mocked:hello"),
    ),
    (
        "aggregate",
        "aggregate acc, 1\n",
        // 聚合器只在 Pregel/BSP 运行期有意义；普通程序里是**纯生产者** no-op。
        Expect::Out(""),
    ),
    (
        "commit",
        "transaction\n  print(1)\nend\ncommit\n",
        Expect::Out("1.0"),
    ),
    (
        "rollback",
        "rollback\n",
        // 事务外的 `rollback` 是运行期错误（`h_rollback` 的既定契约）。
        Expect::RuntimeErr("Transaction rolled back"),
    ),
    // ── 声明形态（TEA）──
    (
        "model",
        "model Counter\n  count: number = 0\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "model_default",
        "model Counter\n  count: number = 7\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "msg",
        "msg CounterMsg\n  Increment\n  Decrement\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "struct",
        "struct P\n  x: number\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "enum",
        "enum E\n  A\n  B\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "tea_standalone",
        "model C\n  count: number = 0\nend\nmsg M\n  Inc\nend\nupdate(msg, model)\n  model\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "update",
        "update(msg, model)\n  model\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "effect_sig",
        "effect MyEff(number) : number\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    // ── 同像性（eval / quote / quasiquote）──
    (
        "eval",
        "print(eval(\"1 + 1\"))\n",
        // spec §14.2：`eval(code)` 运行时编译并执行 Mora 代码 ⇒ `2.0`。
        Expect::Out("2.0"),
    ),
    (
        "eval_bare",
        "eval(1 + 1)\n",
        // 语句形态的 `eval` 是**断言原语**（`h_eval`）：given 与 expects 比对。
        // 无 expects ⇒ 恒通过，「eval '': PASSED」走 stderr，stdout 为空。
        Expect::Out(""),
    ),
    (
        "eval_stmt",
        "eval \"n\" 1, 2\n",
        // given=1 与 expects=[2] 不等 ⇒ 断言失败（运行期错误）。
        Expect::RuntimeErr("assertion failed"),
    ),
    (
        "quasiquote",
        "let q = `(10+20)\nprint(q)\n",
        Expect::Out("code:(10+20)"),
    ),
    // ── 绑定 / 赋值 / 控制流 ──
    ("let", "let x = 1\nprint(x)\n", Expect::Out("1.0")),
    (
        "assign",
        "let x = 1\nassign x = 2\nprint(x)\n",
        Expect::Out("2.0"),
    ),
    (
        "if",
        "let c = 1\nif c == 1 then\n  print(1)\nelse\n  print(2)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "if_elseif",
        "let c = 1\nif c == 1 then\n  print(1)\nelse if c == 2 then\n  print(2)\nelse\n  print(3)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "for",
        "for x in [1, 2, 3]\n  print(x)\nend\n",
        Expect::Out("1.0\n2.0\n3.0"),
    ),
    (
        "while",
        "let i = 0\nwhile i < 3\n  print(i)\n  i = i + 1\nend\n",
        Expect::Out("0.0\n1.0\n2.0"),
    ),
    (
        "match",
        "let v = 1\nlet r = match v {\n  1 => \"a\"\n  _ => \"b\"\n}\nprint(r)\n",
        Expect::Out("a"),
    ),
    (
        "match_guard",
        "let v = 1\nlet r = match v {\n  x when x > 0 => \"pos\"\n  _ => \"neg\"\n}\nprint(r)\n",
        Expect::Out("pos"),
    ),
    (
        "with",
        "with model = \"m\"\n  print(1)\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "task",
        "task t()\n  print(1)\nend\ntask main()\n  t()\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "return",
        "task t()\n  return 1\nend\ntask main()\n  print(t())\nend\n",
        Expect::Out("1.0"),
    ),
    (
        "macro",
        "macro m(x)\n  print(x)\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "closure",
        "let f = fn(x) x + 1\nprint(f(1))\n",
        Expect::Out("2.0"),
    ),
    // ── 逻辑运算符（短路求值）──
    //
    // ⚠ 这四条是 D474 修好的**既有缺陷**的回归护栏：此前短路路径不给结果
    // 寄存器赋值 ⇒ 依赖它的 `print` **整条不执行**（无任何输出）。
    // 真值表 8 例错 4 例。详见 `src/mir/fcfg_lower.rs` 的 D474 注释。
    //
    // v0.104.6 D477：短路从「只作用于取值」升级为**作用于求值** ——
    // 短路成立时 rhs **根本不被求值**（副作用也不发生）。
    // 下面这条把「rhs 的副作用不发生」钉进行为契约表；
    // 完整判据（三个优化档位 + 两条编译路径的形状对照）见
    // `tests/short_circuit_evaluation.rs`。
    (
        "or_and",
        "let x = true or false\nlet y = true and false\nprint(x)\nprint(y)\n",
        Expect::Out("true\nfalse"),
    ),
    (
        "or_short_circuit",
        "let x = false or true\nlet y = true or false\nprint(x)\nprint(y)\n",
        Expect::Out("true\ntrue"),
    ),
    (
        "and_short_circuit",
        "let x = false and true\nlet y = true and true\nprint(x)\nprint(y)\n",
        Expect::Out("false\ntrue"),
    ),
    (
        "or_in_condition",
        "if true or false then\n  print(\"t\")\nelse\n  print(\"f\")\nend\n",
        Expect::Out("t"),
    ),
    // v0.104.6 D477：短路成立 ⇒ rhs **不被求值**（副作用也不发生）。
    (
        "or_and_skip_rhs_evaluation",
        "task bump() : bool\n  print(\"BUMP\")\n  return true\nend\n\
         let a = true or bump()\nlet b = false and bump()\nprint(\"ok\")\n",
        Expect::Out("ok"),
    ),
    // ── 字面量 / 表达式 ──
    (
        "list_lit",
        "let xs = [1, 2, 3]\nprint(xs)\n",
        Expect::Out("[1.0, 2.0, 3.0]"),
    ),
    (
        "dict_lit",
        "let d = {a: 1, b: 2}\nprint(d)\n",
        Expect::Out("{a: 1.0, b: 2.0}"),
    ),
    (
        "template",
        "let n = 1\nprint(p\"v={n}\")\n",
        // 模板插值走 `value_to_string`（Rust `f64::to_string`）⇒ 整数值是 `1`，
        // 而 `print` 走另一条格式化路径 ⇒ `1.0`。两者不一致是**既有事实**。
        Expect::Out("v=1"),
    ),
    ("bigint", "let n = 123n\nprint(n)\n", Expect::Out("123n")),
    (
        "dyn_coerce",
        "let d = 1 as dyn T\nprint(d)\n",
        Expect::Out("<trait_object for=float as T data=Float(1.0)>"),
    ),
    // ── 编排（agent 体**只应执行一次**）──
    (
        "orchestrate",
        "orchestrate graph input -> result\n  agent a => print(\"A\")\n  agent b => print(\"B\")\n  @start -> a\n  a -> b\nend\n",
        // ⚠ D473 修过一个「agent 体双执行」缺陷（编排块此前被发射进主上下文，
        // 出现平铺副本）⇒ agent 体跑两次。这条期望值把它钉住。
        Expect::Out("A\nB"),
    ),
];

/// 跑一个程序，返回 `(stdout, stderr, 退出码)`。
fn run(tag: &str, src: &str) -> (String, String, i32) {
    let dir = TempDir::new(tag);
    let path = dir.join("p.mora");
    std::fs::write(&path, src).expect("写临时程序");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

/// **主判据**：逐条构造必须产出钉死的期望结果。
#[test]
fn every_construct_produces_its_expected_output() {
    let mut failures: Vec<String> = Vec::new();
    for (name, src, expect) in CONSTRUCTS {
        let (stdout, stderr, code) = run(name, src);
        let got = stdout.trim_end_matches('\n');
        let ok = match expect {
            Expect::Out(want) => code == 0 && got == *want,
            Expect::TypeErr(want) => code == 2 && stderr.contains(want),
            Expect::RuntimeErr(want) => code == 1 && stderr.contains(want),
        };
        if !ok {
            failures.push(format!(
                "  [{name}]\n    期望: {expect:?}\n    实得: exit={code} stdout={got:?}\n    stderr: {}",
                stderr.trim_end()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "9 层管线的行为契约表与实测不符（{} 条）：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 覆盖面自检：这张表**不能悄悄变小**。
///
/// D407 的教训：普查表的条数一旦无约束地缩减，它就会在无人察觉的情况下
/// 变成「什么都测不到」。
///
/// v0.104.6 D477：48 → 49（新增 `or_and_skip_rhs_evaluation` —— 短路成立时
/// rhs **不被求值**，这条把「短路作用于求值」钉进行为契约表）。
#[test]
fn conformance_table_covers_the_whole_language_surface() {
    assert_eq!(
        CONSTRUCTS.len(),
        49,
        "行为契约表现在 {} 条 —— 若有意增删，请同步更新本断言与 CHANGELOG（D474 建表、D477 增补）",
        CONSTRUCTS.len()
    );
    // 名字不得重复（重复会让后一条静默覆盖前一条的覆盖）。
    let mut names: Vec<&str> = CONSTRUCTS.iter().map(|(n, _, _)| *n).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before, "契约表里出现了重复的构造名");
}
