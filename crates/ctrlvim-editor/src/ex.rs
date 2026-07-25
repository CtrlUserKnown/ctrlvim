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

/// A single option change from `:set` (`:set number`, `:set ts=4`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetItem {
    Number(bool),
    Wrap(bool),
    Expandtab(bool),
    Tabstop(i64),
    Shiftwidth(i64),
    Scrolloff(i64),
    Foldenable(bool),
    Foldmethod(ctrlvim_options::FoldMethod),
    Foldcolumn(i64),
    /// An unrecognized option name, reported as an error.
    Unknown(String),
}

/// A user-facing Ex command for the frontend's command palette: the text to
/// type (without the leading `:`) and a short description. The catalog lives in
/// the engine so the set of commands has one source of truth; the frontend only
/// *presents* them, and running one still goes back through [`parse_ex`].
#[derive(Debug, Clone, Copy)]
pub struct ExCommand {
    /// Command text as typed, e.g. `"wq"` (no leading colon).
    pub name: &'static str,
    /// One-line description shown next to the command.
    pub desc: &'static str,
}

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
    if rest.starts_with(['>', '<', '=', '&']) {
        return true;
    }
    let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    let name = &rest[..end.max(1)];
    const KNOWN: &[&str] = &[
        // file / quit / buffers / tabs / options / history / theme (also catalog)
        "w", "write", "wq", "x", "xit", "exit", "q", "quit", "qa", "qall", "wa", "wall",
        "wqa", "xa", "xall", "cq", "cquit", "close", "clo", "e", "edit", "new", "enew",
        "sav", "save", "saveas", "up", "update", "Files", "Explore", "Ex", "E",
        "dash", "dashboard", "Dash", "Dashboard", "ls",
        "buffers", "b", "bu", "buf", "buffer", "bn", "bnext", "bp", "bprevious", "bN",
        "bNext", "bf", "bfirst", "br", "brewind", "bl", "blast", "bd", "bdel", "bdelete",
        "on", "only", "tabn", "tabnext", "tabp", "tabprevious", "tabc", "tabclose",
        "tabo", "tabonly", "tabnew", "tabe", "tabedit", "tabs", "tabfir", "tabfirst",
        "tabl", "tablast", "colo", "colors", "colorscheme", "set", "se", "setl",
        "setlocal", "setg", "setglobal", "u", "un", "undo", "red", "redo", "ea",
        "earlier", "lat", "later",
        // range / text-processing
        "s", "su", "sub", "substitute", "g", "global", "v", "vglobal", "d", "de", "del",
        "delete", "y", "ya", "yank", "m", "mo", "move", "t", "co", "copy", "j", "join",
        "sort", "sor", "normal", "norm", "pu", "put", "noh", "nohl", "nohlsearch",
        // tags
        "ta", "tag", "tn", "tnext", "tp", "tprevious", "tprev", "tN", "tf", "tfirst",
        "tr", "trewind", "tl", "tlast", "ts", "tselect", "tj", "tjump", "tags", "po", "pop",
        // folds
        "fo", "fold", "foldo", "foldopen", "foldc", "foldclose",
        // quickfix
        "copen", "cope", "cw", "cwindow", "cclose", "ccl", "cnext", "cn", "cprevious",
        "cprev", "cp", "cN", "cfirst", "cfir", "crewind", "cr", "clast", "cla", "cc",
        "clist", "cl", "make", "grep", "gr", "vimgrep", "vim", "vimg",
        // scripting
        "map", "nmap", "nnoremap", "noremap", "vmap", "vnoremap", "imap", "inoremap",
        "unmap", "let", "echo", "echom", "echomsg", "call", "if", "for", "while",
        "function", "func", "fu", "source", "so", "lua", "luafile", "command", "comclear",
    ];
    KNOWN.contains(&name)
}

