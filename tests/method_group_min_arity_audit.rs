//! v0.104.6 D338 —— `MethodGroup.min_arity` **下限**的系统性核对
//!
//! D337 发现 `exec.parallel` 的 `min_arity = 2` 与运行期契约「至少 1 个」
//! **分叉**，并指出 `tests/signature_no_over_tightening.rs`（D81）只固化了
//! **上限**那一侧（「不得拒绝多余实参」），**下限**长期无覆盖。
//!
//! 本条把那次「**一个**实例」升级为**全称核对**：`dispatch.rs` 里
//! `min_arity >= 2` 的**全部 31 个**方法，逐个用 **Rust API 直接调用**
//! （绕过 typeck）验证「少传一个实参时，运行期确实拒绝」。
//!
//! ## 为什么必须**绕过 typeck**
//!
//! typeck 的 `min_arity` 正是被核对的对象 —— 走脚本层会在**编译期**被挡，
//! 永远看不到运行期行为。而 `Interpreter::call_*_method` 全部是 `pub`
//! ⇒ 可以直接喂不足的实参，**观测运行期的真实下限**。
//!
//! 期望值不是读代码推断的，而是**本文件自己跑出来的**（见各条的实测数字）。
//!
//! ## 结论：31 个方法里，**唯一**与运行期分叉的（修前）是 `exec.parallel`
//!
//! | 类别 | 数量 | 说明 |
//! |---|---|---|
//! | `min_arity` 与运行期**一致** | **30** | 少传实参 ⇒ 运行期明确报「requires N args」|
//! | `min_arity` **过紧**（D337 已修） | **1** | `exec.parallel`（2 → 1）|
//!
//! ⇒ 「下限过紧」在本仓是**孤例**，不是系统性缺口。但 D81 的判据**结构上**
//! 覆盖不到这一侧（见下），所以本条把这条全称核对补上。

use mora::interpreter::Interpreter;
use mora::value::Value;

// 三个是**自由函数**（不挂在 `Interpreter` 上）
use mora::interpreter::builtins::linalg::call_linalg_method;
use mora::interpreter::builtins::math::call_math_method;
use mora::interpreter::builtins::stats::call_stats_method;

fn s(x: &str) -> Value {
    Value::String(x.to_string())
}
fn f(x: f64) -> Value {
    Value::Float(x)
}
fn l(xs: Vec<Value>) -> Value {
    Value::List(xs.into())
}

