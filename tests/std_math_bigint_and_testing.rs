//! Pure-Mora `std/math/bigint` & `std/testing` standard library verification suite.
//!
//! Replaces the external `num-bigint` + `num-traits` Rust wrapper crate and external property-testing
//! dependencies with pure-Mora standard libraries (`std/math/bigint.mora` and `std/testing.mora`)
//! written 100% in Mora itself.

use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors:\n{}\nfor source:\n{src}",
        errs.iter()
            .map(mora::typeck::format_error)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error:\n{e}\nfor source:\n{src}"));
    val
}

#[test]
fn pure_mora_bigint_exact_arithmetic_and_comparisons_beyond_i64() {
    let src = r#"
import "std/math/bigint"

let ten = bigint_from_int(10)
let a = ten.pow(30)
let one = bigint_one()
let sum = a.add(one)
let diff = sum.sub(a)

[a.to_string(), sum.to_string(), diff.to_string(), json.stringify(sum.cmp(a) > 0), json.stringify(diff.eq(one))]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::String("1000000000000000000000000000000".to_string()),
                Value::String("1000000000000000000000000000001".to_string()),
                Value::String("1".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
            ]
            .into()
        )
    );
}

#[test]
fn pure_mora_bigint_div_rem_by_zero_and_i64_checked_boundaries() {
    let src = r#"
import "std/math/bigint"

task unwrap_err_str(res: BigIntResult<BigInt>): string
  return match res with
    BigIntResult::Ok(_) -> "ok"
    BigIntResult::Err(msg) -> msg
  end
end

task unwrap_i64_str(res: BigIntResult<int>): string
  return match res with
    BigIntResult::Ok(n) -> json.stringify(n)
    BigIntResult::Err(msg) -> msg
  end
end

let seven = bigint_from_int(7)
let zero = bigint_zero()
let div_err = unwrap_err_str(seven.checked_div(zero))
let rem_err = unwrap_err_str(seven.checked_rem(zero))

let max_i64 = bigint_from_int(9223372036854775807)
let min_i64 = bigint_from_int(-9223372036854775807 - 1)
let over_max = max_i64.add(bigint_one())
let under_min = min_i64.sub(bigint_one())

[div_err, rem_err, unwrap_i64_str(max_i64.to_i64_checked()), unwrap_i64_str(min_i64.to_i64_checked()), unwrap_i64_str(over_max.to_i64_checked()), unwrap_i64_str(under_min.to_i64_checked())]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::String("division by zero".to_string()),
                Value::String("modulo by zero".to_string()),
                Value::String("9223372036854775807".to_string()),
                Value::String("-9223372036854775808".to_string()),
                Value::String("bigint(9223372036854775808) out of i64 range".to_string()),
                Value::String("bigint(-9223372036854775809) out of i64 range".to_string()),
            ]
            .into()
        )
    );
}

#[test]
fn pure_mora_bigint_radix_gcd_lcm_mod_pow_and_f64_lossy() {
    let src = r#"
import "std/math/bigint"

task unwrap_big(res: BigIntResult<BigInt>): BigInt
  return match res with
    BigIntResult::Ok(v) -> v
    BigIntResult::Err(_) -> bigint_zero()
  end
end

task unwrap_str(res: BigIntResult<string>): string
  return match res with
    BigIntResult::Ok(s) -> s
    BigIntResult::Err(e) -> e
  end
end

task is_err_big(res: BigIntResult<BigInt>): bool
  return match res with
    BigIntResult::Ok(_) -> false
    BigIntResult::Err(_) -> true
  end
end

let hex = unwrap_big(bigint_from_str_radix("dead_beef_cafe_babe_1234", 16))
let hex_str = unwrap_str(hex.to_radix(16))
let bad_r1 = is_err_big(bigint_from_str_radix("10", 1))
let bad_r37 = is_err_big(bigint_from_str_radix("10", 37))
let bad_fmt = is_err_big(bigint_parse("  -  "))

let a = bigint_from_int(-48)
let b = bigint_from_int(18)
let g = a.gcd(b).to_string()
let l = a.lcm(b).to_string()
let l0 = a.lcm(bigint_zero()).to_string()

let mp = unwrap_big(bigint_from_int(2).mod_pow(bigint_from_int(10), bigint_from_int(1000))).to_string()
let mp_zero_mod = is_err_big(bigint_from_int(2).mod_pow(bigint_one(), bigint_zero()))
let mp_neg_exp = is_err_big(bigint_from_int(2).mod_pow(bigint_from_int(-1), bigint_from_int(5)))

let huge = bigint_from_int(2).pow(1200)
let neg_huge = huge.neg()
let f_pos_inf = math.is_inf(huge.to_f64_lossy()) and huge.to_f64_lossy() > 0.0
let f_neg_inf = math.is_inf(neg_huge.to_f64_lossy()) and neg_huge.to_f64_lossy() < 0.0

[hex_str, json.stringify(bad_r1), json.stringify(bad_r37), json.stringify(bad_fmt), g, l, l0, mp, json.stringify(mp_zero_mod), json.stringify(mp_neg_exp), json.stringify(f_pos_inf), json.stringify(f_neg_inf)]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::String("deadbeefcafebabe1234".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
                Value::String("6".to_string()),
                Value::String("144".to_string()),
                Value::String("0".to_string()),
                Value::String("24".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
                Value::String("true".to_string()),
            ]
            .into()
        )
    );
}

#[test]
fn pure_mora_std_testing_proptest_div_rem_and_gcd_identities() {
    let src = r#"
import "std/math/bigint"
import "std/testing"

let gen_big = fn()
  let hi = random.rand_int(-1000000000, 1000000000)
  let lo = random.rand_int(0, 1000000000)
  return bigint_from_int(hi).mul(bigint_from_int(1000000000)).add(bigint_from_int(lo))
end

let gen_nonzero_big = fn()
  let v = gen_big()
  if v.is_zero() then
    return bigint_one()
  end
  return v
end

let p_div_rem = prop_check2("div_rem_identity", gen_big, gen_nonzero_big, fn(a: BigInt, b: BigInt)
  let res = a.div_rem(b)
  return match res with
    BigIntResult::Ok(qr) -> qr.q.mul(b).add(qr.r).eq(a)
    BigIntResult::Err(_) -> false
  end
end, runs=48, seed=1337)

let p_gcd = prop_check2("gcd_divides_both", gen_big, gen_big, fn(a: BigInt, b: BigInt)
  let g = a.gcd(b)
  if g.is_zero() then
    return a.is_zero() and b.is_zero()
  end
  let ra = a.checked_rem(g)
  let rb = b.checked_rem(g)
  let ok_a = match ra with
    BigIntResult::Ok(r) -> r.is_zero()
    BigIntResult::Err(_) -> false
  end
  let ok_b = match rb with
    BigIntResult::Ok(r) -> r.is_zero()
    BigIntResult::Err(_) -> false
  end
  return ok_a and ok_b
end, runs=48, seed=7331)

[p_div_rem.passed, p_div_rem.runs == 48, p_gcd.passed, p_gcd.runs == 48]
"#;
    let out = compile_check_and_run(src);
    assert_eq!(
        out,
        Value::List(
            vec![
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
            ]
            .into()
        )
    );
}
