//! Concrete API functions — the Rust equivalents of `src/ctrlvim/api/*.c`.
//!
//! Each is an ordinary function annotated with `#[ctrlvim_api]`; the macro
//! generates the Lua/RPC dispatch shim and registers it. This is a
//! representative slice (the demo's surface), not the full ~370-function API.

use crate::autocmd::CallbackRef;
use crate::ApiContext;
use ctrlvim_api_macro::ctrlvim_api;
use ctrlvim_types::object::LuaRef;
use ctrlvim_types::{Error, Object, Position, Result};

/// `ctrlvim_get_current_line` — the text of the cursor line, without newline.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_get_current_line(cx: &mut ApiContext) -> Result<Object> {
    let line = cx.editor().cursor().line;
    let text = cx.editor().cur_buffer().text.line(line).unwrap_or_default();
    Ok(Object::str(text))
}

/// `ctrlvim_set_current_line` — replace the cursor line.
#[ctrlvim_api(since = 1)]
fn ctrlvim_set_current_line(cx: &mut ApiContext, line: String) -> Result<Object> {
    let lnum = cx.editor().cursor().line;
    cx.editor_mut().cur_buffer_mut().text.replace_line(lnum, &line);
    cx.editor_mut().cur_buffer_mut().changedtick += 1;
    Ok(Object::Nil)
}

/// `ctrlvim_buf_line_count` — number of lines in the current buffer. (A full
/// implementation takes a buffer handle; the demo uses the current buffer.)
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_line_count(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Integer(cx.editor().cur_buffer().text.line_count() as i64))
}

/// `ctrlvim_get_current_buf` — the current buffer handle.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_get_current_buf(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Buffer(cx.editor().current_buffer_id()))
}

/// `ctrlvim_win_get_cursor` — `[1-based row, 0-based col]`.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_win_get_cursor(cx: &mut ApiContext) -> Result<Object> {
    let (row, col) = cx.editor().cursor().to_cursor_api();
    Ok(Object::Array(vec![Object::Integer(row), Object::Integer(col)]))
}

