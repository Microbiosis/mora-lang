//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v044_orchestrate_validate {
    use crate::parser_v3::ParserV3;

    /// v0.44.0 / v0.92: orchestrate block syntax validation (witness 路径).
    /// 旧 `parse()`（MirExpr）已迁移到 `compile()`（MirWitness）。
    ///
    /// ⚠ v0.104.6 D269：本测试对 `max_rounds` 的**取值零鉴别力**。
    /// 它只断言「能不能解析」；而 D269 修复前，解析器恰恰是靠
    /// **把 `max_rounds: 5` 整行吞掉**才「解析成功」的 —— 值被静默丢弃、
    /// `Loop.rounds` 写死 `Some(1000)`。本测试在缺陷存在与不存在两种情况下
    /// **都绿**。行为级断言（数轮数）见
    /// `tests/orchestrate_loop_parsing.rs::d269_max_rounds_is_honored`。
    fn compile_ok(src: &str) -> bool {
        ParserV3::compile(src)
            .map(|(_func, witnesses)| !witnesses.is_empty())
            .unwrap_or_else(|e| panic!("ParserV3::compile failed: {}", e))
    }

    #[test]
    fn orchestrate_sequential_parses() {
        let src = r#"
orchestrate sequential x -> y
  agent a(x) => "a:" + x
  agent b(x) => "b:" + x
end
"#;
        assert!(compile_ok(src));
    }

    #[test]
    fn orchestrate_loop_with_on_predicate_parses() {
        let src = r#"
orchestrate loop x -> y, max_rounds: 5
  on: x == "done"
  agent a(x) => x
end
"#;
        assert!(compile_ok(src));
    }

    #[test]
    fn orchestrate_graph_with_predicate_edges_parses() {
        // ⚠ v0.104.6 D271：本测试对 `on:` 条件的**生效性零鉴别力**。
        // 它只断言「能不能解析」；而 D271 修复前，`@start -> b on: …` 的条件
        // 被写进一个**没有运行时读者**的字段（引擎只读 `condition_body`），
        // 解析成功、条件完整保存、却**从不生效**。本测试在缺陷存在与
        // 不存在两种情况下**都绿**。行为级断言见
        // `tests/graph_edge_conditions.rs::d271_false_condition_blocks_the_edge`。
        let src = r#"
orchestrate graph x -> y
  @start -> a
  @start -> b on: x == "research"
  a -> @exit
  b -> @exit
end
"#;
        assert!(compile_ok(src));
    }
}
