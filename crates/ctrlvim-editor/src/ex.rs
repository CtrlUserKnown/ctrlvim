//! Ex command-line (`:`) parsing and the host-effect boundary.
//!
//! The engine is UI-less: it can move the cursor for `:N`/`:$` itself, but
//! anything touching the filesystem, the window/tab list, or process lifetime
//! is emitted as an [`ExEffect`] for the host (the frontend) to perform. This
//! mirrors Neovim's split between the core and its UI/RPC layer — the core
//! decides *what* should happen, the host carries it out.

/// A side effect an Ex command asks the host to perform. Drained by the host
/// after feeding keys (see [`crate::session::Session::take_effects`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExEffect {
    /// Write the current buffer to its file (`:w`, `:write`, `:w!`).
    Write { force: bool },
    /// Quit the current window (`:q`, `:quit`, `:q!`).
    Quit { force: bool },
    /// Write, then quit (`:wq`, `:x`, `:wq!`).
    WriteQuit { force: bool },
    /// Close the whole editor (`:close`) — quits ctrlvim regardless of how many
    /// windows/buffers are open, unlike `:q` which closes just the current one.
    CloseApp,
    /// Open the file browser (`:Files`, `:Explore`, `:E`, bare `:e`/`:new`).
    OpenBrowser,
    /// Switch to the dashboard/start screen (`:dash`, `:Dashboard`).
    OpenDashboard,
    /// Edit/create a file by name (`:e <name>`, `:new <name>`). The host opens
    /// the buffer, creating the file on disk if it doesn't exist yet.
    Edit(String),
    /// Write the current buffer to a named file (`:w {file}`, `:saveas {file}`).
    WriteAs { path: String, force: bool },
    /// Write every modified buffer (`:wa`/`:wall`).
    WriteAll,
    /// Quit the whole editor (`:qa`/`:qall`). Refused on unsaved changes unless
    /// forced; `:cq` maps here too.
    QuitAll { force: bool },
    /// Write every modified buffer, then quit (`:wqa`/`:xa`/`:xall`).
    WriteQuitAll,
    /// Set the color theme (`:colorscheme {name}`); `None` reports the current.
    Colorscheme(Option<String>),
    /// A buffer/tab-list navigation command (`:bnext`, `:b N`, `:ls`, …).
    Buffer(BufferCmd),
    /// Run a line of Vimscript (`:let`, `:echo`, `:call`, `:if`, …). Executed by
    /// the core, which owns the interpreter.
    Vimscript(String),
    /// Run a Lua chunk (`:lua {code}`).
    Lua(String),
    /// Source a script file (`:source`/`:so`/`:luafile`) — `.lua` runs as Lua,
    /// anything else as Vimscript.
    Source(String),
    /// Show an informational / error message on the command line.
    Message(String),
    /// A quickfix action for the host: show/hide the list, jump to an entry, or
    /// run a program whose output fills it (`:make`, `:grep`, `:vimgrep`).
    Quickfix(QuickfixCmd),
    /// A tag action for the host (`Ctrl-]`, `Ctrl-T`, `:tag`).
    Tag(TagCmd),
    /// Open the interactive project-wide find & replace panel (`:Find`), seeded
    /// with the given pattern — the word under the cursor when the command was
    /// given bare. The engine owns the replace semantics
    /// ([`crate::replace::ReplacePlan`]); the panel and the files are the
    /// host's.
    OpenReplace { pattern: Option<String> },
    /// Run a raw command line through the user's configured shell and show its
    /// output (`:!{cmd}`). The host owns the shell binary and the process.
    Shell(String),
}

/// What the host should do for a tag command. The engine owns the tag table and
/// the tagstack; the host reads the tags file and opens buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagCmd {
    /// Look `name` up in the tags file and jump to its definition. The host
    /// refreshes the table first, so a tags file written mid-session is picked
    /// up without a manual reload.
    Lookup { name: String },
    /// Jump to an already-resolved position (`Ctrl-T`, `:tnext`).
    Jump { path: String, address: crate::tags::TagAddress },
    /// Return to a position popped off the tagstack.
    Return { path: String, line: usize, col: usize },
}

/// What the host should do for a quickfix command. The engine owns the list
/// itself; everything here needs the filesystem, a process, or the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickfixCmd {
    /// `:copen` / `:cwindow` — show the quickfix pane.
    Open,
    /// `:cclose` — hide it.
    Close,
    /// Open `path` and put the cursor at (0-based) `line`/`col` — what
    /// `:cnext`, `:cc`, and clicking an entry all resolve to.
    Jump { path: String, line: usize, col: usize },
    /// Walk the project and fill the list with matches (`:vimgrep /pat/ glob`).
    /// The host does the walking; [`crate::quickfix::Matcher`] decides matches.
    Grep { pattern: String, glob: Option<String> },
    /// Spawn `program` with `args` and fill the list from its output
    /// (`:make`, `:grep`). The host owns process lifetime.
    Run { program: String, args: Vec<String>, title: String },
}

