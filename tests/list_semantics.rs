//! `Value::List` 迁移（`Vec<Value>` → 分块 + 结构共享）的语义护栏。
//!
//! ## 这个文件在防什么
//!
//! 迁移把语言的**核心值类型**换成了「32 元素一片、`Arc` 共享、不可变」的表示。
//! 分块 + 结构共享的典型事故是**别名**：两个列表共享同一批块，改一个影响到另一个；
//! 以及**嵌套下标赋值** `m[0][1] = v` 能不能回写到外层。
//!
//! 2026-09-28 迁移完成时全量套件 37 组全绿，但「绿」证明不了这些 —— 本会话
//! 已经被「`is_ok()` 断言长期掩盖 `str()` 缺实现」教育过一次。所以这里逐项
//! **实测取值**后再写死断言。
//!
//! ## 一个实测结论：下标赋值从源码**不可达**
//!
//! `xs[0] = 99` 与 `m[0][1] = 42` **都是解析错误**；`xs.set(0, 99)` 报
//! 「no method: set」（`set` 只是 `list::List` 的 Rust 侧能力，未进 Mora 方法表）。
//! 故 `vm::index_assign_value` 在源码层无调用方 —— 别名/嵌套赋值风险被限制在
//! MIR 层（手写 MIR / SSA 降级）。这一点本身就值得钉住：若将来 parser 加上了
//! 下标赋值语法，本文件应立刻扩到那两类用例。
//!
//! 其余 27 项均取自迁移后的**实测输出**，不是推断。

use std::sync::Arc;

fn run(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v}"),
        Err(e) => format!("ERR: {e}"),
    }
}

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;

/// `(用例名, 源码, 期望)` —— 期望值为迁移完成后实测所得。
fn cases() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // ── 值语义：push 不改接收者 ──
        (
            "push_keeps_receiver",
            "let a = [1, 2]\nlet b = a.push(3)\n[a, b]\n",
            "[[1.0, 2.0], [1.0, 2.0, 3.0]]",
        ),
        (
            "pop_keeps_receiver",
            "let xs = [1, 2, 3]\nlet x = xs.pop()\n[xs, x]\n",
            "[[1.0, 2.0, 3.0], 3.0]",
        ),
        ("pop_on_empty", "let xs = []\nxs.pop()\n", "nil"),
        // ── 索引读 ──
        ("index_read", "let xs = [10, 20, 30]\nxs[1]\n", "20.0"),
        (
            "index_out_of_bounds",
            "let xs = [10, 20]\nxs[5]\n",
            "ERR: index 5 out of bounds (len 2)",
        ),
        // ── 相等性（按内容）─────────────────────────────────────────
        (
            "eq_same_content",
            "let a = [1, 2, 3]\nlet b = [1, 2, 3]\nif a == b then 1 else 0 end\n",
            "1.0",
        ),
        (
            "eq_diff_content",
            "let a = [1, 2, 3]\nlet b = [1, 2, 4]\nif a == b then 1 else 0 end\n",
            "0.0",
        ),
        // ── 长度 / 链式 push ────────────────────────────────────────
        (
            "len_empty",
            "let xs = []\n[len(xs), len(xs.push(1))]\n",
            "[0, 1]",
        ),
        (
            "push_chain",
            "let xs = [].push(1).push(2).push(3)\n[xs, len(xs)]\n",
            "[[1.0, 2.0, 3.0], 3]",
        ),
        // ── 其余 list 方法（迁移时改过实现）──────────────────────────
        (
            "map",
            "let xs = [1, 2, 3]\nxs.map(fn(x) x * 2 end)\n",
            "[2.0, 4.0, 6.0]",
        ),
        (
            "filter",
            "let xs = [1, 2, 3, 4]\nxs.filter(fn(x) x % 2 == 0 end)\n",
            "[2.0, 4.0]",
        ),
        (
            "window",
            "let xs = [1, 2, 3, 4, 5]\nxs.window(2)\n",
            "[[1.0, 2.0], [2.0, 3.0], [3.0, 4.0], [4.0, 5.0]]",
        ),
        (
            "batch",
            "let xs = [1, 2, 3, 4, 5]\nxs.batch(2)\n",
            "[[1.0, 2.0], [3.0, 4.0], [5.0]]",
        ),
        ("sort", "let xs = [3, 1, 2]\nxs.sort()\n", "[1.0, 2.0, 3.0]"),
        (
            "shape",
            "let xs = [[1, 2, 3], [4, 5, 6]]\nxs.shape()\n",
            "[2.0, 3.0]",
        ),
        (
            "flatten",
            "let xs = [[1, 2], [3, 4]]\nxs.flatten()\n",
            "[1.0, 2.0, 3.0, 4.0]",
        ),
        (
            "reshape",
            "let xs = [1, 2, 3, 4]\nxs.reshape(2, 2)\n",
            "[[1.0, 2.0], [3.0, 4.0]]",
        ),
        (
            "transpose",
            "let xs = [[1, 2, 3], [4, 5, 6]]\nxs.transpose()\n",
            "[[1.0, 4.0], [2.0, 5.0], [3.0, 6.0]]",
        ),
        ("get", "let xs = [1, 2, 3]\nxs.get(1)\n", "2.0"),
        ("take", "let xs = [1, 2, 3, 4]\nxs.take(2)\n", "[1.0, 2.0]"),
        ("drop", "let xs = [1, 2, 3, 4]\nxs.drop(2)\n", "[3.0, 4.0]"),
        (
            "reduce",
            "let xs = [1, 2, 3]\nxs.reduce(fn(a, b) a + b end, 0)\n",
            "6.0",
        ),
        // ── 与 dict 互嵌 ─────────────────────────────────────────────
        (
            "list_of_dict",
            "let xs = [{a: 1}, {a: 2}]\nxs[1].get(\"a\")\n",
            "2.0",
        ),
        (
            "dict_with_list_value",
            "let d = {k: [1, 2]}\nd.keys()\n",
            "[k]",
        ),
        // `d.set` 返回**新** dict，原 `d` 不变 —— 值语义契约
        (
            "dict_set_is_pure",
            "let d = {a: 1}\nlet e = d.set(\"a\", 7)\n[d.get(\"a\"), e.get(\"a\")]\n",
            "[1.0, 7.0]",
        ),
        // ── 循环累积（本次迁移的靶心）───────────────────────────────
        (
            "accumulate_in_while",
            "let xs = []\nlet i = 0\nwhile i < 4\n  let xs = xs.push(i * 10)\n  let i = i + 1\nend\nxs\n",
            "[0.0, 10.0, 20.0, 30.0]",
        ),
        (
            "accumulate_preserves_prefix",
            "let a = [1, 2, 3]\nlet b = [].push(1).push(2).push(3)\nif a == b then 1 else 0 end\n",
            "1.0",
        ),
    ]
}

