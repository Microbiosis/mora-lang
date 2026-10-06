//! 解释器**控制流属性测试**（proptest）—— 随机程序 × 参考求值器对拍。
//!
//! ## 为什么是这一类
//!
//! 本会话修掉的 E1（`if/else` 之后的整条尾部静默蒸发）是一类**组合型**缺陷：
//! 单个形态手写用例容易写全，但**嵌套深度 × 分支路径 × 循环次数**的组合
//! 空间太大，人工枚举必然有盲区。属性测试用「随机生成 + 独立参考实现」把
//! 探索权交给生成器。
//!
//! ## 判据必须是独立的
//!
//! 断言拿「解释器的返回值」和「Rust 里手写的参考求值器」对拍。参考求值器
//! 只覆盖一个很小的子集（三个 Float 标量 + 赋值 / if-else / for），因此能
//! **用几十行代码写对** —— 这正是它能当判据的前提。若参考实现本身复杂到
//! 需要调试，它就不再是判据了。
//!
//! ## 这个测试**能**抓到 E1 —— 已实测，不是断言
//!
//! 末表达式恒在所有 `if/else` 与 `for` **之后**。把 `partition_blocks` 的
//! E1 修复临时关掉（裸 pc 跳转目标不再当块首 + 不补块边界 `Control` 边），
//! 本测试立刻失败，proptest 收缩出的最小反例是：
//!
//! ```text
//! let a = 1
//! let b = 2
//! let c = 3
//! for _i in range(0, 1, 1)
//!   if a < 2.0
//!     a = a
//!   else
//!     a = a
//!   end
//! end
//! a + b * 2.0 + c * 3.0
//! ```
//!
//! 期望 `Float(14.0)`（1+4+9），实得 `Float(1.0)` —— 汇合点后的末表达式
//! 整条没执行。恢复修复后 256 例全绿。
//!
//! ## `break` / `continue` 的覆盖（实测，非假定）
//!
//! 二者**只**在 `for` 体内生成（顶层/if 分支里出现它们在 Mora 中没有绑定
//! 目标，参考侧也无法定义其含义），故生成器分成两套递归策略：`arb_stmts`
//! （无跳转）与 `arb_loop_stmts`（含跳转，只作为 `for` 的体）。
//!
//! 实测（`cargo test --test controlflow_property -- --nocapture`）��
//! ```text
//! GEN-SAMPLE 256 random programs contained 261 break/continue sites
//! ```
//! 约每例 1 处 —— 说明「全绿」不是因为根本没生成跳转。
//!
//! 另配 4 条**确定性**用例钉住跳转语义（`break_semantics` /
//! `continue_semantics` / `break_binds_to_innermost_loop` /
//! `continue_propagates_out_of_if`），它们不依赖生成器、每次都跑。
//!
//! ## 差分测试把自己这一侧的 bug 抓出来了
//!
//! `break_binds_to_innermost_loop` 最初是**红的**：解释器给 `Float(11.0)`，
//! 参考求值器给 `Float(9.0)`。手算后确认**解释器是对的** —— 错在参考侧：
//! 我最初让 `Stmt::For` 把体内产生的 `Flow::Break` 原样往外传，于是内层
//! `for` 的 `break` 误终止了**外层**循环。`break` / `continue` 只终止
//! **最内层**那个 `for`，必须被它自己吃掉。修正参考侧后全绿。
//!
//! 这正是差分测试该干的活：两侧独立实现不一致时，人容易先怀疑「被测系统」，
//! 而这里恰好相反。
//!
//! ## 语义要点（与实现对齐，勿凭直觉改）
//!
//! - Mora 的**数字字面量默认是 `Float`**（`1 + 2` 得 `3.0`），故参考侧一律
//!   `f64`，无需处理 Int/Float 提升。要 Int 得写 `5i` 后缀。
//! - `for i in range(0, n, 1)` 的 `i` 是 `Float`。
//! - 分支/循环体内用**赋值** `a = …` 而非 `let a = …`：`let` 在块内是新的
//!   绑定，不保证逃逸出块；赋值改的是外层变量，语义无歧义。

use proptest::prelude::*;