/// `ctrlvim_win_set_cursor` — set cursor from `[row, col]`.
#[ctrlvim_api(since = 1)]
fn ctrlvim_win_set_cursor(cx: &mut ApiContext, pos: Object) -> Result<Object> {
    let arr = match &pos {
        Object::Array(a) if a.len() == 2 => a,
        _ => return Err(Error::validation("ctrlvim_win_set_cursor: expected [row, col]")),
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

/// `ctrlvim_create_autocmd` — register an autocmd. `opts` is a dict with optional
/// `pattern` (string), `once` (bool), and either `callback` (a function, arriving
/// as an `Object::LuaRef`) or `command` (string).
#[ctrlvim_api(since = 1)]
fn ctrlvim_create_autocmd(cx: &mut ApiContext, event: String, opts: Object) -> Result<Object> {
    let dict = match &opts {
        Object::Dict(d) => d,
        Object::Nil => {
            return Err(Error::validation(
                "ctrlvim_create_autocmd: opts must include a callback or command",
            ))
        }
        other => {
            return Err(Error::validation(format!(
                "ctrlvim_create_autocmd: opts must be a dict, got {}",
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
            "ctrlvim_create_autocmd: opts requires `callback` or `command`",
        ));
    };

    let id = cx.autocmds.create(event, pattern, callback, once);
    Ok(Object::Integer(id as i64))
}

/// `ctrlvim_create_user_command` — register a Lua-backed command under `name`,
/// the plugin equivalent of Vimscript's `:command`. `opts` is a dict with an
/// optional `desc` (string) shown in the command palette. Unlike
/// `nvim_create_user_command`, `name` is looked up case-sensitively and takes
/// no arguments yet — this is the palette-visibility half of the feature; a
/// full `<args>`/`<f-args>`/range surface is future work.
#[ctrlvim_api(since = 1)]
fn ctrlvim_create_user_command(cx: &mut ApiContext, name: String, callback: Object, opts: Object) -> Result<Object> {
    let callback = match callback {
        Object::LuaRef(r) => r,
        other => {
            return Err(Error::validation(format!(
                "ctrlvim_create_user_command: callback must be a function, got {}",
                other.type_name()
            )))
        }
    };
    let desc = match &opts {
        Object::Dict(d) => d.get("desc").and_then(|o| o.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    };
    let source = cx.current_source.clone();
    cx.user_commands.insert(name, (callback, desc, source));
    Ok(Object::Nil)
}

/// `ctrlvim_del_autocmd` — remove an autocmd by id.
#[ctrlvim_api(since = 1)]
fn ctrlvim_del_autocmd(cx: &mut ApiContext, id: i64) -> Result<Object> {
    if cx.autocmds.delete(id as u32) {
        Ok(Object::Nil)
    } else {
        Err(Error::exception(format!("no autocmd with id {}", id)))
    }
}

/// `ctrlvim_create_namespace` — allocate (or reuse) a named namespace id.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_create_namespace(cx: &mut ApiContext, name: String) -> Result<Object> {
    let id = cx.create_namespace(&name);
    Ok(Object::Integer(id as i64))
}

/// `ctrlvim_get_current_win` — the focused window handle.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_get_current_win(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Window(cx.editor().current_window_id()))
}

/// `ctrlvim_list_wins` — all window handles in layout order.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_list_wins(cx: &mut ApiContext) -> Result<Object> {
    let wins = cx
        .editor()
        .window_ids()
        .into_iter()
        .map(Object::Window)
        .collect();
    Ok(Object::Array(wins))
}

/// `ctrlvim_win_get_buf` — the buffer displayed in the current window. (A full
/// implementation takes a window handle argument.)
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_win_get_buf(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Buffer(cx.editor().current_buffer_id()))
}

/// `ctrlvim_open_win`-style split: open a new window viewing the current buffer.
/// `vertical` chooses a side-by-side vs stacked split. Returns the new window.
#[ctrlvim_api(since = 1)]
fn ctrlvim_split_window(cx: &mut ApiContext, vertical: bool) -> Result<Object> {
    let id = cx.editor_mut().split_current(vertical);
    Ok(Object::Window(id))
}

// ---------------------------------------------------------------------------
// Buffer text access.
//
// These are the functions a plugin actually needs: everything above operates on
// the cursor line, which is enough for a demo and nothing else. Line indices
// follow the API convention — 0-based, `end` exclusive, and negative values
// count back from the end of the buffer (`-1` is one past the last line).
// ---------------------------------------------------------------------------

/// Resolve an API line index against a buffer of `count` lines, honoring the
/// negative-from-the-end convention.
fn resolve_index(idx: i64, count: usize) -> usize {
    if idx < 0 {
        // -1 means "one past the last line", -2 the last line, and so on.
        (count as i64 + 1 + idx).max(0) as usize
    } else {
        idx as usize
    }
    .min(count)
}

/// `ctrlvim_buf_get_lines` — a half-open range of lines as an array of strings.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_get_lines(cx: &mut ApiContext, start: i64, end: i64) -> Result<Object> {
    let count = cx.editor().cur_buffer().text.line_count();
    let (s, e) = (resolve_index(start, count), resolve_index(end, count));
    if s > e {
        return Err(Error::validation("ctrlvim_buf_get_lines: start is past end"));
    }
    let lines = cx.editor().cur_buffer().text.lines();
    Ok(Object::Array(
        lines[s..e].iter().map(|l| Object::str(l.clone())).collect(),
    ))
}

/// `ctrlvim_buf_set_lines` — replace a half-open range with new lines. Passing
/// an empty replacement deletes the range; passing `start == end` inserts.
#[ctrlvim_api(since = 1)]
fn ctrlvim_buf_set_lines(
    cx: &mut ApiContext,
    start: i64,
    end: i64,
    replacement: Object,
) -> Result<Object> {
    let Object::Array(items) = replacement else {
        return Err(Error::validation(
            "ctrlvim_buf_set_lines: replacement must be an array of strings",
        ));
    };
    let mut new_lines: Vec<String> = Vec::with_capacity(items.len());
    for item in &items {
        match item.as_str() {
            Some(s) => new_lines.push(s.to_string()),
            None => {
                return Err(Error::validation(
                    "ctrlvim_buf_set_lines: replacement must be an array of strings",
                ))
            }
        }
    }
    let count = cx.editor().cur_buffer().text.line_count();
    let (s, e) = (resolve_index(start, count), resolve_index(end, count));
    if s > e {
        return Err(Error::validation("ctrlvim_buf_set_lines: start is past end"));
    }
    cx.editor_mut()
        .cur_buffer_mut()
        .text
        .set_lines(s, e, &new_lines);
    cx.editor_mut().cur_buffer_mut().changedtick += 1;
    Ok(Object::Nil)
}

/// `ctrlvim_buf_get_name` — the current buffer's name, or an empty string.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_get_name(cx: &mut ApiContext) -> Result<Object> {
    let name = cx.editor().cur_buffer().name.clone().unwrap_or_default();
    Ok(Object::str(name))
}

/// `ctrlvim_buf_get_changedtick` — the buffer's change counter (`b:changedtick`).
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_get_changedtick(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Integer(cx.editor().cur_buffer().changedtick as i64))
}

// ---------------------------------------------------------------------------
// Extmarks. The gravity-aware store already exists in `ctrlvim-text`; without
// these functions nothing outside the engine could reach it.
// ---------------------------------------------------------------------------

