//! Dynamic list-of-lists matrix multiplication (`@`) helpers separated from `src/flow.rs`
//! so `src/flow.rs` stays focused on scalar/value dispatch, truthiness, and equality.

use crate::common::BinaryOp;
use crate::error::MoraError;
use crate::flow::{numeric, type_name};
use crate::value::Value;

fn eval_dot_slice(a: &[Value], b: &[Value], ctx: &str) -> Result<Value, MoraError> {
    if a.len() != b.len() {
        return Err(MoraError::Runtime(format!(
            "{ctx}: dimension mismatch {} vs {}",
            a.len(),
            b.len()
        )));
    }
    if a.is_empty() {
        return Err(MoraError::Runtime(format!(
            "{ctx}: vectors must not be empty"
        )));
    }
    let mut acc: Option<Value> = None;
    for (x, y) in a.iter().zip(b.iter()) {
        let prod = numeric::eval(&BinaryOp::Mul, x, y)?;
        acc = Some(match acc {
            None => prod,
            Some(prev) => numeric::eval(&BinaryOp::Add, &prev, &prod)?,
        });
    }
    Ok(acc.unwrap_or(Value::Int(0)))
}

fn expect_rect_matrix(rows: &[Value], side: &str) -> Result<(usize, Vec<Vec<Value>>), MoraError> {
    if rows.is_empty() {
        return Err(MoraError::Runtime(format!(
            "matmul (@): {side} matrix must not be empty"
        )));
    }
    let mut out = Vec::with_capacity(rows.len());
    let mut cols: Option<usize> = None;
    for (r_idx, r) in rows.iter().enumerate() {
        let Value::List(row_vals) = r else {
            return Err(MoraError::Runtime(format!(
                "matmul (@): {side} matrix row {r_idx} must be a list"
            )));
        };
        let row_slice = row_vals.to_vec();
        match cols {
            None => {
                if row_slice.is_empty() {
                    return Err(MoraError::Runtime(format!(
                        "matmul (@): {side} matrix rows must not be empty"
                    )));
                }
                cols = Some(row_slice.len());
            }
            Some(c) if row_slice.len() != c => {
                return Err(MoraError::Runtime(format!(
                    "matmul (@): {side} matrix must be rectangular (row {r_idx} has len {}, expected {c})",
                    row_slice.len()
                )));
            }
            _ => {}
        }
        out.push(row_slice);
    }
    Ok((cols.unwrap_or(0), out))
}

fn is_2d_list(vals: &[Value]) -> bool {
    !vals.is_empty() && matches!(vals[0], Value::List(_))
}

fn matrix_columns(mat: &[Vec<Value>], cols: usize) -> Vec<Vec<Value>> {
    let mut out = vec![Vec::with_capacity(mat.len()); cols];
    for row in mat {
        for (col_slot, val) in out.iter_mut().zip(row.iter()) {
            col_slot.push(val.clone());
        }
    }
    out
}

pub(crate) fn eval_list_matmul(left: &Value, right: &Value) -> Result<Value, MoraError> {
    let (Value::List(a_list), Value::List(b_list)) = (left, right) else {
        return Err(MoraError::Runtime(format!(
            "matmul (@) expects lists/matrices, got {} @ {}",
            type_name(left),
            type_name(right)
        )));
    };
    let a_vec = a_list.to_vec();
    let b_vec = b_list.to_vec();
    let a = a_vec.as_slice();
    let b = b_vec.as_slice();
    if a.is_empty() || b.is_empty() {
        return Err(MoraError::Runtime(
            "matmul (@): operands must not be empty".to_string(),
        ));
    }
    match (is_2d_list(a), is_2d_list(b)) {
        (false, false) => eval_dot_slice(a, b, "matmul (@)"),
        (true, true) => {
            let (ka, mat_a) = expect_rect_matrix(a, "left")?;
            let (nb, mat_b) = expect_rect_matrix(b, "right")?;
            if ka != mat_b.len() {
                return Err(MoraError::Runtime(format!(
                    "matmul (@): shape mismatch [{}x{}] @ [{}x{}]",
                    mat_a.len(),
                    ka,
                    mat_b.len(),
                    nb
                )));
            }
            let cols_b = matrix_columns(&mat_b, nb);
            let mut res_rows = Vec::with_capacity(mat_a.len());
            for row_a in &mat_a {
                let mut res_row = Vec::with_capacity(nb);
                for col_b in &cols_b {
                    res_row.push(eval_dot_slice(row_a, col_b, "matmul (@)")?);
                }
                res_rows.push(Value::List(res_row.into()));
            }
            Ok(Value::List(res_rows.into()))
        }
        (true, false) => {
            let (ka, mat_a) = expect_rect_matrix(a, "left")?;
            let mut res = Vec::with_capacity(mat_a.len());
            for row_a in &mat_a {
                if row_a.len() != b.len() {
                    return Err(MoraError::Runtime(format!(
                        "matmul (@): shape mismatch [{}x{}] @ [{}]",
                        mat_a.len(),
                        ka,
                        b.len()
                    )));
                }
                res.push(eval_dot_slice(row_a, b, "matmul (@)")?);
            }
            Ok(Value::List(res.into()))
        }
        (false, true) => {
            let (nb, mat_b) = expect_rect_matrix(b, "right")?;
            if a.len() != mat_b.len() {
                return Err(MoraError::Runtime(format!(
                    "matmul (@): shape mismatch [{}] @ [{}x{}]",
                    a.len(),
                    mat_b.len(),
                    nb
                )));
            }
            let cols_b = matrix_columns(&mat_b, nb);
            let mut res = Vec::with_capacity(nb);
            for col_b in &cols_b {
                res.push(eval_dot_slice(a, col_b, "matmul (@)")?);
            }
            Ok(Value::List(res.into()))
        }
    }
}
