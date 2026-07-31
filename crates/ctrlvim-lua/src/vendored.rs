//! Neovim's own `vim.lsp`/`vim.diagnostic` runtime Lua, embedded into the
//! binary — see `runtime/NOTICE.md` at the repo root for exactly what's
//! vendored, from where, and why.
//!
//! Embedding (rather than reading from `runtime/lua/` on disk at startup)
//! means this works regardless of the current directory or install
//! location — the same reason Rust's own `include_str!` exists. Each entry
//! is registered as a `package.preload[name]` loader (see
//! [`crate::host::Host::install_require`]), so `require('vim.lsp')` resolves
//! to the embedded source exactly as it would to a file on disk.

/// `(dotted module name, source)` for every vendored file. Order doesn't
/// matter — Lua's `require()` resolves modules on demand, not eagerly.
pub const MODULES: &[(&str, &str)] = &[
    ("vim._core.shared", include_str!("../../../runtime/lua/vim/_core/shared.lua")),
    ("vim._core.util", include_str!("../../../runtime/lua/vim/_core/util.lua")),
    ("vim._core.stringbuffer", include_str!("../../../runtime/lua/vim/_core/stringbuffer.lua")),
    ("vim.lsp", include_str!("../../../runtime/lua/vim/lsp.lua")),
    ("vim.lsp.client", include_str!("../../../runtime/lua/vim/lsp/client.lua")),
    ("vim.lsp.rpc", include_str!("../../../runtime/lua/vim/lsp/rpc.lua")),
    ("vim.lsp._transport", include_str!("../../../runtime/lua/vim/lsp/_transport.lua")),
    ("vim.lsp._watchfiles", include_str!("../../../runtime/lua/vim/lsp/_watchfiles.lua")),
    ("vim.lsp.protocol", include_str!("../../../runtime/lua/vim/lsp/protocol.lua")),
    ("vim.lsp.util", include_str!("../../../runtime/lua/vim/lsp/util.lua")),
    ("vim.lsp.buf", include_str!("../../../runtime/lua/vim/lsp/buf.lua")),
    ("vim.lsp.handlers", include_str!("../../../runtime/lua/vim/lsp/handlers.lua")),
    ("vim.lsp.sync", include_str!("../../../runtime/lua/vim/lsp/sync.lua")),
    ("vim.lsp._changetracking", include_str!("../../../runtime/lua/vim/lsp/_changetracking.lua")),
    ("vim.lsp.completion", include_str!("../../../runtime/lua/vim/lsp/completion.lua")),
    ("vim.lsp.log", include_str!("../../../runtime/lua/vim/lsp/log.lua")),
    ("vim.lsp.semantic_tokens", include_str!("../../../runtime/lua/vim/lsp/semantic_tokens.lua")),
    ("vim.lsp._folding_range", include_str!("../../../runtime/lua/vim/lsp/_folding_range.lua")),
    ("vim.lsp.inline_completion", include_str!("../../../runtime/lua/vim/lsp/inline_completion.lua")),
    ("vim.lsp.document_color", include_str!("../../../runtime/lua/vim/lsp/document_color.lua")),
    ("vim.lsp._capability", include_str!("../../../runtime/lua/vim/lsp/_capability.lua")),
    // Not the real vendored file: the real one builds an LPeg grammar (a
    // whole PEG-parsing C module Neovim bundles) at *module load time*, just
    // to parse `${1:placeholder}`-style LSP snippet bodies later. ctrlvim has
    // no LPeg. `M.parse` is only ever called when actually expanding a
    // completion item's snippet text — never at load time — so a stub that
    // errors there (not here) is enough to unblock `client:initialize()`,
    // which loads this module unconditionally (see the `-- HACK: Capability
    // modules must be loaded` comment in the real `vim/lsp/client.lua`).
    // Real gap: snippet placeholder parsing doesn't work.
    ("vim.lsp._snippet_grammar", SNIPPET_GRAMMAR_STUB),
    ("vim.treesitter._range", include_str!("../../../runtime/lua/vim/treesitter/_range.lua")),
    ("vim.diagnostic", include_str!("../../../runtime/lua/vim/diagnostic.lua")),
    ("vim.hl", include_str!("../../../runtime/lua/vim/hl.lua")),
    ("vim.F", include_str!("../../../runtime/lua/vim/F.lua")),
    ("vim.uri", include_str!("../../../runtime/lua/vim/uri.lua")),
    ("vim.inspect", include_str!("../../../runtime/lua/vim/inspect.lua")),
    // Not vendored from Neovim — LuaJIT bundles this (Mike Pall's BitOp,
    // MIT-licensed) as a built-in C module; real Lua 5.4 (what `mlua`'s
    // `lua54` feature gives us) has native bitwise operators instead, so
    // `vim.uri`'s `require('bit').tohex` needs *something* registered under
    // the plain top-level name `bit`. This is a from-scratch reimplication
    // of just the handful of functions anything here reaches for, not a
    // vendored file — hence no license note in `runtime/NOTICE.md`.
    ("bit", BIT_COMPAT),
    ("vim._watch", WATCH_STUB),
    ("vim.glob", GLOB_STUB),
];