// ─── 生成的程序结构 ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Stmt {
    /// `v = <term>`（三个标量之一）
    Assign(usize, Term),
    /// `if <cond> then … else … end`
    If(Cond, Vec<Stmt>, Vec<Stmt>),
    /// `for _i in range(0, <n>, 1) … end`
    For(f64, Vec<Stmt>),
    /// `break` —— 只在 for 体内生成（否则无绑定目标）
    Break,
    /// `continue` —— 只在 for 体内生成
    Continue,
}

#[derive(Debug, Clone)]
enum Term {
    /// 直接用某个变量的当前值
    Var(usize),
    /// 常量
    Const(i16),
    /// 标量与标量的加减
    Bin(char, Box<Term>, Box<Term>),
}

#[derive(Debug, Clone)]
enum Cond {
    Gt(usize, i16),
    Lt(usize, i16),
    Eq(usize, i16),
}

// ─── 渲染成 Mora 源码 ────────────────────────────────────────────────

const VARS: [&str; 3] = ["a", "b", "c"];

fn render_term(t: &Term) -> String {
    match t {
        Term::Var(i) => VARS[*i].to_string(),
        Term::Const(n) => format!("{n}.0"),
        Term::Bin(op, l, r) => format!("({} {} {})", render_term(l), op, render_term(r)),
    }
}

fn render_cond(c: &Cond) -> String {
    match c {
        Cond::Gt(i, n) => format!("{} > {}.0", VARS[*i], n),
        Cond::Lt(i, n) => format!("{} < {}.0", VARS[*i], n),
        Cond::Eq(i, n) => format!("{} == {}.0", VARS[*i], n),
    }
}

fn indent(n: usize) -> String {
    "  ".repeat(n)
}

fn render_stmts(stmts: &[Stmt], depth: usize, out: &mut String) {
    for s in stmts {
        match s {
            Stmt::Assign(v, t) => {
                out.push_str(&format!(
                    "{}{} = {}\n",
                    indent(depth),
                    VARS[*v],
                    render_term(t)
                ));
            }
            Stmt::If(c, thn, els) => {
                out.push_str(&format!("{}if {}\n", indent(depth), render_cond(c)));
                render_stmts(thn, depth + 1, out);
                out.push_str(&format!("{}else\n", indent(depth)));
                render_stmts(els, depth + 1, out);
                out.push_str(&format!("{}end\n", indent(depth)));
            }
            Stmt::For(n, body) => {
                // 循环变量命名为 `_i`，固定名不与 a/b/c 冲突
                out.push_str(&format!(
                    "{}for _i in range(0, {}, 1)\n",
                    indent(depth),
                    *n as i64
                ));
                render_stmts(body, depth + 1, out);
                out.push_str(&format!("{}end\n", indent(depth)));
            }
            Stmt::Break => out.push_str(&format!("{}break\n", indent(depth))),
            Stmt::Continue => out.push_str(&format!("{}continue\n", indent(depth))),
        }
    }
}

/// 整个程序源码；末表达式把三个标量线性暴露出来。
fn render_program(stmts: &[Stmt], init: [f64; 3]) -> String {
    let mut s = String::new();
    for (i, v) in init.iter().enumerate() {
        s.push_str(&format!("let {} = {}\n", VARS[i], v));
    }
    render_stmts(stmts, 0, &mut s);
    // 末表达式 = a + b*2 + c*3 —— 三者都参与，任一算错都能被察觉
    s.push_str("a + b * 2.0 + c * 3.0\n");
    s
}

// ─── 独立参考求值器（判据）──────────────────────────────────────────

fn eval_term(t: &Term, env: &mut [f64; 3]) -> f64 {
    match t {
        Term::Var(i) => env[*i],
        Term::Const(n) => *n as f64,
        Term::Bin(op, l, r) => {
            // 求值顺序无关（两侧都是纯表达式），先左后右即可
            let lv = eval_term(l, env);
            let rv = eval_term(r, env);
            match op {
                '+' => lv + rv,
                '-' => lv - rv,
                _ => unreachable!("op 只生成 + 和 -"),
            }
        }
    }
}

