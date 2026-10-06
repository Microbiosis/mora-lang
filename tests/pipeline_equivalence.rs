//! v0.104.6 D14：**witness 的索引编码没有被 lower 解码** —— `handle` body 与
//! `import` 进来的模块里，`obj[i]` 被降级成「调用一个名叫 `[]` 的函数」。
//!
//! ## 现象
//!
//! 同一段代码，**内联**（走 9 层管线）正确，`import` 进来就坏：
//!
//! ```text
//! // 内联
//! handle random_random { x = t[1] } { 0.5 }   →  20.0
//!
//! // import 进来的同一段
//! → Runtime error (MIR main): Undefined function or task: []
//! ```
//!
//! ## 根因
//!
//! witness 侧**有意**把读索引编码成
//! `Call { callee: Name("[]"), args: [obj, i] }` —— 见 `parser_v3/emit.rs`
//! 的索引发射分支，以及 `tests/parser_v3_coverage.rs::index_expr_parses`
//! 的断言「expected Index to parse as Call("[]")」。
//!
//! 直接 emit 路径**同时**发出真正的 `MirInst::Index`，所以顶层没事。但
//! `WitnessLowerer` 的 `Call` 分支**没有解码**这个编码，产出
//! `MirInst::Call(dst, "[]", [obj, idx])`，运行期解释成函数调用。
//!
//! 实测两条路径产出的 handle body（修复前）：
//!
//! ```text
//! 裸   [Var(0,"t"), Const(1,1.0), Call(2,"[]",[0,1]),  Assign("x",2)]
//! 管线 [Var(8,"t"), Const(9,1.0), Index(10,8,9),     Assign("x",10)]
//! ```
//!
//! ## 为什么只在这两处显形
//!
//! | 入口 | 走哪条 | 修复前 |
//! |------|--------|--------|
//! | `mora run` 顶层 | 直接 emit → `Index` | ✅ 正常 |
//! | `handle` body / handler | `lower_block_witness_to_mir`（witness） | ❌ |
//! | `import` 进来的模块 | `ParserV3::compile` 的 witness 路径 | ❌ |
//! | `eval()` 编译的代码 | 同上 | ❌ |
//!
//! `import` / `eval` 之所以中招，是因为它们**不过 9 层管线**
//! （`cli::compile_and_opt` 才跑 `run_pipeline`），直接用裸
//! `ParserV3::compile` 的 witness 产出。
//!
//! ## 本文件的判据
//!
//! **两条编译路径跑同一份源码，结果必须逐字相同。** 这类分叉最难自查 ——
//! 因为「测试跑的管线」和「用户跑的管线」根本不是同一条。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;

