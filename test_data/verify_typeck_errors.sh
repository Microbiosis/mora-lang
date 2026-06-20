#!/bin/bash
# v0.05 e2e 验证脚本 — 检查 20 类 typeck 错误是否全部命中
# 用法: bash test_data/verify_typeck_errors.sh

set -e
MORA=${MORA:-./target/debug/mora}
SCRIPT="test_data/typeck_errors.mora"
EXPECTED_MIN=20  # 至少 20 个错误（级联可能更多）

echo "=== v0.05 e2e typeck 验证 ==="
OUTPUT=$($MORA --check "$SCRIPT" 2>&1) || true
ACTUAL=$(echo "$OUTPUT" | grep -c "Type error at line" || true)

echo "找到 $ACTUAL 个 type error（期望 >= $EXPECTED_MIN）"

# 检查每类错误是否命中
PASS=0
FAIL=0

check() {
  local label="$1"
  local pattern="$2"
  if echo "$OUTPUT" | grep -q "$pattern"; then
    echo "  ✅ $label"
    PASS=$((PASS + 1))
  else
    echo "  ❌ $label (pattern: $pattern)"
    FAIL=$((FAIL + 1))
  fi
}

check "类别 1:  let 类型不匹配"        "type mismatch: let"
check "类别 2:  无法推断"              "cannot infer type"
check "类别 3:  number * list"         "operator '\*' requires number"
check "类别 4:  number + list"         "operator '+' not defined"
check "类别 5:  比较类型错误"           "comparison requires number or string"
check "类别 6:  with model 类型"       "with model = ..."
check "类别 7:  with 未知 binding"     "with: unknown binding"
check "类别 8:  with temperature 类型" "with temperature = ..."
check "类别 9:  record_tokens input"   "record_tokens: input must be number"
check "类别 10: record_tokens output"  "record_tokens: output must be number"
check "类别 11: try-catch 不支持"      "try/catch: type 'MyError'"
check "类别 12: return 类型不匹配"     "return type mismatch: expected 'number', got 'string'"
check "类别 13: return 无值"           "missing return.*bad_return2"
check "类别 14: print 参数个数"        "function 'print' expects"
check "类别 15: range 参数个数"        "function 'range' expects"
check "类别 16: route 参数个数"        "route.*fast.*expects"
check "类别 17: ai_model model 类型"   "ai_model: model name must be string"
check "类别 18: ai_model temperature"  "ai_model: temperature must be number"
check "类别 19: ai_model max_tokens"   "ai_model: max_tokens must be number"
check "类别 20: ai_model system"       "ai_model: system must be string"

echo ""
echo "结果: $PASS / 20 通过, $FAIL 失败"

if [ "$FAIL" -gt 0 ]; then
  echo "❌ 有失败项"
  exit 1
fi
if [ "$ACTUAL" -lt "$EXPECTED_MIN" ]; then
  echo "❌ 错误数不足 ($ACTUAL < $EXPECTED_MIN)"
  exit 1
fi

echo "✅ 全部 20 类 typeck 错误命中，共 $ACTUAL 个错误"
exit 0
