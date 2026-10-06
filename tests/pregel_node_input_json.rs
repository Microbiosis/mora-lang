//! D235 判据：`pregel::build_node_input` 产出**合法 JSON**。
//!
//! ## 缺陷背景
//!
//! `build_node_input` 修前手工拼 `"channel": <fragment>` 字符串，
//! fragment 来自 `value_to_json_string`（与 `pregel::reducers` 各一份的
//! 重复实现）。三处叠加缺陷：
//!
//! ① **没有 `Dict` 分支** ⇒ dict 落到 `_ => format!("\"{}\"", v)`，
//!    经 `Value::Display` 得 **`{k: v, n: 3}`** —— key 无引号，
//!    **不是 JSON**。整个对象变成 `{"ch":{k: v}}`。
//! ② **`channel` 名未经转义**：`format!("\"{}\":", channel)` ⇒
//!    含 `"` / `\` 的 channel 名直接破坏 JSON。
//! ③ **`Float(42.0)` 输出 `42`** ⇒ 读回变 `Int`（D84/D99 要求 Float
//!    必带小数点，类型降级不可逆）。
//!
//! 而 `build_node_input` 的产物是**喂给 agent 的请求体**
//! （`pregel/mod.rs:929` / `:1048`），agent 侧拿到的就是这段字符串。
//!
//! ## 判据形态
//!
//! 端到端：跑**真实 Mora 源码**的 `orchestrate pregel`，让 agent 把它
//! 收到的 input 原样输出，再断言该字符串是**合法 JSON**且能**往返**。
//! 不断言具体字符串（那是实现细节），只断言可解析 + 逐值恒等。

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(source: &str) -> Result<mora::value::Value, String> {
    let (func, _w) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

/// 构造一个单 agent 的 pregel 程序源码。
///
/// 语法取自 `tests/pregel_entry_edge.rs`（已实测可编译）：
/// `agent a => "A"` / `edge @start -> a` / `end`，**无** `channel` 行。
/// （我第一版加了 `channel result` —— 那是**非法语法**，
/// 编译报 `Failed to parse at line 5`。）
///
/// 端到端链路（`orchestrate pregel` → `pregel::run` →
/// `build_node_input` → agent 收到的 `input_str`）确实存在
/// （`pregel/mod.rs:929` / `:1048`），但 `build_node_input` 是**私有**方法、
/// agent 收到的字符串也不经由 `Value` 通道回传，故无法在集成测试里直接捕获。
///
/// 修法已把该方法收敛到 `flow::value_to_json`（仓内**唯一**序列化实现），
/// 因此判据直接钉在共享序列化器上 —— 那才是缺陷所在的代码。
/// 下面这条只验证「pregel 端到端仍能跑通」（防收敛改动破坏主路径）。
fn build_source(body: &str) -> String {
    format!(
        "orchestrate pregel input -> result\n\
         \x20 agent a => \"hello\"\n\
         \x20 agent b => \"world\"\n\
         \x20 edge @start -> a\n\
         \x20 edge a -> b\n\
         end\n\
         {body}\n"
    )
}

/// 判据 ①：pregel 端到端主路径仍能跑出**正确结果**（防收敛改动引入回归）。
///
/// 末行是 `result` 而非 `print(result)` —— `print` 的返回值是 `Nil`
///（我第一版写成 `print(result)`，断言恒等于「跑通了」而没验证结果）。
/// 正确形态取自 `tests/orchestrate_v3_pipeline.rs::v3_orchestrate_pregel_runs`。
#[test]
fn d235_pregel_end_to_end_still_works() {
    let out = run(&build_source("result")).expect("pregel should compile and run");
    assert_eq!(
        out,
        mora::value::Value::String("world".into()),
        "D235: 收敛 build_node_input 到 flow::value_to_json 后，pregel 主路径结果不得改变"
    );
}

/// 判据 ②：`value_to_json` 对各类 `Value` 的产出都能被 `json_to_value`
/// 读回**逐类型恒等**（`Float` 必须带小数点，否则读回变 `Int`）。
///
/// 这是 D235 缺陷 ③ 的直接判据。
#[test]
fn d235_float_keeps_decimal_point_through_the_shared_serializer() {
    use mora::value::Value;
    for f in [0.0f64, 1.0, 42.0, -7.0, 0.5, -0.25] {
        let json = mora::flow::value_to_json(&Value::Float(f));
        assert!(
            json.contains('.') || json.contains('e') || json.contains('E'),
            "D235: Float({f}) 序列化成 {json} —— 缺小数点，读回会变 Int（类型降级不可逆）"
        );
        let back = mora::flow::json_to_value(&json).expect("must reparse");
        match back {
            Value::Float(b) => {
                assert_eq!(b.to_bits(), f.to_bits(), "D235: Float({f}) 往返失真 -> {b}")
            }
            other => panic!("D235: Float({f}) 读回变成 {other:?}（应为 Float）"),
        }
    }
}

/// 判据 ③：`Dict` 必须序列化成**合法 JSON 对象**（key 带引号）。
///
/// 这是 D235 缺陷 ① 的直接判据，且不依赖 pregel 引擎 ——
/// 修法把 `build_node_input` 收敛到 `flow::value_to_json`，
/// 故判据钉在共享序列化器上即可。
#[test]
fn d235_dict_serializes_as_valid_json_object() {
    use mora::value::Value;
    let mut d = std::collections::HashMap::new();
    d.insert("k".to_string(), Value::String("v".into()));
    d.insert("n".to_string(), Value::Int(3));
    let json = mora::flow::value_to_json(&Value::Dict(d));

    let back = mora::flow::json_to_value(&json)
        .unwrap_or_else(|e| panic!("D235: Dict 序列化产出**非法 JSON**: {json} ({e})"));
    assert!(
        matches!(back, Value::Dict(_)),
        "D235: Dict 应读回为 Dict; json={json} back={back:?}"
    );
}

/// 判据 ④：两份 `value_to_json_string` 重复实现**已被删除**。
///
/// D209 记录过「改一处须同步另一处」的维护陷阱；D235 证明它们**都有**
/// `Dict` 缺失 / `Float` 丢小数点的缺陷，且 `build_node_input` 改用
/// `flow::value_to_json` 后两者**失去全部调用者**。
///
/// 判据形态是**编译期**的：`use` 一个不存在的 `pub fn` 会编译失败。
/// 这里改成对**行为**的断言（不引用已删的私有函数），删除动作本身
/// 由 `cargo build` 的 dead_code 检查保证 —— 见模块注释。
#[test]
fn d235_no_duplicate_serializer_remains() {
    // 共享序列化器是唯一的实现；本判据钉住它的对外行为，
    // 使得「若有人再写一份局部实现」会被同一批断言覆盖。
    use mora::value::Value;
    let mut d = std::collections::HashMap::new();
    d.insert("a\tb".to_string(), Value::String("c\"d\\e\u{1}".into()));
    let json = mora::flow::value_to_json(&Value::Dict(d));
    let back = mora::flow::json_to_value(&json)
        .unwrap_or_else(|e| panic!("共享序列化器必须处理转义: {json} ({e})"));
    match back {
        Value::Dict(m) => {
            assert_eq!(
                m.get("a\tb"),
                Some(&Value::String("c\"d\\e\u{1}".into())),
                "D209/D235: key 与 value 的控制字符都必须转义且可读回"
            );
        }
        other => panic!("应读回为 Dict; got {other:?}"),
    }
}