fn exec(func: mora::mir::MirFunction) -> String {
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// 裸路径：`ParserV3::compile`，不过 9 层管线。`import` / `eval` 走的就是它。
fn run_bare(src: &str) -> String {
    match ParserV3::compile(src) {
        Ok((f, _)) => exec(f),
        Err(e) => format!("COMPILE-ERR: {e}"),
    }
}

/// 管线路径：`cli::compile_and_opt`，与 `mora run` 同构。
fn run_pipeline(src: &str) -> String {
    match mora::cli::compile_and_opt(src, None) {
        Ok((f, _)) => exec(f),
        Err(e) => format!("COMPILE-ERR: {e}"),
    }
}

/// handle body 里的索引（witness 嵌套 lower 路径）
const HANDLE_LIT: &str = "let x = 0.0\nhandle random_random {\n  x = [1,2][99]\n} {\n  0.5\n}\nx\n";
const HANDLE_STR: &str =
    "let x = 0.0\nhandle random_random {\n  x = \"abc\"[99]\n} {\n  0.5\n}\nx\n";
const HANDLE_VAR: &str =
    "let x = 0.0\nlet t = [1,2]\nhandle random_random {\n  x = t[99]\n} {\n  0.5\n}\nx\n";
const HANDLE_OK: &str =
    "let x = 0.0\nlet t = [10,20,30]\nhandle random_random {\n  x = t[1]\n} {\n  0.5\n}\nx\n";

#[test]
fn handle_body_index_agrees_across_both_compile_paths() {
    let cases: &[(&str, &str)] = &[
        ("handle 列表字面量索引越界", HANDLE_LIT),
        ("handle 字符串索引越界", HANDLE_STR),
        ("handle 变量索引越界", HANDLE_VAR),
        ("handle 合法索引", HANDLE_OK),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        let bare = run_bare(src);
        let pipe = run_pipeline(src);
        if bare != pipe {
            failures.push(format!("  [{name}]\n    裸   = {bare}\n    管线 = {pipe}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个 handle+索引用例在两条编译路径下结果不同：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 不能只验「两条一致」—— 两条一起错也会通过。所以另钉一组**绝对值**。
#[test]
fn handle_body_index_has_correct_absolute_value() {
    // 合法索引：t = [10,20,30]，t[1] = 20.0
    assert_eq!(run_pipeline(HANDLE_OK), "Float(20.0)", "管线路径应得 20.0");
    assert_eq!(
        run_bare(HANDLE_OK),
        "Float(20.0)",
        "裸路径（import/eval 走的）此前报 Undefined function or task: []"
    );
    // 越界：真实错误信息，不是「调用一个不存在的函数」
    assert!(
        run_bare(HANDLE_VAR).contains("index 99 out of bounds (len 2)"),
        "裸路径应报真实的越界错误，实得：{}",
        run_bare(HANDLE_VAR)
    );
    assert!(
        run_bare(HANDLE_STR).contains("string index 99 out of bounds (len 3)"),
        "裸路径字符串越界应报真实错误，实得：{}",
        run_bare(HANDLE_STR)
    );
}

/// 回归点本身：witness 侧把索引编码成 `Call("[]")` 是**有意**的，
/// 但 lower 必须把它解码回 `MirInst::Index`。锁住「解码后没有残留的
/// `Call("[]")`」。
#[test]
fn witness_index_encoding_is_decoded_back_to_index_instruction() {
    use mora::mir::witness::{WitnessCallee, WitnessKind};

    let (func, witnesses) = ParserV3::compile("let t = [1,2]\nt[0]\n").expect("compile");

    // witness 侧：确实是 Call("[]") 编码（有意为之）
    fn has_index_encoding(w: &mora::mir::witness::MirWitness) -> bool {
        let self_hit = matches!(
            &w.kind,
            WitnessKind::Call { callee: WitnessCallee::Name(n), .. } if n == "[]"
        );
        self_hit || w.child_witnesses().iter().any(|c| has_index_encoding(c))
    }
    let encoded = witnesses.iter().any(has_index_encoding);
    assert!(
        encoded,
        "witness 侧应仍把索引编码为 Call(\"[]\") —— 那是既定契约，\
         若不再如此说明编码变了，本测试需重新审视"
    );

    // 但 lower 之后不得残留 Call("[]")，必须是 Index
    let has_call_bracket = func
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::Call(_, n, _) if n == "[]"));
    assert!(
        !has_call_bracket,
        "lower 后不应残留 Call(\"[]\") —— 索引必须解码成 MirInst::Index"
    );
    assert!(
        func.body
            .iter()
            .any(|i| matches!(i, mora::mir::MirInst::Index(..))),
        "顶层索引应产出 MirInst::Index"
    );
}

/// 覆盖范围：handle 是唯一「嵌套 lower 块」的场景吗？不是 —— `import` 与
/// `eval()` 编译的代码整体走 witness 路径，所以它们比 handle 受影响更广。
/// 这里钉住「顶层源码本身两条路径一致」，防止将来新增别的分叉。
#[test]
fn ordinary_programs_agree_across_both_compile_paths() {
    let cases: &[(&str, &str)] = &[
        ("顶层列表索引越界", "[1,2][99]\n"),
        ("顶层列表索引正常", "[1,2][0]\n"),
        (
            "普通循环",
            "let s = 0\nfor i in range(0, 5)\n  assign s = s + i\nend\ns\n",
        ),
        ("if/else", "let c = 1\nif c == 1\n  8.0\nelse\n  7.0\nend\n"),
        ("dict 取键", "let d = {a: 1, b: 2}\nd[\"a\"]\n"),
        ("list 方法", "[3,1,2].sort()\n"),
        (
            "while+continue",
            "let i = 0\nlet s = 0\nwhile i < 5\n  assign i = i + 1\n  if i % 2 == 0\n    continue\n  end\n  assign s = s + i\nend\ns\n",
        ),
        ("dict 索引嵌套", "let d = {a: {b: 7}}\nd[\"a\"][\"b\"]\n"),
        ("字符串索引", "let s = \"abc\"\ns[1]\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        let bare = run_bare(src);
        let pipe = run_pipeline(src);
        if bare != pipe {
            failures.push(format!("  [{name}]\n    裸   = {bare}\n    管线 = {pipe}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个普通用例在两条编译路径下结果不同：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// 逐语言特性铺开扫描
// ─────────────────────────────────────────────────────────────────────

/// D14 只修了「handle body 里的索引」这一处分叉。**其余分叉不知道还有没有**
/// —— 所以把上面两个手挑用例集扩成逐特性铺开：字面量 / 算术 / 索引 / 控制流 /
/// 函数闭包 / 方法 / 转换 builtin / 块构造 / 杂项，逐类各取代表。
///
/// 实测 68 条里只有 1 条分叉（D14 那个 dict 字面量的 HashMap 序，与编译
/// 无关），即两条路径目前**语义等价**。本测试的作用是：将来任何新的分叉都
/// 会在这里显形，而不是等到某个用户 import 一个模块才发现。
fn sweep_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        // ── 字面量 ─────────────────────────────────────────────
        ("lit int", "1\n"),
        ("lit float", "1.5\n"),
        ("lit str", "\"a\"\n"),
        ("lit bool", "true\n"),
        ("lit nil", "nil\n"),
        ("lit bigint", "999n\n"),
        ("lit list", "[1,2,3]\n"),
        ("lit char", "'x'\n"),
        // ── 算术 ───────────────────────────────────────────────
        ("add", "1 + 2\n"),
        ("div float", "1.0 / 2.0\n"),
        ("mod", "7 % 3\n"),
        ("unary neg", "0 - 5\n"),
        ("cmp eq", "1 == 1\n"),
        ("cmp lt", "1 < 2\n"),
        ("logic and", "true and false\n"),
        ("logic or", "true or false\n"),
        ("not", "not true\n"),
        // ── 索引 ───────────────────────────────────────────────
        ("index list", "[10,20,30][1]\n"),
        ("index dict", "let d = {a: 1}\nd[\"a\"]\n"),
        ("index str", "let s = \"abc\"\ns[1]\n"),
        ("index nested list", "[[1,2],[3,4]][0][1]\n"),
        (
            "index nested dict",
            "let d = {a: {b: 7}}\nd[\"a\"][\"b\"]\n",
        ),
        ("index oob", "[1,2][9]\n"),
        ("index by var", "let t = [1,2,3]\nlet i = 1\nt[i]\n"),
        // ── 控制流 ─────────────────────────────────────────────
        ("if", "let c = 1\nif c == 1\n  8.0\nelse\n  7.0\nend\n"),
        ("if no else", "let c = 1\nif c == 1\n  8.0\nend\n"),
        (
            "if elseif",
            "let c = 2\nif c == 1\n  1.0\nelse if c == 2\n  2.0\nelse\n  3.0\nend\n",
        ),
        (
            "while",
            "let i = 0\nlet s = 0\nwhile i < 4\n  assign s = s + i\n  assign i = i + 1\nend\ns\n",
        ),
        (
            "while break",
            "let i = 0\nwhile true\n  assign i = i + 1\n  if i > 3\n    break\n  end\nend\ni\n",
        ),
        (
            "while continue",
            "let i = 0\nlet s = 0\nwhile i < 5\n  assign i = i + 1\n  if i % 2 == 0\n    continue\n  end\n  assign s = s + i\nend\ns\n",
        ),
        (
            "for over list",
            "let s = 0\nfor x in [1,2,3]\n  assign s = s + x\nend\ns\n",
        ),
        (
            "for over dict keys",
            "let d = {a: 1, b: 2}\nlet s = 0\nfor k in d.keys()\n  assign s = s + 1\nend\ns\n",
        ),
        (
            "for over range",
            "let s = 0\nfor i in range(0, 4)\n  assign s = s + i\nend\ns\n",
        ),
        (
            "nested loops",
            "let s = 0\nfor i in range(0, 3)\n  for j in range(0, 3)\n    assign s = s + i * j\n  end\nend\ns\n",
        ),
        // ── 函数 / 闭包 ────────────────────────────────────────
        // 注意：Mora 的具名函数是 `task name(...) ... end`，**没有**
        // `fn name(...) ... end` 这种写法（spec §6.2 作用域表只列了
        // `task name()` / `fn(x)` / `let` 三种）。早先这里误写了 `fn add(...)`，
        // 两条编译路径都解析失败，而扫描里「两边都编译失败就跳过」的分支
        // 把它**静默吞了** —— 于是「68 条全一致」这个结论其实只有 65 条成立。
        // 现改成规范写法，并把该分支改为硬失败（见本文件末尾的说明）。
        (
            "具名函数 task 定义+调用",
            "task add(a, b)\n  a + b\nend\nadd(2, 3)\n",
        ),
        (
            "闭包",
            "let mk = fn(x) fn(y) x + y end end\nlet f = mk(10)\nf(5)\n",
        ),
        (
            "递归 task",
            "task fact(n)\n  if n <= 1\n    1.0\n  else\n    n * fact(n - 1)\n  end\nend\nfact(5)\n",
        ),
        // ── 方法 ───────────────────────────────────────────────
        ("list push", "[1].push(2)\n"),
        ("list pop", "[1,2].pop()\n"),
        ("list map", "[1,2].map(fn(x) x * 2 end)\n"),
        ("list filter", "[1,2,3].filter(fn(x) x > 1 end)\n"),
        ("list sort", "[3,1,2].sort()\n"),
        ("list sum", "[1,2,3].sum()\n"),
        ("dict get", "let d = {a: 1}\nd.get(\"a\")\n"),
        ("dict keys", "let d = {a: 1, b: 2}\nd.keys()\n"),
        ("dict values", "let d = {a: 1, b: 2}\nd.values()\n"),
        ("str upper", "\"ab\".upper()\n"),
        ("str split", "\"a,b\".split(\",\")\n"),
        ("str replace", "\"abc\".replace(\"a\", \"z\")\n"),
        ("num abs", "(0 - 3).abs()\n"),
        // ── 转换 builtin ───────────────────────────────────────
        ("str()", "str(42)\n"),
        ("int()", "int(\"42\")\n"),
        ("float()", "float(\"1.5\")\n"),
        ("bool()", "bool(0)\n"),
        ("len() list", "len([1,2,3])\n"),
        ("len() str", "len(\"abc\")\n"),
        // ── 块构造 ─────────────────────────────────────────────
        (
            "handle 正常",
            "let x = 0.0\nhandle random_random {\n  x = random.random()\n} {\n  0.5\n}\nx\n",
        ),
        (
            "handle 内 if",
            "let x = 0.0\nhandle random_random {\n  if 1 == 1\n    x = 7.0\n  end\n} {\n  0.5\n}\nx\n",
        ),
        (
            "handle 内循环",
            "let x = 0.0\nhandle random_random {\n  let t = [1,2,3]\n  for i in range(0, 3)\n    assign x = x + t[i]\n  end\n} {\n  0.5\n}\nx\n",
        ),
        (
            "handle 内 let",
            "let x = 0.0\nhandle random_random {\n  let y = 3.0\n  x = y * 2.0\n} {\n  0.5\n}\nx\n",
        ),
        (
            "handler 内索引",
            "let v = 0.0\nlet t = [10,20]\nhandle random_random {\n  v = random.random()\n} {\n  t[1]\n}\nv\n",
        ),
        ("with 块", "with model = \"m\"\n  1.0\nend\n"),
        // ── 杂项 ───────────────────────────────────────────────
        ("let 多绑定", "let a = 1\nlet b = 2\na + b\n"),
        ("assign 写变量", "let a = 1\nassign a = 5\na\n"),
        ("字符串拼接", "\"a\" + \"b\"\n"),
        (
            "链式索引+方法",
            "let d = {items: [1,2,3]}\nd[\"items\"][1]\n",
        ),
        // v0.104.6 D123：原为 `outer(5)(10)` —— 那**正是**「对表达式的结果
        // 再次调用」（`f(…)(…)`）的形态。修复前两条路径都能编译，但算出的
        // 末值是 `10` 而不是 `5 + 10 = 15`，**exit 0、零诊断** —— 这条用例
        // 一直在比对**同一个错误结果**，因此从未测到「深层嵌套闭包」的语义。
        // 该形态现已改为**明确报错**（spec §14.2 的 EBNF 无 `postfix` 产生式，
        // 属未承诺的能力缺口，见 CHANGELOG D123）。
        //
        // 改为**分两步调用**：仍是「深层嵌套闭包」，且要求两条路径给出
        // **正确且一致**的 `15`。
        (
            "深层嵌套闭包",
            "let outer = fn(a) fn(b) a + b end end\nlet g = outer(5)\ng(10)\n",
        ),
    ]
}

#[test]
fn every_language_feature_agrees_across_both_compile_paths() {
    let all = sweep_cases();
    assert!(
        all.len() >= 60,
        "扫描集只剩 {} 条 —— 特性用例被误删，扫描会退化",
        all.len()
    );
    let mut failures = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    for (name, src) in &all {
        let b = run_bare(src);
        let p = run_pipeline(src);
        if b.starts_with("COMPILE-ERR") && p.starts_with("COMPILE-ERR") {
            // **不再静默跳过。** 早先这里是 `continue`，于是三条误写成
            // `fn name(...)`（Mora 无此语法）的用例被吞掉，「68 条全一致」
            // 的结论其实只有 65 条成立 —— 扫描集用无效语法会让它退化成
            // 「什么都没测」而**看不出来**。现在直接列出并失败。
            invalid.push(format!("  [{name}] 两条路径都编译不过：{b}"));
            continue;
        }
        if b != p {
            failures.push(format!("  [{name}]\n    裸   = {b}\n    管线 = {p}"));
        }
    }
    assert!(
        invalid.is_empty(),
        "扫描集里有 {} 条用例在两条路径下都编译不过 —— 它们根本没测到语义，\
         扫描会静默退化。修正这些用例的语法（注意：Mora 的具名函数是 \
         `task name(...)`，不是 `fn name(...)`）：\n{}",
        invalid.len(),
        invalid.join("\n")
    );
    assert!(
        failures.is_empty(),
        "{} 个语言特性在两条编译路径下结果不同 —— `import` / `eval()` \
         编译的代码与内联代码语义分叉：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// typeck 层面的路径一致性（D14 同源，但作用在诊断上）
// ─────────────────────────────────────────────────────────────────────

/// `question = expr "?"`（spec :1278）**未实现** —— 语义在 spec §13.3 里
/// 明写「待补充」。这里只作对照，不参与路径一致性断言。
const UNIMPLEMENTED: &[(&str, &str)] = &[("expr question", "[1,2]?\n")];

/// **typeck 层面的路径一致性** —— 与 D14 同源，但作用在**诊断**上。
///
/// D14 修的是「两条路径**执行**结果不同」（witness 的 `Call("[]")` 未解码）。
/// 本条问的是更上游的问题：**两条路径对同一份源码的「接受 / 拒绝」判断是否
/// 一致**？因为 witness 来源不同（9 层管线 vs 裸 emit），typeck 拿到的输入
/// 就不同。
///
/// 一旦不一致，后果是**编辑器对能跑的代码报红**：
///
/// | 入口 | witness 来源 |
/// |------|-------------|
/// | `mora run <file>` | `cli::compile_and_opt`（9 层管线） |
/// | `mora --check <file>` | 裸 `ParserV3::compile` |
/// | LSP `check_diagnostics` | 裸 `ParserV3::compile` |
/// | REPL / `import` | 裸 `ParserV3::compile` |
///
/// 实测 36 条构造两路判定完全相同（0 分叉）。
fn path_agreement_cases() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&'static str, &'static str)> = vec![
        ("基础", "let a = 1\na\n"),
        ("带类型标注", "let a: Int = 1\na\n"),
        (
            "for",
            "let s = 0\nfor x in [1,2]\n  assign s = s + x\nend\ns\n",
        ),
        (
            "while",
            "let i = 0\nlet s = 0\nwhile i < 3\n  assign s = s + i\n  assign i = i + 1\nend\ns\n",
        ),
        ("if/else", "let c = 1\nif c == 1\n  1.0\nelse\n  2.0\nend\n"),
        ("task", "task t()\n  1\nend\n"),
        ("match", "let v = 1\nmatch v with\n  1 -> \"one\"\nend\n"),
        ("with", "with a = 1\n  a\nend\n"),
        (
            "handle",
            "let x = 0.0\nhandle random_random {\n  x = random.random()\n} {\n  0.5\n}\nx\n",
        ),
        (
            "perform",
            "let v = 0.0\nhandle E {\n  v = perform E(\"a\")\n} {\n  __arg0\n}\nv\n",
        ),
        ("observe", "observe trace do\n  1\nend\n"),
        ("worker", "worker w\n  1\nend\n"),
        ("transaction", "transaction\n  1\nend\n"),
        ("parallel", "parallel\n  1\nend\n"),
        ("macro", "macro m(x)\n  x\nend\n"),
        ("TEA model", "model M\n  count: Int\nend\n"),
        ("TEA app", "app tea_app\n  1\nend\n"),
        ("闭包", "let g = fn(x) x end\ng(1)\n"),
        ("dict", "let d = {a: 1}\nd[\"a\"]\n"),
        ("字符串方法", "\"a,b\".split(\",\")\n"),
        ("list 方法", "[1,2].len()\n"),
        ("int() 转换", "int(\"42\")\n"),
        ("bool() 转换", "bool(1)\n"),
        ("char 字面量", "'a'\n"),
        ("bigint", "999n\n"),
        ("pipe", "[1,2] |> len()\n"),
        ("quasiquote", "`(1)\n"),
        ("eval", "eval(\"1\")\n"),
        ("quote", "quote(1)\n"),
        ("嵌套闭包", "let o = fn(a) fn(b) a + b end end\no(1)(2)\n"),
        ("Router", "let r = Router::new()\nprint(r)\n"),
        ("McpServer", "let m = McpServer::new()\nprint(m)\n"),
        ("agent", "let a = agent.create(\"x\", {})\nprint(a)\n"),
        ("succeed", "succeed()\n"),
        // 应当两路**一致拒绝**的：类型错误
        ("类型错误 String=int", "let a: String = 1\na\n"),
        ("类型错误 Bool=float", "let a: Bool = 1.5\na\n"),
    ];
    v.extend_from_slice(UNIMPLEMENTED);
    v
}

fn accept_run_path(src: &str) -> Result<usize, String> {
    let (_f, ws) = mora::cli::compile_and_opt(src, None)?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&ws);
    if errs.is_empty() {
        Ok(ws.len())
    } else {
        Err("type errors".into())
    }
}

fn accept_check_path(src: &str) -> Result<usize, String> {
    let (_f, ws) = mora::parser_v3::ParserV3::compile(src)?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&ws);
    if errs.is_empty() {
        Ok(ws.len())
    } else {
        Err("type errors".into())
    }
}

#[test]
fn typeck_accepts_the_same_programs_on_both_paths() {
    let all = path_agreement_cases();
    let mut diffs = Vec::new();
    for (name, src) in &all {
        let a = accept_run_path(src);
        let b = accept_check_path(src);
        // 判据是「接受 / 拒绝」这一层，不是具体错误条数 —— 后者允许合理差异。
        // 两个谓词都是「是否被接受」，不等即为分叉。
        if a.is_ok() != b.is_ok() {
            diffs.push(format!("  [{name}]\n    run   = {a:?}\n    check = {b:?}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} / {} 条构造在两条 typeck 路径下判定不同 —— 后果是**编辑器对能跑的\
         代码报红**（`mora --check` 与 LSP 走裸 witness 路径，`mora run` 走管线）：\n{}",
        diffs.len(),
        all.len(),
        diffs.join("\n")
    );
}
