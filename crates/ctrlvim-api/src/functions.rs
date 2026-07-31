//! Concrete API functions — the Rust equivalents of `src/ctrlvim/api/*.c`.
//!
//! Each is an ordinary function annotated with `#[ctrlvim_api]`; the macro
//! generates the Lua/RPC dispatch shim and registers it. This is a
//! representative slice (the demo's surface), not the full ~370-function API.
//!
//! A handful of real Neovim API names use a double underscore
//! (`nvim__redraw`, `nvim__get_runtime`, ...) to mark them private/internal —
//! matching that exact spelling is what lets vendored runtime Lua
//! (`runtime/lua/vim/`) call them unmodified, so `non_snake_case` is off for
//! this file rather than fighting the naming convention we don't control.
#![allow(non_snake_case)]

use crate::autocmd::CallbackRef;
use crate::ApiContext;
use ctrlvim_api_macro::ctrlvim_api;
use ctrlvim_editor::{ExtmarkMeta, FloatConfig, FloatRelative};
use ctrlvim_text::{Gravity, Namespace};
use ctrlvim_types::object::LuaRef;
use ctrlvim_types::{BufferId, Error, Object, Position, Result, WindowId};

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
        "cursorline" | "cul" => Object::Boolean(o.cursorline()),
        "tabstop" | "ts" => Object::Integer(o.tabstop()),
        "shiftwidth" | "sw" => Object::Integer(o.shiftwidth()),
        "scrolloff" | "so" => Object::Integer(o.scrolloff()),
        "foldcolumn" | "fdc" => Object::Integer(o.foldcolumn()),
        "foldmethod" | "fdm" => Object::str(o.foldmethod().as_str().to_string()),
        "iskeyword" | "isk" => Object::str(o.iskeyword().to_string()),
        "guicursor" | "gcr" => Object::str(o.guicursor().to_string()),
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

// ---------------------------------------------------------------------------
// Real `nvim_*` names — buffer/window-handle-explicit variants of the
// functions above, plus what's genuinely new: floating windows, decorated
// extmarks, and buffer-change watching. Additive: the `ctrlvim_*` functions
// above are untouched, so nothing that already calls them breaks.
//
// Handle `0`: real Neovim treats a bare integer `0` as "the current buffer/
// window" because a real handle is never 0 there (handles start at 1).
// ctrlvim's ids are 0-based, so 0 is a legitimate handle here, not a free
// sentinel — a caller that wants "current" fetches it explicitly via
// `nvim_get_current_buf`/`nvim_get_current_win`, same as any other handle.
// ---------------------------------------------------------------------------

/// `nvim_get_current_buf`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_get_current_buf(cx: &mut ApiContext) -> Result<Object> {
    ctrlvim_get_current_buf(cx)
}

/// `nvim_get_current_win`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_get_current_win(cx: &mut ApiContext) -> Result<Object> {
    ctrlvim_get_current_win(cx)
}

/// `nvim_list_wins` — includes any open floats, since `Editor::window_ids`
/// does.
#[ctrlvim_api(since = 1, fast)]
fn nvim_list_wins(cx: &mut ApiContext) -> Result<Object> {
    ctrlvim_list_wins(cx)
}

/// `nvim_list_bufs` — every live buffer handle, in creation order.
#[ctrlvim_api(since = 1, fast)]
fn nvim_list_bufs(cx: &mut ApiContext) -> Result<Object> {
    Ok(Object::Array(cx.editor().buffer_ids().into_iter().map(Object::Buffer).collect()))
}

/// `nvim_set_decoration_provider` — real Neovim invokes `opts.on_win`/
/// `on_line` callbacks during redraw, computing decorations lazily for only
/// the lines actually on screen (`vim.lsp.semantic_tokens` uses this to
/// highlight semantic tokens as you scroll). ctrlvim has no equivalent
/// redraw-hook rendering pipeline, so this registers nothing and the
/// callbacks are simply never invoked — a real gap (no semantic-token
/// highlighting via this path), not a crash: `require('vim.lsp.semantic_tokens')`
/// calls this unconditionally at load time and needs it to at least exist.
#[ctrlvim_api(since = 1)]
fn nvim_set_decoration_provider(cx: &mut ApiContext, _ns: i64, _opts: Object) -> Result<Object> {
    let _ = cx;
    Ok(Object::Nil)
}

