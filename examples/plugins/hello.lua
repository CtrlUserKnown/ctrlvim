-- A minimal ctrlvim plugin, run once at startup.
--
-- To enable it, add a line to `~/.config/ctrlvim/config.toml`:
--
--   plugin = "/absolute/path/to/ctrlvim/examples/plugins/hello.lua"
--
-- (or copy this file somewhere under your own config and point at that).
-- Every `plugin = "path"` line in config.toml is run once, in order, at
-- startup, over the same path as `:luafile`.
--
-- Try it: launch `cvi`, open any file, then run `:lua Hello.greet()` — it
-- replaces the current line with a greeting.

Hello = {}

function Hello.greet()
  vim.api.ctrlvim_set_current_line("Hello from ctrlvim!")
end