/// Buffer-list navigation, shared by the `:b*` and (tab-aliased) `:tab*`
/// commands. ctrlvim's tabs *are* its buffer list, so both map here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferCmd {
    /// `:bnext` / `:tabnext`
    Next,
    /// `:bprevious` / `:tabprevious`
    Prev,
    /// `:bfirst` / `:tabfirst`
    First,
    /// `:blast` / `:tablast`
    Last,
    /// `:b N` — go to the 1-based buffer/tab.
    Goto(usize),
    /// `:bdelete [N]` / `:tabclose` — close the given (or current) buffer.
    Delete(Option<usize>),
    /// `:only` / `:tabonly` — close every buffer but the current one.
    Only,
    /// `:ls` / `:buffers` / `:tabs` — list open buffers.
    List,
}

/// How a boolean option is being changed. `:set inv{opt}` and `:set {opt}!`
/// can only be resolved against the option's *current* value, which parsing
/// has no access to — so the parser records the intent and [`SetItem`] is
/// resolved later, against live state, by the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoolOp {
    On,
    Off,
    Toggle,
}

impl BoolOp {
    /// Resolve against the option's current effective value.
    pub(crate) fn apply(self, current: bool) -> bool {
        match self {
            BoolOp::On => true,
            BoolOp::Off => false,
            BoolOp::Toggle => !current,
        }
    }
}

/// Which layer of the option model a `:set` family command writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetScope {
    /// `:set` — write the global value *and* clear any local override, so the
    /// new global actually shows through. This is Neovim's behaviour for
    /// options that have both a global and a local layer.
    Auto,
    /// `:setlocal` — write the buffer/window-local override only, leaving the
    /// global alone.
    Local,
    /// `:setglobal` — write the global only, leaving local overrides intact
    /// (so the current buffer/window may not visibly change).
    Global,
}

/// A single option change from `:set` (`:set number`, `:set ts=4`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetItem {
    Number(BoolOp),
    Relativenumber(BoolOp),
    Wrap(BoolOp),
    Expandtab(BoolOp),
    Autoindent(BoolOp),
    Ignorecase(BoolOp),
    Smartcase(BoolOp),
    Hlsearch(BoolOp),
    Splitbelow(BoolOp),
    Splitright(BoolOp),
    Foldenable(BoolOp),
    Tabstop(i64),
    Shiftwidth(i64),
    Scrolloff(i64),
    Foldmethod(ctrlvim_options::FoldMethod),
    Foldcolumn(i64),
    /// An unrecognized option name, reported as an error.
    Unknown(String),
}

/// A recognized Ex command: its canonical name, any other spellings Vim
/// users expect (`clo` for `close`), and a one-line description. This one
/// table backs both [`is_ex_command`] (so an abbreviation like `:cl` is
/// recognized as *some* real command) and [`commands`] (so every recognized
/// command — abbreviation or not — is offered in the frontend's command
/// palette). Previously these were two independently hand-maintained lists
/// and could silently drift apart: `:clist` parsed and ran but had no
/// palette entry, so typing its `:cl` abbreviation looked like it did
/// nothing. A command can no longer exist in one without the other.
#[derive(Debug, Clone, Copy)]
pub struct ExCommand {
    /// Canonical command text, e.g. `"wq"` (no leading colon). This is what
    /// the palette both displays and runs; [`aliases`](Self::aliases) are
    /// only for recognizing other spellings, never displayed.
    pub name: &'static str,
    /// Other recognized spellings of this same command, e.g. `&["clo"]` for
    /// `"close"`. Not shown in the palette label, but typing one runs the
    /// command and `is_ex_command` recognizes it.
    pub aliases: &'static [&'static str],
    /// One-line description shown next to the command.
    pub desc: &'static str,
}

impl ExCommand {
    /// Every recognized spelling of this command, canonical name first.
    fn spellings(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }
}

