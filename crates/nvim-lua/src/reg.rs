//! `LuaRef` storage — the mechanism that lets Rust hold and later re-invoke Lua
//! callbacks (`executor.c`'s `nlua_ref`/`nlua_call_ref`).
//!
//! Neovim's `LuaRef` is an `int` from `luaL_ref(LUA_REGISTRYINDEX)`. mlua's
//! equivalent is [`mlua::RegistryKey`]. We wrap a map from our stable integer
//! ids (the value carried inside [`Object::LuaRef`]) to registry keys, so
//! callbacks survive being stored in the autocmd/keymap/timer tables and called
//! back later — the single primitive every callback subsystem reuses.

use mlua::{Function, Lua, RegistryKey};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// A cheaply-clonable handle to the shared registry-key map. Cloning shares the
/// same underlying store (it is `Rc`-backed), so the copy handed to each Lua
/// closure refers to the same callback table.
#[derive(Clone, Default)]
pub struct LuaRefStore {
    inner: Rc<RefCell<HashMap<i64, RegistryKey>>>,
    next_id: Rc<Cell<i64>>,
}

impl LuaRefStore {
    pub fn new() -> Self {
        LuaRefStore {
            inner: Rc::new(RefCell::new(HashMap::new())),
            next_id: Rc::new(Cell::new(1)),
        }
    }

    /// Store a Lua function, returning a stable id (the `LuaRef`).
    pub fn store<'lua>(&self, lua: &'lua Lua, func: Function<'lua>) -> mlua::Result<i64> {
        let key = lua.create_registry_value(func)?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.inner.borrow_mut().insert(id, key);
        Ok(id)
    }

    /// Resolve an id back to its function, if still held.
    pub fn get<'lua>(&self, lua: &'lua Lua, id: i64) -> mlua::Result<Option<Function<'lua>>> {
        let map = self.inner.borrow();
        match map.get(&id) {
            Some(key) => Ok(Some(lua.registry_value::<Function>(key)?)),
            None => Ok(None),
        }
    }

    /// Release a stored callback (`nlua_unref`).
    pub fn remove(&self, lua: &Lua, id: i64) -> mlua::Result<()> {
        if let Some(key) = self.inner.borrow_mut().remove(&id) {
            lua.remove_registry_value(key)?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }
}