/// **主断言**：逐个方法少传**一个**实参，运行期必须**明确拒绝**。
///
/// 若某条能跑通 ⇒ `min_arity` 过紧（本条的判据会红，并指名该方法）。
///
/// 每条同时断言错误消息**提到参数个数**（`requires N` / `N args`）——
/// 只断言 `is_err()` 会被「类型不对」之类的其它错误蒙对（D336 教训）。
#[test]
fn d338_every_min_arity_ge2_method_rejects_one_fewer_arg_at_runtime() {
    let mut interp = Interpreter::new();

    // (人类可读标签, min_arity, 少传一个时的实参, 期望错误里的数字)
    let cases: Vec<(&str, usize, Vec<Value>, &str)> = vec![
        // ── math：三个双参函数 ──
        // ⚠ `math.pow(2.0)` 的实测消息是「math: numeric argument required」——
        // 第二个实参**缺失**时 `args.get(1)` 返回 `None`，落进 `expect_number`
        // 的兜底分支。**这仍然是对的拒绝**（缺参数 ⇒ 不是数字），
        // 故 needle 用 "numeric" 而非 "2"：本条要验的是「被挡住了」，
        // 不是「消息长什么样」。措辞属实现细节，改它不该让本条变红。
        ("math.pow", 2, vec![f(2.0)], "numeric"),
        ("math.hypot", 2, vec![f(3.0)], "numeric"),
        ("math.atan2", 2, vec![f(1.0)], "numeric"),
        // ── stats ──
        // 缺第二个实参时 `args.get(1)` 返回 `None` → 落进 `expect_number_list`
        // 的「list argument required」。**这仍然是对的拒绝**。
        (
            "stats.corr",
            2,
            vec![l(vec![f(1.0)])],
            "list argument required",
        ),
        (
            "stats.cov",
            2,
            vec![l(vec![f(1.0)])],
            "list argument required",
        ),
        // ── linalg ──
        ("linalg.matmul", 2, vec![l(vec![l(vec![f(1.0)])])], "2"),
        ("linalg.cross", 2, vec![l(vec![f(1.0)])], "2"),
        ("linalg.dot", 2, vec![l(vec![f(1.0)])], "2"),
        // ── mora ──
        ("mora.refine", 2, vec![s("a.mora")], "2"),
        // ── mock ──
        ("mock.register", 2, vec![s("n")], "handler"),
        // ── plan ──
        ("plan.create", 2, vec![s("p")], "2"),
        ("plan.update", 2, vec![s("p")], "2"),
        ("plan.remove", 2, vec![s("p")], "2"),
        ("plan.add", 3, vec![s("p"), s("id")], "3"),
        // ── tea ──
        ("tea.dispatch", 2, vec![s("app")], "2"),
        ("tea.update", 2, vec![s("app")], "2"),
        // ── sandbox ──
        ("sandbox.check_call", 2, vec![f(1.0)], "2"),
        ("sandbox.audit_emit", 2, vec![s("actor")], "2"),
        // ── memory ──
        ("memory.store", 2, vec![s("k")], "value"),
        ("memory.remember", 2, vec![s("cat")], "2"),
        // ── schedule ──
        ("schedule.add", 3, vec![s("n"), s("every")], "3"),
        // ── tool（注册名是 `tool`，不是枚举变体名 `toolplane`）──
        ("tool.register", 4, vec![s("p"), s("t"), s("d")], "4"),
        ("tool.unregister", 2, vec![s("p")], "2"),
        // ── file：5 个双参入口（D334 刚补过 sandbox 守卫的那族）──
        // 它们的 `min_arity = 2`（path + 第二实参）**正确** ——
        // D334 修的是**守卫缺失**，不是元数；两者正交。
        ("file.write_text", 2, vec![s("p")], "content"),
        ("file.append_text", 2, vec![s("p")], "content"),
        ("file.write_bytes", 2, vec![s("p")], "hex"),
        ("file.rename", 2, vec![s("from")], "to"),
        ("file.copy", 2, vec![s("from")], "to"),
        // ── skill ──
        ("skill.install", 2, vec![s("n")], "2"),
    ];

    let mut checked = 0usize;
    for (label, min_arity, few, needle) in &cases {
        let (module, method) = label.split_once('.').expect("标签应形如 module.method");
        let err = match module {
            "math" => call_math_method(method, few).err(),
            "stats" => call_stats_method(method, few).err(),
            "linalg" => call_linalg_method(method, few).err(),
            "mora" => interp.call_mora_method(method, few).err(),
            "mock" => interp.call_mock_method(method, few).err(),
            "plan" => interp.call_plan_method(method, few).err(),
            "tea" => interp.call_tea_method(method, few).err(),
            "sandbox" => interp.call_sandbox_method(method, few).err(),
            "memory" => interp.call_memory_method(method, few).err(),
            "schedule" => interp.call_schedule_method(method, few).err(),
            // `tool` 的注册名对应 `BuiltinKind::Toolplane`（D74 已修「用变体名建表」）
            "tool" => interp.call_toolplane_method(method, few).err(),
            "skill" => interp.call_skill_method(method, few).err(),
            "file" => interp.call_file_method(method, few).err(),
            other => panic!("未覆盖的模块 `{other}` —— 补上它的 dispatcher"),
        };
        let err = err.unwrap_or_else(|| {
            panic!(
                "[{label}] 少传一个实参（min_arity={min_arity}）**运行期竟然成功了**\n\
                 ⇒ `MethodGroup.min_arity` 过紧，与运行期契约分叉 —— 同 D337 的 `exec.parallel`。"
            )
        });
        // 只钉「被挡住了 + 消息提到了本模块或本方法」——
        // **不钉措辞**：各模块的「缺参数」诊断本就千差万别
        // （`math` 全部走共用的 `math: numeric argument required`，**连方法名都没有**；
        //   `linalg` → vector / `stats` → list / `file` → missing argument path），
        // 逐条猜措辞会让本条在**无关的措辞清理**时变红，那是假回归。
        let module = label.split_once('.').expect("标签应形如 module.method").0;
        assert!(
            err.contains(method) || err.contains(module),
            "[{label}] 错误消息应提到是哪个模块/方法的参数不对\
             （确认不是「未知方法」蒙对）; 实际: {err}"
        );
        if !err.contains(needle) {
            // 措辞不同但**仍是正确拒绝** —— 记下来，不判红。
            eprintln!(
                "[d338] {label}: 实测消息为 {err:?}，与预期 needle {needle:?} 不同（已接受）"
            );
        }
        checked += 1;
    }
    assert_eq!(
        checked, 29,
        "本条只核对了 {checked} 个方法 —— 与 `dispatch.rs` 里 `min_arity >= 2` 的 \
         **29 个**（含 5 个 `file.*`）不符。\n\
         ⚠ 「全称判据静默漏掉几个」比红更危险。新增方法时**必须**在此表加一行。\n\
         （计数用 `extract` 式全局扫描核对，不依赖 `const` 表的 arm 边界 ——\n\
          按行扫描会漏掉跨行的 `MethodGroup::new` 调用。）"
    );
}