/// `nvim__redraw` — a private/internal Neovim function that hints the UI to
/// redraw specific things (a statusline, a buffer's screen lines, etc.).
/// ctrlvim's TUI redraws unconditionally every frame, so honoring the hint
/// has no observable effect beyond "not erroring when vendored runtime code
/// calls it" — which is exactly what several `vim.diagnostic`/`vim.lsp`
/// internals do after changing state.
#[ctrlvim_api(since = 1, fast)]
fn nvim__redraw(cx: &mut ApiContext, _opts: Object) -> Result<Object> {
    let _ = cx;
    Ok(Object::Nil)
}

/// `nvim_get_mode` — `{mode = "n", blocking = false}`. Real Neovim's `mode`
/// reflects full modal state (operator-pending, visual sub-modes, etc.);
/// `ApiContext` doesn't track mode at all (see [`ctrlvim_get_mode`]), so this
/// always reports Normal/non-blocking — real for a caller that just checks
/// "are we blocked waiting on input" (no), not for one that branches on the
/// actual mode.
#[ctrlvim_api(since = 1, fast)]
fn nvim_get_mode(cx: &mut ApiContext) -> Result<Object> {
    let _ = cx;
    let mut d = std::collections::BTreeMap::new();
    d.insert("mode".to_string(), Object::str("n"));
    d.insert("blocking".to_string(), Object::Boolean(false));
    Ok(Object::Dict(d))
}

/// `nvim_get_current_line`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_get_current_line(cx: &mut ApiContext) -> Result<Object> {
    ctrlvim_get_current_line(cx)
}

/// `nvim_set_current_line`.
#[ctrlvim_api(since = 1)]
fn nvim_set_current_line(cx: &mut ApiContext, line: String) -> Result<Object> {
    ctrlvim_set_current_line(cx, line)
}

/// `nvim_create_namespace`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_create_namespace(cx: &mut ApiContext, name: String) -> Result<Object> {
    ctrlvim_create_namespace(cx, name)
}

/// `nvim_get_namespaces` — `{name -> id}` for every named namespace.
#[ctrlvim_api(since = 1, fast)]
fn nvim_get_namespaces(cx: &mut ApiContext) -> Result<Object> {
    let dict = cx.namespaces().iter().map(|(name, id)| (name.clone(), Object::Integer(*id as i64))).collect();
    Ok(Object::Dict(dict))
}

/// `nvim_create_autocmd`.
#[ctrlvim_api(since = 1)]
fn nvim_create_autocmd(cx: &mut ApiContext, event: String, opts: Object) -> Result<Object> {
    ctrlvim_create_autocmd(cx, event, opts)
}

/// `nvim_create_augroup`. See [`ApiContext::create_augroup`] for what's real
/// (a stable id per name) and what isn't yet (`opts.clear`).
#[ctrlvim_api(since = 1)]
fn nvim_create_augroup(cx: &mut ApiContext, name: String, _opts: Object) -> Result<Object> {
    Ok(Object::Integer(cx.create_augroup(&name) as i64))
}

/// `nvim_del_autocmd`.
#[ctrlvim_api(since = 1)]
fn nvim_del_autocmd(cx: &mut ApiContext, id: i64) -> Result<Object> {
    ctrlvim_del_autocmd(cx, id)
}

/// `nvim_create_user_command`.
#[ctrlvim_api(since = 1)]
fn nvim_create_user_command(cx: &mut ApiContext, name: String, callback: Object, opts: Object) -> Result<Object> {
    ctrlvim_create_user_command(cx, name, callback, opts)
}

/// `nvim_buf_get_lines`. `_strict_indexing` matches real Neovim's signature
/// (error vs. clamp on an out-of-range `end`) but isn't enforced yet — this
/// always clamps, i.e. behaves as `strict_indexing = false`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_get_lines(cx: &mut ApiContext, buf: BufferId, start: i64, end: i64, _strict_indexing: bool) -> Result<Object> {
    let count = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_get_lines: invalid buffer"))?
        .text
        .line_count();
    let (s, e) = (resolve_index(start, count), resolve_index(end, count));
    if s > e {
        return Err(Error::validation("nvim_buf_get_lines: start is past end"));
    }
    let lines = cx.editor().buffer(buf).unwrap().text.lines();
    Ok(Object::Array(lines[s..e].iter().map(|l| Object::str(l.clone())).collect()))
}

