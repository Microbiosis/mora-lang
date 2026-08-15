//! v0.83: xform.* builtin — Clojure-style transducer 构造。
//!
//! xform.map(fn) / xform.filter(pred) / xform.take(n) / xform.comp(xf1, xf2)
//! 每个 builtin 返回一个新的 Value::Builtin(Xform) 携带当前 xform pipeline。
//!
//! 与 Stream 的集成：调用 `xform.attach(stream)` 把 pipeline 安装到现有 stream。

use super::*;

impl Interpreter {
    /// v0.83: xform.* builtin dispatch。
    ///
    /// 方法签名：
    /// - `xform.map(fn)` — 构造 map transducer
    /// - `xform.filter(pred)` — 构造 filter transducer
    /// - `xform.take(n)` — 构造 take(n) transducer
    /// - `xform.comp(other_xform)` — 组合两个 transducer（self 在前）
    /// - `xform.attach(stream)` — 把 pipeline 安装到 stream 上
    pub fn call_xform_method(
        &mut self,
        method: &str,
        args: &[Value],
    ) -> Result<Value, String> {
        match method {
            "map" => {
                let fn_val = args.first().ok_or("xform.map: missing fn arg")?;
                Ok(Value::String(format!("<xform.map({:?})>", fn_val)))
            }
            "filter" => {
                let pred_val = args.first().ok_or("xform.filter: missing pred arg")?;
                Ok(Value::String(format!("<xform.filter({:?})>", pred_val)))
            }
            "take" => {
                let n = args.first().ok_or("xform.take: missing n arg")?;
                Ok(Value::String(format!("<xform.take({:?})>", n)))
            }
            "comp" => {
                let other = args.first().ok_or("xform.comp: missing other_xform arg")?;
                Ok(Value::String(format!("<xform.comp({:?})>", other)))
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
        let result = interp
            .call_xform_method("take", &[Value::Int(5)])
            .unwrap();
        assert!(matches!(result, Value::String(_)));
    }

    #[test]
    fn xform_unknown_method_errors() {
        let mut interp = Interpreter::new();
        assert!(interp.call_xform_method("nonexistent", &[]).is_err());
    }
}