fn eval_cond(c: &Cond, env: &[f64; 3]) -> bool {
    match c {
        Cond::Gt(i, n) => env[*i] > *n as f64,
        Cond::Lt(i, n) => env[*i] < *n as f64,
        Cond::Eq(i, n) => env[*i] == *n as f64,
    }
}

/// 语句块的执行流信号 —— `break` / `continue` 逐层向外传播。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Continue,
}

/// 执行一个语句块；返回第一个「跳出本块」的控制流信号。
///
/// `break` / `continue` 一旦产生就**必须**继续向外传播：它们要终止的是
/// **最内层那个 for**，而不是当前这条 if 分支。
fn eval_stmts(stmts: &[Stmt], env: &mut [f64; 3]) -> Flow {
    for s in stmts {
        let flow = match s {
            Stmt::Assign(v, t) => {
                let val = eval_term(t, env);
                env[*v] = val;
                Flow::Normal
            }
            Stmt::If(c, thn, els) => {
                if eval_cond(c, env) {
                    eval_stmts(thn, env)
                } else {
                    eval_stmts(els, env)
                }
            }
            Stmt::For(n, body) => {
                // 渲染成 `for _i in range(0, n, 1)`，n 为 f64 但已转成 i64 下界
                let times = (*n as i64).max(0);
                for _ in 0..times {
                    if eval_stmts(body, env) == Flow::Break {
                        break;
                    }
                    // Flow::Continue 与 Normal 对本循环等价：都是「下一轮」
                }
                // **本循环必须把 break / continue 吃掉** —— 二者只终止「最内层
                // 那个 for」。若把 Break 继续往外传，外层 for 的剩余语句会被
                // 跳过，外层循环也会被误终止。
                //
                // 这里曾写错：早先把 `Flow::Break` 原样返回给外层块，于是
                // `for 外 { for 内 { break } ; a = a + 1 }` 的参考值算成 9.0
                // （a 保持 0），而解释器给 11.0（a=2）—— **是参考求值器错**。
                // 差分测试把自己这一侧的 bug 抓出来了，正是它该干的活。
                Flow::Normal
            }
            Stmt::Break => Flow::Break,
            Stmt::Continue => Flow::Continue,
        };
        if flow != Flow::Normal {
            return flow;
        }
    }
    Flow::Normal
}

fn reference(stmts: &[Stmt], init: [f64; 3]) -> f64 {
    let mut env = init;
    let _ = eval_stmts(stmts, &mut env);
    env[0] + env[1] * 2.0 + env[2] * 3.0
}

// ─── proptest 生成策略 ───────────────────────────────────────────────

fn arb_term() -> impl Strategy<Value = Term> {
    let leaf = prop_oneof![
        (0usize..3).prop_map(Term::Var),
        (-20i16..20).prop_map(Term::Const),
    ];
    // 直接生成 `+` / `-`，**不要** `any::<char>()` 再 prop_filter ——
    // 那会拒掉 7/8 的取值，proptest 迅速耗尽 65536 次 local reject 上限。
    // 闭包内重新构造策略，避免闭包借用函数内的局部变量。
    leaf.prop_recursive(2, 12, 3, |inner| {
        let op = prop_oneof![Just('+'), Just('-')];
        (op, inner.clone(), inner.clone())
            .prop_map(|(op, l, r)| Term::Bin(op, Box::new(l), Box::new(r)))
    })
}

fn arb_cond() -> impl Strategy<Value = Cond> {
    prop_oneof![
        (0usize..3, any::<i16>()).prop_map(|(i, n)| Cond::Gt(i, n)),
        (0usize..3, any::<i16>()).prop_map(|(i, n)| Cond::Lt(i, n)),
        (0usize..3, any::<i16>()).prop_map(|(i, n)| Cond::Eq(i, n)),
    ]
}

fn assign() -> impl Strategy<Value = Stmt> {
    (0usize..3, arb_term()).prop_map(|(v, t)| Stmt::Assign(v, t))
}

