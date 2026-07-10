//! Vimscript value coercion over [`Object`].
//!
//! We reuse `nvim-types`' [`Object`] as the value type (Number→`Integer`,
//! String→`String`, List→`Array`, Dict→`Dict`, Float→`Float`) so values flow to
//! Lua/RPC without a second conversion. This module implements Vimscript's
//! (quirky) coercion rules: strings coerce to their leading integer, and truthy
//! means "non-zero number".

use nvim_types::{Error, Object, Result};

/// Coerce to a Vimscript number. A string yields its leading integer (or 0),
/// a float truncates, a bool is 0/1.
pub fn to_number(obj: &Object) -> i64 {
    match obj {
        Object::Integer(i) => *i,
        Object::Float(f) => *f as i64,
        Object::Boolean(b) => *b as i64,
        Object::String(bytes) => {
            let s = String::from_utf8_lossy(bytes);
            let s = s.trim_start();
            let mut end = 0;
            let bytes = s.as_bytes();
            if !bytes.is_empty() && (bytes[0] == b'-' || bytes[0] == b'+') {
                end = 1;
            }
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            s[..end].parse().unwrap_or(0)
        }
        _ => 0,
    }
}

/// Coerce to a Vimscript string.
pub fn to_string(obj: &Object) -> String {
    match obj {
        Object::Integer(i) => i.to_string(),
        Object::Float(f) => format!("{f}"),
        Object::Boolean(b) => (*b as i64).to_string(),
        Object::String(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Object::Nil => String::new(),
        Object::Array(items) => {
            let parts: Vec<String> = items.iter().map(display_inner).collect();
            format!("[{}]", parts.join(", "))
        }
        Object::Dict(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("'{}': {}", k, display_inner(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        _ => String::new(),
    }
}

fn display_inner(obj: &Object) -> String {
    match obj {
        Object::String(bytes) => format!("'{}'", String::from_utf8_lossy(bytes)),
        other => to_string(other),
    }
}

/// Vimscript truthiness: non-zero number (strings coerce first).
pub fn is_truthy(obj: &Object) -> bool {
    to_number(obj) != 0
}

/// The Vimscript `type()` code for a value.
pub fn type_code(obj: &Object) -> i64 {
    match obj {
        Object::Integer(_) | Object::Boolean(_) => 0, // v:t_number
        Object::String(_) => 1,                       // v:t_string
        Object::LuaRef(_) => 2,                       // v:t_func
        Object::Array(_) => 3,                        // v:t_list
        Object::Dict(_) => 4,                         // v:t_dict
        Object::Float(_) => 5,                        // v:t_float
        _ => 0,
    }
}

/// Add two values with Vimscript semantics: `+` is always numeric (use `.` for
/// string concat).
pub fn arith(op: char, a: &Object, b: &Object) -> Result<Object> {
    // Float contagion: if either is a float, compute in floating point.
    if matches!(a, Object::Float(_)) || matches!(b, Object::Float(_)) {
        let x = as_f64(a);
        let y = as_f64(b);
        let r = match op {
            '+' => x + y,
            '-' => x - y,
            '*' => x * y,
            '/' => x / y,
            '%' => x % y,
            _ => return Err(Error::exception(format!("bad operator {op}"))),
        };
        return Ok(Object::Float(r));
    }
    let x = to_number(a);
    let y = to_number(b);
    let r = match op {
        '+' => x + y,
        '-' => x - y,
        '*' => x * y,
        '/' => {
            if y == 0 {
                return Err(Error::exception("divide by zero"));
            }
            x / y
        }
        '%' => {
            if y == 0 {
                return Err(Error::exception("divide by zero"));
            }
            x % y
        }
        _ => return Err(Error::exception(format!("bad operator {op}"))),
    };
    Ok(Object::Integer(r))
}

fn as_f64(obj: &Object) -> f64 {
    match obj {
        Object::Float(f) => *f,
        other => to_number(other) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_number_takes_leading_int() {
        assert_eq!(to_number(&Object::str("42abc")), 42);
        assert_eq!(to_number(&Object::str("abc")), 0);
        assert_eq!(to_number(&Object::str("-7x")), -7);
    }

    #[test]
    fn truthiness_is_numeric() {
        assert!(!is_truthy(&Object::Integer(0)));
        assert!(is_truthy(&Object::Integer(1)));
        assert!(!is_truthy(&Object::str("nope"))); // coerces to 0
        assert!(is_truthy(&Object::str("3 apples")));
    }

    #[test]
    fn arithmetic_int_and_float() {
        assert_eq!(arith('+', &Object::Integer(2), &Object::Integer(3)).unwrap(), Object::Integer(5));
        assert_eq!(arith('*', &Object::Float(2.0), &Object::Integer(3)).unwrap(), Object::Float(6.0));
    }
}
