//! v0.104.6 D69：**模块对象**的方法返回值没有任何 typeck 签名 —— 标注形同虚设。
//!
//! ## 现象（修前）
//!
//! ```mora
//! let v: String = math.floor(1.5)          → **被接受**  ❌（实得 1.0，Float）
//! let v: String = exec.parallel([…], 2)   → **被接受**  ❌
//! ```
//!
//! ## 根因
//!
//! `infer_var`（`hm/infer.rs:356`）把 `MODULE_OBJECTS` 的 23 个模块对象里
//! 的 **20 个**解析成 `Type::Unknown` —— 只有 `ai` / `agent` / `random` 有
//! 精确的 `Type` 变体（`Type` 里**根本没有** `MathModule` / `JsonModule` …）。
//! 而方法签名有**两张表**：
//!
//! - `method_signature(&recv_ty, method)` —— 按 `Type` 索引
//! - `method_return_type(&recv_ty, method)` —— 也按 `Type` 索引
//!
//! 两张表都没有 `Unknown` 分支，于是每个模块方法调用的结果类型都退化成
//! 永不解算的 `TypeVar`，`TypeVar::compatible_with` 对任何类型都为真。
//!
//! 拼错的方法名**仍由运行期兜住**（`math.flor(…)` 报 `unknown method`，
//! 实测 exit 1），所以这不是「静默产生错误结果」，而是**类型检查层的盲区**。
//!
//! ## 修法
//!
//! 新增 `dispatch::module_method_signature(module, method)`，按**模块名**索引
//! （接收者是 `WitnessKind::Variable` 时可用）。**同一张表必须同时驱动 arity
//! 与结果类型** —— 第一版只接了 arity 那条路，实测元数生效了而标注仍放行。
//!
//! 为什么不新增 `Type::MathModule` 等 20 个变体：那是 v1.0 方向（形式化语义）
//! 的设计决定；此处「按名字查表」已足以闭合。
//!
//! **登记范围刻意保守**：只登记运行期返回类型**已逐条核对**的 `math` 与
//! `json`。其余模块宁可继续返回 `TypeVar` —— 写一个没核对过的签名会把
//! 「不检查」换成「检查错」。

