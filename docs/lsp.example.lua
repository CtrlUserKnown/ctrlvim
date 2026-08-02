-- ctrlvim language server / linker declarations — copy to
-- ~/.config/ctrlvim/lsp.lua
--
-- This file is the *only* place a language server or build linker exists as
-- far as ctrlvim is concerned. The compiled editor has no built-in list of
-- servers, no filetype-to-server mapping, and no install commands baked in —
-- every name below is one you chose, and a server you don't declare here
-- never appears anywhere in the editor: not in the Settings tab, not spawned,
-- not even shown as "not found".
--
-- Return a plain array of tables. Each entry:
--
--   name      Required. Whatever you want it called in the Settings tab.
--   filetypes Which buffers it attaches to (matched against the buffer's
--             filetype name, e.g. "rust", "typescript", "lua"). Omit — or
--             leave empty — for a presence-only entry: it still gets a
--             status row and can be installed, but it's never spawned as a
--             language server. That's the shape a build linker wants.
--   cmd       Required. The program plus its LSP-mode arguments — cmd[1] is
--             looked up on PATH, the rest are passed through as-is.
--   install   Optional. A shell command line, run verbatim when you press
--             `I` on that row in the Settings tab. ctrlvim doesn't inspect
--             or understand it — it's entirely yours.
--   enabled   Optional, defaults to true. `false` keeps the row visible
--             (so you can still install/inspect it) without ever attaching
--             it to a buffer.

return {
  {
    name = "rust_analyzer",
    filetypes = { "rust" },
    cmd = { "rust-analyzer" },
    install = "rustup component add rust-analyzer",
  },
  {
    name = "ts_ls",
    filetypes = { "typescript", "javascript", "tsx" },
    cmd = { "typescript-language-server", "--stdio" },
    install = "npm install -g typescript-language-server typescript",
  },
  {
    name = "taplo",
    filetypes = { "toml" },
    cmd = { "taplo", "lsp", "stdio" },
    install = "cargo install taplo-cli --locked --features lsp",
  },

  -- lua_ls has no cargo/npm/pip package — it only ships as a GitHub release
  -- archive. `cvi tool fetch-release` (see `wikis/lsp-mason-plan.md`) is
  -- ctrlvim's generic downloader for exactly this case: it resolves `--tag`
  -- (a version, or "latest") through the GitHub API, downloads `--asset`
  -- by its exact filename, extracts it under
  -- ~/.local/share/ctrlvim/tools/<--dest>/, and chmod +x's `--bin`. `cmd`
  -- here is a bare name because that's what `locate()` (PATH, then
  -- ctrlvim's own tools dir) resolves against — no need to touch $PATH.
  {
    name = "lua_ls",
    filetypes = { "lua" },
    cmd = { "lua-language-server" },
    -- Pin `--tag` to a real release (check
    -- github.com/LuaLS/lua-language-server/releases) and match `--asset` to
    -- one of that release's actual asset names for your platform — swap
    -- `darwin-arm64` for `darwin-x64`/`linux-x64`/`linux-arm64` as needed.
    -- `--tag latest` doesn't work here: the asset filename bakes in the
    -- version, so it has to match whatever `latest` actually resolves to.
    install = "cvi tool fetch-release --repo LuaLS/lua-language-server "
      .. "--tag 3.18.2 --asset lua-language-server-3.18.2-darwin-arm64.tar.gz "
      .. "--dest lua_ls --bin bin/lua-language-server",
  },

  -- A build linker: no `filetypes`, so it's never attached to a buffer —
  -- just checked for presence and shown as a status row.
  { name = "mold", cmd = { "mold" } },
}