/// The catalog of Ex commands offered in the command palette, in display order.
pub fn commands() -> &'static [ExCommand] {
    &[
        ExCommand { name: "w", desc: "write buffer to file" },
        ExCommand { name: "wq", desc: "write buffer and quit" },
        ExCommand { name: "x", desc: "write if changed, then quit" },
        ExCommand { name: "q", desc: "quit window" },
        ExCommand { name: "q!", desc: "quit without saving" },
        ExCommand { name: "qa", desc: "quit all windows" },
        ExCommand { name: "wa", desc: "write all buffers" },
        ExCommand { name: "wqa", desc: "write all and quit" },
        ExCommand { name: "close", desc: "close ctrlvim" },
        ExCommand { name: "new", desc: "create a new file (:new name)" },
        ExCommand { name: "dashboard", desc: "go to the dashboard" },
        ExCommand { name: "ls", desc: "list open buffers" },
        ExCommand { name: "bnext", desc: "go to the next buffer" },
        ExCommand { name: "bprevious", desc: "go to the previous buffer" },
        ExCommand { name: "bdelete", desc: "close the current buffer" },
        ExCommand { name: "only", desc: "close all other buffers" },
        ExCommand { name: "undo", desc: "undo the last change" },
        ExCommand { name: "redo", desc: "redo the last undone change" },
        ExCommand { name: "set", desc: "set an option (:set number, ts=4)" },
        ExCommand { name: "colorscheme", desc: "change the color theme" },
        ExCommand { name: "substitute", desc: "search & replace (:s/old/new/g)" },
        ExCommand { name: "nohlsearch", desc: "clear search highlighting" },
        ExCommand { name: "source", desc: "run a script file (:source file)" },
        ExCommand { name: "Files", desc: "open the fuzzy file browser" },
        ExCommand { name: "vimgrep", desc: "search files into the quickfix list (:vimgrep /pat/)" },
        ExCommand { name: "grep", desc: "run grep into the quickfix list" },
        ExCommand { name: "make", desc: "build the project into the quickfix list" },
        ExCommand { name: "copen", desc: "open the quickfix list" },
        ExCommand { name: "cclose", desc: "close the quickfix list" },
        ExCommand { name: "cnext", desc: "jump to the next quickfix entry" },
        ExCommand { name: "cprevious", desc: "jump to the previous quickfix entry" },
    ]
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
    /// Apply option changes (`:set …`).
    Set(Vec<SetItem>),
    /// Define a Normal-mode mapping (`:map`/`:nnoremap` …).
    Map { lhs: String, rhs: String },
    /// Define a user command (`:command Name expansion`).
    DefineCommand { name: String, repl: String },
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
            // Boolean options: `name`, `noname`, `invname`/`name!`.
            let (base, value) = if let Some(rest) = tok.strip_prefix("no") {
                (rest, false)
            } else if let Some(rest) = tok.strip_prefix("inv") {
                (rest, true) // toggle; treated as "on" (no query state here)
            } else if let Some(rest) = tok.strip_suffix('!') {
                (rest, true)
            } else {
                (tok, true)
            };
            match base {
                "number" | "nu" => SetItem::Number(value),
                "wrap" => SetItem::Wrap(value),
                "expandtab" | "et" => SetItem::Expandtab(value),
                "foldenable" | "fen" => SetItem::Foldenable(value),
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
        "se" | "set" | "setl" | "setlocal" | "setg" | "setglobal" => {
            return ExParsed::Set(parse_set(arg));
        }
        // Normal-mode mappings (variants collapse to normal mode for now).
        "map" | "nmap" | "nnoremap" | "noremap" | "nore" => {
            let (lhs, rhs) = split_first_word(arg);
            if !lhs.is_empty() && !rhs.is_empty() {
                return ExParsed::Map { lhs, rhs };
            }
            return ExParsed::Nop;
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
            ExParsed::Set(items) => {
                assert_eq!(items[0], SetItem::Number(true));
                assert_eq!(items[1], SetItem::Tabstop(4));
                assert_eq!(items[2], SetItem::Wrap(false));
            }
            _ => panic!("expected Set"),
        }
    }
}