/// The full table of recognized Ex commands: one entry per distinct command,
/// covering everything [`crate::session::Session::execute_ex`] can dispatch.
/// [`is_ex_command`] and [`commands`] both read this, so a command can't be
/// runnable without also being visible in the palette (or vice versa).
const TABLE: &[ExCommand] = &[
    // -- file / quit ------------------------------------------------------
    ExCommand { name: "w", aliases: &["write"], desc: "write buffer to file" },
    ExCommand { name: "wq", aliases: &[], desc: "write buffer and quit" },
    ExCommand { name: "x", aliases: &["xit", "exit"], desc: "write if changed, then quit" },
    ExCommand { name: "q", aliases: &["quit"], desc: "quit window" },
    ExCommand { name: "q!", aliases: &[], desc: "quit without saving" },
    ExCommand { name: "qa", aliases: &["qall", "quita", "quitall", "cq", "cquit"], desc: "quit all windows" },
    ExCommand { name: "wa", aliases: &["wall"], desc: "write all buffers" },
    ExCommand { name: "wqa", aliases: &["wqall", "xa", "xall"], desc: "write all and quit" },
    ExCommand { name: "close", aliases: &["clo"], desc: "close ctrlvim" },
    ExCommand { name: "new", aliases: &["enew", "tabnew", "tabe", "tabedit"], desc: "create a new file (:new name), or the file browser if bare" },
    ExCommand { name: "edit", aliases: &["e"], desc: "edit a file by name (:edit name), or the file browser if bare" },
    ExCommand { name: "saveas", aliases: &["sav", "save"], desc: "write buffer to a new file (:saveas name)" },
    ExCommand { name: "update", aliases: &["up"], desc: "write buffer to file if modified" },
    ExCommand { name: "Files", aliases: &["Explore", "Ex", "E"], desc: "open the fuzzy file browser" },
    ExCommand { name: "dashboard", aliases: &["dash", "Dash", "Dashboard"], desc: "go to the dashboard" },
    ExCommand { name: "colorscheme", aliases: &["colo", "colors"], desc: "change the color theme" },
    ExCommand { name: "set", aliases: &["se", "setl", "setlocal", "setg", "setglobal"], desc: "set an option (:set number, ts=4)" },
    ExCommand { name: "undo", aliases: &["u", "un"], desc: "undo the last change" },
    ExCommand { name: "redo", aliases: &["red"], desc: "redo the last undone change" },
    ExCommand { name: "earlier", aliases: &["ea"], desc: "undo N changes at once (:earlier 3)" },
    ExCommand { name: "later", aliases: &["lat"], desc: "redo N changes at once (:later 3)" },

    // -- buffers / tabs (ctrlvim's tabs are its buffer list) --------------
    ExCommand { name: "ls", aliases: &["buffers", "tabs"], desc: "list open buffers" },
    ExCommand { name: "bnext", aliases: &["bn", "tabn", "tabnext"], desc: "go to the next buffer" },
    ExCommand { name: "bprevious", aliases: &["bp", "bN", "bNext", "tabp", "tabprevious", "tabN", "tabNext"], desc: "go to the previous buffer" },
    ExCommand { name: "bfirst", aliases: &["bf", "br", "brewind", "tabfir", "tabfirst", "tabr", "tabrewind"], desc: "go to the first buffer" },
    ExCommand { name: "blast", aliases: &["bl", "tabl", "tablast"], desc: "go to the last buffer" },
    ExCommand { name: "buffer", aliases: &["b", "bu", "buf"], desc: "go to buffer N (:buffer 2)" },
    ExCommand { name: "bdelete", aliases: &["bd", "bdel", "tabc", "tabclose"], desc: "close the current buffer" },
    ExCommand { name: "only", aliases: &["on", "tabo", "tabonly"], desc: "close all other buffers" },

    // -- range / text-processing ------------------------------------------
    ExCommand { name: "substitute", aliases: &["s", "su", "sub"], desc: "search & replace (:s/old/new/g)" },
    ExCommand { name: "global", aliases: &["g"], desc: "run a command on every matching line (:g/pat/d)" },
    ExCommand { name: "vglobal", aliases: &["v"], desc: "run a command on every non-matching line" },
    ExCommand { name: "delete", aliases: &["d", "de", "del"], desc: "delete the line(s) in range" },
    ExCommand { name: "yank", aliases: &["y", "ya"], desc: "yank the line(s) in range" },
    ExCommand { name: "move", aliases: &["m", "mo"], desc: "move the line(s) in range (:m $)" },
    ExCommand { name: "copy", aliases: &["t", "co"], desc: "copy the line(s) in range (:t $)" },
    ExCommand { name: "join", aliases: &["j"], desc: "join the line(s) in range" },
    ExCommand { name: "sort", aliases: &["sor"], desc: "sort the line(s) in range" },
    ExCommand { name: "normal", aliases: &["norm"], desc: "run normal-mode keys over the range (:%norm A;)" },
    ExCommand { name: "put", aliases: &["pu"], desc: "paste the unnamed register below the line" },
    ExCommand { name: "nohlsearch", aliases: &["noh", "nohl"], desc: "clear search highlighting" },

    // -- tags ---------------------------------------------------------------
    ExCommand { name: "tag", aliases: &["ta"], desc: "jump to a tag's definition (:tag name)" },
    ExCommand { name: "tnext", aliases: &["tn"], desc: "jump to the next match for the last tag" },
    ExCommand { name: "tprevious", aliases: &["tp", "tprev", "tN"], desc: "jump to the previous match for the last tag" },
    ExCommand { name: "tfirst", aliases: &["tf", "tr", "trewind"], desc: "jump to the first match for the last tag" },
    ExCommand { name: "tlast", aliases: &["tl"], desc: "jump to the last match for the last tag" },
    ExCommand { name: "tselect", aliases: &["ts", "tj", "tjump"], desc: "list matches for a tag and jump" },
    ExCommand { name: "tags", aliases: &[], desc: "show the tag stack" },
    ExCommand { name: "pop", aliases: &["po"], desc: "jump back up the tag stack" },

    // -- folds --------------------------------------------------------------
    ExCommand { name: "fold", aliases: &["fo"], desc: "create a fold over the range" },
    ExCommand { name: "foldopen", aliases: &["foldo"], desc: "open the fold(s) in range" },
    ExCommand { name: "foldclose", aliases: &["foldc"], desc: "close the fold(s) in range" },

    // -- quickfix -------------------------------------------------------
    ExCommand { name: "copen", aliases: &["cope", "cw", "cwindow"], desc: "open the quickfix list" },
    ExCommand { name: "cclose", aliases: &["ccl"], desc: "close the quickfix list" },
    ExCommand { name: "cnext", aliases: &["cn"], desc: "jump to the next quickfix entry" },
    ExCommand { name: "cprevious", aliases: &["cprev", "cp", "cN"], desc: "jump to the previous quickfix entry" },
    ExCommand { name: "cfirst", aliases: &["cfir", "crewind", "cr"], desc: "jump to the first quickfix entry" },
    ExCommand { name: "clast", aliases: &["cla"], desc: "jump to the last quickfix entry" },
    ExCommand { name: "cc", aliases: &[], desc: "jump to the current (or Nth) quickfix entry" },
    ExCommand { name: "clist", aliases: &["cl"], desc: "show how many quickfix entries there are" },
    ExCommand { name: "make", aliases: &[], desc: "build the project into the quickfix list" },
    ExCommand { name: "grep", aliases: &["gr"], desc: "run grep into the quickfix list" },
    ExCommand { name: "vimgrep", aliases: &["vim", "vimg"], desc: "search files into the quickfix list (:vimgrep /pat/)" },

    // -- find & replace panel ----------------------------------------------
    ExCommand { name: "Find", aliases: &[], desc: "find & replace across the project (:Find [pattern])" },
    ExCommand { name: "Replace", aliases: &["Repl"], desc: "find & replace across the project" },

    // -- scripting ----------------------------------------------------------
    ExCommand { name: "map", aliases: &["nmap", "nnoremap", "noremap", "nore"], desc: "define a normal-mode mapping (:map lhs rhs)" },
    ExCommand { name: "command", aliases: &["com", "comm"], desc: "define a user command (:command Name expansion)" },
    ExCommand { name: "comclear", aliases: &[], desc: "remove all user-defined commands" },
    ExCommand { name: "let", aliases: &[], desc: "set a script variable (:let g:x = 1)" },
    ExCommand { name: "echo", aliases: &["echom", "echomsg"], desc: "show a message (:echo expr)" },
    ExCommand { name: "call", aliases: &[], desc: "call a function (:call Foo())" },
    ExCommand { name: "function", aliases: &["func", "fu"], desc: "define a script function" },
    ExCommand { name: "source", aliases: &["so", "luafile"], desc: "run a script file (:source file)" },
    ExCommand { name: "lua", aliases: &[], desc: "run a Lua chunk (:lua code)" },
    ExCommand { name: "if", aliases: &[], desc: "vimscript: start a conditional block" },
    ExCommand { name: "elseif", aliases: &[], desc: "vimscript: conditional else-if branch" },
    ExCommand { name: "else", aliases: &[], desc: "vimscript: conditional else branch" },
    ExCommand { name: "endif", aliases: &[], desc: "vimscript: end a conditional block" },
    ExCommand { name: "for", aliases: &[], desc: "vimscript: start a for loop" },
    ExCommand { name: "endfor", aliases: &[], desc: "vimscript: end a for loop" },
    ExCommand { name: "while", aliases: &[], desc: "vimscript: start a while loop" },
    ExCommand { name: "endwhile", aliases: &[], desc: "vimscript: end a while loop" },
    ExCommand { name: "endfunction", aliases: &["endfunc"], desc: "vimscript: end a function definition" },
    ExCommand { name: "return", aliases: &[], desc: "vimscript: return from a function" },
    ExCommand { name: "unlet", aliases: &[], desc: "vimscript: delete a variable" },
    ExCommand { name: "const", aliases: &[], desc: "vimscript: declare a constant" },
    ExCommand { name: "eval", aliases: &[], desc: "vimscript: evaluate an expression" },
    ExCommand { name: "execute", aliases: &["exe"], desc: "vimscript: execute a built command string" },
    ExCommand { name: "echoerr", aliases: &[], desc: "vimscript: show an error message" },
];

