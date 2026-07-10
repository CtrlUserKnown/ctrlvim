//! Concrete API functions — the Rust equivalents of `src/nvim/api/*.c`.
//!
//! Each is an ordinary function annotated with `#[nvim_api]`; the macro
//! generates the Lua/RPC dispatch shim and registers it. This is a
//! representative slice (the demo's surface), not the full ~370-function API.

use crate::autocmd::CallbackRef;
use crate::ApiContext;
use nvim_api_macro::nvim_api;
use nvim_types::object::LuaRef;
use nvim_types::{Error, Object, Position, Result};

/// `nvim_get_current_line` — the text of the cursor line, without newline.
#[nvim_api(since = 1, fast)]
fn nvim_get_current_line(cx: &mut ApiContext) -> Result<Object> {
    let line = cx.editor().cursor().line;
    let text = cx.editor().cur_buffer().text.line(line).unwrap_or_default();
    Ok(Object::str(text))
}

/// `nvim_set_current_line` — replace the cursor line.
#[nvim_api(since = 1)]
fn nvim_set_current_line(cx: &mut ApiContext, line: String) -> Result<Object> {
    let lnum = cx.editor().cursor().line;
    cx.editor_mut().cur_buffer_mut().text.replace_line(lnum, &line);
    cx.editor_mut().cur_buffer_mut().changedtick += 1;
    Ok(Object::Nil)
}

/// `nvim_buf_line_count` — number of lines in the current buffer. (A full
/// implementation takes a buffer handle; the demo uses the current buffer.)
#[nvim_api(since = 1, fast)]
fn nvim_buf_line_count(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Integer(cx.editor().cur_buffer().text.line_count() as i64))
}

/// `nvim_get_current_buf` — the current buffer handle.
#[nvim_api(since = 1, fast)]
fn nvim_get_current_buf(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Buffer(cx.editor().current_buffer_id()))
}

/// `nvim_win_get_cursor` — `[1-based row, 0-based col]`.
#[nvim_api(since = 1, fast)]
fn nvim_win_get_cursor(cx: &mut ApiContext) -> Result<Object> {
    let (row, col) = cx.editor().cursor().to_cursor_api();
    Ok(Object::Array(vec![Object::Integer(row), Object::Integer(col)]))
}

/// `nvim_win_set_cursor` — set cursor from `[row, col]`.
#[nvim_api(since = 1)]
fn nvim_win_set_cursor(cx: &mut ApiContext, pos: Object) -> Result<Object> {
    let arr = match &pos {
        Object::Array(a) if a.len() == 2 => a,
        _ => return Err(Error::validation("nvim_win_set_cursor: expected [row, col]")),
    };
    let row = arr[0]
        .as_int()
        .ok_or_else(|| Error::validation("cursor row must be an integer"))?;
    let col = arr[1]
        .as_int()
        .ok_or_else(|| Error::validation("cursor col must be an integer"))?;
    cx.editor_mut().set_cursor(Position::from_cursor_api(row, col));
    Ok(Object::Nil)
}

/// `nvim_create_autocmd` — register an autocmd. `opts` is a dict with optional
/// `pattern` (string), `once` (bool), and either `callback` (a function, arriving
/// as an `Object::LuaRef`) or `command` (string).
#[nvim_api(since = 1)]
fn nvim_create_autocmd(cx: &mut ApiContext, event: String, opts: Object) -> Result<Object> {
    let dict = match &opts {
        Object::Dict(d) => d,
        Object::Nil => {
            return Err(Error::validation(
                "nvim_create_autocmd: opts must include a callback or command",
            ))
        }
        other => {
            return Err(Error::validation(format!(
                "nvim_create_autocmd: opts must be a dict, got {}",
                other.type_name()
            )))
        }
    };

    let pattern = dict
        .get("pattern")
        .and_then(|o| o.as_str())
        .unwrap_or("*")
        .to_string();
    let once = dict.get("once").and_then(|o| o.as_bool()).unwrap_or(false);

    let callback = if let Some(Object::LuaRef(LuaRef(id))) = dict.get("callback") {
        CallbackRef::Lua(LuaRef(*id))
    } else if let Some(cmd) = dict.get("command").and_then(|o| o.as_str()) {
        CallbackRef::Command(cmd.to_string())
    } else {
        return Err(Error::validation(
            "nvim_create_autocmd: opts requires `callback` or `command`",
        ));
    };

    let id = cx.autocmds.create(event, pattern, callback, once);
    Ok(Object::Integer(id as i64))
}

/// `nvim_del_autocmd` — remove an autocmd by id.
#[nvim_api(since = 1)]
fn nvim_del_autocmd(cx: &mut ApiContext, id: i64) -> Result<Object> {
    if cx.autocmds.delete(id as u32) {
        Ok(Object::Nil)
    } else {
        Err(Error::exception(format!("no autocmd with id {}", id)))
    }
}

/// `nvim_create_namespace` — allocate (or reuse) a named namespace id.
#[nvim_api(since = 1, fast)]
fn nvim_create_namespace(cx: &mut ApiContext, name: String) -> Result<Object> {
    let id = cx.create_namespace(&name);
    Ok(Object::Integer(id as i64))
}

/// `nvim_get_current_win` — the focused window handle.
#[nvim_api(since = 1, fast)]
fn nvim_get_current_win(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Window(cx.editor().current_window_id()))
}

/// `nvim_list_wins` — all window handles in layout order.
#[nvim_api(since = 1, fast)]
fn nvim_list_wins(cx: &mut ApiContext) -> Result<Object> {
    let wins = cx
        .editor()
        .window_ids()
        .into_iter()
        .map(Object::Window)
        .collect();
    Ok(Object::Array(wins))
}

/// `nvim_win_get_buf` — the buffer displayed in the current window. (A full
/// implementation takes a window handle argument.)
#[nvim_api(since = 1, fast)]
fn nvim_win_get_buf(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Buffer(cx.editor().current_buffer_id()))
}

/// `nvim_open_win`-style split: open a new window viewing the current buffer.
/// `vertical` chooses a side-by-side vs stacked split. Returns the new window.
#[nvim_api(since = 1)]
fn nvim_split_window(cx: &mut ApiContext, vertical: bool) -> Result<Object> {
    let id = cx.editor_mut().split_current(vertical);
    Ok(Object::Window(id))
}