/// `nvim_buf_set_lines`. `_strict_indexing`: see [`nvim_buf_get_lines`].
#[ctrlvim_api(since = 1)]
fn nvim_buf_set_lines(
    cx: &mut ApiContext,
    buf: BufferId,
    start: i64,
    end: i64,
    _strict_indexing: bool,
    replacement: Object,
) -> Result<Object> {
    let Object::Array(items) = replacement else {
        return Err(Error::validation("nvim_buf_set_lines: replacement must be an array of strings"));
    };
    let mut new_lines: Vec<String> = Vec::with_capacity(items.len());
    for item in &items {
        match item.as_str() {
            Some(s) => new_lines.push(s.to_string()),
            None => return Err(Error::validation("nvim_buf_set_lines: replacement must be an array of strings")),
        }
    }
    let state = cx
        .editor_mut()
        .buffer_mut(buf)
        .ok_or_else(|| Error::validation("nvim_buf_set_lines: invalid buffer"))?;
    let count = state.text.line_count();
    let (s, e) = (resolve_index(start, count), resolve_index(end, count));
    if s > e {
        return Err(Error::validation("nvim_buf_set_lines: start is past end"));
    }
    state.text.set_lines(s, e, &new_lines);
    state.changedtick += 1;
    Ok(Object::Nil)
}

/// `nvim_buf_line_count`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_line_count(cx: &mut ApiContext, buf: BufferId) -> Result<Object> {
    let count = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_line_count: invalid buffer"))?
        .text
        .line_count();
    Ok(Object::Integer(count as i64))
}

/// `nvim_buf_get_name`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_get_name(cx: &mut ApiContext, buf: BufferId) -> Result<Object> {
    let name = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_get_name: invalid buffer"))?
        .name
        .clone()
        .unwrap_or_default();
    Ok(Object::str(name))
}

/// `nvim_buf_get_changedtick`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_get_changedtick(cx: &mut ApiContext, buf: BufferId) -> Result<Object> {
    let tick = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_get_changedtick: invalid buffer"))?
        .changedtick;
    Ok(Object::Integer(tick as i64))
}

/// `nvim_buf_is_valid`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_is_valid(cx: &mut ApiContext, buf: BufferId) -> Result<Object> {
    Ok(Object::Boolean(cx.editor().buffer(buf).is_some()))
}

/// `nvim_buf_is_loaded` — ctrlvim has no unloaded-but-listed buffer state
/// (real Neovim's `:bunload` leaves a buffer entry around but drops its
/// text); a buffer here either exists (and is loaded) or doesn't, so this is
/// exactly [`nvim_buf_is_valid`].
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_is_loaded(cx: &mut ApiContext, buf: BufferId) -> Result<Object> {
    Ok(Object::Boolean(cx.editor().buffer(buf).is_some()))
}

/// `nvim_win_get_cursor`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_win_get_cursor(cx: &mut ApiContext, win: WindowId) -> Result<Object> {
    let w = cx.editor().window(win).ok_or_else(|| Error::validation("nvim_win_get_cursor: invalid window"))?;
    let (row, col) = w.cursor.to_cursor_api();
    Ok(Object::Array(vec![Object::Integer(row), Object::Integer(col)]))
}

/// `nvim_win_set_cursor`. Clamped to a valid Normal-mode position when `win`
/// is the current window (matching [`ctrlvim_win_set_cursor`] exactly); set
/// directly for any other window, since clamping today is only wired to
/// "the current buffer" — a real bounds check for arbitrary windows is a
/// straightforward follow-up, not a correctness trap (an out-of-range
/// position there just renders like any other unclamped one already can).
#[ctrlvim_api(since = 1)]
fn nvim_win_set_cursor(cx: &mut ApiContext, win: WindowId, pos: Object) -> Result<Object> {
    let arr = match &pos {
        Object::Array(a) if a.len() == 2 => a,
        _ => return Err(Error::validation("nvim_win_set_cursor: expected [row, col]")),
    };
    let row = arr[0].as_int().ok_or_else(|| Error::validation("cursor row must be an integer"))?;
    let col = arr[1].as_int().ok_or_else(|| Error::validation("cursor col must be an integer"))?;
    let target = Position::from_cursor_api(row, col);
    if win == cx.editor().current_window_id() {
        cx.editor_mut().set_cursor(target);
        return Ok(Object::Nil);
    }
    let w = cx
        .editor_mut()
        .window_mut(win)
        .ok_or_else(|| Error::validation("nvim_win_set_cursor: invalid window"))?;
    w.cursor = target;
    Ok(Object::Nil)
}