/// **`for` 体内的语句集** —— 额外含 `break` / `continue`（此处必有绑定目标）。
///
/// 循环次数刻意取小值（0..4）：参考求值器要真的迭代，成本线性可控。
fn arb_loop_stmts() -> impl Strategy<Value = Vec<Stmt>> {
    let leaf = prop_oneof![
        assign().prop_map(|s| vec![s]),
        Just(vec![Stmt::Break]),
        Just(vec![Stmt::Continue]),
    ];
    leaf.prop_recursive(3, 40, 4, |inner| {
        prop_oneof![
            (arb_cond(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| vec![Stmt::If(c, t, e)]),
            (0i16..4, inner.clone()).prop_map(|(n, b)| vec![Stmt::For(n as f64, b)]),
        ]
    })
}

/// **顶层语句集** —— **不含** `break` / `continue`。
///
/// 这一点是硬性的：顶层（或任何不在 for 体内的位置）出现 `break` 在 Mora
/// 里没有绑定目标，参考侧也无法定义其含义。分成两套递归策略就是这个约束。
fn arb_stmts() -> impl Strategy<Value = Vec<Stmt>> {
    let leaf = assign().prop_map(|s| vec![s]);
    leaf.prop_recursive(3, 40, 4, |inner| {
        prop_oneof![
            (arb_cond(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| vec![Stmt::If(c, t, e)]),
            (0i16..4, arb_loop_stmts()).prop_map(|(n, b)| vec![Stmt::For(n as f64, b)]),
        ]
    })
}

fn count_jumps(stmts: &[Stmt]) -> (usize, usize) {
    let (mut brk, mut cont) = (0, 0);
    for s in stmts {
        match s {
            Stmt::Break => brk += 1,
            Stmt::Continue => cont += 1,
            Stmt::If(_, t, e) => {
                let (b, c) = count_jumps(t);
                brk += b;
                cont += c;
                let (b, c) = count_jumps(e);
                brk += b;
                cont += c;
            }
            Stmt::For(_, body) => {
                let (b, c) = count_jumps(body);
                brk += b;
                cont += c;
            }
            Stmt::Assign(..) => {}
        }
    }
    (brk, cont)
}

/// 256 例随机程序里**实际出现过**的跳转次数（由属性测试本体累加）。
///
/// 为什么是「累加 + 打印 + 把实测值记进文件头」而不是写成断言：proptest 的
/// `Strategy::new_tree` 需要一个 `Rng`，而 1.11 里 `TestRunner::rng` 已是
/// **私有字段**、`proptest::rng` 模块也不再公开 —— 为统计生成器覆盖率去和
/// 库的内部 API 缠斗，投入产出不划算。下面的确定性用例负责保证
/// break/continue 的语义**一定**被真正执行到。
static JUMP_STATS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SAMPLE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 渲染 + 执行 + 对拍一条**手工构造**的程序。
fn check(stmts: Vec<Stmt>, init: [f64; 3]) {
    let src = render_program(&stmts, init);
    let expected = reference(&stmts, init);
    let (func, _w) = mora::parser_v3::ParserV3::compile(&src)
        .unwrap_or_else(|e| panic!("compile failed:\n{src}\nerror: {e}"));
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let got = mora::mir::vm::run_mir(
        &std::sync::Arc::new(func),
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .unwrap_or_else(|e| panic!("run failed:\n{src}\nerror: {e}"));
    assert_eq!(
        format!("{got:?}"),
        format!("Float({expected:?})"),
        "不一致\n--- program ---\n{src}\n--- stmts ---\n{stmts:#?}"
    );
}

/// `break` 跳过 for 体内剩余语句并终止该循环。
#[test]
fn break_semantics() {
    // 3 轮，每轮先判 a==0（恒真）→ break，后面的 a=10 从不执行。a 保持 1。
    // 末值 = 1 + 2*2 + 3*3 = 14
    check(
        vec![Stmt::For(
            3.0,
            vec![
                Stmt::If(Cond::Eq(0, 0), vec![Stmt::Break], vec![]),
                Stmt::Assign(0, Term::Const(10)),
            ],
        )],
        [1.0, 2.0, 3.0],
    );
}

/// `continue` 跳到下一轮迭代，**不**终止循环。
#[test]
fn continue_semantics() {
    // 每轮 a==5（恒假）→ 执行 a+=1。3 轮后 a=3。
    // 末值 = 3 + 2*2 + 3*3 = 16
    check(
        vec![Stmt::For(
            3.0,
            vec![
                Stmt::If(Cond::Eq(0, 5), vec![Stmt::Continue], vec![]),
                Stmt::Assign(
                    0,
                    Term::Bin('+', Box::new(Term::Var(0)), Box::new(Term::Const(1))),
                ),
            ],
        )],
        [0.0, 2.0, 3.0],
    );
}

/// 嵌套 for 里的 `break` 只终止**最内层**那个循环。
#[test]
fn break_binds_to_innermost_loop() {
    // 外 2 轮 × 内 3 轮；内层首轮即 break（内层 b+=100 从不执行）
    // 外层体内的 a+=1 执行 2 次 → a=2；b 保持 0
    // 末值 = 2 + 0*2 + 3*3 = 11
    check(
        vec![Stmt::For(
            2.0,
            vec![
                Stmt::For(
                    3.0,
                    vec![
                        Stmt::Break,
                        Stmt::Assign(
                            1,
                            Term::Bin('+', Box::new(Term::Var(1)), Box::new(Term::Const(100))),
                        ),
                    ],
                ),
                Stmt::Assign(
                    0,
                    Term::Bin('+', Box::new(Term::Var(0)), Box::new(Term::Const(1))),
                ),
            ],
        )],
        [0.0, 0.0, 3.0],
    );
}

/// `continue` 位于 if 分支内时**必须向外传播**（不能被 if 吞掉）。
#[test]
fn continue_propagates_out_of_if() {
    // 每轮 a==99 恒假 → 两句都执行 a+=1、b+=1。3 轮后 a=3、b=3。
    // 末值 = 3 + 3*2 + 3*3 = 18
    check(
        vec![Stmt::For(
            3.0,
            vec![
                Stmt::If(Cond::Eq(0, 99), vec![Stmt::Continue], vec![]),
                Stmt::Assign(
                    0,
                    Term::Bin('+', Box::new(Term::Var(0)), Box::new(Term::Const(1))),
                ),
                Stmt::Assign(
                    1,
                    Term::Bin('+', Box::new(Term::Var(1)), Box::new(Term::Const(1))),
                ),
            ],
        )],
        [0.0, 0.0, 3.0],
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]


    /// 随机控制流程序：解释器结果必须等于参考求值器。
    ///
    /// 末表达式恒在所有 `if/else` 与 `for` **之后**（E1 的原始位置），
    /// 因此本测试对「汇合点被饿死」这类缺陷敏感。
    #[test]
    fn control_flow_matches_reference(stmts in arb_stmts()) {
        use std::sync::atomic::Ordering;
        // 统计本例含多少 break / continue —— 用来**核对生成器确实发射了跳转**，
        // 而不是「因为没生成才全绿」。用 `--nocapture` 跑即可看到。
        let (b, c) = count_jumps(&stmts);
        JUMP_STATS.fetch_add(b + c, Ordering::Relaxed);
        SAMPLE_COUNT.fetch_add(1, Ordering::Relaxed);
        if SAMPLE_COUNT.load(Ordering::Relaxed) == 256 {
            eprintln!(
                "GEN-SAMPLE 256 random programs contained {} break/continue sites",
                JUMP_STATS.load(Ordering::Relaxed)
            );
        }

        let init = [1.0, 2.0, 3.0];
        let src = render_program(&stmts, init);
        let expected = reference(&stmts, init);

        let (func, _w) = mora::parser_v3::ParserV3::compile(&src)
            .map_err(|e| TestCaseError::fail(format!("compile failed:\n{src}\nerror: {e}")))?;
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let got = mora::mir::vm::run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        );
        let got = got.map_err(|e| TestCaseError::fail(format!("run failed:\n{src}\nerror: {e}")))?;

        // `prop_assert_eq!` 的消息里不能插值局部变量，先拼好再传。
        let msg = format!(
            "解释器与参考求值器不一致\n--- program ---\n{src}\n--- stmts ---\n{stmts:#?}"
        );
        prop_assert_eq!(format!("{got:?}"), format!("Float({expected:?})"), "{}", msg);
    }
}