/// Whether `cmd` looks like a real Ex command (so the frontend command line
/// runs it verbatim instead of treating the text as a fuzzy palette query). A
/// leading range/line-number counts, as does any recognized command name.
pub fn is_ex_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    let (spec, rest) = crate::range::parse_range(cmd);
    if rest.is_empty() {
        return !matches!(spec, crate::range::RangeSpec::None);
    }
    // Leading command token: a glued single-char command, or an alphabetic run.
    if rest.starts_with(['>', '<', '=', '&', '!']) {
        return true;
    }
    let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    let name = &rest[..end.max(1)];
    TABLE.iter().any(|cmd| cmd.spellings().any(|s| s == name))
}

/// The catalog of Ex commands offered in the command palette, in display order.
pub fn commands() -> &'static [ExCommand] {
    TABLE
}

/// The parsed outcome of a `:` command line.
pub(crate) enum ExParsed {
    /// Move the cursor to a 1-based line (`:N`).
    GotoLine(usize),
    /// Move the cursor to the last line (`:$`).
    GotoLast,
    /// Undo `n` changes (`:undo`, `:earlier N`).
    Undo(usize),
    /// Redo `n` changes (`:redo`, `:later N`).
    Redo(usize),
    /// Apply option changes (`:set …`/`:setlocal …`/`:setglobal …`).
    Set { scope: SetScope, items: Vec<SetItem> },
    /// Define a mapping (`:map`/`:nnoremap`/`:imap`/`:vmap` …).
    Map { mode: crate::keymap::MapMode, lhs: String, rhs: String },
    /// Remove a mapping (`:unmap`/`:iunmap`/`:vunmap`).
    Unmap { mode: crate::keymap::MapMode, lhs: String },
    /// Define a user command (`:command Name expansion`).
    DefineCommand { name: String, repl: String },
    /// Remove all user-defined commands (`:comclear`).
    ClearUserCommands,
    /// A side effect for the host to perform.
    Effect(ExEffect),
    /// Empty command line — do nothing.
    Nop,
}

