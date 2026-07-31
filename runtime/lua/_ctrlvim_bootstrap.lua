-- ctrlvim's own glue, NOT vendored Neovim source (see ../NOTICE.md for what
-- is). Real Neovim's C side wires the lazily-`require()`d `vim.lsp`/
-- `vim.diagnostic`/`vim.uri` modules onto the global `vim` table itself,
-- via a bootstrap sequence spread across the C executor and several runtime
-- files (`vim/_meta.lua` documents the *result* but isn't itself executable
-- -- it errors if required, being a `@meta` type-stub). This is the minimal
-- Lua that reproduces that same end state: require what's needed, assign it
-- onto `vim`.
require('vim._core.shared')

vim.F = require('vim.F')
vim.hl = require('vim.hl')
vim.inspect = require('vim.inspect')
vim._watch = require('vim._watch')
vim.glob = require('vim.glob')
vim.lsp = require('vim.lsp')
vim.diagnostic = require('vim.diagnostic')

local uri = require('vim.uri')
vim.uri_from_fname = uri.uri_from_fname
vim.uri_from_bufnr = uri.uri_from_bufnr
vim.uri_to_fname = uri.uri_to_fname
vim.uri_to_bufnr = uri.uri_to_bufnr
