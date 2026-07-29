# LSP, linting, formatting & tool management — plan

Status: **phase 5 (Tool Manager) implemented; phases 1-4 (the real LSP
client) not started.** This is the scoped-down alternative to a general
plugin ecosystem: bake in LSP + linters + formatters + a Mason-style
installer, and treat that as the *only* "plugin-shaped" feature ctrlvim
ships natively. See [`plugin-components-plan.md`](./plugin-components-plan.md)
for the separate (later, optional) idea of letting users build their own
plugins on top of components.

## What's built (the Mason-equivalent, phase 5)

- New crate `ctrlvim-tools`: a static registry (`ctrlvim_tools::REGISTRY`) of
  language servers, formatters, and linters, each with candidate binary
  names and an [`InstallMethod`](../crates/ctrlvim-tools/src/lib.rs) (Cargo,
  Npm, Pip, GoInstall, Rustup, or Unsupported for tools with no scriptable
  install yet).
- Detection (`ctrlvim_tools::locate`) checks `PATH` first, then ctrlvim's own
  `~/.local/share/ctrlvim/tools/<name>/bin/` — so a tool ctrlvim installed
  itself is found without touching the user's shell profile.
- The Settings tab's tool table (`crates/ctrlvim-tui/src/ui/dashboard.rs`)
  now shows every registry entry (not just LSP servers) plus the pre-existing
  PATH-only build-linker rows, colors "not found" orange when installable vs
  red when not, and shows a footer hint ("Press I to install taplo (cargo
  install)") for the focused row.
- `I` in the Settings tab (`App::install_focused_tool` /
  `App::install_tool`, `crates/ctrlvim-tui/src/app.rs`) shells out to the
  registry's install command, reusing the existing `:!{cmd}` job plumbing —
  the output overlay doubles as the install log, and on exit 0 the row's
  installed/managed status refreshes live.
- This did **not** need the persistent-stdin job extension described below —
  installs are one-shot commands (`cargo install`, `npm install`, …), not
  long-lived processes, so `Jobs::spawn_shell` (already used for `:!{cmd}`)
  was sufficient.

Still open from the original registry design: no GitHub-release-binary
install method yet (several LSP servers — `lua_ls`, `marksman`, `jdtls`,
`lemminx`, `mesonlsp` — and `shellcheck` are marked `Unsupported` until one
exists).

## Where things stood before the Tool Manager work

Nothing talked to a language server, or installed anything:

- `detect_lsp()` in `crates/ctrlvim-tui/src/data.rs:346` just checks `PATH`
  for known binary names (`rust-analyzer`, `taplo`, `lua-language-server`,
  etc.) and reports installed/not-installed as a bool. It never spawns
  anything.
- The Settings tab lets you `ToggleLsp(i)` (`crates/ctrlvim-tui/src/app.rs:741`),
  but that only flips a `Vec<bool>` used for display — no process starts or
  stops.
- There's no JSON-RPC/LSP protocol code anywhere in the workspace, no
  diagnostics model, no completion, no formatting integration.
- The job system (`crates/ctrlvim-async/src/job.rs`) can spawn a process and
  stream its merged stdout+stderr back as line-oriented `Event`s, but
  `stdin` is hard-wired to `Stdio::null()` — there's no way to write to a
  child process today. LSP needs bidirectional stdio; several formatters
  (`prettier`, `stylua -`, `biome format --stdin-file-path`) also read
  source from stdin.

So this is greenfield work, not wiring up something half-built. That's good
— it means the pieces can be built in the order that gives working features
fastest, rather than untangling a mock.

## Scope

In scope, per your ask — this is *not* a general plugin system:

1. Real LSP client: diagnostics, hover, goto-definition, references,
   rename, code actions, completion, signature help.
2. Formatting: via LSP (`textDocument/formatting`) where the attached
   server supports it, or an external CLI otherwise (`stylua`, `prettier`,
   `rustfmt`, `shfmt`, ...).
3. Linting for tools that aren't LSP servers (`shellcheck`, standalone
   `eslint`, etc.) via the existing quickfix pipeline.
4. A Mason-equivalent: a built-in registry of installable tools (language
   servers, formatters, linters) that ctrlvim can download/install/update
   itself, so the user doesn't need `rustup component add` /
   `npm i -g` / manual PATH management for each project.

Explicitly out of scope: a general Lua plugin API for arbitrary
third-party extensions beyond what already exists in `ctrlvim-lua`. This
plan only grows *editor-native* LSP/lint/format/tool-install features.

## Architecture

### 1. Persistent, bidirectional jobs (prerequisite for everything else)

Extend `crates/ctrlvim-async/src/job.rs` with a second spawn path alongside
the existing fire-and-forget `Jobs::spawn`:

```rust
pub fn spawn_persistent(&mut self, program: &str, args: &[String], cwd: &Path)
    -> (u64, JobStdin) // JobStdin wraps a writer half + the child handle
```

- `stdin(Stdio::piped())` instead of `null()`.
- Returns a handle the caller can write framed bytes to (an mpsc channel
  into the tokio task, same shape as the existing `tx: Sender<Event>` used
  for output).
- Existing `Event::ProcessOutput`/`ProcessExit` continue to carry output;
  no change needed there since LSP framing is done by the *consumer*
  parsing raw bytes (`Content-Length: N\r\n\r\n{json}`), not by `LineBuffer`
  (which stays exactly as-is for `:make`/`:grep`/quickfix use).
