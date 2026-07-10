//! The dynamic [`Object`] value type — Neovim's `Object`/`typval` at the
//! Lua/RPC boundary.
//!
//! Every value crossing the C↔Lua boundary in Neovim funnels through
//! `converter.c`; the fidelity of that conversion is what keeps plugins working.
//! [`Object`] is the Rust-side representation both the Lua converter
//! (`nvim-lua`) and the msgpack-rpc codec (`nvim-async`) marshal to and from.

use crate::handle::{BufferId, TabpageId, WindowId};
use std::collections::BTreeMap;

/// An opaque reference to a Lua value held in the registry. In Neovim this is
/// `LuaRef` (an `int` index into `LUA_REGISTRYINDEX`); on the Rust side the
/// concrete registry key lives in `nvim-lua`, so here it is just an id the
/// `Object` can carry across a boundary that must not depend on `mlua`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LuaRef(pub i64);

/// Neovim's dynamic object type.
///
/// Note `Float` compares via bit pattern so `Object` can be `Eq`/`Hash`; this
/// matches the need to treat `NaN` payloads deterministically when round-
/// tripping through the converter (see the NaN handling in `converter.c`).
#[derive(Debug, Clone)]
pub enum Object {
    /// `nil` / `vim.NIL`.
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    /// A byte string. Neovim strings are byte-oriented (may contain NUL), so we
    /// use `Vec<u8>` rather than `String`.
    String(Vec<u8>),
    Array(Vec<Object>),
    /// A string-keyed map. Neovim dict keys are always strings.
    Dict(BTreeMap<String, Object>),
    Buffer(BufferId),
    Window(WindowId),
    Tabpage(TabpageId),
    LuaRef(LuaRef),
}

impl Object {
    /// Convenience constructor for a UTF-8 string object.
    pub fn str(s: impl Into<String>) -> Self {
        Object::String(s.into().into_bytes())
    }

    /// Interpret a string object as UTF-8, if it is a valid one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Object::String(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Neovim's notion of truthiness for callback returns (`LUARET_TRUTHY`):
    /// only `true` (and, per Neovim, a non-nil non-false value) is truthy. Here
    /// we treat `nil`/`false` as falsey and everything else as truthy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Object::Nil | Object::Boolean(false))
    }

    /// Short type name, used in validation error messages that must match the
    /// phrasing plugins expect (e.g. "Expected Lua string").
    pub fn type_name(&self) -> &'static str {
        match self {
            Object::Nil => "nil",
            Object::Boolean(_) => "boolean",
            Object::Integer(_) => "integer",
            Object::Float(_) => "float",
            Object::String(_) => "string",
            Object::Array(_) => "array",
            Object::Dict(_) => "dict",
            Object::Buffer(_) => "buffer",
            Object::Window(_) => "window",
            Object::Tabpage(_) => "tabpage",
            Object::LuaRef(_) => "function",
        }
    }
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        use Object::*;
        match (self, other) {
            (Nil, Nil) => true,
            (Boolean(a), Boolean(b)) => a == b,
            (Integer(a), Integer(b)) => a == b,
            // Bit-pattern equality so NaN payloads round-trip deterministically.
            (Float(a), Float(b)) => a.to_bits() == b.to_bits(),
            (String(a), String(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            (Dict(a), Dict(b)) => a == b,
            (Buffer(a), Buffer(b)) => a == b,
            (Window(a), Window(b)) => a == b,
            (Tabpage(a), Tabpage(b)) => a == b,
            (LuaRef(a), LuaRef(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Object {}

impl From<bool> for Object {
    fn from(v: bool) -> Self {
        Object::Boolean(v)
    }
}
impl From<i64> for Object {
    fn from(v: i64) -> Self {
        Object::Integer(v)
    }
}
impl From<f64> for Object {
    fn from(v: f64) -> Self {
        Object::Float(v)
    }
}
impl From<&str> for Object {
    fn from(v: &str) -> Self {
        Object::str(v)
    }
}
impl From<String> for Object {
    fn from(v: String) -> Self {
        Object::String(v.into_bytes())
    }
}
impl<T: Into<Object>> From<Vec<T>> for Object {
    fn from(v: Vec<T>) -> Self {
        Object::Array(v.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_neovim() {
        assert!(!Object::Nil.is_truthy());
        assert!(!Object::Boolean(false).is_truthy());
        assert!(Object::Boolean(true).is_truthy());
        assert!(Object::Integer(0).is_truthy()); // 0 is truthy in Lua
        assert!(Object::str("").is_truthy());
    }

    #[test]
    fn nan_bit_pattern_equality() {
        let a = Object::Float(f64::NAN);
        let b = Object::Float(f64::NAN);
        assert_eq!(a, b);
    }

    #[test]
    fn conversions() {
        assert_eq!(Object::from(true), Object::Boolean(true));
        assert_eq!(Object::from(7i64), Object::Integer(7));
        assert_eq!(Object::from("hi").as_str(), Some("hi"));
    }
}