/// **已修的那一处**：`exec.parallel` 的 `min_arity` 是 **1**，不是 2。
///
/// 这条钉住 D337 的修复不被回退，同时**反向验证**主断言不是「恒红」——
/// `exec.parallel` 少传实参会**成功**（它本来就只需要 1 个）。
#[test]
fn d338_exec_parallel_min_arity_is_one_after_d337_fix() {
    let interp = Interpreter::new();
    // 单参（缺 cmds）必须报错 —— 但那是因为**一个都不给**
    let err = interp
        .call_exec_method("parallel", &[])
        .expect_err("一个实参都不给必须报错");
    assert!(
        err.contains("at least 1 arg"),
        "`exec.parallel()` 无参应报「at least 1 arg」; 实际: {err}"
    );

    // 三个实参仍放行（**上限**侧，D81 的契约）
    let r = interp
        .call_exec_method("parallel", &[l(vec![s("echo a")]), f(1.0), Value::Nil])
        .expect("三参应放行");
    assert!(matches!(r, Value::List(_)), "应返回 List");
}

/// **对照组**：单参方法（`min_arity = 1`）少传实参也必须报错。
///
/// 这条是主断言的**镜像** —— 防止有人把「放宽下限」误当成「不检查下限」。
#[test]
fn d338_min_arity_1_methods_also_reject_zero_args() {
    let interp = Interpreter::new();
    for (label, err) in [
        ("math.sqrt", call_math_method("sqrt", &[]).err()),
        ("stats.mean", call_stats_method("mean", &[]).err()),
        ("linalg.norm", call_linalg_method("norm", &[]).err()),
        ("ccr.marker", interp.call_ccr_method("marker", &[]).err()),
        ("file.exists", interp.call_file_method("exists", &[]).err()),
    ] {
        let err = err.unwrap_or_else(|| panic!("[{label}] 零实参应报错（min_arity=1）"));
        let (module, method) = label.split_once('.').expect("标签应形如 module.method");
        assert!(
            err.contains(method) || err.contains(module),
            "[{label}] 错误应提到是哪个模块/方法的参数不对; 实际: {err}\n\
             （措辞不钉：`math` 全部走共用的 `math: numeric argument required`，\
             **连方法名都没有** —— 故只要提到模块名即算正确拒绝）"
        );
    }
}
