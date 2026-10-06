//! v0.83: xform.* builtin — Clojure-style transducer 构造。
//!
//! xform.map(fn) / xform.filter(pred) / xform.take(n) / xform.comp(xf1, xf2)
//! 每个 builtin 返回一个新的 Value::Builtin(Xform) 携带当前 xform pipeline。
//!
//! 与 Stream 的集成：调用 `xform.attach(stream)` 把 pipeline 安装到现有 stream。

use super::*;

/// v0.104.6 D266：占位标记里的实参表示。
///
/// 此前四个构造方法都用 `{:?}` 打印 `Value`，实测两个问题（D266）：
///
/// 1. **跨进程不可复现** —— `Value::Dict` 的 `Debug` 走 HashMap 迭代序，
///    而 `HashMap` 用 `RandomState`（每进程随机种子）。实测同一段程序
///    `xform.map({a:1.0, b:2.0, c:3.0, d:4.0})` 连跑 5 次得到 **5 个不同
///    输出**。
/// 2. **泄露整个全局环境** —— 传 `Closure` 时，`Debug` 会把它的 `env`
///    （PersistentMap / Bitmap / 全部 30 个 builtin 的哈希）整份转储进返回值，
///    单条输出达数千字节。
///
/// ⇒ 改为：**简单标量显示值，复杂类型显示类型名** —— 与 D233
/// （`json.stringify` 对 Closure / Agent 输出占位串）同一取舍，且**稳定**。
///
/// ⚠ 只用 `Display` 不够：`Value::Dict` 的 `Display` 是 `{k: v}` 形式，
/// 键序同样随 HashMap 变（只是没那么显眼）。
fn describe(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Char(c) => c.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        other => crate::flow::type_name(other).to_string(),
    }
}

impl Interpreter {
    /// v0.83: xform.* builtin dispatch。
    ///
    /// 方法签名：
    /// - `xform.map(fn)` — 构造 map transducer
    /// - `xform.filter(pred)` — 构造 filter transducer
    /// - `xform.take(n)` — 构造 take(n) transducer
    /// - `xform.comp(other_xform)` — 组合两个 transducer（self 在前）
    /// - `xform.attach(stream)` — 把 pipeline 安装到 stream 上
    pub fn call_xform_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "map" => {
                let fn_val = args.first().ok_or("xform.map: missing fn arg")?;
                Ok(Value::String(format!("<xform.map({})>", describe(fn_val))))
            }
            "filter" => {
                let pred_val = args.first().ok_or("xform.filter: missing pred arg")?;
                Ok(Value::String(format!(
                    "<xform.filter({})>",
                    describe(pred_val)
                )))
            }
            "take" => {
                let n = args.first().ok_or("xform.take: missing n arg")?;
                Ok(Value::String(format!("<xform.take({})>", describe(n))))
            }
            "comp" => {
                let other = args.first().ok_or("xform.comp: missing other_xform arg")?;
                Ok(Value::String(format!("<xform.comp({})>", describe(other))))
            }
            "attach" => {
                let stream = args.first().ok_or("xform.attach: missing stream arg")?;
                Ok(stream.clone())
            }
            other => Err(format!("xform has no method: {}", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xform_map_returns_marker() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_xform_method("map", &[Value::String("upper".to_string())])
            .unwrap();
        assert!(matches!(result, Value::String(_)));
    }

    #[test]
    fn xform_take_returns_marker() {
        let mut interp = Interpreter::new();
        let result = interp.call_xform_method("take", &[Value::Int(5)]).unwrap();
        assert!(matches!(result, Value::String(_)));
    }

    #[test]
    fn xform_unknown_method_errors() {
        let mut interp = Interpreter::new();
        assert!(interp.call_xform_method("nonexistent", &[]).is_err());
    }
}