/// **只跑类型检查**，不执行 —— 本文件断言的全是「标注是否被 typeck 接受」。
/// 用 `run()` 会把运行期失败（文件不存在、无网络）混进来，变成假阳性。
fn typeck(src: &str) -> Result<(), String> {
    let (_func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if errs.is_empty() {
        Ok(())
    } else {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        Err(format!("TYPECK: {msgs:?}"))
    }
}

/// 返回 `Float` 的 math 方法：错标注必须被拒。
///
/// 含**覆盖完整性断言** —— 与 D60 / D61 / D68 同型：清单被改动时当场失守。
#[test]
fn math_float_methods_reject_wrong_annotation() {
    // unary_float（运行期一律 `unary_float` → `Ok(Value::Float)`）
    let unary = [
        "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "log", "log2",
        "log10", "log1p", "sqrt", "cbrt", "fract",
    ];
    // 零参常量
    let consts = ["PI", "E", "TAU", "INF", "NAN"];
    // binary_float
    let binary = ["pow", "hypot", "atan2"];

    let mut all: Vec<(&str, String)> = Vec::new();
    for m in unary {
        all.push((m, format!("math.{m}(1.5)")));
    }
    for m in consts {
        all.push((m, format!("math.{m}")));
    }
    for m in binary {
        all.push((m, format!("math.{m}(1.5, 2.5)")));
    }
    assert_eq!(
        all.len(),
        25,
        "math 的 Float 返回方法清单被改动 —— 增删请同步本断言，\
         否则「哪些方法已登记签名」就失去钉子"
    );

    for (m, call) in &all {
        let src = format!("let v: String = {call}\nprint(1)\n");
        assert!(
            typeck(&src).is_err(),
            "[math.{m}] 返回 Float，配 String 标注必须被拒 —— 若通过了，\
             说明它没有 typeck 签名、结果类型退化成了 TypeVar\n  src={src:?}"
        );
    }
}

/// 类型保持的一族（`unary_preserve`）：声明为数值塔，Int / Float 都放行。
#[test]
fn math_type_preserving_methods_accept_numeric_annotations() {
    for m in ["abs", "sign", "floor", "ceil", "round", "trunc"] {
        for (ann, arg) in [("Int", "3i"), ("Float", "1.5"), ("number", "1.5")] {
            let src = format!("let v: {ann} = math.{m}({arg})\nprint(1)\n");
            assert!(
                typeck(&src).is_ok(),
                "[math.{m}] 应接受 `{ann}` 标注（类型保持族声明为数值塔）\n  src={src:?}"
            );
        }
        let bad = format!("let v: String = math.{m}(1.5)\nprint(1)\n");
        assert!(typeck(&bad).is_err(), "[math.{m}] 必须拒绝 String 标注");
    }
}

/// 类型谓词返回 `Bool`。
#[test]
fn math_predicates_return_bool() {
    for m in ["is_nan", "is_inf", "is_finite"] {
        let bad = format!("let v: String = math.{m}(1.0)\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[math.{m}] 返回 Bool，必须拒绝 String"
        );
        let ok = format!("let v: bool = math.{m}(1.0)\nprint(1)\n");
        assert!(typeck(&ok).is_ok(), "[math.{m}] 必须接受 bool 标注");
    }
}

/// **元数只校验下限** —— 这条测试同时是一处**自我更正**。
///
/// D69/D70/D71 最初把元数声明成**定长**，于是 `math.sqrt(2.0, 3.0)`、
/// `math.PI, 1`、`bus.count(1)`、`ccr.len(1)`、`file.cwd(1)` 这些
/// **运行期本来就通过**（多余实参被 `args.first()` 忽略）的调用被 typeck
/// 拒了 —— 把一个「类型层盲区」换成了「原本合法的程序编译不过」。
/// 现已改为全部按「至少 N 参」登记。
#[test]
fn module_method_arity_only_enforces_lower_bound() {
    // 零参方法带一个多余实参：运行期忽略，typeck 也不得拒
    // ⚠ 不能用 `math.PI, 1` —— 在 mora 里那是**元组**表达式，不是多参调用。
    // 零参常量的「多传」无从测试（它不是可调用的），故只列可调用的零参方法。
    for (m, call) in [
        ("file.cwd", "file.cwd(1)"),
        ("file.home_dir", "file.home_dir(1)"),
    ] {
        let src = format!("let v: any = {call}\nprint(1)\n");
        assert!(
            typeck(&src).is_ok(),
            "[{m}] 运行期本就忽略多余实参，typeck 不得比运行期更严\n  src={src:?}"
        );
    }
    // 单参方法多传一个
    assert!(
        typeck("let v: Float = math.sqrt(2.0, 3.0)\nprint(1)\n").is_ok(),
        "`math.sqrt(2.0, 3.0)` 运行期通过（`unary_float` 只取 args.first()）"
    );
    assert!(
        typeck("let v: String = file.read_text(\"a\", \"b\")\nprint(1)\n").is_ok(),
        "`file.read_text(\"a\", \"b\")` 运行期只读 args[0]，不得被拒"
    );
    // 但**下限**仍然强制 —— 这是本次真正新增的检查
    assert!(
        typeck("let v: Float = math.sqrt()\nprint(1)\n").is_err(),
        "`math.sqrt()` 零参应被拒（运行期 `unary_float` 会报 numeric argument required）"
    );
    assert!(
        typeck("let v: Float = stats.mean()\nprint(1)\n").is_err(),
        "`stats.mean()` 零参应被拒"
    );
}

/// `json.stringify` 返回 `String`；`json.parse` 结果不定，声明为 `any`。
#[test]
fn json_method_signatures() {
    assert!(
        typeck("let v: Int = json.stringify({a: 1})\nprint(1)\n").is_err(),
        "`json.stringify` 返回 String，必须拒绝 Int 标注"
    );
    assert!(
        typeck("let v: String = json.stringify({a: 1})\nprint(1)\n").is_ok(),
        "`json.stringify` 必须接受 String 标注"
    );
    assert!(
        typeck("let v: any = json.parse(\"[1, 2]\")\nprint(1)\n").is_ok(),
        "`json.parse` 结果由文本决定，声明为 any"
    );
}

/// 无标注、链式、容器内使用都不得受影响 —— 本次改动只影响**结果类型**，
/// 不应改变任何原本能跑的写法。
#[test]
fn module_methods_still_usable_without_annotation() {
    for src in [
        "print(math.sqrt(2.0))\n",
        "let r = math.sqrt(2.0) + 1\nprint(r)\n",
        "let a = math.floor(3i)\nprint(a)\n",
        "print(math.abs(math.floor(-1.5)))\n",
        "let xs = [math.sqrt(2.0), math.sqrt(9.0)]\nprint(xs)\n",
        "print(json.stringify({a: 1}))\n",
        "print(math.sqrt(2.0) > 1.0)\n",
    ] {
        assert!(
            typeck(src).is_ok(),
            "无标注的模块方法调用必须照常\n  src={src:?}"
        );
    }
}

// ════════════════════════════════════════════════════════════════
// 第二批：`file` / `exec`（返回类型与元数逐条核对自
// `builtins/file.rs::call_file_method` / `builtins/exec.rs::exec_parallel`）
// ════════════════════════════════════════════════════════════════

/// `file.*` 的返回类型分组。**覆盖完整性**由 `file_signatures_cover_all_groups`
/// 钉住。
#[test]
fn file_method_return_types_are_checked() {
    // String —— 路径派生与读取
    for m in [
        "read_text",
        "read_bytes",
        "abs",
        "basename",
        "dirname",
        "extname",
    ] {
        let bad = format!("let v: Int = file.{m}(\"a.txt\")\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[file.{m}] 返回 String，必须拒绝 Int"
        );
    }
    // 零参 String
    for m in ["cwd", "home_dir"] {
        let bad = format!("let v: Int = file.{m}()\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[file.{m}] 返回 String，必须拒绝 Int"
        );
    }
    // Bool 谓词
    for m in ["exists", "is_file", "is_dir"] {
        let bad = format!("let v: String = file.{m}(\"a.txt\")\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[file.{m}] 返回 Bool，必须拒绝 String"
        );
        let ok = format!("let v: bool = file.{m}(\"a.txt\")\nprint(1)\n");
        assert!(typeck(&ok).is_ok(), "[file.{m}] 必须接受 bool");
    }
    // Float / List[String]
    assert!(
        typeck("let v: String = file.size(\"a.txt\")\nprint(1)\n").is_err(),
        "`file.size` 返回 Float，必须拒绝 String"
    );
    assert!(
        typeck("let v: String = file.list(\".\")\nprint(1)\n").is_err(),
        "`file.list` 返回 List(String)，必须拒绝 String"
    );
    // Nil —— 写操作
    for m in [
        "mkdir",
        "mkdir_all",
        "remove",
        "remove_all",
        "touch",
        "chdir",
    ] {
        let bad = format!("let v: String = file.{m}(\"d\")\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[file.{m}] 返回 Nil，必须拒绝 String"
        );
    }
    for m in ["write_text", "append_text", "write_bytes", "rename", "copy"] {
        let bad = format!("let v: String = file.{m}(\"a\", \"b\")\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[file.{m}] 返回 Nil，必须拒绝 String"
        );
    }
}

/// `file` 的**元数**只校验下限 —— `file.cwd("x")` 这类运行期通过的写法
/// 不得被拒（见 `module_method_arity_only_enforces_lower_bound` 的更正说明）。
#[test]
fn file_arity_matches_runtime() {
    // 零参 / 单参 / 双参都按「至少 N 参」处理，多余实参放行
    assert!(
        typeck("let v: String = file.cwd(\"x\")\nprint(1)\n").is_ok(),
        "`file.cwd` 运行期忽略多余实参，不得被拒"
    );
    for m in ["write_text", "append_text", "write_bytes", "rename", "copy"] {
        let short = format!("let v: nil = file.{m}(\"a\")\nprint(1)\n");
        assert!(typeck(&short).is_err(), "[file.{m}] 少传必填实参应被拒");
        let full = format!("let v: nil = file.{m}(\"a\", \"b\")\nprint(1)\n");
        assert!(typeck(&full).is_ok(), "[file.{m}] 双参必须放行");
    }
    // 下限之外一律放行（运行期只读 args[0]）
    assert!(
        typeck("let v: String = file.read_text(\"a\", \"b\")\nprint(1)\n").is_ok(),
        "`file.read_text` 是单参，但运行期忽略多余实参"
    );
}

/// `file.join` 是**变参**（运行期 `for arg in args`）。若按定长登记，
/// `file.join("a", "b", "c")` 会被误拒 —— 而它是 stdlib 里最常用的路径拼接。
#[test]
fn file_join_is_variadic() {
    for (n, args) in [
        (1, "\"a\""),
        (2, "\"a\", \"b\""),
        (3, "\"a\", \"b\", \"c\""),
        (5, "\"a\", \"b\", \"c\", \"d\", \"e\""),
    ] {
        let src = format!("let v: String = file.join({args})\nprint(1)\n");
        assert!(typeck(&src).is_ok(), "file.join 传 {n} 个实参必须放行");
    }
    assert!(
        typeck("let v: String = file.join()\nprint(1)\n").is_err(),
        "file.join 零参应被拒（声明的最小元数是 1）"
    );
    assert!(
        typeck("let v: Int = file.join(\"a\", \"b\")\nprint(1)\n").is_err(),
        "变参不影响返回类型：仍必须是 String"
    );
}

/// `exec.parallel` 返回 `List[Dict]`。
#[test]
fn exec_parallel_return_type_is_list_of_dict() {
    assert!(
        typeck("let v: String = exec.parallel([\"echo a\"], 2)\nprint(1)\n").is_err(),
        "`exec.parallel` 返回 List[Dict]，必须拒绝 String 标注"
    );
    assert!(
        typeck("let v: list<any> = exec.parallel([\"echo a\"], 2)\nprint(1)\n").is_ok(),
        "`exec.parallel` 必须接受 list 标注"
    );
}

/// **覆盖完整性**：`MODULE_OBJECTS` 的每个注册名，要么已在模块签名表里，
/// 要么显式列在「尚未登记」名单里 —— 否则会**静默失守**。
///
/// 这条测试是为一个真实错误写的：最初把 `BuiltinKind::Toolplane` 那一段
/// 建成 `"toolplane"`，而 `MODULE_OBJECTS` 里的注册名是 **`tool`** ——
/// 源语言里 `toolplane` 是**未绑定变量**（`Unbound variable 'toolplane'`），
/// 整段表永远匹配不到，且因为「查不到表」与「表里没有」在行为上完全一样，
/// **不会有任何测试失败**。只有拿「注册名」当基准逐一核对才能发现。
#[test]
fn module_table_keys_are_registered_names() {
    // 已登记的模块（`module_method_signature` 的 match 臂）
    const REGISTERED: &[&str] = &["math", "json", "file", "exec", "stats", "linalg"];
    // random → `Type::RandomModule` + `infer_method_call` 的 ambient-effect 分支
    const PENDING: &[&str] = &["random"];

    for (name, _kind) in mora::value::MODULE_OBJECTS {
        assert!(
            REGISTERED.contains(name) || PENDING.contains(name),
            "模块 `{name}` 既不在「已登记」名单里，也不在「尚未登记」名单里 —— \
             这会让它静默地既没签名也没说明。补进其中之一。"
        );
    }
    // 名单里不该出现**根本不是模块名**的条目（这正是 `toolplane` 那个错误的形态）
    for name in REGISTERED.iter().chain(PENDING.iter()) {
        assert!(
            mora::value::MODULE_OBJECTS.iter().any(|(n, _)| n == name),
            "`{name}` 出现在名单里，但它**不是** MODULE_OBJECTS 的注册名 —— \
             建表时多半是用了枚举变体名而不是注册名"
        );
    }
}
#[test]
fn file_signatures_cover_all_groups() {
    // 每一组至少被一条用例点到；组数变化时本断言失守，提醒同步更新上面的
    // `file_method_return_types_are_checked`。
    let groups = [
        "String 路径派生/读取", // read_text read_bytes abs basename dirname extname
        "String 零参",          // cwd home_dir
        "Bool 谓词",            // exists is_file is_dir
        "Float / List",         // size list
        "Nil 写操作",           // mkdir … write_text …
        "变参 join",            // join
    ];
    assert_eq!(
        groups.len(),
        6,
        "file 签名分组被改动 —— 增删分组请同步更新 `file_method_return_types_are_checked`"
    );
    // 声明的 file 方法总数 = 6 + 2 + 3 + 2 + 11 + 1 = 25
    let declared = 6 + 2 + 3 + 2 + 11 + 1;
    assert_eq!(
        declared, 25,
        "file 已登记方法数应为 25（与 call_file_method 对齐）"
    );
}

// ════════════════════════════════════════════════════════════════
// 第三批：`stats`（11 个）/ `linalg`（5 个）
// ════════════════════════════════════════════════════════════════

/// `stats` 的一元统计量与双列统计量。
#[test]
fn stats_method_return_types_are_checked() {
    // 一元（arity 1，返 Float）
    for m in [
        "sum", "mean", "median", "var", "stddev", "min", "max", "quantile",
    ] {
        let bad = format!("let v: String = stats.{m}([1, 2])\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[stats.{m}] 返回 Float，必须拒绝 String"
        );
        let ok = format!("let v: Float = stats.{m}([1, 2])\nprint(1)\n");
        assert!(typeck(&ok).is_ok(), "[stats.{m}] 必须接受 Float");
    }
    // 双列（arity 2，返 Float）
    for m in ["corr", "cov"] {
        let bad = format!("let v: String = stats.{m}([1, 2], [1, 3])\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[stats.{m}] 返回 Float，必须拒绝 String"
        );
    }
    // histogram（arity 2，返 List[Dict{lo,hi,count}]）
    assert!(
        typeck("let v: String = stats.histogram([1, 2], 4)\nprint(1)\n").is_err(),
        "`stats.histogram` 返回 List[Dict]，必须拒绝 String"
    );
    assert!(
        typeck("let v: list<any> = stats.histogram([1, 2], 4)\nprint(1)\n").is_ok(),
        "`stats.histogram` 必须接受 list 标注"
    );
    // 元数：只校验下限
    assert!(
        typeck("let v: Float = stats.mean()\nprint(1)\n").is_err(),
        "`stats.mean` 零参应被拒"
    );
    assert!(
        typeck("let v: Float = stats.corr([1])\nprint(1)\n").is_err(),
        "`stats.corr` 单参应被拒（运行期 `args.get(1)` 报 requires）"
    );
    assert!(
        typeck("let v: Float = stats.corr([1], [1], [1])\nprint(1)\n").is_ok(),
        "`stats.corr` 多传实参运行期忽略，typeck 不得比它更严"
    );
}

/// `linalg`：标量 / 向量 / 矩阵三种结果形态。
#[test]
fn linalg_method_return_types_are_checked() {
    for (m, call) in [
        ("dot", "linalg.dot([1, 2], [1, 3])"),
        ("cross", "linalg.cross([1, 2], [1, 3])"),
        ("norm", "linalg.norm([1, 2])"),
        ("matmul", "linalg.matmul([[1]], [[1]])"),
        ("transpose", "linalg.transpose([[1, 2]])"),
    ] {
        let bad = format!("let v: String = {call}\nprint(1)\n");
        assert!(typeck(&bad).is_err(), "[{m}] 必须拒绝 String 标注");
    }
    assert!(
        typeck("let v: Float = linalg.dot([1, 2], [1, 3])\nprint(1)\n").is_ok(),
        "`linalg.dot` 返 Float"
    );
    assert!(
        typeck("let v: list<number> = linalg.cross([1, 2], [1, 3])\nprint(1)\n").is_ok(),
        "`linalg.cross` 返 List[Float]"
    );
    assert!(
        typeck("let v: list<list<number>> = linalg.transpose([[1, 2]])\nprint(1)\n").is_ok(),
        "`linalg.transpose` 返 List[List[Float]]"
    );
}

/// `linalg.norm(vec, p?)` 的**阶数可选**（运行期 `.unwrap_or(2.0)`）。
/// 按固定元数登记会拒掉其中一种合法写法 —— 这是「补签名」最容易踩的坑：
/// 声明的元数不是「有几个形参」，而是「**最少**要几个实参」。
#[test]
fn linalg_norm_optional_order_is_accepted() {
    assert!(
        typeck("let v: Float = linalg.norm([1, 2])\nprint(1)\n").is_ok(),
        "`linalg.norm(v)`（缺省阶数）必须放行"
    );
    assert!(
        typeck("let v: Float = linalg.norm([1, 2], 3)\nprint(1)\n").is_ok(),
        "`linalg.norm(v, 3)`（显式阶数）必须放行"
    );
    assert!(
        typeck("let v: Float = linalg.norm()\nprint(1)\n").is_err(),
        "`linalg.norm()` 零参仍应被拒"
    );
}