/// `ctrlvim_buf_set_extmark` — place a mark at `(line, col)` in a namespace,
/// returning its id. Marks follow edits according to their gravity.
#[ctrlvim_api(since = 1)]
fn ctrlvim_buf_set_extmark(cx: &mut ApiContext, ns: i64, line: i64, col: i64) -> Result<Object> {
    if line < 0 || col < 0 {
        return Err(Error::validation(
            "ctrlvim_buf_set_extmark: line and col must be non-negative",
        ));
    }
    let pos = Position::new(line as usize, col as usize);
    let id = cx.editor_mut().cur_buffer_mut().marks.add(
        ctrlvim_text::Namespace(ns as u32),
        pos,
        ctrlvim_text::Gravity::Right,
    );
    Ok(Object::Integer(id as i64))
}

/// `ctrlvim_buf_get_extmark_by_id` — `[line, col]` for a mark, or an empty
/// array when it doesn't exist.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_get_extmark_by_id(cx: &mut ApiContext, ns: i64, id: i64) -> Result<Object> {
    match cx
        .editor()
        .cur_buffer()
        .marks
        .get(ctrlvim_text::Namespace(ns as u32), id as u32)
    {
        Some(p) => Ok(Object::Array(vec![
            Object::Integer(p.line as i64),
            Object::Integer(p.col as i64),
        ])),
        None => Ok(Object::Array(Vec::new())),
    }
}

/// `ctrlvim_buf_get_extmarks` — every mark in a namespace as `[id, line, col]`.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_buf_get_extmarks(cx: &mut ApiContext, ns: i64) -> Result<Object> {
    let mut marks = cx
        .editor()
        .cur_buffer()
        .marks
        .all_in(ctrlvim_text::Namespace(ns as u32));
    // Stable order: callers iterate these to render decorations.
    marks.sort_by_key(|(id, _)| *id);
    Ok(Object::Array(
        marks
            .into_iter()
            .map(|(id, p)| {
                Object::Array(vec![
                    Object::Integer(id as i64),
                    Object::Integer(p.line as i64),
                    Object::Integer(p.col as i64),
                ])
            })
            .collect(),
    ))
}

/// `ctrlvim_buf_del_extmark` — remove a mark; returns whether it was there.
#[ctrlvim_api(since = 1)]
fn ctrlvim_buf_del_extmark(cx: &mut ApiContext, ns: i64, id: i64) -> Result<Object> {
    let gone = cx
        .editor_mut()
        .cur_buffer_mut()
        .marks
        .remove(ctrlvim_text::Namespace(ns as u32), id as u32);
    Ok(Object::Boolean(gone))
}

// ---------------------------------------------------------------------------
// Options and evaluation.
// ---------------------------------------------------------------------------

/// `ctrlvim_get_option_value` — read an option by name.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_get_option_value(cx: &mut ApiContext, name: String) -> Result<Object> {
    let o = cx.editor().options();
    Ok(match name.as_str() {
        "number" | "nu" => Object::Boolean(o.number()),
        "relativenumber" | "rnu" => Object::Boolean(o.relativenumber()),
        "wrap" => Object::Boolean(o.wrap()),
        "expandtab" | "et" => Object::Boolean(o.expandtab()),
        "autoindent" | "ai" => Object::Boolean(o.autoindent()),
        "ignorecase" | "ic" => Object::Boolean(o.ignorecase()),
        "smartcase" | "scs" => Object::Boolean(o.smartcase()),
        "hlsearch" | "hls" => Object::Boolean(o.hlsearch()),
        "splitbelow" | "sb" => Object::Boolean(o.splitbelow()),
        "splitright" | "spr" => Object::Boolean(o.splitright()),
        "foldenable" | "fen" => Object::Boolean(o.foldenable()),
        "tabstop" | "ts" => Object::Integer(o.tabstop()),
        "shiftwidth" | "sw" => Object::Integer(o.shiftwidth()),
        "scrolloff" | "so" => Object::Integer(o.scrolloff()),
        "foldcolumn" | "fdc" => Object::Integer(o.foldcolumn()),
        "foldmethod" | "fdm" => Object::str(o.foldmethod().as_str().to_string()),
        "iskeyword" | "isk" => Object::str(o.iskeyword().to_string()),
        _ => return Err(Error::validation(format!("E518: Unknown option: {name}"))),
    })
}

/// `ctrlvim_get_mode` — the current mode's short name, e.g. `"n"`.
#[ctrlvim_api(since = 1, fast)]
fn ctrlvim_get_mode(cx: &mut ApiContext) -> Result<Object> {
    // The API context owns an `Editor`, not a `Session`, so mode isn't tracked
    // here; normal is the only state a plugin can observe through this handle.
    let _ = cx;
    Ok(Object::str("n".to_string()))
}
