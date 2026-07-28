#!/bin/bash
# Script to add if/else implementation to Parser V3 and MirExpr

echo "=== Implementing If/Else Expressions for v0.54 ==="

# Step 1: Add if_else() helper to src/mir/expr.rs
echo "Step 1: Adding MirExpr::if_else() helper..."
cat >> /tmp/if_else_helper.txt << 'EOF'

    /// Create an if/else expression
    pub fn if_else(
        cond: Self,
        then: Self,
        r#else: Option<Self>,
        span: Span,
    ) -> Self {
        Self {
            kind: MirExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                r#else: r#else.map(Box::new),
            },
            span,
            ty: None,
        }
    }
EOF

# Insert after pub fn dict method (around line 138)
head -n 137 src/mir/expr.rs > /tmp/expr_part1.rs
tail -n +138 src/mir/expr.rs > /tmp/expr_part2.rs
cat /tmp/expr_part1.rs /tmp/if_else_helper.txt /tmp/expr_part2.rs > src/mir/expr.rs
rm /tmp/expr_part1.rs /tmp/expr_part2.rs /tmp/if_else_helper.txt

echo "✓ Added MirExpr::if_else() helper"

# Step 2: Check if TokenType has If/Then/Else keywords
echo "Step 2: Checking TokenType definitions..."
if ! grep -q "TokenType::If" src/lexer.rs; then
    echo "⚠️ TokenType::If not found in lexer, may need to add keywords"
fi

# Step 3: Compile check
echo "Step 3: Running cargo build --lib..."
cargo build --lib 2>&1 | Select-String "Finished|error" -Context 0 | Select-Object -First 2

echo ""
echo "=== Summary ==="
echo "✅ Added if_else() helper to MirExpr"
echo "🔍 Need to verify TokenType exists for IF/THEN/ELSE"
echo "📝 Next: Implement parse_if_expression() in ParserV3"
