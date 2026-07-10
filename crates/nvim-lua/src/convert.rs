//! Object ↔ Lua value conversion — the replacement for `converter.c`.
//!
//! This is the load-bearing boundary: every value crossing between Rust and Lua
//! goes through here, and plugin compatibility depends on the semantics matching
//! Neovim's. Key behaviors preserved:
//!
//! * Lua functions become [`Object::LuaRef`] by storing the function in the Lua
//!   registry (Neovim's `LuaRef`), so Rust can hold and re-invoke callbacks.
//! * Tables are classified as array vs dict the way Neovim does: a table whose
//!   keys are exactly `1..=n` is an Array, otherwise a string-keyed Dict.
//! * `vim.NIL` and Lua `nil` both map to [`Object::Nil`].

use crate::reg::LuaRefStore;
use mlua::{Lua, MultiValue, Value};
use nvim_types::object::LuaRef;
use nvim_types::Object;
use std::collections::BTreeMap;

/// Convert a Lua value into an [`Object`]. Functions are captured into the
/// registry via `store`.
pub fn to_object<'lua>(lua: &'lua Lua, value: &Value<'lua>, store: &LuaRefStore) -> mlua::Result<Object> {
    Ok(match value {
        Value::Nil => Object::Nil,
        Value::Boolean(b) => Object::Boolean(*b),
        Value::Integer(i) => Object::Integer(*i),
        Value::Number(n) => Object::Float(*n),
        Value::String(s) => Object::String(s.as_bytes().to_vec()),
        Value::Table(t) => {
            // Array iff keys are exactly 1..=len with no gaps and no non-int keys.
            let len = t.raw_len();
            let mut is_array = len > 0;
            let mut pair_count = 0usize;
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair?;
                pair_count += 1;
                if !matches!(k, Value::Integer(_)) {
                    is_array = false;
                }
            }
            if is_array && pair_count == len {
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    let v: Value = t.raw_get(i)?;
                    arr.push(to_object(lua, &v, store)?);
                }
                Object::Array(arr)
            } else if pair_count == 0 {
                // Empty table: Neovim treats it as an empty dict.
                Object::Dict(BTreeMap::new())
            } else {
                let mut map = BTreeMap::new();
                for pair in t.clone().pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    let key = match k {
                        Value::String(s) => s.to_str()?.to_string(),
                        Value::Integer(i) => i.to_string(),
                        _ => continue,
                    };
                    map.insert(key, to_object(lua, &v, store)?);
                }
                Object::Dict(map)
            }
        }
        Value::Function(f) => {
            let id = store.store(lua, f.clone())?;
            Object::LuaRef(LuaRef(id))
        }
        // Userdata/thread/lightuserdata are not representable as Object; Neovim
        // errors here. We map them to Nil for the demo surface.
        _ => Object::Nil,
    })
}

/// Convert a slice of Lua multi-values (call arguments) into `Object`s.
pub fn args_to_objects<'lua>(
    lua: &'lua Lua,
    args: &MultiValue<'lua>,
    store: &LuaRefStore,
) -> mlua::Result<Vec<Object>> {
    args.iter().map(|v| to_object(lua, v, store)).collect()
}

/// Convert an [`Object`] back into a Lua value.
pub fn to_lua<'lua>(lua: &'lua Lua, obj: &Object, store: &LuaRefStore) -> mlua::Result<Value<'lua>> {
    Ok(match obj {
        Object::Nil => Value::Nil,
        Object::Boolean(b) => Value::Boolean(*b),
        Object::Integer(i) => Value::Integer(*i),
        Object::Float(f) => Value::Number(*f),
        Object::String(bytes) => Value::String(lua.create_string(bytes)?),
        Object::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, item, store)?)?;
            }
            Value::Table(t)
        }
        Object::Dict(map) => {
            let t = lua.create_table()?;
            for (k, v) in map {
                t.raw_set(k.as_str(), to_lua(lua, v, store)?)?;
            }
            Value::Table(t)
        }
        // Handle types surface to Lua as their integer ids (Neovim does the same
        // for buffer/window/tabpage handles).
        Object::Buffer(b) => Value::Integer(b.0 as i64),
        Object::Window(w) => Value::Integer(w.0 as i64),
        Object::Tabpage(t) => Value::Integer(t.0 as i64),
        Object::LuaRef(LuaRef(id)) => {
            // Resolve back to the stored function if we still hold it.
            match store.get(lua, *id)? {
                Some(f) => Value::Function(f),
                None => Value::Nil,
            }
        }
    })
}