- This one primitive is shared by the LSP client and by stdin-based
  formatters — no separate process-management code needed for each.

### 2. New crate: `ctrlvim-lsp`

Protocol-level code, independent of the TUI:

- LSP message framing (`Content-Length` header parsing/writing) over the
  `spawn_persistent` byte stream.
- Request/response correlation (id → oneshot/callback), notification
  dispatch (`textDocument/publishDiagnostics`, etc.).
- `initialize`/`initialized` handshake with capability negotiation.
- Typed(-ish) request/response structs for the methods in scope:
  `didOpen`/`didChange`/`didClose`, `publishDiagnostics`, `hover`,
  `definition`, `references`, `rename`, `codeAction`, `completion`,
  `signatureHelp`, `formatting`.
- Position encoding: LSP defaults to UTF-16 code units, ctrlvim's rope
  (`ctrlvim-text`) needs conversion at the boundary — this is the classic
  correctness trap in LSP clients, worth a dedicated test module.
- Consider `lsp-types` (the crate) for the wire structs rather than hand
  rolling them — worth confirming license/fit before committing, but no
  reason to reinvent well-tested serde structs for the protocol.

One client instance per attached server per project; a server is started
lazily the first time a buffer of a matching filetype is opened, and shut
down when the last buffer of that filetype closes (or on `:LspStop`).

### 3. Diagnostics

- A `Diagnostic { range, severity, message, source }` type, populated from
  `publishDiagnostics` notifications.
- Surfaced two ways, reusing what already exists rather than inventing new
  UI: sign-column markers / virtual text in the buffer view, and a
  `:LspDiagnostics` list that feeds the *existing* quickfix machinery
  (`crates/ctrlvim-editor/src/quickfix.rs`) so `:copen`/`:cnext` already
  work on it for free.

### 4. Editor-facing actions

New `Action` variants in `ctrlvim-tui` (same dispatch pattern already used
for jobs/git elsewhere in `app.rs`): `LspHover`, `LspGotoDefinition`,
`LspReferences`, `LspRename`, `LspCodeAction`, `LspCompletion`,
`LspSignatureHelp`. Each maps to the obvious default keybinding (`gd`, `K`,
`gr`, etc.) matching Neovim's own LSP defaults so muscle memory carries
over.

Completion needs a popup menu — likely the first consumer of whatever the
component/layout work produces (see the components plan), but can ship
first with a minimal bespoke list widget and get upgraded later; it
shouldn't block on that other doc.

### 5. Formatting

`:Format` (and `gq` over a range) resolves to, in order:
1. `textDocument/formatting` if a server is attached and advertises the
   capability.
2. Otherwise, a configured external formatter run through
   `spawn_persistent`, writing the buffer to its stdin and replacing the
   buffer with stdout — the same `formatprg`/`formatexpr` idea real Vim
   uses, just resolved automatically per filetype instead of needing
   manual `formatprg` configuration.

`config.toml` gets a `[format]` table mapping filetype → command, with
sensible built-in defaults (`rustfmt`, `stylua`, `prettier`, `shfmt`) that
the user can override.

### 6. Linting (non-LSP tools)

Tools like `shellcheck` or a standalone `eslint` aren't LSP servers — they
just emit compiler-style output. Rather than building a second diagnostics
pathway, run them through the *existing* `:make`-style errorformat parsing
into quickfix. Only genuinely LSP-shaped linters go through the `ctrlvim-lsp`
client.

### 7. Tool Manager (the Mason-equivalent) — done

Implemented as described in "What's built" above, in the existing Settings
tab (not the separate Plugin Manager screen — that stays about general
plugins, per the scope split with `plugin-components-plan.md`). Remaining
gap: no GitHub-release-binary install method, so a handful of registry
entries are still `Unsupported` until that lands.

## Phasing

1. ~~**Tool Manager**: registry + installer + real install behavior in the
   Settings tab.~~ **Done** — see "What's built" above.
2. **Plumbing**: `spawn_persistent` + stdin support in `ctrlvim-async`.
   Nothing user-visible yet, but the LSP client and stdin-based formatters
   both depend on it.
3. **One server, end-to-end**: `ctrlvim-lsp` crate with just enough to
   `initialize` `rust_analyzer`, send `didOpen`/`didChange`/`didClose`, and
   render `publishDiagnostics` as buffer markers. Proves the whole pipe
   works before generalizing.
4. **Core LSP features**: hover, goto-definition, completion, code actions,
   rename, signature help — generalized across all servers in the
   registry, not just rust-analyzer.
5. **Formatting + non-LSP linting**: `:Format`, `[format]` config table,
   errorformat-based linting reusing quickfix.

Each phase is independently useful and shippable — diagnostics-only (phase
3) is already a big step up from PATH-detection, so there's no need to land
all of this before any of it is useful. Doing the Tool Manager first (out of
its original order) turned out fine: it has no dependency on the LSP client
work, since installing a binary and speaking its protocol are separate
concerns.

## Open questions

- `lsp-types` crate vs. hand-rolled structs — worth a quick spike before
  committing to the dependency.
- How much of `rust-analyzer`'s custom (non-standard) protocol extensions
  (e.g. `rust-analyzer/expandMacro`) are worth supporting given it's likely
  to be the most-used server for this project's own development.
- Whether the completion popup ships as a bespoke minimal widget first
  (phase 3) or waits on the components/layout work — leaning toward
  bespoke-first so LSP work isn't blocked on a separate, unscheduled effort.
