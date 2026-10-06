//! v0.104.6 D68：4 个**运行期在册**的内建在 typeck 侧**完全没有登记**，
//! 返回类型是永不解算的 `TypeVar` —— 任何标注都被接受。
//!
//! ## 现象（修前）
//!
//! ```mora
//! let v = compress("abcdef", "head_tail")   → 运行期得 "abcdef"（String）
//! let v: Int = compress("abcdef", "head_tail")   → **被接受**  ❌
//! ```
//!
//! 判据一句话：**把返回值标成明显错误的类型，看是否被拒**。修前全部通过。
//!
//! ## 根因
//!
//! `Interpreter::new()`（`interpreter/mod.rs:495-511`）把 7 个内建一起 define
//! 进 globals：
//!
//! ```text
//! print  range  len  compose_prompt  tail  compress  crush_json
//! ```
//!
//! 而 typeck 侧（`builtin_callee_ty` 的 match 臂 + `builtin_signatures()`）
//! 只登记了前三个。剩下 4 个落到 `None`，调用方
//! `unwrap_or_else(|| self.fresh_type_var())` 给出**永不解算的 TypeVar**。
//! 与 D67 是同一类后果，但成因不同：D67 是「类型没算出来」，这里是
//! **「压根没查表」**。
//!
//! 四个的运行期返回类型逐个核对过（`builtin_impls.rs`）：
//! `compress` → `compress_top` 两条 `Ok` 都是 `Value::String`；
//! `crush_json` → `Ok(Value::String(...))`；`tail` → `Ok(Value::String(tail_str))`；
//! `compose_prompt` → `Ok(Value::String(buf))`。**全部是 String。**
//!
//! ## 连带修好的：`range` 的残差 `Arrow`
//!
//! 补签名时暴露出一个更普遍的问题 —— **声明元数大于实参数**时，curried 消解
//! 留下的残差 `Arrow` 会**原样当作返回类型**流出去：
//!
//! ```mora
//! let v: String = range(0, 3)   → expected string, got fn ('') -> list<float>
//! ```
//!
//! 而 `let v = range(0, 3); print(len(v)); print(v[0])` 却「正常」—— 只因
//! `len` / `[]` 接受任何类型，把这个错误的结果类型吞掉了。修法：内建被调
//! 一律返回**声明的**结果类型（`peel_all_arrows`）。内建的返回类型永远不是
//! 函数，故剥到最里层安全；用户闭包可能真的返回函数，那条路径**不剥**。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// **只跑类型检查**，不执行。
///
/// D68 的断言全部是「标注是否被 typeck 接受 / 拒绝」，与运行期无关。若改用
/// `run()`，会引入**假阳性**：`tail("f.txt", 10)` 因**文件不存在**而报
/// 「系统找不到指定的文件」，`run().is_err()` 照样为真 —— 测的就不是标注
/// 了。`compose_prompt` 同理（需要先定义 prompt section）。
fn typeck(src: &str) -> Result<(), String> {
    let (_func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = check_program_witnesses_bidirectional(&witnesses);
    if errs.is_empty() {
        Ok(())
    } else {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        Err(format!("TYPECK: {msgs:?}"))
    }
}

/// 4 个补登记的内建：**错标注必须被拒**。修前全部静默通过。
#[test]
fn newly_registered_builtins_reject_wrong_annotation() {
    // **覆盖完整性**：钉住「4 个」这个数 —— 将来增删内建而无人更新本文件时
    // 会当场失守（与 D60 / D61 的覆盖完整性断言同型）。
    let cases = [
        (
            "compress",
            "let v: Int = compress(\"abcdef\", \"head_tail\")\nprint(1)\n",
        ),
        (
            "crush_json",
            "let v: Int = crush_json([1, 2], 10)\nprint(1)\n",
        ),
        ("tail", "let v: Int = tail(\"f.txt\", 10)\nprint(1)\n"),
        (
            "compose_prompt",
            "let v: Int = compose_prompt(\"s\")\nprint(1)\n",
        ),
    ];
    assert_eq!(
        cases.len(),
        4,
        "内建负例清单被改动 —— 增删用例请同步更新本断言，否则「返回类型是否被追踪」\
         就失去钉子"
    );
    for (name, src) in cases {
        assert!(
            typeck(src).is_err(),
            "[{name}] 返回 String 的内建配 `Int` 标注必须被拒 —— 若通过了，\
             说明它又没有 typeck 签名、返回类型退化成了 TypeVar\n  src={src:?}"
        );
    }
}

/// 正确标注必须照常通过；无标注也必须照常。
#[test]
fn newly_registered_builtins_accept_string_annotation() {
    for (name, src) in [
        (
            "compress",
            "let v: String = compress(\"abcdef\", \"head_tail\")\nprint(1)\n",
        ),
        (
            "crush_json",
            "let v: String = crush_json([1, 2], 10)\nprint(1)\n",
        ),
        ("tail", "let v: String = tail(\"f.txt\", 10)\nprint(1)\n"),
        (
            "compose_prompt",
            "let v: String = compose_prompt(\"s\")\nprint(1)\n",
        ),
        (
            "compress 无标注",
            "let v = compress(\"abcdef\", \"head_tail\")\nprint(1)\n",
        ),
    ] {
        if let Err(e) = typeck(src) {
            panic!("[{name}] String 标注必须被接受\n  err={e}\n  src={src:?}");
        }
    }
}

/// **可选尾参**：补签名时最容易被打破的地方 —— 声明元数一旦大于实参数，
/// 残差 `Arrow` 会顶掉返回类型（`range` 修前就是这样）。两种写法都必须过。
#[test]
fn optional_trailing_argument_still_typechecks() {
    for (name, src) in [
        // 3 参（带 opts）
        (
            "compress 带 opts",
            "let v: String = compress(\"abcdefgh\", \"head_tail\", {head_pct: 0.4, tail_pct: 0.4})\nprint(1)\n",
        ),
        // 2 参（省略 opts）
        (
            "compress 省略 opts",
            "let v: String = compress(\"abcdefgh\", \"head_tail\")\nprint(1)\n",
        ),
        (
            "crush_json 带 opts",
            "let v: String = crush_json([1, 2], 10, {})\nprint(1)\n",
        ),
        (
            "crush_json 省略 opts",
            "let v: String = crush_json([1, 2], 10)\nprint(1)\n",
        ),
        // range 声明 3 参却常被 2 参调用 —— 修前返回残差 Arrow
        (
            "range 少传 step",
            "let v: list<number> = range(0, 3)\nprint(1)\n",
        ),
    ] {
        if let Err(e) = typeck(src) {
            panic!("[{name}] 可选尾参省略时返回类型不得退化成残差 Arrow\n  err={e}\n  src={src:?}");
        }
    }
}

/// `range` 的残差 `Arrow` 修前只在**有标注**时才显形 —— 无标注时被
/// `len` / `[]` 的宽松参数吞掉了。本测试用标注把它钉住。
#[test]
fn range_return_type_is_list_not_residual_arrow() {
    assert!(
        typeck("let v: list<number> = range(0, 3)\nprint(1)\n").is_ok(),
        "`range(0, 3)` 的返回类型必须是 list，而不是残差 `Arrow`"
    );
    assert!(
        typeck("let v: String = range(0, 3)\nprint(1)\n").is_err(),
        "`range(0, 3)` 返回 list，配 String 标注必须被拒"
    );
}

/// 既有 3 个内建 + 多传实参的行为不得被本次改动波及。
#[test]
fn pre_existing_builtins_unaffected() {
    // 多传实参仍被拒（`builtin_declared_ret` 只在实参用尽后兜底，
    // 不能把「多传」也一起放过）
    assert!(
        typeck("let v: String = merge_with(\"k\", \"append\", 1)\nprint(1)\n").is_err(),
        "多传实参仍应被拒（merge_with 声明 2 参）"
    );
    assert!(
        typeck("let v: String = range(0, 3, 1, 5)\nprint(1)\n").is_err(),
        "多传实参仍应被拒（range 声明 3 参）"
    );
    // 既有内建的返回类型不变
    assert!(typeck("print(1)\n").is_ok(), "print 仍可用");
    assert!(
        typeck("let v: Int = len(\"ab\")\nprint(v)\n").is_ok(),
        "len 仍返 Int"
    );
    assert!(
        typeck("let v: String = print(1)\nprint(1)\n").is_err(),
        "print 仍返 Nil，配 String 标注必须被拒"
    );
    // print 变参未被波及
    assert!(typeck("print(1, 2, 3)\n").is_ok(), "print 仍是变参");
}