/// Not vendored: real `vim._watch` uses `uv.new_fs_event` (OS-level file
/// watching — inotify/FSEvents/kqueue), a primitive this engine doesn't
/// implement yet. Stubbed as a no-op so `workspace/didChangeWatchedFiles`
/// registration/cleanup (`vim.lsp._watchfiles`, always touched during a
/// client's normal lifecycle) doesn't crash. Real gap: ctrlvim won't notice
/// files changing on disk outside its own edits and tell the server about it.
/// Not vendored: real `vim.glob.to_lpeg` compiles a glob pattern into an
/// LPeg pattern object (see `WATCH_STUB`'s note on LPeg). Returns a harmless
/// stand-in that supports `+` (glob patterns are combined with it, e.g.
/// `vim.lsp._watchfiles`'s default excludes) but can't actually match
/// anything — paired with the `vim._watch` stub above (which never fires a
/// callback to match against), so this only matters if something *else*
/// starts calling `:match()` on the result.
const GLOB_STUB: &str = r#"
local mt = {}
mt.__add = function(_a, _b) return setmetatable({}, mt) end
return {
  to_lpeg = function(_pattern) return setmetatable({}, mt) end,
}
"#;

const WATCH_STUB: &str = r#"
local function noop_watcher(_path, _opts, _callback)
  return function() end
end
return {
  watch = noop_watcher,
  inotify = noop_watcher,
  watchdirs = noop_watcher,
  FileChangeType = { Created = 1, Changed = 2, Deleted = 3 },
}
"#;

const SNIPPET_GRAMMAR_STUB: &str = r#"
return {
  parse = function(_input)
    error('ctrlvim: LSP snippet placeholder parsing is not implemented yet (no LPeg)')
  end,
}
"#;

const BIT_COMPAT: &str = r#"
return {
  tohex = function(x, n)
    n = n or 8
    x = x & 0xFFFFFFFF
    return string.format('%0' .. n .. 'x', x)
  end,
  band = function(a, b) return a & b end,
  bor = function(a, b) return a | b end,
  bxor = function(a, b) return a ~ b end,
  bnot = function(a) return (~a) & 0xFFFFFFFF end,
  lshift = function(a, n) return (a << n) & 0xFFFFFFFF end,
  rshift = function(a, n) return (a & 0xFFFFFFFF) >> n end,
  arshift = function(a, n) return a >> n end,
  tobit = function(a) return a & 0xFFFFFFFF end,
}
"#;

/// The bootstrap that assigns `vim.lsp`/`vim.diagnostic`/`vim.uri_*` onto the
/// global `vim` table — see `runtime/lua/_ctrlvim_bootstrap.lua`'s own
/// comment for why this exists as ctrlvim's own glue rather than more
/// vendored source.
pub const BOOTSTRAP: &str = include_str!("../../../runtime/lua/_ctrlvim_bootstrap.lua");
