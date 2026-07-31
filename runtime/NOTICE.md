# Vendored Neovim runtime Lua

`runtime/lua/vim/` contains **unmodified** Lua source copied from
[neovim/neovim](https://github.com/neovim/neovim), pinned to tag `v0.12.4`,
under the Apache License 2.0 (see `LICENSE` in that repository — Neovim's own
Lua-authored runtime files, which these all are, carry no separate Vim-license
encumbrance).

Copied files:

- `vim/lsp.lua`, `vim/lsp/{client,rpc,protocol,util,buf,handlers,sync,
  _changetracking,completion,log}.lua`
- `vim/diagnostic.lua`
- `vim/uri.lua`
- `vim/_core/shared.lua`

This is the real, load-bearing part of "run the same `lspconfig.lua`" — these
files are executed as-is; ctrlvim only supplies the `vim.api`/`vim.uv`/
`vim.fn`/etc. primitives they call into (see `crates/ctrlvim-lua`) and a small
bootstrap (`runtime/lua/_ctrlvim_bootstrap.lua`, *not* vendored — that one is
ctrlvim's own glue) that replicates the handful of lines real Neovim's C side
runs to wire `vim.lsp`/`vim.diagnostic`/`vim.uri` onto the global `vim` table.

Not vendored (accessed lazily via `vim.lsp._defer_require`, so their absence
only matters if something actually touches them): `vim.lsp.semantic_tokens`,
`inlay_hint`, `codelens`, `document_color`, `_folding_range`,
`linked_editing_range`, `on_type_formatting`, `inline_completion`,
`_snippet_grammar`, `_capability`, `_watchfiles`, `_tagfunc`, `health`. These
are advanced/optional LSP features beyond the core start → diagnostics →
hover → definition → references → rename → format flow this integration
targets first.

To update the pin: re-run the fetch against a newer tag, diff against what's
here, and re-check `crates/ctrlvim-lua/src/host.rs`'s primitive surface
against whatever new `vim.*` symbols the updated files reach for.