/// Parse a leading count argument (`:earlier 3`), defaulting when absent.
fn count_arg(arg: &str, default: usize) -> usize {
    arg.trim().parse().unwrap_or(default)
}

/// Split `arg` into its first whitespace-delimited word and the trimmed rest.
fn split_first_word(arg: &str) -> (String, String) {
    let arg = arg.trim();
    match arg.split_once(char::is_whitespace) {
        Some((a, b)) => (a.to_string(), b.trim().to_string()),
        None => (arg.to_string(), String::new()),
    }
}

/// Parse a `:set` argument list (`number nowrap ts=4`) into option changes.
fn parse_set(arg: &str) -> Vec<SetItem> {
    arg.split_whitespace()
        .map(|tok| {
            // `name=value` numeric options.
            if let Some((name, value)) = tok.split_once('=') {
                let n: i64 = value.parse().unwrap_or(0);
                return match name {
                    "tabstop" | "ts" => SetItem::Tabstop(n),
                    "shiftwidth" | "sw" => SetItem::Shiftwidth(n),
                    "scrolloff" | "so" => SetItem::Scrolloff(n),
                    "foldcolumn" | "fdc" => SetItem::Foldcolumn(n),
                    // `'foldmethod'` takes a name, not a number.
                    "foldmethod" | "fdm" => match ctrlvim_options::FoldMethod::parse(value) {
                        Some(m) => SetItem::Foldmethod(m),
                        None => SetItem::Unknown(format!("{name}={value}")),
                    },
                    _ => SetItem::Unknown(name.to_string()),
                };
            }
            // Boolean options: `name`, `noname`, `invname`/`name!`. The `inv`
            // and `!` forms are genuine toggles — they need the current value,
            // so they resolve later against live option state.
            let (base, op) = if let Some(rest) = tok.strip_prefix("inv") {
                (rest, BoolOp::Toggle)
            } else if let Some(rest) = tok.strip_suffix('!') {
                (rest, BoolOp::Toggle)
            } else if let Some(rest) = tok.strip_prefix("no") {
                (rest, BoolOp::Off)
            } else {
                (tok, BoolOp::On)
            };
            match base {
                "number" | "nu" => SetItem::Number(op),
                "relativenumber" | "rnu" => SetItem::Relativenumber(op),
                "wrap" => SetItem::Wrap(op),
                "expandtab" | "et" => SetItem::Expandtab(op),
                "autoindent" | "ai" => SetItem::Autoindent(op),
                "ignorecase" | "ic" => SetItem::Ignorecase(op),
                "smartcase" | "scs" => SetItem::Smartcase(op),
                "hlsearch" | "hls" => SetItem::Hlsearch(op),
                "splitbelow" | "sb" => SetItem::Splitbelow(op),
                "splitright" | "spr" => SetItem::Splitright(op),
                "foldenable" | "fen" => SetItem::Foldenable(op),
                other => SetItem::Unknown(other.to_string()),
            }
        })
        .collect()
}