/// `nvim_win_get_buf`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_win_get_buf(cx: &mut ApiContext, win: WindowId) -> Result<Object> {
    let w = cx.editor().window(win).ok_or_else(|| Error::validation("nvim_win_get_buf: invalid window"))?;
    Ok(Object::Buffer(w.buffer))
}

/// `nvim_win_is_valid`.
#[ctrlvim_api(since = 1, fast)]
fn nvim_win_is_valid(cx: &mut ApiContext, win: WindowId) -> Result<Object> {
    Ok(Object::Boolean(cx.editor().window(win).is_some()))
}

/// `nvim_win_close` — closes a float or a split window, whichever `win` is.
/// `force` is accepted for signature compatibility; neither close path has
/// an "unsaved changes" confirmation gate to bypass yet, so it has no effect.
#[ctrlvim_api(since = 1)]
fn nvim_win_close(cx: &mut ApiContext, win: WindowId, _force: bool) -> Result<Object> {
    let ed = cx.editor_mut();
    if ed.close_float(win) || ed.close_window(win) {
        Ok(Object::Nil)
    } else {
        Err(Error::validation("nvim_win_close: invalid window, or it's the last one"))
    }
}

/// `nvim_open_win` — open a floating window over `buf`. `config` covers the
/// subset of real Neovim's dict this engine can act on: `relative`
/// (`"cursor"` or `"editor"`, default `"editor"`), `row`/`col`, `width`/
/// `height` (required), and `border` (any value other than absent/`"none"`
/// draws one). `relative = "win"` (anchored to another window) isn't
/// supported — it errors rather than silently drawing in the wrong place.
#[ctrlvim_api(since = 1)]
fn nvim_open_win(cx: &mut ApiContext, buf: BufferId, enter: bool, config: Object) -> Result<Object> {
    let Object::Dict(cfg) = &config else {
        return Err(Error::validation("nvim_open_win: config must be a dict"));
    };
    let relative = match cfg.get("relative").and_then(|o| o.as_str()) {
        None | Some("editor") | Some("") => FloatRelative::Editor,
        Some("cursor") => FloatRelative::Cursor,
        Some(other) => {
            return Err(Error::validation(format!(
                "nvim_open_win: relative={other} is not supported (only \"editor\" and \"cursor\")"
            )))
        }
    };
    let width = cfg
        .get("width")
        .and_then(|o| o.as_int())
        .ok_or_else(|| Error::validation("nvim_open_win: config.width is required"))?;
    let height = cfg
        .get("height")
        .and_then(|o| o.as_int())
        .ok_or_else(|| Error::validation("nvim_open_win: config.height is required"))?;
    let row = cfg.get("row").and_then(|o| o.as_int()).unwrap_or(0);
    let col = cfg.get("col").and_then(|o| o.as_int()).unwrap_or(0);
    let border = match cfg.get("border") {
        None | Some(Object::Nil) => false,
        Some(Object::String(s)) => s.as_slice() != b"none",
        Some(_) => true,
    };
    if cx.editor().buffer(buf).is_none() {
        return Err(Error::validation("nvim_open_win: invalid buffer"));
    }
    let float = FloatConfig { relative, row, col, width: width.max(0) as usize, height: height.max(0) as usize, border };
    let win = cx.editor_mut().open_float(buf, float);
    if enter {
        cx.editor_mut().focus_window(win);
    }
    Ok(Object::Window(win))
}

