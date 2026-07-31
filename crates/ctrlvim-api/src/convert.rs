//! Object ↔ native conversion traits used by the generated API dispatch shims.
//!
//! In C this logic is spread through `converter.c` (Lua side) and the generated
//! `handle_*`/`nlua_pop_*` code. Here the `#[ctrlvim_api]` macro calls
//! [`FromObject::from_object`] per argument and [`IntoObject::into_object`] on
//! the return value; these impls are the single place native types learn how to
//! cross the API boundary.

use ctrlvim_types::{BufferId, Error, Object, Result, WindowId};

/// Convert an incoming [`Object`] argument into a native type, producing a
/// validation error (with the argument label) on mismatch.
pub trait FromObject: Sized {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self>;
}

/// Convert a native return value into an [`Object`].
pub trait IntoObject {
    fn into_object(self) -> Object;
}

fn type_err(ctx: &str, expected: &str, got: &Object) -> Error {
    Error::validation(format!(
        "{}: expected {}, got {}",
        ctx,
        expected,
        got.type_name()
    ))
}

impl FromObject for Object {
    fn from_object(obj: &Object, _ctx: &str) -> Result<Self> {
        Ok(obj.clone())
    }
}

impl FromObject for i64 {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::Integer(i) => Ok(*i),
            other => Err(type_err(ctx, "integer", other)),
        }
    }
}

impl FromObject for bool {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::Boolean(b) => Ok(*b),
            other => Err(type_err(ctx, "boolean", other)),
        }
    }
}

impl FromObject for String {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::String(bytes) => String::from_utf8(bytes.clone())
                .map_err(|_| Error::validation(format!("{}: string is not valid UTF-8", ctx))),
            other => Err(type_err(ctx, "string", other)),
        }
    }
}

impl FromObject for Vec<String> {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::Array(items) => items
                .iter()
                .map(|o| String::from_object(o, ctx))
                .collect(),
            other => Err(type_err(ctx, "array of strings", other)),
        }
    }
}

impl FromObject for BufferId {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::Buffer(b) => Ok(*b),
            // Real Neovim also accepts integer 0 to mean "current buffer" —
            // safe there because a real buffer handle is never 0 (the first
            // buffer is 1). ctrlvim's ids are 0-based, so 0 is a legitimate
            // buffer, not a free sentinel; callers that want "the current
            // buffer" must fetch it explicitly via `nvim_get_current_buf()`,
            // same as any other handle. A plain integer is still accepted
            // here (matching how a handle round-trips through Lua as a
            // number) — it just isn't given special-cased meaning at 0.
            Object::Integer(i) if *i >= 0 => Ok(BufferId(*i as u32)),
            other => Err(type_err(ctx, "buffer", other)),
        }
    }
}

impl FromObject for WindowId {
    fn from_object(obj: &Object, ctx: &str) -> Result<Self> {
        match obj {
            Object::Window(w) => Ok(*w),
            // See the `BufferId` impl above: no special meaning for 0 here.
            Object::Integer(i) if *i >= 0 => Ok(WindowId(*i as u32)),
            other => Err(type_err(ctx, "window", other)),
        }
    }
}

impl IntoObject for Object {
    fn into_object(self) -> Object {
        self
    }
}
impl IntoObject for () {
    fn into_object(self) -> Object {
        Object::Nil
    }
}
impl IntoObject for i64 {
    fn into_object(self) -> Object {
        Object::Integer(self)
    }
}
impl IntoObject for bool {
    fn into_object(self) -> Object {
        Object::Boolean(self)
    }
}
impl IntoObject for String {
    fn into_object(self) -> Object {
        Object::String(self.into_bytes())
    }
}
impl IntoObject for Vec<String> {
    fn into_object(self) -> Object {
        Object::Array(self.into_iter().map(|s| Object::String(s.into_bytes())).collect())
    }
}
impl IntoObject for BufferId {
    fn into_object(self) -> Object {
        Object::Buffer(self)
    }
}
impl IntoObject for WindowId {
    fn into_object(self) -> Object {
        Object::Window(self)
    }
}