/// Parse a `:` command line (without the leading `:`).
pub(crate) fn parse_ex(cmd: &str) -> ExParsed {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return ExParsed::Nop;
    }
    if let Ok(n) = cmd.parse::<usize>() {
        return ExParsed::GotoLine(n);
    }
    if cmd == "$" {
        return ExParsed::GotoLast;
    }

    // Split into the command word and its argument (the rest of the line).
    // A trailing `!` on the word is the "force" bang (`:w!`, `:q!`).
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let word = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim).unwrap_or("");
    let (name, force) = match word.strip_suffix('!') {
        Some(n) => (n, true),
        None => (word, false),
    };

    // Engine-side commands (executed in-core, no host effect).
    match name {
        "u" | "un" | "undo" => return ExParsed::Undo(1),
        "red" | "redo" => return ExParsed::Redo(1),
        "ea" | "earlier" => return ExParsed::Undo(count_arg(arg, 1)),
        "lat" | "later" => return ExParsed::Redo(count_arg(arg, 1)),
        "se" | "set" => return ExParsed::Set { scope: SetScope::Auto, items: parse_set(arg) },
        "setl" | "setlocal" => {
            return ExParsed::Set { scope: SetScope::Local, items: parse_set(arg) }
        }
        "setg" | "setglobal" => {
            return ExParsed::Set { scope: SetScope::Global, items: parse_set(arg) }
        }
        // Mappings. `:map`/`:noremap` without a mode letter apply to normal
        // mode, matching how a bare `:map` behaves here.
        "map" | "nmap" | "nnoremap" | "noremap" | "nore" | "imap" | "inoremap" | "vmap"
        | "vnoremap" | "xmap" | "xnoremap" => {
            let mode = match name {
                "imap" | "inoremap" => crate::keymap::MapMode::Insert,
                "vmap" | "vnoremap" | "xmap" | "xnoremap" => crate::keymap::MapMode::Visual,
                _ => crate::keymap::MapMode::Normal,
            };
            let (lhs, rhs) = split_first_word(arg);
            if !lhs.is_empty() && !rhs.is_empty() {
                return ExParsed::Map { mode, lhs, rhs };
            }
            return ExParsed::Nop;
        }
        "unmap" | "nunmap" | "iunmap" | "vunmap" | "xunmap" => {
            let mode = match name {
                "iunmap" => crate::keymap::MapMode::Insert,
                "vunmap" | "xunmap" => crate::keymap::MapMode::Visual,
                _ => crate::keymap::MapMode::Normal,
            };
            let lhs = arg.trim().to_string();
            if lhs.is_empty() {
                return ExParsed::Nop;
            }
            return ExParsed::Unmap { mode, lhs };
        }
        "command" | "com" | "comm" => {
            // Skip attribute flags like `-nargs=1` before the command name.
            let rest = arg
                .split_whitespace()
                .skip_while(|w| w.starts_with('-'))
                .collect::<Vec<_>>()
                .join(" ");
            let (cmd_name, repl) = split_first_word(&rest);
            if !cmd_name.is_empty() && !repl.is_empty() {
                return ExParsed::DefineCommand { name: cmd_name, repl };
            }
            return ExParsed::Nop;
        }
        "comclear" => return ExParsed::ClearUserCommands,
        _ => {}
    }

    let effect = match name {
        "w" | "write" => {
            if arg.is_empty() {
                ExEffect::Write { force }
            } else {
                ExEffect::WriteAs { path: arg.to_string(), force }
            }
        }
        "sav" | "save" | "saveas" => ExEffect::WriteAs { path: arg.to_string(), force },
        "up" | "update" => ExEffect::Write { force },
        "q" | "quit" => ExEffect::Quit { force },
        "wq" | "x" | "xit" | "exit" => ExEffect::WriteQuit { force },
        "qa" | "qall" | "quita" | "quitall" | "cq" | "cquit" => ExEffect::QuitAll { force },
        "wa" | "wall" => ExEffect::WriteAll,
        "wqa" | "wqall" | "xa" | "xall" => ExEffect::WriteQuitAll,
        "close" | "clo" => ExEffect::CloseApp,
        "colo" | "colors" | "colorscheme" => {
            ExEffect::Colorscheme((!arg.is_empty()).then(|| arg.to_string()))
        }
        // Buffer / tab list navigation (tabs are aliases for the buffer list).
        "ls" | "buffers" | "tabs" => ExEffect::Buffer(BufferCmd::List),
        "bn" | "bnext" | "tabn" | "tabnext" => ExEffect::Buffer(BufferCmd::Next),
        "bp" | "bprevious" | "bN" | "bNext" | "tabp" | "tabprevious" | "tabN" | "tabNext" => {
            ExEffect::Buffer(BufferCmd::Prev)
        }
        "bf" | "bfirst" | "br" | "brewind" | "tabfir" | "tabfirst" | "tabr" | "tabrewind" => {
            ExEffect::Buffer(BufferCmd::First)
        }
        "bl" | "blast" | "tabl" | "tablast" => ExEffect::Buffer(BufferCmd::Last),
        "b" | "bu" | "buf" | "buffer" => match arg.parse::<usize>() {
            Ok(n) => ExEffect::Buffer(BufferCmd::Goto(n)),
            Err(_) => ExEffect::Buffer(BufferCmd::List),
        },
        "bd" | "bdel" | "bdelete" | "tabc" | "tabclose" => {
            ExEffect::Buffer(BufferCmd::Delete(arg.parse::<usize>().ok()))
        }
        "on" | "only" | "tabo" | "tabonly" => ExEffect::Buffer(BufferCmd::Only),
        // `:e`/`:edit`/`:new`/`:tabnew` with a name creates/opens that file;
        // bare, they fall back to the file browser so you can pick/name one.
        "e" | "edit" | "new" | "enew" | "tabnew" | "tabe" | "tabedit" => {
            if arg.is_empty() {
                ExEffect::OpenBrowser
            } else {
                ExEffect::Edit(arg.to_string())
            }
        }
        "Files" | "Explore" | "Ex" | "E" => ExEffect::OpenBrowser,
        // Bare `:Find` seeds itself from the word under the cursor, which only
        // the session knows — it fills the `None` in before the host sees it.
        "Find" | "Replace" | "Repl" => {
            ExEffect::OpenReplace { pattern: (!arg.is_empty()).then(|| arg.to_string()) }
        }
        "dash" | "dashboard" | "Dash" | "Dashboard" => ExEffect::OpenDashboard,
        // Scripting: run in the core (which owns the interpreters).
        "lua" if !arg.is_empty() => ExEffect::Lua(arg.to_string()),
        "luafile" | "source" | "so" => ExEffect::Source(arg.to_string()),
        "let" | "echo" | "echom" | "echomsg" | "echoerr" | "call" | "eval" | "execute"
        | "exe" | "if" | "elseif" | "else" | "endif" | "for" | "endfor" | "while"
        | "endwhile" | "function" | "func" | "fu" | "endfunction" | "endfunc"
        | "return" | "unlet" | "const" => ExEffect::Vimscript(cmd.to_string()),
        _ => ExEffect::Message(format!("E492: Not an editor command: {cmd}")),
    };
    ExParsed::Effect(effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(cmd: &str) -> ExEffect {
        match parse_ex(cmd) {
            ExParsed::Effect(e) => e,
            _ => panic!("expected an effect for {cmd:?}"),
        }
    }

    #[test]
    fn write_quit_variants() {
        assert_eq!(effect("w"), ExEffect::Write { force: false });
        assert_eq!(effect("w!"), ExEffect::Write { force: true });
        assert_eq!(effect("q"), ExEffect::Quit { force: false });
        assert_eq!(effect("q!"), ExEffect::Quit { force: true });
        assert_eq!(effect("wq"), ExEffect::WriteQuit { force: false });
        assert_eq!(effect("x"), ExEffect::WriteQuit { force: false });
    }

    #[test]
    fn browser_and_unknown() {
        assert_eq!(effect("Files"), ExEffect::OpenBrowser);
        assert!(matches!(effect("nope"), ExEffect::Message(_)));
    }

    #[test]
    fn close_quits_the_app() {
        assert_eq!(effect("close"), ExEffect::CloseApp);
        assert_eq!(effect("clo"), ExEffect::CloseApp);
    }

    #[test]
    fn edit_with_and_without_a_name() {
        assert_eq!(effect("e notes.txt"), ExEffect::Edit("notes.txt".into()));
        assert_eq!(effect("new src/lib.rs"), ExEffect::Edit("src/lib.rs".into()));
        assert_eq!(effect("edit  spaced.md"), ExEffect::Edit("spaced.md".into()));
        // Bare forms fall back to the file browser.
        assert_eq!(effect("e"), ExEffect::OpenBrowser);
        assert_eq!(effect("new"), ExEffect::OpenBrowser);
    }

    #[test]
    fn line_goto() {
        assert!(matches!(parse_ex("42"), ExParsed::GotoLine(42)));
        assert!(matches!(parse_ex("$"), ExParsed::GotoLast));
        assert!(matches!(parse_ex(""), ExParsed::Nop));
    }

    #[test]
    fn command_catalog_is_runnable() {
        // Every advertised palette command must be a recognized Ex command, so
        // selecting or typing it always runs (rather than a fuzzy fallback).
        for cmd in commands() {
            assert!(is_ex_command(cmd.name), "catalog command {:?} is not recognized", cmd.name);
        }
    }

    #[test]
    fn every_spelling_is_globally_unique() {
        // Two commands can't claim the same abbreviation -- that's exactly
        // the bug that made `:cl` silently run `:clist` instead of the
        // `:close` the palette was highlighting.
        let mut seen = std::collections::HashMap::new();
        for cmd in TABLE {
            for spelling in cmd.spellings() {
                if let Some(owner) = seen.insert(spelling, cmd.name) {
                    assert_eq!(
                        owner, cmd.name,
                        "{spelling:?} is claimed by both {owner:?} and {:?}",
                        cmd.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_command_has_a_description() {
        for cmd in TABLE {
            assert!(!cmd.desc.is_empty(), "{:?} has no description", cmd.name);
        }
    }

    #[test]
    fn comclear_removes_user_commands() {
        assert!(matches!(parse_ex("comclear"), ExParsed::ClearUserCommands));
    }

    #[test]
    fn norm_is_an_alias_of_normal() {
        assert!(is_ex_command("norm"));
        assert!(is_ex_command("norm A;"));
    }

    #[test]
    fn bang_is_a_recognized_ex_command() {
        // So the palette runs `:!{cmd}` verbatim rather than falling through to
        // a fuzzy-matched entry.
        assert!(is_ex_command("!echo hi"));
        assert!(is_ex_command("!"));
    }

    #[test]
    fn buffer_and_tab_commands() {
        use BufferCmd::*;
        assert_eq!(effect("bnext"), ExEffect::Buffer(Next));
        assert_eq!(effect("tabnext"), ExEffect::Buffer(Next)); // tabs alias buffers
        assert_eq!(effect("bprevious"), ExEffect::Buffer(Prev));
        assert_eq!(effect("bfirst"), ExEffect::Buffer(First));
        assert_eq!(effect("blast"), ExEffect::Buffer(Last));
        assert_eq!(effect("b 3"), ExEffect::Buffer(Goto(3)));
        assert_eq!(effect("bdelete"), ExEffect::Buffer(Delete(None)));
        assert_eq!(effect("bd 2"), ExEffect::Buffer(Delete(Some(2))));
        assert_eq!(effect("only"), ExEffect::Buffer(Only));
        assert_eq!(effect("ls"), ExEffect::Buffer(List));
    }

    #[test]
    fn write_quit_all_and_colorscheme() {
        assert_eq!(effect("qa"), ExEffect::QuitAll { force: false });
        assert_eq!(effect("qa!"), ExEffect::QuitAll { force: true });
        assert_eq!(effect("wa"), ExEffect::WriteAll);
        assert_eq!(effect("wqa"), ExEffect::WriteQuitAll);
        assert_eq!(effect("xa"), ExEffect::WriteQuitAll);
        assert_eq!(effect("colorscheme Gruvbox"), ExEffect::Colorscheme(Some("Gruvbox".into())));
        assert_eq!(effect("colo"), ExEffect::Colorscheme(None));
        assert_eq!(effect("w notes.txt"), ExEffect::WriteAs { path: "notes.txt".into(), force: false });
        assert_eq!(effect("saveas out.rs"), ExEffect::WriteAs { path: "out.rs".into(), force: false });
    }

    #[test]
    fn engine_side_commands() {
        assert!(matches!(parse_ex("undo"), ExParsed::Undo(1)));
        assert!(matches!(parse_ex("redo"), ExParsed::Redo(1)));
        assert!(matches!(parse_ex("earlier 3"), ExParsed::Undo(3)));
        assert!(matches!(parse_ex("later 2"), ExParsed::Redo(2)));
        match parse_ex("set number ts=4 nowrap") {
            ExParsed::Set { scope, items } => {
                assert_eq!(scope, SetScope::Auto);
                assert_eq!(items[0], SetItem::Number(BoolOp::On));
                assert_eq!(items[1], SetItem::Tabstop(4));
                assert_eq!(items[2], SetItem::Wrap(BoolOp::Off));
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn map_commands_select_their_mode() {
        use crate::keymap::MapMode;
        for (cmd, want) in [
            ("nnoremap gx :dash<CR>", MapMode::Normal),
            ("map gx :dash<CR>", MapMode::Normal),
            ("inoremap jk <Esc>", MapMode::Insert),
            ("vnoremap gx :dash<CR>", MapMode::Visual),
            ("xnoremap gx :dash<CR>", MapMode::Visual),
        ] {
            match parse_ex(cmd) {
                ExParsed::Map { mode, .. } => assert_eq!(mode, want, "{cmd}"),
                _ => panic!("expected Map for {cmd}"),
            }
        }
        match parse_ex("iunmap jk") {
            ExParsed::Unmap { mode, lhs } => {
                assert_eq!(mode, MapMode::Insert);
                assert_eq!(lhs, "jk");
            }
            _ => panic!("expected Unmap"),
        }
    }

    #[test]
    fn set_scope_and_toggle_forms_parse() {
        // `inv{opt}` and `{opt}!` are toggles, not "set to on".
        for form in ["set invnumber", "set number!"] {
            match parse_ex(form) {
                ExParsed::Set { items, .. } => {
                    assert_eq!(items[0], SetItem::Number(BoolOp::Toggle), "{form}")
                }
                _ => panic!("expected Set for {form}"),
            }
        }
        // The three spellings select three different write targets.
        for (form, want) in [
            ("set nu", SetScope::Auto),
            ("setlocal nu", SetScope::Local),
            ("setglobal nu", SetScope::Global),
        ] {
            match parse_ex(form) {
                ExParsed::Set { scope, .. } => assert_eq!(scope, want, "{form}"),
                _ => panic!("expected Set for {form}"),
            }
        }
    }
}