/// Parse an `nvim_buf_set_extmark` `opts` dict into [`ExtmarkMeta`]. `Nil`
/// (no opts given) is a plain position-only mark.
fn parse_extmark_opts(opts: &Object) -> Result<ExtmarkMeta> {
    let dict = match opts {
        Object::Dict(d) => d,
        Object::Nil => return Ok(ExtmarkMeta::default()),
        other => return Err(Error::validation(format!("extmark opts must be a dict, got {}", other.type_name()))),
    };
    let end_line = dict.get("end_row").and_then(|o| o.as_int()).map(|n| n.max(0) as usize);
    let end_col = dict.get("end_col").and_then(|o| o.as_int()).map(|n| n.max(0) as usize);
    let hl_group = dict.get("hl_group").and_then(|o| o.as_str()).map(str::to_string);
    let virt_text = match dict.get("virt_text") {
        Some(Object::Array(chunks)) => chunks
            .iter()
            .filter_map(|c| match c {
                Object::Array(pair) if !pair.is_empty() => {
                    let text = pair[0].as_str()?.to_string();
                    let hl = pair.get(1).and_then(|o| o.as_str()).map(str::to_string);
                    Some((text, hl))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(ExtmarkMeta { end_line, end_col, hl_group, virt_text })
}

fn wants_details(opts: &Object) -> bool {
    matches!(opts, Object::Dict(d) if d.get("details").and_then(|o| o.as_bool()).unwrap_or(false))
}

fn extmark_details(meta: Option<&ExtmarkMeta>) -> Object {
    let mut d = std::collections::BTreeMap::new();
    if let Some(meta) = meta {
        if let Some(end_line) = meta.end_line {
            d.insert("end_row".to_string(), Object::Integer(end_line as i64));
        }
        if let Some(end_col) = meta.end_col {
            d.insert("end_col".to_string(), Object::Integer(end_col as i64));
        }
        if let Some(hl) = &meta.hl_group {
            d.insert("hl_group".to_string(), Object::str(hl.clone()));
        }
        if !meta.virt_text.is_empty() {
            let chunks = meta
                .virt_text
                .iter()
                .map(|(text, hl)| {
                    let mut pair = vec![Object::str(text.clone())];
                    if let Some(hl) = hl {
                        pair.push(Object::str(hl.clone()));
                    }
                    Object::Array(pair)
                })
                .collect();
            d.insert("virt_text".to_string(), Object::Array(chunks));
        }
    }
    Object::Dict(d)
}

/// `nvim_buf_set_extmark` — [`ctrlvim_buf_set_extmark`] plus the decoration
/// options `vim.diagnostic` actually needs: `end_row`/`end_col` (the marked
/// range), `hl_group`, and `virt_text` (an array of `{text, hl_group}` pairs
/// appended after the line).
#[ctrlvim_api(since = 1)]
fn nvim_buf_set_extmark(cx: &mut ApiContext, buf: BufferId, ns: i64, line: i64, col: i64, opts: Object) -> Result<Object> {
    if line < 0 || col < 0 {
        return Err(Error::validation("nvim_buf_set_extmark: line and col must be non-negative"));
    }
    let meta = parse_extmark_opts(&opts)?;
    let pos = Position::new(line as usize, col as usize);
    let namespace = Namespace(ns as u32);
    let state = cx
        .editor_mut()
        .buffer_mut(buf)
        .ok_or_else(|| Error::validation("nvim_buf_set_extmark: invalid buffer"))?;
    let id = state.marks.add(namespace, pos, Gravity::Right);
    if meta != ExtmarkMeta::default() {
        state.extmark_meta.insert((namespace, id), meta);
    }
    Ok(Object::Integer(id as i64))
}

/// `nvim_buf_get_extmark_by_id` — `[line, col]`, or `[line, col, details]`
/// when `opts.details` is truthy.
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_get_extmark_by_id(cx: &mut ApiContext, buf: BufferId, ns: i64, id: i64, opts: Object) -> Result<Object> {
    let state = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_get_extmark_by_id: invalid buffer"))?;
    let namespace = Namespace(ns as u32);
    let Some(pos) = state.marks.get(namespace, id as u32) else {
        return Ok(Object::Array(Vec::new()));
    };
    let mut out = vec![Object::Integer(pos.line as i64), Object::Integer(pos.col as i64)];
    if wants_details(&opts) {
        out.push(extmark_details(state.extmark_meta.get(&(namespace, id as u32))));
    }
    Ok(Object::Array(out))
}

/// `nvim_buf_get_extmarks` — every mark in `ns` whose line falls in
/// `[start, end)`; `-1` for either bound means "to the end of the buffer".
#[ctrlvim_api(since = 1, fast)]
fn nvim_buf_get_extmarks(cx: &mut ApiContext, buf: BufferId, ns: i64, start: i64, end: i64, opts: Object) -> Result<Object> {
    let state = cx
        .editor()
        .buffer(buf)
        .ok_or_else(|| Error::validation("nvim_buf_get_extmarks: invalid buffer"))?;
    let namespace = Namespace(ns as u32);
    let line_count = state.text.line_count();
    let lo = if start < 0 { 0 } else { start as usize };
    let hi = if end < 0 { line_count } else { end as usize };
    let mut marks = state.marks.all_in(namespace);
    marks.retain(|(_, pos)| pos.line >= lo && pos.line < hi);
    marks.sort_by_key(|(id, pos)| (pos.line, pos.col, *id));
    let details = wants_details(&opts);
    Ok(Object::Array(
        marks
            .into_iter()
            .map(|(id, pos)| {
                let mut row = vec![Object::Integer(id as i64), Object::Integer(pos.line as i64), Object::Integer(pos.col as i64)];
                if details {
                    row.push(extmark_details(state.extmark_meta.get(&(namespace, id))));
                }
                Object::Array(row)
            })
            .collect(),
    ))
}

/// `nvim_buf_del_extmark`.
#[ctrlvim_api(since = 1)]
fn nvim_buf_del_extmark(cx: &mut ApiContext, buf: BufferId, ns: i64, id: i64) -> Result<Object> {
    let namespace = Namespace(ns as u32);
    let state = cx
        .editor_mut()
        .buffer_mut(buf)
        .ok_or_else(|| Error::validation("nvim_buf_del_extmark: invalid buffer"))?;
    let gone = state.marks.remove(namespace, id as u32);
    state.extmark_meta.remove(&(namespace, id as u32));
    Ok(Object::Boolean(gone))
}

/// `nvim_buf_clear_namespace` — remove every mark `ns` owns whose line falls
/// in `[line_start, line_end)`; `-1` for `line_end` means "to the end".
/// `ns = -1` ("every namespace", real Neovim's convention) isn't supported
/// yet — a caller doing that today needs one call per namespace.
#[ctrlvim_api(since = 1)]
fn nvim_buf_clear_namespace(cx: &mut ApiContext, buf: BufferId, ns: i64, line_start: i64, line_end: i64) -> Result<Object> {
    if ns < 0 {
        return Err(Error::validation("nvim_buf_clear_namespace: ns = -1 (all namespaces) isn't supported yet"));
    }
    let namespace = Namespace(ns as u32);
    let state = cx
        .editor_mut()
        .buffer_mut(buf)
        .ok_or_else(|| Error::validation("nvim_buf_clear_namespace: invalid buffer"))?;
    let line_count = state.text.line_count();
    let lo = if line_start < 0 { 0 } else { line_start as usize };
    let hi = if line_end < 0 { line_count } else { line_end as usize };
    let doomed: Vec<u32> = state
        .marks
        .all_in(namespace)
        .into_iter()
        .filter(|(_, pos)| pos.line >= lo && pos.line < hi)
        .map(|(id, _)| id)
        .collect();
    for id in doomed {
        state.marks.remove(namespace, id);
        state.extmark_meta.remove(&(namespace, id));
    }
    Ok(Object::Nil)
}

/// `nvim_buf_attach` — register `opts.on_lines` to be notified when `buf`'s
/// content changes. Real signature also accepts `on_bytes`/`on_reload`/
/// `on_detach`/`on_changedtick`; only `on_lines` is wired up (it's the one
/// `vim.lsp`'s change-tracking actually uses to drive `didChange`). Without
/// it, this still "succeeds" (matching a real attach with none of its
/// optional callbacks set) but watches nothing.
///
/// Firing is driven by [`ApiContext::check_buf_watcher`] /
/// `Host::notify_buf_lines_changed`, called by the embedder after an edit —
/// see that method's doc comment for what's wired up today (Lua-driven
/// edits) and what isn't (ctrlvim-tui's interactive typing, which runs
/// against a separate `Editor` instance from this one).
#[ctrlvim_api(since = 1)]
fn nvim_buf_attach(cx: &mut ApiContext, buf: BufferId, _send_buffer: bool, opts: Object) -> Result<Object> {
    if cx.editor().buffer(buf).is_none() {
        return Err(Error::validation("nvim_buf_attach: invalid buffer"));
    }
    let on_lines = match &opts {
        Object::Dict(d) => d.get("on_lines").and_then(|o| match o {
            Object::LuaRef(r) => Some(*r),
            _ => None,
        }),
        _ => None,
    };
    if let Some(luaref) = on_lines {
        let snapshot = cx.editor().buffer(buf).unwrap().text.lines();
        cx.buf_watchers.insert(buf, (luaref, snapshot));
    }
    Ok(Object::Boolean(true))
}