#[test]
fn list_semantics_match_baseline() {
    let all = cases();
    let mut failures = Vec::new();
    for (name, src, expected) in &all {
        let got = run(src);
        if &got != expected {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    expected = {expected}\n    actual   = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个 list 语义用例与基线不符：\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

/// 下标赋值**从源码不可达** —— 把这个事实钉住。
///
/// 注意 `xs.set(0, 99)` 是另一回事：它**能解析**（只是 `set` 不在 list 方法表里，
/// 运行时报 "no method: set"），所以不归入本守卫。真正没有语法的是
/// `xs[0] = v` 形式的赋值。
///
/// 若将来 parser 加上 `xs[0] = v` 语法，本测试会失败，从而提醒补上
/// 「别名隔离」与「嵌套回写」两类用例（分块 + 结构共享下正是最需要盯的）。
#[test]
fn index_assignment_is_not_reachable_from_source() {
    // 这条要观察**编译失败**，故不能用上面的 `run()`（它遇错即 panic）。
    let compile = |src: &str| match ParserV3::compile(src) {
        Ok(_) => "COMPILES".to_string(),
        Err(e) => format!("PARSE_ERR: {e}"),
    };
    for (name, src) in [
        ("xs[0] = 99", "let xs = [1, 2, 3]\nxs[0] = 99\nprint(xs)\n"),
        (
            "m[0][1] = 42",
            "let m = [[1, 2], [3, 4]]\nm[0][1] = 42\nprint(m)\n",
        ),
    ] {
        let r = compile(src);
        assert!(
            r.starts_with("PARSE_ERR"),
            "{name} 现在可解析了（{r}）—— 需补别名隔离 / 嵌套回写用例（见本文件头）"
        );
    }
}
