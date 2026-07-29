//! Application state and transitions.
//!
//! This is the Rust port of the design prototype's component: the same state
//! fields and the same set of discrete mutations, driven by [`Action`]s that
//! both the keymap ([`crate::input`]) and mouse hit-testing ([`crate::ui`])
//! dispatch through [`App::dispatch`].
//!
//! The app owns a real [`ctrlvim_core::Ctrlvim`] engine. A **File buffer is a
//! live editor window**: keystrokes are fed to [`ctrlvim_core::Session`] via
//! [`App::feed_engine`], and the view renders the engine's real buffer text,
//! cursor, and mode. Because the facade currently exposes a single working
//! buffer, the frontend keeps per-tab text: switching buffers snapshots the
//! outgoing file's text and loads the incoming one (see [`App::set_active`]).
//!
//! The dashboard's recent-files/git/plugin/LSP data is drawn from the real
//! project ([`crate::data::Project::load`]), not mock data.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ctrlvim_ai::{Completer, Request as AiRequest, Status as AiStatus};
use ctrlvim_core::syntax::{self, Filetype};
use ctrlvim_core::{
    grep_text, AiCmd, BufferCmd, ContextWindow, Ctrlvim, Event, EventLoop, ExEffect, HlSpan, Jobs,
    Key, LineBuffer, MapMode, Matcher, OutputParser, PinCmd, QfItem, QfKind, QuickfixCmd, Selection,
    Suggestion, TagAddress, TagCmd, TimerService,
};

use crate::config::{expand_tilde, Config, PluginEntry};
use crate::data::{list_dir, FinderEntry};
use crate::data::Project;
use crate::replace::{by_file, Field, ReplacePanel, MAX_HITS};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DashboardSection {
    Workspace,
    Settings,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelId {
    Git,
}

/// What kind of screen a buffer/tab shows.
#[derive(Clone, PartialEq, Eq)]
pub enum BufferKind {
    Dashboard,
    File,
    Plugins,
}

pub struct Buffer {
    pub label: String,
    pub kind: BufferKind,
    /// Absolute path on disk for a File buffer (used by `:w` and to dedup
    /// re-opens). `None` for the dashboard / plugin manager.
    pub path: Option<PathBuf>,
    /// Cached text for a File buffer, so edits survive switching away and back
    /// while the engine facade only holds one working buffer. Empty for
    /// non-file buffers.
    pub text: Vec<String>,
    /// When this is a markdown File buffer, whether live rendering is on. Only
    /// meaningful for markdown files; auto-enabled when such a file is opened.
    pub render_md: bool,
    /// Unsaved-changes flag, persisted per buffer across the engine's
    /// single-buffer facade (the engine owns the live value while active).
    pub modified: bool,
}

impl Buffer {
    fn dashboard() -> Self {
        Buffer { label: "Dashboard".into(), kind: BufferKind::Dashboard, path: None, text: Vec::new(), render_md: false, modified: false }
    }
    pub fn closable(&self) -> bool {
        !matches!(self.kind, BufferKind::Dashboard)
    }
    /// True when this buffer's file is markdown (by extension).
    pub fn is_markdown(&self) -> bool {
        matches!(self.kind, BufferKind::File) && is_markdown_name(&self.label)
    }
}

/// The full-screen fuzzy file browser (telescope `file_browser` style): browse
/// a directory, fuzzy-filter its entries, drill into subdirectories, open files.
pub struct Finder {
    /// Directory currently being browsed.
    pub dir: PathBuf,
    /// Every entry in `dir` (plus a trailing `../`), unfiltered.
    pub entries: Vec<FinderEntry>,
    /// Live search text typed into the File Browser box.
    pub query: String,
    /// Selection index into the *filtered* view.
    pub selected: usize,
}

/// A command typed into the file browser's prompt, entered by prefixing the
/// query with `:` (e.g. `:c foo.txt`, `:d`, `:dir src`). This mirrors the
/// telescope/oil habit of driving file operations from the browser itself.
pub enum FinderCommand {
    /// `:c` / `:create <name>` — make (and open) a new file here.
    Create(Option<String>),
    /// `:d` / `:delete [name]` — delete `name`, or the highlighted entry when
    /// no name is given.
    Delete(Option<String>),
    /// `:dir` / `:create-directory <name>` — make a new directory here.
    Mkdir(Option<String>),
}

impl FinderCommand {
    /// One-line description of what pressing Enter will do, for the prompt hint.
    pub fn describe(&self) -> String {
        match self {
            FinderCommand::Create(None) => "type a file name".into(),
            FinderCommand::Create(Some(n)) => format!("create file “{}”", n.trim()),
            FinderCommand::Mkdir(None) => "type a directory name".into(),
            FinderCommand::Mkdir(Some(n)) => format!("create directory “{}/”", n.trim()),
            FinderCommand::Delete(Some(n)) => format!("delete “{}”", n.trim()),
            FinderCommand::Delete(None) => "delete highlighted entry".into(),
        }
    }
}

/// Match a `/`-separated project path against a shell-style glob.
///
/// Supports the forms `:vimgrep` is actually given: `*` (any run within one
/// path segment), `**` (any run across segments), and `?`. A pattern with no
/// `/` matches against the file name alone, so `:vimgrep /x/ *.rs` searches
/// every Rust file rather than only those at the root.
pub fn glob_match(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return true;
    }
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return glob_segment(name, pattern);
    }
    glob_segment(path, pattern)
}

/// Backtracking wildcard match over chars. `**` crosses `/`, a single `*`
/// doesn't.
fn glob_segment(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    fn go(t: &[char], ti: usize, p: &[char], pi: usize) -> bool {
        if pi == p.len() {
            return ti == t.len();
        }
        match p[pi] {
            '*' => {
                let double = pi + 1 < p.len() && p[pi + 1] == '*';
                let (next_pi, crosses) = if double { (pi + 2, true) } else { (pi + 1, false) };
                // `**/` also matches zero directories, so skip a following `/`.
                let next_pi = if double && next_pi < p.len() && p[next_pi] == '/' {
                    next_pi + 1
                } else {
                    next_pi
                };
                for skip in ti..=t.len() {
                    if !crosses && t[ti..skip].contains(&'/') {
                        break;
                    }
                    if go(t, skip, p, next_pi) {
                        return true;
                    }
                }
                false
            }
            '?' => ti < t.len() && t[ti] != '/' && go(t, ti + 1, p, pi + 1),
            c => ti < t.len() && t[ti] == c && go(t, ti + 1, p, pi + 1),
        }
    }
    go(&t, 0, &p, 0)
}

/// A path's file name for use as a buffer label, falling back to the whole
/// path when there isn't one.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A cheap content fingerprint used to invalidate the highlight cache; any edit
/// changes it, and it costs a pass over the text rather than a re-parse.
fn text_hash(lines: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines.len().hash(&mut hasher);
    for line in lines {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether a file name looks like markdown.
fn is_markdown_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown")
}

/// Every discrete action the UI can perform, shared by keyboard and mouse.
#[derive(Clone)]
pub enum Action {
    /// Swallows a click (e.g. on overlay chrome) without doing anything.
    None,
    SelectBuffer(usize),
    CloseBuffer(usize),
    GotoSection(DashboardSection),
    TogglePanel(PanelId),
    /// `[c]` — put every file git reports as changed into the quickfix list.
    GitChangedFiles,
    /// `[l]` / `[d]` / `[F]` — read-only git commands, shown in the `:!`
    /// output overlay. Nothing here rewrites the working tree.
    GitLog,
    GitDiff,
    GitFetch,
    OpenFile(usize),
    OpenPlugins,
    OpenDashboard,
    ToggleLsp(usize),
    /// Install the tool at this row in `project.lsp` (see `App::install_tool`).
    InstallTool(usize),
    ToggleMouse,
    CycleIconMode,
    /// Flip inline AI suggestions from the Settings tab, persisting the choice.
    ToggleAi,
    /// Select (and jump to) a quickfix entry by list index.
    QuickfixSelect(usize),
    SetSettingsIndex(usize),
    OpenPalette,
    ClosePalette,
    RunPalette(usize),
    OpenFinder,
    CloseFinder,
    RunFinder(usize),
    /// Open the find & replace panel, seeded with the word under the cursor.
    OpenReplace,
    CloseReplace,
    /// Select a result row in the replace panel by index.
    SelectReplaceHit(usize),
    /// Give the replace panel's focus to a specific field (clicking a box).
    FocusReplaceField(Field),
    ToggleSidebar,
    CloseSidebar,
    ToggleHelp,
    CloseHelp,
    ToggleMarkdown,
    /// Run an Ex command through the engine (e.g. `"w"`, `"q!"`). Carries the
    /// command text without the leading colon.
    RunEx(String),
    /// Run a plugin-registered command by name
    /// (`vim.api.ctrlvim_create_user_command`).
    RunPluginCommand(String),
    /// Switch to the theme at this index in [`theme::ALL`](crate::theme::ALL).
    SetTheme(usize),
    /// Start a new file: opens the file browser where a typed name is created.
    NewFile,
    /// Flip the "open file drawer on startup" config setting (Settings tab).
    ToggleStartupDrawer,
    /// Dismiss the save-as prompt without saving.
    CloseSavePrompt,
    /// Dismiss the `:!{cmd}` output overlay.
    CloseShellOutput,
}

pub struct App {
    pub engine: Ctrlvim,

    /// Real data for the project the editor was launched in.
    pub project: Project,
    /// The project root (current working directory), used to open files.
    pub root: PathBuf,

    pub buffers: Vec<Buffer>,
    pub active: usize,

    pub section: DashboardSection,

    pub sidebar_visible: bool,
    /// While the drawer is open, `/` drops into an inline fuzzy search.
    pub drawer_search: bool,
    pub drawer_query: String,
    pub file_index: usize, // selection in recent files (also highlights Recent Files)

    /// The full-screen fuzzy file browser, present only while open.
    pub finder: Option<Finder>,

    /// The project-wide find & replace panel, present only while open.
    pub replace: Option<ReplacePanel>,

    pub palette_open: bool,
    pub palette_query: String,
    pub palette_index: usize,

    /// The "save as" prompt: the filename being typed while saving an unnamed
    /// buffer (`Some` while the prompt is open).
    pub save_prompt: Option<String>,

    /// A transient one-line message shown on the status line (e.g. `:w` acks,
    /// set from engine [`ExEffect::Message`]).
    pub message: String,

    pub expand_git: bool,

    pub lsp_enabled: Vec<bool>,
    /// Selection index across the Settings tab (editor options + LSP list).
    pub settings_index: usize,

    pub help_open: bool,

    /// User configuration loaded from `~/.config/ctrlvim/config.toml`.
    pub config: Config,

    /// Tree-sitter highlights for the active buffer, recomputed when its text
    /// changes. Rendering takes `&App`, so this is behind a `RefCell`.
    syntax: RefCell<Option<SyntaxCache>>,

    /// Whether the quickfix pane is showing (`:copen` / `:cclose`).
    pub quickfix_open: bool,
    /// Selected row in the quickfix pane.
    pub quickfix_index: usize,
    /// Queue background jobs push their output onto.
    events: EventLoop,
    /// Job runner + its runtime, created on the first `:make`/`:grep`.
    jobs: Option<Jobs>,
    timers: Option<TimerService>,
    /// The job currently filling the quickfix list, if any.
    job: Option<RunningJob>,
    /// The `:!{cmd}` job currently filling [`App::shell_output`], if any.
    shell_job: Option<RunningShell>,
    /// The tool install currently running through `shell_job`, if any: its
    /// name, its row index in `project.lsp`, and the job id — so completion
    /// is attributed to the right job even if a `:!{cmd}` interleaves.
    installing_tool: Option<(String, usize, u64)>,
    /// Whether the `:!{cmd}` output overlay is showing.
    pub shell_open: bool,
    /// When the currently half-typed mapping last took a key, or `None` when
    /// none is pending. Drives `'timeoutlen'` (see `tick_keymap_timeout`).
    keymap_pending_since: Option<std::time::Instant>,
    /// Keys buffered while a mapping is half-typed *in the shell*. The engine
    /// owns the equivalent buffer for the editor; the shell needs its own
    /// because its keys never reach the engine (see `shell_keymap`).
    shell_map_pending: Vec<Key>,
    /// What can still follow the keys typed so far — the which-key popup's
    /// contents, read straight from the engine's mapping table so it can never
    /// disagree with what the keys actually do.
    pub which_key: Vec<ctrlvim_core::Continuation>,
    /// Overlay title: the command line plus its exit code once it's back.
    pub shell_title: String,
    /// Output lines of the last `:!{cmd}`, shown in the overlay.
    pub shell_output: Vec<String>,
    /// Scroll offset (in lines) into `shell_output`.
    pub shell_scroll: usize,
    /// Modified time of the tags file when it was last loaded, so a
    /// regenerated one is picked up without a reload command.
    tags_loaded_at: Option<std::time::SystemTime>,
    /// Top of the editor viewport in screen rows — what the mouse wheel moves.
    /// The renderer clamps it so the cursor is always visible, so it doesn't
    /// need updating when the cursor moves by keyboard.
    view_top: usize,
    /// Editor viewport height, recorded by the renderer for scroll clamping.
    viewport_rows: std::cell::Cell<usize>,

    /// The inline-suggestion worker (CodeGemma on candle), created the first
    /// time suggestions are switched on — so an editor that never asks for AI
    /// never spawns the thread, let alone downloads a model.
    ai: Option<Completer>,
    /// When the editor last changed while in Insert mode. A completion is
    /// requested once this is `debounce_ms` old: asking on every keystroke
    /// would cancel each request with the next one and never finish any.
    ai_idle_since: Option<Instant>,

    pub should_quit: bool,
}

/// A `:make`/`:grep` in flight: its output is reassembled into lines, parsed
/// into entries, and installed on the engine when the process exits.
struct RunningJob {
    id: u64,
    title: String,
    /// Reassembles pipe chunks into lines…
    lines: LineBuffer,
    /// …which this turns into entries, stitching multi-line diagnostics.
    parser: OutputParser,
    items: Vec<QfItem>,
}

/// A `:!{cmd}` in flight: raw output lines, with no quickfix parsing.
struct RunningShell {
    id: u64,
    command: String,
    lines: LineBuffer,
    output: Vec<String>,
}

/// Cached highlight spans plus the buffer state they were computed from.
struct SyntaxCache {
    /// Active buffer, a hash of its text, and the visible window (`top`,
    /// `rows`) — an edit or a scroll invalidates.
    key: (usize, u64, usize, usize),
    /// Spans per visible row, indexed from the window's first line.
    lines: Vec<Vec<HlSpan>>,
}

/// Delete the previous word from the end of `s`: trailing whitespace, then
/// back to (and keeping) the whitespace before it, or to the start if there
/// is none. Shared by every single-line frontend field's Option+Backspace /
/// Ctrl+Backspace handling — see [`crate::input`].
pub(crate) fn delete_word_backward(s: &mut String) {
    let trimmed_len = s.trim_end().len();
    s.truncate(trimmed_len);
    match s.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((idx, ch)) => s.truncate(idx + ch.len_utf8()),
        None => s.clear(),
    }
}

impl App {
    /// Build an app rooted at the current working directory.
    pub fn new() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_root(root, Instant::now())
    }

    /// Build an app rooted at `root`, measuring startup from `start`.
    pub fn with_root(root: PathBuf, start: Instant) -> Self {
        let project = Project::load(root.clone(), start);
        let lsp_enabled = project.lsp.iter().map(|s| s.installed).collect();
        // Restore the theme chosen in a previous session (defaults to Terminal,
        // which follows the host terminal's own palette).
        if let Some(name) = crate::data::saved_theme() {
            crate::theme::set_by_name(&name);
        }
        let config = Config::load();
        let mut engine = Ctrlvim::new();
        let startup_message = load_startup_plugins(&mut engine, &config.plugins);
        App {
            engine,
            project,
            root,
            buffers: vec![Buffer::dashboard()],
            active: 0,
            section: DashboardSection::Workspace,
            // The drawer opens on startup when the config asks for it.
            sidebar_visible: config.drawer,
            drawer_search: false,
            drawer_query: String::new(),
            file_index: 0,
            finder: None,
            replace: None,
            palette_open: false,
            palette_query: String::new(),
            palette_index: 0,
            save_prompt: None,
            message: startup_message,
            expand_git: false,
            lsp_enabled,
            settings_index: 0,
            help_open: false,
            config,
            syntax: RefCell::new(None),
            quickfix_open: false,
            quickfix_index: 0,
            events: EventLoop::new(),
            jobs: None,
            timers: None,
            job: None,
            shell_job: None,
            installing_tool: None,
            shell_open: false,
            keymap_pending_since: None,
            shell_map_pending: Vec::new(),
            which_key: Vec::new(),
            shell_title: String::new(),
            shell_output: Vec::new(),
            shell_scroll: 0,
            tags_loaded_at: None,
            view_top: 0,
            viewport_rows: std::cell::Cell::new(24),
            ai: None,
            ai_idle_since: None,
            should_quit: false,
        }
    }

    /// Toggle the "open file drawer on startup" setting, persist it to the
    /// config file, and apply it live so the drawer reflects the change now.
    pub fn toggle_startup_drawer(&mut self) {
        self.config.drawer = !self.config.drawer;
        self.config.save();
        self.sidebar_visible = self.config.drawer;
        self.drawer_search = false;
        self.drawer_query.clear();
        self.message = format!(
            "file drawer on startup: {}",
            if self.config.drawer { "on" } else { "off" }
        );
    }

    // --- queries -----------------------------------------------------------

    pub fn active_buffer(&self) -> &Buffer {
        &self.buffers[self.active]
    }
    pub fn is_dashboard(&self) -> bool {
        matches!(self.active_buffer().kind, BufferKind::Dashboard)
    }
    /// True when the active buffer is an editable file window.
    pub fn is_file(&self) -> bool {
        matches!(self.active_buffer().kind, BufferKind::File)
    }

    /// The active buffer is a markdown file (whether or not rendering is on).
    pub fn active_is_markdown(&self) -> bool {
        self.active_buffer().is_markdown()
    }

    /// Live markdown rendering should be applied to the active buffer right now.
    pub fn md_render_active(&self) -> bool {
        let b = self.active_buffer();
        b.is_markdown() && b.render_md
    }

    /// Toggle live markdown rendering for the active buffer (no-op off markdown).
    pub fn toggle_md_render(&mut self) {
        if self.active_buffer().is_markdown() {
            self.buffers[self.active].render_md = !self.buffers[self.active].render_md;
        }
    }

    pub fn panel_expanded(&self, p: PanelId) -> bool {
        match p {
            PanelId::Git => self.expand_git,
        }
    }

    pub fn lsp_active_count(&self) -> usize {
        self.lsp_enabled.iter().filter(|&&b| b).count()
    }

    // --- transitions -------------------------------------------------------

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::SelectBuffer(i) => {
                if i < self.buffers.len() {
                    self.set_active(i);
                }
            }
            Action::CloseBuffer(i) => self.close_buffer(i),
            Action::GotoSection(sec) => self.section = sec,
            Action::TogglePanel(p) => self.toggle_panel(p),
            Action::GitChangedFiles => self.git_changed_files(),
            Action::GitLog => self.git_command("log --oneline --graph --decorate -40"),
            Action::GitDiff => self.git_command("diff HEAD"),
            Action::GitFetch => self.git_command("fetch --all --prune"),
            Action::OpenFile(i) => self.open_file(i),
            Action::OpenPlugins => self.open_plugins(),
            Action::OpenDashboard => self.open_dashboard(),
            Action::ToggleLsp(i) => self.toggle_lsp(i),
            Action::InstallTool(i) => self.install_tool(i),
            Action::ToggleMouse => self.toggle_mouse(),
            Action::CycleIconMode => self.cycle_icon_mode(),
            Action::ToggleAi => self.toggle_ai(),
            Action::QuickfixSelect(i) => self.quickfix_select(i),
            Action::SetSettingsIndex(i) => {
                if i < self.settings_count() {
                    self.settings_index = i;
                }
            }
            Action::OpenPalette => {
                self.palette_open = true;
                self.palette_query.clear();
                self.palette_index = 0;
            }
            Action::ClosePalette => self.close_palette(),
            Action::RunPalette(i) => self.run_palette(i),
            Action::OpenFinder => self.open_finder(),
            Action::CloseFinder => self.close_finder(),
            Action::RunFinder(i) => {
                if let Some(f) = &mut self.finder {
                    f.selected = i;
                }
                self.finder_select();
            }
            // A click-opened panel has no cursor context to seed from, so it
            // reuses whatever `:Find` would find under the cursor.
            Action::OpenReplace => {
                let seed = self.engine.session.word_at_cursor();
                self.open_replace(seed);
            }
            Action::CloseReplace => self.replace = None,
            Action::SelectReplaceHit(i) => {
                if let Some(p) = &mut self.replace {
                    p.focus = Field::Results;
                    if i < p.hits.len() {
                        p.selected = i;
                    }
                }
            }
            Action::FocusReplaceField(field) => {
                if let Some(p) = &mut self.replace {
                    p.focus = field;
                }
            }
            // The file drawer is only available when enabled in the config.
            Action::ToggleSidebar => {
                if self.config.drawer {
                    self.toggle_sidebar();
                }
            }
            Action::CloseSidebar => self.close_sidebar(),
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::CloseHelp => self.help_open = false,
            Action::ToggleMarkdown => self.toggle_md_render(),
            Action::RunEx(cmd) => self.run_ex_command(&cmd),
            Action::RunPluginCommand(name) => self.run_plugin_command(&name),
            Action::SetTheme(i) => self.set_theme(i),
            Action::NewFile => self.new_untitled(),
            Action::ToggleStartupDrawer => self.toggle_startup_drawer(),
            Action::CloseSavePrompt => self.close_save_prompt(),
            Action::CloseShellOutput => self.shell_open = false,
        }
    }

    /// Start editing a fresh, unnamed buffer (like Vim's `:enew`). It gets a
    /// name only when saved — `:w` opens the [save prompt](Self::open_save_prompt).
    pub fn new_untitled(&mut self) {
        self.buffers.push(Buffer {
            label: "[No Name]".into(),
            kind: BufferKind::File,
            path: None,
            text: vec![String::new()],
            render_md: false,
            modified: false,
        });
        self.set_active(self.buffers.len() - 1);
    }

    // --- save-as prompt ----------------------------------------------------

    /// Open the prompt asking for a filename (used when writing an unnamed
    /// buffer).
    pub fn open_save_prompt(&mut self) {
        self.save_prompt = Some(String::new());
    }

    pub fn close_save_prompt(&mut self) {
        self.save_prompt = None;
    }

    pub fn save_prompt_type(&mut self, c: char) {
        if let Some(name) = &mut self.save_prompt {
            name.push(c);
        }
    }

    pub fn save_prompt_backspace(&mut self) {
        if let Some(name) = &mut self.save_prompt {
            name.pop();
        }
    }

    pub fn save_prompt_word_backspace(&mut self) {
        if let Some(name) = &mut self.save_prompt {
            delete_word_backward(name);
        }
    }

    pub fn save_prompt_clear_to_start(&mut self) {
        if let Some(name) = &mut self.save_prompt {
            name.clear();
        }
    }

    /// Confirm the save prompt: write the buffer to the typed name and adopt it.
    pub fn save_prompt_confirm(&mut self) {
        let Some(name) = self.save_prompt.take() else { return };
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.host_write_as(&name);
    }

    /// Create the file at `path` (empty, plus any missing parent dirs) if it
    /// doesn't exist yet, then open it as a buffer. An existing path is just
    /// opened. This is the shared "make a new file" primitive behind `:e name`,
    /// the file browser's create action, and the dashboard's New File key.
    fn create_and_open(&mut self, path: PathBuf) {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, "") {
                self.message = format!("E212: Can't open file for writing: {e}");
                return;
            }
        }
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.open_path(path, label);
    }

    /// `:e <name>` / `:new <name>`: create or open `name` (relative to the
    /// project root unless absolute). An empty name opens the file browser.
    pub fn new_file(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.open_finder();
            return;
        }
        let path = if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            self.root.join(name)
        };
        self.create_and_open(path);
    }

    /// Apply the theme at `i` in [`theme::ALL`](crate::theme::ALL) and persist
    /// the choice so it survives across sessions.
    pub fn set_theme(&mut self, i: usize) {
        if let Some(t) = crate::theme::ALL.get(i) {
            crate::theme::set(*t);
            crate::data::save_theme(t.name);
            self.message = format!("theme: {}", t.name);
        }
    }

    /// Apply everything the config file declares: options, mappings, plugins.
    ///
    /// Each one is expressed as an Ex command and run through the engine, so the
    /// config gets exactly the semantics the `:` command line has — one code
    /// path, one source of truth for what an option or mapping means.
    pub fn apply_config(&mut self) {
        // Pins are per-project state rather than config, but this is the one
        // startup hook, and they must be back before the first `<A-1>`.
        self.engine.session.pins.set(crate::data::load_pins(&self.root));

        // `[options]` → a single `:set` with every argument.
        if !self.config.set_args.is_empty() {
            let args = self.config.set_args.join(" ");
            self.run_ex_command(&format!("set {args}"));
        }

        // `[keymaps] defaults = false` starts from an empty table, for a config
        // that would rather define every chord itself than shadow ours.
        if !self.config.keymap_defaults {
            self.engine.session.keymap = ctrlvim_core::Keymap::new();
        }
        // `mapleader` applies at definition time, as in Vim — so it has to be
        // set before any `[[keymap]]` entry is parsed.
        self.engine.session.keymap.set_leader(self.config.leader);

        // `[[command]]` → user commands, defined *before* the keymaps so a
        // mapping's rhs can name one that config itself contributed.
        for c in self.config.commands.clone() {
            self.run_ex_command(&format!("command {} {}", c.name, c.expansion));
        }

        // `[[unmap]]` → drop a built-in outright. Binding the same lhs already
        // replaces one; this is for chords you want simply gone.
        for u in self.config.unmaps.clone() {
            self.engine.session.keymap.remove(MapMode::parse(&u.mode), &u.lhs);
        }

        // `[[keymap]]` → the engine's per-mode mapping table. This goes direct
        // rather than through `:nnoremap` because the right-hand side is key
        // *notation* (`<Esc>`, `<CR>`), which the command line would consume as
        // real keys before the mapping was ever defined.
        for m in self.config.keymaps.clone() {
            // A mapping whose lhs doesn't parse is reported rather than
            // dropped: a silent no-op here is exactly the failure mode that
            // makes a config feel haunted.
            if let Err(e) = self.engine.session.keymap.set_with_desc(
                MapMode::parse(&m.mode),
                &m.lhs,
                &m.rhs,
                m.desc.clone(),
            ) {
                self.message = format!("config: {e}");
            }
        }

        // `[[plugin]]` → source the plugin's entry point now, unless it asked to
        // be loaded lazily on an event.
        for p in self.config.plugins.clone() {
            if p.enabled && p.event.is_none() {
                self.load_plugin(&p);
            }
        }

        // Config autocmds and plugins alike need the Lua host to exist before
        // any event can reach a callback.
        if !self.config.autocmds.is_empty() {
            let _ = self.engine.ensure_host();
        }

        // `[ai] enabled = true` → arm inline suggestions. The weights are still
        // loaded lazily, on the first completion, so startup stays instant.
        if self.config.ai.enabled {
            self.set_ai_enabled(true);
        }

        self.fire_autocmd("VimEnter");
    }

    /// Load a plugin: source its `init.lua` if present, else its `init.vim`.
    ///
    /// Plugins are ordinary script files; the config decides *which* ones load
    /// and in what order, which is why there is no `runtimepath` search here.
    fn load_plugin(&mut self, p: &crate::config::PluginEntry) {
        let dir = expand_tilde(&p.path);
        for entry in ["init.lua", "init.vim"] {
            let candidate = dir.join(entry);
            if candidate.exists() {
                self.run_ex_command(&format!("source {}", candidate.display()));
                return;
            }
        }
        self.message = format!("plugin '{}': no init.lua or init.vim in {}", p.name, dir.display());
    }

    /// Fire an autocommand event: run every matching `[[autocmd]]` from the
    /// config, then notify Lua callbacks registered through the API.
    ///
    /// The file the event is *about* is the active buffer's label, which is what
    /// the `pattern` field matches against.
    pub fn fire_autocmd(&mut self, event: &str) {
        let file = self.active_buffer().label.clone();
        let matching: Vec<String> = self
            .config
            .autocmds
            .iter()
            .filter(|a| a.event.eq_ignore_ascii_case(event) && pattern_matches(&a.pattern, &file))
            .map(|a| a.command.clone())
            .collect();
        for cmd in matching {
            self.run_ex_command(&cmd);
        }
        self.engine.fire_autocmd(event, &file);

        // A lazily-loaded plugin waiting on this event loads now, then is
        // marked loaded so it doesn't source twice.
        let pending: Vec<crate::config::PluginEntry> = self
            .config
            .plugins
            .iter()
            .filter(|p| {
                p.enabled && p.event.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(event))
            })
            .cloned()
            .collect();
        for p in pending {
            self.load_plugin(&p);
            if let Some(slot) = self.config.plugins.iter_mut().find(|q| q.name == p.name) {
                slot.event = None;
                slot.enabled = false;
            }
        }
    }

    /// Run an Ex command through the engine, exactly as if it were typed on the
    /// `:` command line. Keeps the engine authoritative over what commands do
    /// (and what unknown ones report) — the palette is only a nicer entry point.
    pub fn run_ex_command(&mut self, cmd: &str) {
        let cmd = cmd.trim().trim_start_matches(':');
        if cmd.is_empty() {
            return;
        }
        self.feed_engine(Key::Char(':'));
        for c in cmd.chars() {
            self.feed_engine(Key::Char(c));
        }
        self.feed_engine(Key::Enter);
    }

    /// Run a plugin-registered command by name (selecting it from the
    /// palette) — the frontend counterpart to [`run_ex_command`](Self::run_ex_command)
    /// for commands a Lua plugin registered with `ctrlvim_create_user_command`.
    pub fn run_plugin_command(&mut self, name: &str) {
        match self.engine.run_plugin_command(name) {
            Ok(true) => {}
            Ok(false) => self.message = format!("E492: not a plugin command: {name}"),
            Err(e) => self.message = e,
        }
    }

    pub fn cycle_section(&mut self, dir: i32) {
        let order = [
            DashboardSection::Workspace,
            DashboardSection::Settings,
            DashboardSection::About,
        ];
        let i = order.iter().position(|&s| s == self.section).unwrap_or(0) as i32;
        let n = order.len() as i32;
        self.section = order[(((i + dir) % n + n) % n) as usize];
    }

    pub fn cycle_buffer(&mut self, dir: i32) {
        let n = self.buffers.len() as i32;
        let i = self.active as i32;
        self.set_active((((i + dir) % n + n) % n) as usize);
    }

    fn toggle_panel(&mut self, p: PanelId) {
        match p {
            PanelId::Git => self.expand_git = !self.expand_git,
        }
    }

    /// Run a read-only `git` subcommand into the `:!` output overlay.
    ///
    /// These go through the same job machinery `:!` uses, so they are async,
    /// scrollable, and report their exit code — and because the overlay is
    /// where `:!git …` output already lands, the dashboard keys and the
    /// command line agree about what running git looks like.
    fn git_command(&mut self, args: &str) {
        if self.project.git.is_none() {
            self.message = "not a git repository".into();
            return;
        }
        self.host_run_shell(format!("git {args}"));
    }

    /// `[c]` — the changed files as a quickfix list.
    ///
    /// The paths come from the porcelain output already parsed at startup, so
    /// this costs no git call; only the *contents* are stale, and opening an
    /// entry reads the file fresh.
    fn git_changed_files(&mut self) {
        let Some(g) = &self.project.git else {
            self.message = "not a git repository".into();
            return;
        };
        let items: Vec<QfItem> = g
            .changed
            .iter()
            .map(|c| QfItem {
                path: self.root.join(&c.path),
                line: 0,
                col: 0,
                text: format!("{}: {}", c.label, c.path),
                kind: match c.label {
                    "conflict" => QfKind::Error,
                    "untracked" => QfKind::Note,
                    _ => QfKind::Info,
                },
            })
            .collect();
        self.finish_quickfix(items, "git changed files".to_string());
    }

    pub fn move_file_selection(&mut self, dir: i32) {
        let n = self.project.recent_files.len() as i32;
        if n == 0 {
            return;
        }
        let i = self.file_index as i32;
        self.file_index = (((i + dir) % n + n) % n) as usize;
    }

    /// Number of Settings rows: the editor options plus each LSP server.
    pub const SETTINGS_EDITOR_OPTIONS: usize = 4; // drawer, mouse, icons, AI

    pub fn settings_count(&self) -> usize {
        Self::SETTINGS_EDITOR_OPTIONS + self.project.lsp.len()
    }

    /// Move the Settings selection, wrapping across the EDITOR options and the
    /// LSP list as one continuous list.
    pub fn move_settings(&mut self, dir: i32) {
        let n = self.settings_count() as i32;
        if n == 0 {
            return;
        }
        let i = self.settings_index as i32;
        self.settings_index = (((i + dir) % n + n) % n) as usize;
    }

    /// Toggle whatever Settings row is selected.
    pub fn settings_toggle(&mut self) {
        match self.settings_index {
            0 => self.toggle_startup_drawer(),
            1 => self.toggle_mouse(),
            2 => self.cycle_icon_mode(),
            3 => self.toggle_ai(),
            i => self.toggle_lsp(i - Self::SETTINGS_EDITOR_OPTIONS),
        }
    }

    pub fn toggle_lsp(&mut self, i: usize) {
        if let Some(slot) = self.lsp_enabled.get_mut(i) {
            *slot = !*slot;
            self.settings_index = i + Self::SETTINGS_EDITOR_OPTIONS;
        }
    }

    /// Install whichever Settings row is focused (`I`), if it's a tool row
    /// with a known install method.
    pub fn install_focused_tool(&mut self) {
        if let Some(i) = self.settings_index.checked_sub(Self::SETTINGS_EDITOR_OPTIONS) {
            self.install_tool(i);
        }
    }

    /// Install the tool at row `i` of `project.lsp` by shelling out to its
    /// registry install command (`cargo install`, `npm install`, …), reusing
    /// the same `:!{cmd}` job machinery as [`App::host_run_shell`] — the
    /// output overlay doubles as the install log.
    pub fn install_tool(&mut self, i: usize) {
        let Some(lsp) = self.project.lsp.get(i) else { return };
        if lsp.installed {
            self.message = format!("{}: already installed", lsp.name);
            return;
        }
        if self.installing_tool.is_some() {
            self.message = "an install is already running".into();
            return;
        }
        let Some(tool) = ctrlvim_tools::REGISTRY.iter().find(|t| t.name == lsp.name) else {
            self.message = format!("{}: no install method available", lsp.name);
            return;
        };
        let Some(command) = ctrlvim_tools::install_command(tool) else {
            self.message = format!("{}: no install method available", lsp.name);
            return;
        };
        if let Some(dir) = ctrlvim_tools::install_dir(tool.name) {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.message = format!("{}: could not create {}: {e}", lsp.name, dir.display());
                return;
            }
        }
        let name = tool.name.to_string();
        self.host_run_shell(command);
        let Some(id) = self.shell_job.as_ref().map(|j| j.id) else { return };
        self.installing_tool = Some((name.clone(), i, id));
        self.message = format!("installing {name}…");
    }

    /// Toggle mouse support, persisting it to the config.
    pub fn toggle_mouse(&mut self) {
        self.config.mouse = !self.config.mouse;
        self.config.save();
        self.message = format!("mouse: {}", if self.config.mouse { "on" } else { "off" });
    }

    /// Cycle file icons through auto → nerd → text, applying it live and
    /// persisting it to the config.
    pub fn cycle_icon_mode(&mut self) {
        self.config.icons = self.config.icons.next();
        self.config.save();
        self.message = format!("file icons: {}", self.config.icons.label());
    }

    /// Open (or focus) one of the dashboard's recent files by list index.
    pub fn open_file(&mut self, file_idx: usize) {
        let Some(f) = self.project.recent_files.get(file_idx) else { return };
        let name = f.name.clone();
        let path = self.root.join(&f.path);
        self.open_path(path, name);
    }

    /// Open (or focus) an arbitrary file by absolute path. New buffers are
    /// seeded by reading the file from disk; [`set_active`](Self::set_active)
    /// loads that text into the engine. Re-opening focuses the existing buffer.
    pub fn open_path(&mut self, path: PathBuf, name: String) {
        if let Some(i) = self.buffers.iter().position(|b| b.path.as_deref() == Some(path.as_path())) {
            self.set_active(i);
            return;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let render_md = is_markdown_name(&name); // live-render markdown by default
        self.buffers.push(Buffer { label: name, kind: BufferKind::File, path: Some(path), text, render_md, modified: false });
        self.set_active(self.buffers.len() - 1);
    }

    /// Switch to the dashboard (`:dash` / `<leader>d`).
    pub fn open_dashboard(&mut self) {
        if let Some(i) = self.buffers.iter().position(|b| b.kind == BufferKind::Dashboard) {
            self.set_active(i);
        }
    }

    pub fn open_plugins(&mut self) {
        if let Some(i) = self.buffers.iter().position(|b| b.kind == BufferKind::Plugins) {
            self.set_active(i);
        } else {
            self.buffers.push(Buffer { label: "Plugin Manager".into(), kind: BufferKind::Plugins, path: None, text: Vec::new(), render_md: false, modified: false });
            self.set_active(self.buffers.len() - 1);
        }
    }

    fn close_buffer(&mut self, i: usize) {
        if i >= self.buffers.len() || !self.buffers[i].closable() {
            return;
        }
        let was_active = i == self.active;
        self.buffers.remove(i);
        if was_active {
            if self.active >= self.buffers.len() {
                self.active = self.buffers.len() - 1;
            }
            // The active buffer's identity changed; sync the engine to it.
            self.load_active_into_engine();
        } else if self.active > i {
            // Index shifted but the same buffer stays active; engine unchanged.
            self.active -= 1;
        }
    }

    // --- editor / engine wiring -------------------------------------------

    /// Switch the active buffer, keeping the engine's single working buffer in
    /// sync: snapshot the outgoing file's text, then load the incoming file's.
    pub fn set_active(&mut self, idx: usize) {
        if idx == self.active || idx >= self.buffers.len() {
            return;
        }
        self.fire_autocmd("BufLeave");
        self.snapshot_active();
        self.active = idx;
        self.load_active_into_engine();
        self.fire_autocmd("BufEnter");
    }

    /// Save the engine's current text (and dirty state) back into the active
    /// file buffer's cache.
    fn snapshot_active(&mut self) {
        if matches!(self.active_buffer().kind, BufferKind::File) {
            self.buffers[self.active].text = self.engine.lines();
            self.buffers[self.active].modified = self.engine.is_modified();
        }
    }

    /// Load the active file buffer's cached text into the engine, restoring its
    /// per-buffer dirty state (the engine's single buffer would otherwise reset).
    fn load_active_into_engine(&mut self) {
        if matches!(self.active_buffer().kind, BufferKind::File) {
            let text = self.buffers[self.active].text.join("\n");
            let label = self.buffers[self.active].label.clone();
            self.engine.open(&text, Some(&label));
            self.engine.set_modified(self.buffers[self.active].modified);
        }
    }

    /// Whether the active buffer has unsaved changes (live value from the
    /// engine while a file is focused).
    pub fn active_modified(&self) -> bool {
        self.is_file() && self.engine.is_modified()
    }

    /// Scroll the editor by `lines` (negative = up), when mouse support is on
    /// and a file buffer is focused. Moves the cursor, which drags the viewport.
    pub fn scroll_editor(&mut self, lines: i32) {
        if !self.config.mouse || !self.editor_focus() {
            return;
        }
        let text = self.editor_lines();
        let line_count = text.len();
        let rows = self.viewport_rows.get().max(1);
        // Screen rows, not buffer lines: a closed fold is one row.
        let max_top = self.folds().screen_line_count(line_count).saturating_sub(1);
        let cur_row = self.screen_line_of(self.editor_cursor().0);

        // Start from the row actually on screen, which may differ from the
        // stored offset after the cursor was moved by the keyboard.
        let effective = self.view_top.clamp(cur_row.saturating_sub(rows - 1), cur_row);
        self.view_top = if lines >= 0 {
            (effective + lines as usize).min(max_top)
        } else {
            effective.saturating_sub(lines.unsigned_abs() as usize)
        };

        // Vim drags the cursor along only when the view would leave it behind.
        let (first, last) = (self.view_top, self.view_top + rows - 1);
        if cur_row < first || cur_row > last {
            let row = cur_row.clamp(first, last);
            let line = self.folds().buffer_line_of(row, line_count);
            let col = self.editor_cursor().1;
            self.engine.session.set_cursor_clamped(line, col);
        }
    }

    /// The stored viewport offset, in screen rows.
    pub fn view_top(&self) -> usize {
        self.view_top
    }

    /// Record how many rows the editor viewport has. Called from the renderer,
    /// which is the only place that knows — mouse scrolling needs it to keep
    /// the cursor inside the window.
    pub fn set_viewport_rows(&self, rows: usize) {
        self.viewport_rows.set(rows);
    }

    /// Feed one key to the engine's editing session, then perform any host
    /// effects the engine requested (`:w`/`:q`/…).
    pub fn feed_engine(&mut self, key: Key) {
        self.engine.session.feed(key);
        // Every keystroke restarts the inline-suggestion idle countdown, so a
        // completion is only ever asked for once typing has paused.
        self.touch_ai();
        self.apply_effects();
        // ...and the `'timeoutlen'` clock, so a half-typed chord resolves on
        // its own and the which-key popup tracks what can still follow.
        self.sync_keymap_pending();
    }

    /// Drain and perform the engine's queued [`ExEffect`]s. This is the host
    /// side of the Ex-command boundary: the UI-less engine decides *what* should
    /// happen; the frontend does the file I/O, buffer/quit management, and UI.
    fn apply_effects(&mut self) {
        for effect in self.engine.take_effects() {
            match effect {
                ExEffect::Write { .. } => {
                    self.host_write();
                }
                ExEffect::Quit { .. } => self.host_quit(),
                ExEffect::WriteQuit { .. } => {
                    if self.host_write() {
                        self.host_quit();
                    }
                }
                ExEffect::WriteAs { path, .. } => {
                    self.host_write_as(&path);
                }
                ExEffect::WriteAll => {
                    let n = self.host_write_all();
                    self.message = format!("{n} buffer(s) written");
                }
                ExEffect::QuitAll { force } => self.host_quit_all(force),
                ExEffect::WriteQuitAll => {
                    self.host_write_all();
                    self.should_quit = true;
                }
                // `:close` exits the whole editor, not just the active window.
                ExEffect::CloseApp => self.should_quit = true,
                ExEffect::Colorscheme(name) => self.host_colorscheme(name),
                ExEffect::Buffer(cmd) => self.host_buffer_cmd(cmd),
                ExEffect::Vimscript(src) => self.host_vimscript(&src),
                ExEffect::Lua(code) => match self.engine.run_lua(&code) {
                    Ok(()) => {}
                    Err(e) => self.message = format!("E5108: {}", first_line(&e)),
                },
                ExEffect::Source(path) => self.host_source(&path),
                ExEffect::OpenBrowser => self.open_finder(),
                ExEffect::OpenDashboard => self.open_dashboard(),
                ExEffect::Edit(name) => self.new_file(&name),
                ExEffect::Message(m) => self.message = m,
                ExEffect::Quickfix(cmd) => self.host_quickfix(cmd),
                ExEffect::Tag(cmd) => self.host_tag(cmd),
                ExEffect::OpenReplace { pattern } => self.open_replace(pattern),
                ExEffect::Shell(command) => self.host_run_shell(command),
                ExEffect::Ai(cmd) => self.host_ai(cmd),
                ExEffect::HostAction(name) => self.host_action(&name),
                ExEffect::Pin(cmd) => self.host_pin(cmd),
            }
        }
    }

    /// Host side of the pinned-file commands. The engine owns the list; opening
    /// a file and showing the menu need the buffer list and the UI.
    fn host_pin(&mut self, cmd: PinCmd) {
        match cmd {
            // Reuse the ordinary open path, so a pinned file behaves exactly
            // like one opened any other way (an existing tab is reused rather
            // than duplicated).
            PinCmd::Open(path) => self.new_file(&path),
            PinCmd::Message(msg) => {
                // Pins outlive the session, so every change is written through.
                crate::data::save_pins(&self.root, self.engine.session.pins.files());
                self.message = msg;
            }
            PinCmd::Menu(files) => {
                self.message = if files.is_empty() {
                    "no pinned files".to_string()
                } else {
                    files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| format!("{}: {f}", i + 1))
                        .collect::<Vec<_>>()
                        .join("  ")
                };
            }
        }
    }

    /// Perform a named frontend action (`ExEffect::HostAction`).
    ///
    /// This registry is what makes the TUI's own vocabulary bindable. Before
    /// it, [`Action`] and [`ExEffect`] were disjoint: the palette, the drawer,
    /// the plugin manager and the help modal existed only behind hardcoded key
    /// handlers, so no `[[keymap]]` entry could reach them. Now each has a name
    /// that an Ex command — and therefore a mapping — can call for.
    fn host_action(&mut self, name: &str) {
        let action = match name {
            "palette" => Action::OpenPalette,
            "sidebar" => Action::ToggleSidebar,
            "help" => Action::ToggleHelp,
            "plugins" => Action::OpenPlugins,
            "markdown" => Action::ToggleMarkdown,
            "newfile" => Action::NewFile,
            other => {
                self.message = format!("E5555: unknown action: {other}");
                return;
            }
        };
        self.dispatch(action);
    }

    // --- inline AI suggestions ---------------------------------------------

    /// Host side of the `:AI…` commands. The engine owns the suggestion state
    /// machine; the model, its thread, and its weights are entirely here.
    fn host_ai(&mut self, cmd: AiCmd) {
        match cmd {
            AiCmd::Enable(want) => {
                let on = want.unwrap_or(!self.engine.session.suggest.enabled);
                self.set_ai_enabled(on);
                self.message = if on {
                    match self.ai_status() {
                        // A previous load already failed; saying "on" would be
                        // a lie the user only discovers by waiting.
                        Some(s) if s.is_failed() => s.describe(),
                        _ => "inline AI suggestions on".to_string(),
                    }
                } else {
                    "inline AI suggestions off".to_string()
                };
            }
            AiCmd::Suggest => {
                if !self.engine.session.suggest.enabled {
                    self.set_ai_enabled(true);
                }
                self.engine.session.suggest.arm();
                // Bypass the idle debounce: this *is* the explicit request.
                // `checked_sub` because a monotonic clock started moments ago
                // has nothing to subtract from — falling back to "now" just
                // means the explicit request waits one debounce.
                self.ai_idle_since =
                    Some(Instant::now().checked_sub(self.debounce()).unwrap_or_else(Instant::now));
            }
            AiCmd::Status => match self.ai_status() {
                // A failure is re-shown in full: `:AIStatus` is where someone
                // goes *after* dismissing the panel, and sending them back to a
                // clipped one-liner would be a dead end.
                Some(status @ ctrlvim_ai::Status::Failed(_)) => {
                    let ctrlvim_ai::Status::Failed(e) = &status else { unreachable!() };
                    let e = e.clone();
                    self.show_ai_error(&e);
                }
                Some(status) => self.message = status.describe(),
                None => self.message = "AI: off (`:AI on` to enable)".to_string(),
            },
            AiCmd::Load => {
                // Re-read `[ai]` before retrying. The overwhelmingly common
                // reason to ask for an explicit load is that the previous one
                // failed and the user has just edited the model repo or path in
                // response — retrying with the config the editor booted with
                // would fail identically, which makes `:AILoad` look broken and
                // makes the advice in `gated_repo_help` untrue.
                let reloaded = self.reload_ai_config();
                self.ensure_ai().preload();
                self.message = if reloaded {
                    "AI: config reloaded, loading the model…".to_string()
                } else {
                    "AI: loading the model…".to_string()
                };
            }
        }
    }

    /// Re-read the `[ai]` section from disk. Returns whether anything changed;
    /// when it did, the worker is dropped so the next request builds one with
    /// the new settings (the model repo, device, and precision are all fixed at
    /// worker construction). An unchanged config keeps the worker — and its
    /// already-loaded weights — exactly where they are.
    fn reload_ai_config(&mut self) -> bool {
        match Config::path() {
            Some(path) => self.reload_ai_config_from(&path),
            None => false,
        }
    }

    /// [`reload_ai_config`](Self::reload_ai_config) against a specific file.
    ///
    /// Split out so the reload can be tested without reading — and depending
    /// on — whoever's real `~/.config/ctrlvim/config.toml` happens to say, the
    /// same hazard the render tests pin `App::config` to avoid.
    pub fn reload_ai_config_from(&mut self, path: &Path) -> bool {
        let fresh = Config::load_from(path).ai;
        if fresh == self.config.ai {
            return false;
        }
        self.config.ai = fresh;
        self.ai = None;
        if self.ai_enabled() {
            self.engine.session.suggest.context = ContextWindow {
                before: self.config.ai.context_before,
                after: self.config.ai.context_after,
            };
        }
        true
    }

    /// Report a model failure: a one-line summary on the status line, and the
    /// whole explanation in the output panel.
    ///
    /// The panel matters. These errors are the only ones in the editor that
    /// have to *teach* — "the repo is gated" is useless without the two
    /// paragraphs saying what to do about it — and the status line clips at the
    /// terminal edge, which silently ate exactly the actionable half. A failed
    /// load parks the worker until `:AILoad` asks again, so this pops up once
    /// per attempt rather than once per keystroke.
    pub fn show_ai_error(&mut self, error: &str) {
        self.message = format!("AI: {}", first_line(error));
        // Only interrupt for something worth reading; a one-line failure is
        // fully visible on the status line already.
        if error.lines().count() < 2 {
            return;
        }
        self.shell_title = "AI — model could not be loaded".to_string();
        self.shell_output = error.lines().map(str::to_string).collect();
        self.shell_scroll = 0;
        self.shell_open = true;
    }

    /// Whether inline suggestions are currently being offered. This is the
    /// *live* state, which is what the Settings row shows — `:AI on` and the
    /// checkbox must never disagree about whether the feature is running.
    pub fn ai_enabled(&self) -> bool {
        self.engine.session.suggest.enabled
    }

    /// Settings-tab toggle: flip inline suggestions **and persist the choice**,
    /// so it survives a restart. `:AI` is the session-scoped counterpart, the
    /// same way `:set mouse` is to the mouse checkbox.
    pub fn toggle_ai(&mut self) {
        let on = !self.ai_enabled();
        self.config.ai.enabled = on;
        self.config.save();
        self.set_ai_enabled(on);
        self.message = format!(
            "inline AI suggestions: {}",
            if on { "on" } else { "off" }
        );
    }

    /// Turn inline suggestions on or off, starting the worker on the way in and
    /// dropping it (and its several gigabytes of weights) on the way out.
    pub fn set_ai_enabled(&mut self, on: bool) {
        self.engine.session.set_suggestions_enabled(on);
        if on {
            // How much buffer context a request carries is the engine's to
            // slice but the user's to choose, so `[ai]` feeds it in here.
            self.engine.session.suggest.context = ContextWindow {
                before: self.config.ai.context_before,
                after: self.config.ai.context_after,
            };
            self.ensure_ai();
            self.ai_idle_since = Some(Instant::now());
        } else {
            self.ai = None;
            self.ai_idle_since = None;
        }
    }

    /// The worker, started if this is the first time it's needed.
    fn ensure_ai(&mut self) -> &Completer {
        self.ai
            .get_or_insert_with(|| Completer::new(self.config.ai.clone()))
    }

    /// The model's status, or `None` when suggestions have never been on.
    pub fn ai_status(&self) -> Option<AiStatus> {
        self.ai.as_ref().map(|c| c.status())
    }

    /// A short status-line marker for the model's state, or `None` when there
    /// is nothing worth the space.
    pub fn ai_badge(&self) -> Option<&'static str> {
        // "Thinking" is the engine's business (a request is out) rather than
        // the worker's, because the worker is still `Ready` between the moment
        // it finishes and the moment the reply is collected.
        if self.engine.session.suggest.is_pending() {
            return Some("AI …");
        }
        // Armed but never loaded still gets a marker: the user switched the
        // feature on and should be able to see that it's on, even before the
        // first completion has given the worker anything to load for.
        Some(self.ai.as_ref()?.status().badge().unwrap_or("AI"))
    }

    /// The ghost text to draw, if any.
    pub fn suggestion(&self) -> Option<&Suggestion> {
        self.engine.session.suggest.current()
    }

    /// Note that the editor changed, restarting the idle countdown.
    fn touch_ai(&mut self) {
        if self.engine.session.suggest.enabled {
            self.ai_idle_since = Some(Instant::now());
        }
    }

    fn debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.ai.debounce_ms)
    }

    /// Drive the completion worker: issue a request once typing has paused, and
    /// install anything that came back. Called once per main-loop turn, next to
    /// [`poll_jobs`](Self::poll_jobs).
    ///
    /// Returns whether anything changed, so the caller knows to redraw.
    pub fn poll_ai(&mut self) -> bool {
        if self.ai.is_none() {
            return false;
        }
        let mut changed = false;

        // Drained into a Vec first, rather than handled inside the loop: acting
        // on a reply mutates the app (installing ghost text, opening the error
        // panel), which can't happen while the worker is still borrowed.
        // Collecting also means a reply that landed during the last frame is
        // shown now, before anything decides to ask again.
        let replies: Vec<ctrlvim_ai::Reply> = {
            let ai = self.ai.as_ref().expect("checked above");
            std::iter::from_fn(|| ai.poll()).collect()
        };
        for reply in replies {
            match reply.result {
                Ok(text) if !text.is_empty() => {
                    changed |= self.engine.session.fulfill_suggestion(reply.seq, &text);
                }
                Ok(_) => self.engine.session.fail_suggestion(reply.seq),
                Err(e) => {
                    self.engine.session.fail_suggestion(reply.seq);
                    self.show_ai_error(&e);
                    changed = true;
                }
            }
        }

        // Only ask once typing has actually paused, and only while a file
        // buffer has focus — a completion for a buffer the user has navigated
        // away from is wasted work.
        let idle = self
            .ai_idle_since
            .is_some_and(|since| since.elapsed() >= self.debounce());
        if idle && self.editor_focus() {
            if let Some(req) = self.engine.session.suggest_request() {
                if let Some(ai) = self.ai.as_ref() {
                    ai.submit(AiRequest {
                        seq: req.seq,
                        prefix: req.prefix,
                        suffix: req.suffix,
                        filename: req.filename,
                    });
                }
                changed = true;
            }
        }
        changed
    }

    /// How long the main loop may block waiting for a key.
    ///
    /// Normally a quarter second is plenty. While inline suggestions are armed
    /// it has to be shorter than the debounce, or the request would be issued a
    /// poll interval late — visibly so, on top of an already slow model. And
    /// while a mapping is half-typed it must not outlast `'timeoutlen'`, or the
    /// chord would resolve late by however much the poll overshot.
    pub fn poll_interval(&self) -> std::time::Duration {
        let idle = std::time::Duration::from_millis(250);
        let mut wait = if self.ai.is_none() {
            idle
        } else {
            (self.debounce() / 3).clamp(std::time::Duration::from_millis(25), idle)
        };
        if let Some(remaining) = self.keymap_timeout_remaining() {
            wait = wait.min(remaining);
        }
        wait
    }

    /// Time left on the `'timeoutlen'` clock, or `None` when no mapping is
    /// half-typed. Zero means it has already expired.
    fn keymap_timeout_remaining(&self) -> Option<std::time::Duration> {
        let started = self.keymap_pending_since?;
        let len = self.engine.session.timeoutlen();
        Some(len.saturating_sub(started.elapsed()))
    }

    /// Resolve a half-typed mapping whose `'timeoutlen'` has run out.
    ///
    /// Called from the event loop on every tick. An ambiguous chord
    /// (`<leader>q` with `<leader>qq` also mapped) fires here; a prefix that
    /// was never completed is replayed literally so the keys aren't lost.
    pub fn tick_keymap_timeout(&mut self) {
        if self.keymap_timeout_remaining() != Some(std::time::Duration::ZERO) {
            return;
        }
        self.keymap_pending_since = None;
        // The shell keeps its own buffer, since its keys never reach the
        // engine; an ambiguous chord there resolves to the shorter mapping the
        // same way it would in the editor.
        if !self.shell_map_pending.is_empty() {
            let pending = std::mem::take(&mut self.shell_map_pending);
            if let ctrlvim_core::KeymapMatch::FullAmbiguous(rhs) =
                self.engine.session.keymap.match_mode(MapMode::Normal, &pending)
            {
                self.run_mapped_ex(&rhs);
            }
        } else {
            self.engine.session.keymap_timeout();
            self.apply_effects();
        }
        // The chord resolved, so whatever the popup was offering is stale.
        self.which_key = Vec::new();
    }

    /// Run a user's mapping from the *shell* (dashboard, plugin manager) —
    /// where there is no editor buffer to feed keys into.
    ///
    /// The shell used to reimplement a cut-down leader machine of its own, so
    /// only `<leader>1-9`, `<leader>d` and `<leader>S` worked there and a
    /// user's `[[keymap]]` entries were silently editor-only. This consults the
    /// one real table instead. Keys aren't fed to the engine — the session's
    /// buffer isn't what's on screen — so only mappings that expand to an Ex
    /// command mean anything here; the rest fall through to the shell's own
    /// navigation keys.
    ///
    /// Returns whether the key was consumed.
    pub fn shell_keymap(&mut self, key: Key) -> bool {
        let km = &self.engine.session.keymap;
        if self.shell_map_pending.is_empty() && !km.can_start(MapMode::Normal, key) {
            return false;
        }
        self.shell_map_pending.push(key);
        match km.match_mode(MapMode::Normal, &self.shell_map_pending) {
            ctrlvim_core::KeymapMatch::Full(rhs) => {
                self.shell_map_pending.clear();
                self.sync_shell_pending();
                self.run_mapped_ex(&rhs);
                true
            }
            ctrlvim_core::KeymapMatch::FullAmbiguous(_) | ctrlvim_core::KeymapMatch::Prefix => {
                self.sync_shell_pending();
                true
            }
            ctrlvim_core::KeymapMatch::None => {
                // Not a mapping after all. Drop the buffered keys and let the
                // shell's own handler have this one.
                self.shell_map_pending.clear();
                self.sync_shell_pending();
                false
            }
        }
    }

    /// Run a mapping's right-hand side in the shell, which can only honour the
    /// Ex-command form (`:Files<CR>`). Anything else would need a buffer.
    fn run_mapped_ex(&mut self, rhs: &[Key]) {
        let text = ctrlvim_core::keys_notation(rhs);
        let Some(cmd) = text.strip_prefix(':') else { return };
        let cmd = cmd.strip_suffix("<CR>").unwrap_or(cmd);
        if !cmd.is_empty() {
            self.run_ex_command(cmd);
        }
    }

    /// Mirror of [`sync_keymap_pending`](Self::sync_keymap_pending) for the
    /// shell's own pending buffer.
    fn sync_shell_pending(&mut self) {
        if self.shell_map_pending.is_empty() {
            self.keymap_pending_since = None;
            self.which_key = Vec::new();
        } else {
            self.keymap_pending_since = Some(std::time::Instant::now());
            self.which_key = self
                .engine
                .session
                .keymap
                .continuations(MapMode::Normal, &self.shell_map_pending);
        }
    }

    /// Refresh the `'timeoutlen'` clock and the which-key popup after feeding a
    /// key. Called wherever keys reach the engine.
    fn sync_keymap_pending(&mut self) {
        if self.engine.session.keymap_pending() {
            // Restart the clock on every key: a chord in progress gets a full
            // `'timeoutlen'` for its *next* key, as in Vim.
            self.keymap_pending_since = Some(std::time::Instant::now());
            self.which_key = self.engine.session.keymap_continuations();
        } else {
            self.keymap_pending_since = None;
            self.which_key = Vec::new();
        }
    }

    /// Host side of the tag commands: read the tags file, open the file a tag
    /// points at, and place the cursor. The table, the stack, and the search
    /// itself stay in the engine.
    fn host_tag(&mut self, cmd: TagCmd) {
        match cmd {
            TagCmd::Lookup { name } => {
                self.load_tags_if_changed();
                if self.engine.session.tags().is_empty() {
                    self.message = "E433: no tags file (run `ctags -R .`)".into();
                    return;
                }
                let from = self
                    .active_buffer()
                    .path
                    .as_ref()
                    .map(|p| crate::data::relative_to(&self.root, p))
                    .unwrap_or_default();
                match self.engine.session.select_tag(&name, &from) {
                    Some(tag) => {
                        let total = self.engine.session.tag_match_count();
                        self.tag_goto(&tag.path, &tag.address);
                        if total > 1 {
                            self.message = format!("tag 1 of {total}");
                        }
                    }
                    None => self.message = format!("E426: tag not found: {name}"),
                }
            }
            TagCmd::Jump { path, address } => self.tag_goto(&path, &address),
            TagCmd::Return { path, line, col } => {
                if !path.is_empty() {
                    let full = self.root.join(&path);
                    let name = file_name_of(&full);
                    self.open_path(full, name);
                }
                self.engine.session.set_cursor_clamped(line, col);
            }
        }
    }

    /// Open a tag's file and put the cursor on its definition. A pattern
    /// address is resolved against the file *as it is now*, so a definition
    /// that moved since `ctags` ran is still found.
    fn tag_goto(&mut self, path: &str, address: &TagAddress) {
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let name = file_name_of(&full);
        self.open_path(full, name);
        let lines = self.editor_lines();
        match ctrlvim_core::resolve_tag_address(address, &lines) {
            Some(line) => self.engine.session.set_cursor_clamped(line, 0),
            None => self.message = "E434: tag pattern not found".into(),
        }
    }

    /// Load the tags file if it appeared or changed since the last load.
    ///
    /// Checking the mtime on each lookup means running `ctags -R .` in another
    /// terminal takes effect immediately, without a reload command.
    fn load_tags_if_changed(&mut self) {
        let path = self.root.join("tags");
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if stamp.is_none() {
            return;
        }
        if stamp == self.tags_loaded_at {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.engine.session.set_tags(ctrlvim_core::TagTable::parse(&text));
            self.tags_loaded_at = stamp;
        }
    }

    /// Host side of the quickfix commands: the engine owns the list, the
    /// frontend owns the filesystem, the process, and the pane.
    fn host_quickfix(&mut self, cmd: QuickfixCmd) {
        match cmd {
            QuickfixCmd::Open => self.quickfix_open = !self.engine.session.quickfix().is_empty(),
            QuickfixCmd::Close => self.quickfix_open = false,
            QuickfixCmd::Jump { path, line, col } => self.quickfix_goto(&path, line, col),
            QuickfixCmd::Grep { pattern, glob } => self.host_vimgrep(&pattern, glob.as_deref()),
            QuickfixCmd::Run { program, args, title } => self.host_run_job(program, args, title),
        }
    }

    /// Open a quickfix entry's file and put the cursor on the match.
    fn quickfix_goto(&mut self, path: &str, line: usize, col: usize) {
        let path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.open_path(path, name);
        self.engine.session.set_cursor_clamped(line, col);
    }

    /// `:vimgrep` — walk the project and match each file's contents. The walk
    /// and the reads are the host's (the engine never touches the filesystem);
    /// what counts as a match is the engine's, via [`Matcher`].
    fn host_vimgrep(&mut self, pattern: &str, glob: Option<&str>) {
        let matcher = match Matcher::new(pattern) {
            Ok(m) => m,
            Err(e) => {
                self.message = e;
                return;
            }
        };
        let mut items = Vec::new();
        for path in crate::data::walk_project(&self.root) {
            let rel = crate::data::relative_to(&self.root, &path);
            if let Some(glob) = glob {
                if !glob_match(&rel, glob) {
                    continue;
                }
            }
            // Unreadable or binary files are skipped, not reported.
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            items.extend(grep_text(&matcher, Path::new(&rel), &text));
        }
        let title = format!(":vimgrep /{pattern}/");
        self.finish_quickfix(items, title);
    }

    /// `:make` / `:grep` — spawn the program and collect its output into the
    /// list as it streams (see [`App::poll_jobs`]).
    fn host_run_job(&mut self, program: String, args: Vec<String>, title: String) {
        let root = self.root.clone();
        let jobs = match self.jobs_mut() {
            Some(jobs) => jobs,
            None => {
                self.message = "E902: could not start the job runtime".into();
                return;
            }
        };
        let id = jobs.spawn(&program, &args, &root);
        self.message = format!("{title}: running…");
        self.job = Some(RunningJob {
            id,
            title,
            lines: LineBuffer::new(),
            parser: OutputParser::new(),
            items: Vec::new(),
        });
    }

    /// `:!{cmd}` — run a raw command line through the configured shell
    /// ([`Config::shell`]) and collect its output for the overlay (see
    /// [`App::poll_jobs`]).
    fn host_run_shell(&mut self, command: String) {
        let root = self.root.clone();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
        let jobs = match self.jobs_mut() {
            Some(jobs) => jobs,
            None => {
                self.message = "E902: could not start the job runtime".into();
                return;
            }
        };
        let id = jobs.spawn_shell(&shell, &command, &root);
        self.message = format!(":!{command} — running…");
        self.shell_job = Some(RunningShell { id, command, lines: LineBuffer::new(), output: Vec::new() });
    }

    /// Install a finished `:!{cmd}` job's output and open the overlay.
    fn finish_shell(&mut self, job: RunningShell, code: i64) {
        self.shell_title = format!(":!{} (exit {code})", job.command);
        self.shell_output = if job.output.is_empty() {
            vec!["(no output)".to_string()]
        } else {
            job.output.iter().map(|l| sanitize_for_display(l)).collect()
        };
        self.shell_scroll = 0;
        self.shell_open = true;
        self.message = format!("{}: exit {code}", job.command);

        // Any shell command may have moved the repo underneath us — `[F] fetch`
        // certainly did, and so does a `:!git commit`. Re-reading here is what
        // keeps the panel from showing pre-command state until restart; it is a
        // handful of git invocations, and only on a command the user ran.
        self.project.git = crate::data::reload_git(&self.root);

        if let Some((name, index, id)) = self.installing_tool.take() {
            if id != job.id {
                // A `:!{cmd}` finished first; the install is still running.
                self.installing_tool = Some((name, index, id));
                return;
            }
            self.shell_title = format!("install {name} (exit {code})");
            if let Some(lsp) = self.project.lsp.get_mut(index).filter(|l| l.name == name) {
                if let Some(tool) = ctrlvim_tools::REGISTRY.iter().find(|t| t.name == name) {
                    let (installed, managed) = ctrlvim_tools::installed_and_managed(tool);
                    lsp.installed = installed;
                    lsp.managed = managed;
                }
            }
            self.message = if code == 0 {
                format!("{name}: installed")
            } else {
                format!("{name}: install failed (exit {code}) — see the output overlay")
            };
        }
    }

    /// Scroll the `:!{cmd}` output overlay by `dir` lines (`j`/`k`, wheel).
    pub fn scroll_shell_output(&mut self, dir: i32) {
        let max = self.shell_output.len().saturating_sub(1) as i32;
        self.shell_scroll = (self.shell_scroll as i32 + dir).clamp(0, max) as usize;
    }

    /// The job runtime, created on first use so a session that never runs a
    /// job never pays for a tokio runtime.
    fn jobs_mut(&mut self) -> Option<&mut Jobs> {
        if self.jobs.is_none() {
            let timers = TimerService::new(self.events.sender()).ok()?;
            self.jobs = Some(Jobs::new(timers.runtime().handle().clone(), self.events.sender()));
            self.timers = Some(timers);
        }
        self.jobs.as_mut()
    }

    /// Drain background job output. Called from the main loop's poll tick so a
    /// long build streams in without blocking keystrokes.
    ///
    /// Returns true when something changed and the screen needs a repaint.
    pub fn poll_jobs(&mut self) -> bool {
        let events = self.events.drain();
        if events.is_empty() {
            return false;
        }
        let mut finished = None;
        let mut shell_finished = None;
        for event in events {
            match event {
                Event::ProcessOutput { id, data } => {
                    if let Some(job) = self.job.as_mut().filter(|j| j.id == id) {
                        for line in job.lines.push(&data) {
                            if let Some(item) = job.parser.push(&line) {
                                job.items.push(item);
                            }
                        }
                        continue;
                    }
                    if let Some(job) = self.shell_job.as_mut().filter(|j| j.id == id) {
                        let lines = job.lines.push(&data);
                        job.output.extend(lines);
                    }
                }
                Event::ProcessExit { id, code } => {
                    if let Some(job) = self.job.as_mut().filter(|j| j.id == id) {
                        if let Some(last) = job.lines.flush() {
                            if let Some(item) = job.parser.push(&last) {
                                job.items.push(item);
                            }
                        }
                        finished = self.job.take().map(|j| (j, code));
                        continue;
                    }
                    if let Some(job) = self.shell_job.as_mut().filter(|j| j.id == id) {
                        if let Some(last) = job.lines.flush() {
                            job.output.push(last);
                        }
                        shell_finished = self.shell_job.take().map(|j| (j, code));
                    }
                }
                // Timers/RPC are not wired into the frontend yet.
                _ => {}
            }
        }
        if let Some((job, code)) = finished {
            let title = format!("{} (exit {code})", job.title);
            self.finish_quickfix(job.items, title);
        }
        if let Some((job, code)) = shell_finished {
            self.finish_shell(job, code);
        }
        true
    }

    /// Store a freshly-built list on the engine and report what came back.
    fn finish_quickfix(&mut self, items: Vec<QfItem>, title: String) {
        let n = items.len();
        self.engine.session.set_quickfix(items, title.clone());
        if n == 0 {
            self.quickfix_open = false;
            self.message = format!("{title}: no matches");
        } else {
            self.quickfix_open = true;
            self.quickfix_index = 0;
            self.message = format!("{title}: {n} entries");
        }
    }

    /// Select entry `i` and jump to it — clicking a row, or `j`/`k` + Enter.
    pub fn quickfix_select(&mut self, i: usize) {
        if let Some(item) = self.engine.session.quickfix_select(i) {
            self.quickfix_index = i;
            let path = item.path.to_string_lossy().into_owned();
            self.quickfix_goto(&path, item.line, item.col);
        }
    }

    /// Move the quickfix selection without jumping (the pane's `j`/`k`).
    pub fn move_quickfix_selection(&mut self, dir: i32) {
        let n = self.engine.session.quickfix().len();
        if n == 0 {
            return;
        }
        let i = self.quickfix_index as i32;
        self.quickfix_index = (((i + dir) % n as i32 + n as i32) % n as i32) as usize;
    }

    /// Run a line of Vimscript (`:let`/`:echo`/…) and surface `:echo` output or
    /// an error on the command line.
    fn host_vimscript(&mut self, src: &str) {
        match self.engine.run_vimscript(src) {
            Ok(out) if !out.is_empty() => self.message = out.join(" "),
            Ok(_) => {}
            Err(e) => self.message = first_line(&e),
        }
    }

    /// `:source {file}` / `:luafile` — read a script (relative to the project
    /// root unless absolute) and run it as Lua (`.lua`) or Vimscript.
    fn host_source(&mut self, rel: &str) {
        let rel = rel.trim();
        if rel.is_empty() {
            self.message = "E471: Argument required".into();
            return;
        }
        let path = if Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            self.root.join(rel)
        };
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.message = format!("E484: Can't open file {rel}: {e}");
                return;
            }
        };
        if path.extension().and_then(|e| e.to_str()) == Some("lua") {
            if let Err(e) = self.engine.run_lua(&contents) {
                self.message = format!("E5108: {}", first_line(&e));
            }
        } else {
            match self.engine.run_vimscript(&contents) {
                Ok(out) if !out.is_empty() => self.message = out.join(" "),
                Ok(_) => {}
                Err(e) => self.message = first_line(&e),
            }
        }
    }

    /// `:colorscheme [name]` — switch the theme (persisting it), or report the
    /// current one when no name is given.
    fn host_colorscheme(&mut self, name: Option<String>) {
        match name {
            None => self.message = format!("colorscheme {}", crate::theme::current().name),
            Some(name) => {
                if let Some((i, _)) = crate::theme::ALL
                    .iter()
                    .enumerate()
                    .find(|(_, t)| t.name.eq_ignore_ascii_case(&name))
                {
                    self.set_theme(i);
                } else {
                    self.message = format!("E185: Cannot find color scheme '{name}'");
                }
            }
        }
    }

    /// Buffer/tab list navigation (`:bnext`, `:b N`, `:bd`, `:only`, `:ls`).
    fn host_buffer_cmd(&mut self, cmd: BufferCmd) {
        match cmd {
            BufferCmd::Next => self.cycle_buffer(1),
            BufferCmd::Prev => self.cycle_buffer(-1),
            BufferCmd::First => self.set_active(0),
            BufferCmd::Last => self.set_active(self.buffers.len().saturating_sub(1)),
            BufferCmd::Goto(n) => {
                if n >= 1 && n <= self.buffers.len() {
                    self.set_active(n - 1);
                } else {
                    self.message = format!("E86: Buffer {n} does not exist");
                }
            }
            BufferCmd::Delete(which) => {
                let idx = which.map(|n| n.saturating_sub(1)).unwrap_or(self.active);
                self.close_buffer(idx);
            }
            BufferCmd::Only => {
                // Close every closable buffer except the active one.
                let keep = self.active;
                let keep_id = self.buffers.get(keep).map(|b| b.label.clone());
                self.buffers.retain(|b| {
                    !b.closable() || Some(&b.label) == keep_id.as_ref()
                });
                self.active = self
                    .buffers
                    .iter()
                    .position(|b| Some(&b.label) == keep_id.as_ref())
                    .unwrap_or(0);
                self.load_active_into_engine();
            }
            BufferCmd::List => {
                let list: Vec<String> = self
                    .buffers
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let mark = if i == self.active { "%" } else { " " };
                        format!("{}{mark} {}", i + 1, b.label)
                    })
                    .collect();
                self.message = list.join("  ");
            }
        }
    }

    /// `:w {file}` / `:saveas {file}` — write the active buffer's text to a new
    /// path (relative to the project root unless absolute) and adopt it.
    fn host_write_as(&mut self, rel: &str) -> bool {
        if !self.is_file() {
            self.message = "E382: Cannot write, no file for this buffer".into();
            return false;
        }
        let path = if Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            self.root.join(rel)
        };
        self.snapshot_active();
        let mut body = self.buffers[self.active].text.join("\n");
        body.push('\n');
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, body) {
            Ok(()) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                // Adopt the new path/name (like Vim's :saveas).
                self.buffers[self.active].path = Some(path);
                self.buffers[self.active].label = name.clone();
                self.buffers[self.active].modified = false;
                self.engine.set_modified(false);
                self.message = format!("\"{name}\" written");
                true
            }
            Err(e) => {
                self.message = format!("E212: Can't open file for writing: {e}");
                false
            }
        }
    }

    /// Write every modified file buffer to disk. Returns how many were written.
    fn host_write_all(&mut self) -> usize {
        self.snapshot_active();
        let mut written = 0;
        for i in 0..self.buffers.len() {
            if self.buffers[i].kind != BufferKind::File || !self.buffers[i].modified {
                continue;
            }
            let Some(path) = self.buffers[i].path.clone() else { continue };
            let mut body = self.buffers[i].text.join("\n");
            body.push('\n');
            if std::fs::write(&path, body).is_ok() {
                self.buffers[i].modified = false;
                if i == self.active {
                    self.engine.set_modified(false);
                }
                written += 1;
            }
        }
        written
    }

    /// `:qa[!]` — quit the whole editor. Refused (unless forced) when any buffer
    /// has unsaved changes.
    fn host_quit_all(&mut self, force: bool) {
        self.snapshot_active();
        if !force && self.buffers.iter().any(|b| b.modified) {
            self.message = "E37: No write since last change (add ! to override)".into();
            return;
        }
        self.should_quit = true;
    }

    /// Write the active file buffer to disk. Returns whether it succeeded and
    /// sets a status message either way.
    fn host_write(&mut self) -> bool {
        if !self.is_file() {
            self.message = "E382: Cannot write, no file for this buffer".into();
            return false;
        }
        // `BufWritePre` runs before the text is captured, so a formatter
        // autocmd's edits are part of what gets written.
        self.fire_autocmd("BufWritePre");
        // Sync the engine's live text into the buffer cache, then persist it.
        self.snapshot_active();
        let Some(path) = self.buffers[self.active].path.clone() else {
            // Unnamed buffer (`:enew`/New File): ask for a name, then save.
            self.open_save_prompt();
            return false;
        };
        let mut body = self.buffers[self.active].text.join("\n");
        body.push('\n'); // POSIX trailing newline
        match std::fs::write(&path, body) {
            Ok(()) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.message = format!("\"{name}\" {}L written", self.buffers[self.active].text.len());
                self.fire_autocmd("BufWritePost");
                true
            }
            Err(e) => {
                self.message = format!("E212: Can't open file for writing: {e}");
                false
            }
        }
    }

    /// Quit the active window: close the buffer, or quit the app if it's the
    /// last one (or the dashboard).
    fn host_quit(&mut self) {
        if self.is_dashboard() || self.buffers.len() <= 1 {
            self.should_quit = true;
        } else {
            self.close_buffer(self.active);
        }
    }

    /// The engine's cursor as 0-based `(line, col)`.
    pub fn editor_cursor(&self) -> (usize, usize) {
        let p = self.engine.session.cursor();
        (p.line, p.col)
    }

    /// The editing lines to render for the active file buffer.
    pub fn editor_lines(&self) -> Vec<String> {
        self.engine.lines()
    }

    /// Editor mode is engine-owned; `"n"`, `"i"`, `"v"`, `"V"`, or `"\x16"`.
    pub fn editor_mode(&self) -> &'static str {
        self.engine.mode()
    }

    /// The active visual selection to highlight, or `None` outside Visual mode.
    pub fn editor_selection(&self) -> Option<Selection> {
        self.engine.selection()
    }

    /// The active buffer's filetype, when it's one the engine can highlight.
    pub fn editor_filetype(&self) -> Option<Filetype> {
        let b = self.active_buffer();
        if !matches!(b.kind, BufferKind::File) {
            return None;
        }
        let name = b.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| b.label.clone());
        Filetype::from_path(&name)
    }

    /// Tree-sitter highlight spans for rows `[top, top + rows)` of the active
    /// buffer — empty when the filetype has no grammar, or when live markdown
    /// rendering already owns the buffer's styling.
    ///
    /// Only the visible window is highlighted, and the result is cached until
    /// the text (or the window) changes, so an idle frame costs a hash of the
    /// buffer rather than a re-parse.
    pub fn editor_highlights(&self, lines: &[String], top: usize, rows: usize) -> Vec<Vec<HlSpan>> {
        let Some(ft) = self.editor_filetype() else { return Vec::new() };
        if self.md_render_active() {
            return Vec::new();
        }
        let key = (self.active, text_hash(lines), top, rows);
        let mut cache = self.syntax.borrow_mut();
        if cache.as_ref().map(|c| c.key) != Some(key) {
            let spans = syntax::highlight_window(ft, &lines.join("\n"), top, top + rows);
            *cache = Some(SyntaxCache { key, lines: spans });
        }
        cache.as_ref().expect("just populated").lines.clone()
    }

    /// The active window's folds.
    pub fn folds(&self) -> &ctrlvim_core::Folds {
        self.engine.session.folds()
    }

    /// The closed fold whose *head* is `line`, i.e. the one this row should draw
    /// a summary for instead of the line's text.
    pub fn fold_head_at(&self, line: usize) -> Option<&ctrlvim_core::Fold> {
        self.folds().closed_at(line).filter(|f| f.start == line)
    }

    /// The buffer lines a viewport of `rows` rows shows, starting at screen row
    /// `top`. With no closed folds this is just `top..top + rows`; with them,
    /// hidden lines are skipped, so **the renderer must index lines through this
    /// rather than assuming row == line**.
    pub fn visible_lines(&self, top: usize, rows: usize, line_count: usize) -> Vec<usize> {
        let folds = self.folds();
        let mut out = Vec::with_capacity(rows);
        let mut line = folds.buffer_line_of(top, line_count);
        for _ in 0..rows {
            if line >= line_count {
                break;
            }
            out.push(line);
            match folds.next_visible(line, 1, line_count) {
                Some(next) => line = next,
                None => break,
            }
        }
        out
    }

    /// The screen row a buffer line draws on (its fold's row when hidden).
    pub fn screen_line_of(&self, line: usize) -> usize {
        self.folds().screen_line_of(line)
    }

    /// `hlsearch` match column ranges on `line` (empty when highlighting is off).
    pub fn editor_search_matches(&self, line: usize) -> Vec<(usize, usize)> {
        self.engine.search_line_matches(line)
    }

    /// True when a File buffer is focused and no overlay is capturing input —
    /// i.e. keystrokes should drive the editor.
    pub fn editor_focus(&self) -> bool {
        self.is_file()
            && !self.palette_open
            && !self.help_open
            && !self.sidebar_visible
            && self.finder.is_none()
            && self.save_prompt.is_none()
    }

    // --- command palette ---------------------------------------------------

    /// The command palette is the unified command line: it lists **commands**
    /// (never files — file finding is the finder / the `Find File` entry) and
    /// fuzzy-filters them against the typed query. It opens with `:`.
    ///
    /// Every recognized Ex command shows up here (see `ctrlvim_core::ex_commands`,
    /// itself the single source of truth `is_ex_command` also reads from), plus
    /// anything defined at runtime: `:command`-defined user commands and
    /// Lua-registered plugin commands (`ctrlvim_create_user_command`) — a
    /// command can't be runnable without also being listed, the same way a
    /// VS Code extension's commands all land in one palette regardless of
    /// where they came from.
    pub fn palette_results(&self) -> Vec<PaletteItem> {
        let q = &self.palette_query;
        let mut items: Vec<PaletteItem> = Vec::new();

        // Engine-defined Ex commands (`:w`, `:q`, …). The catalog and execution
        // both live in the engine; the palette is only a nicer entry point.
        for cmd in ctrlvim_core::ex_commands() {
            items.push(PaletteItem {
                label: format!(":{}", cmd.name),
                hint: cmd.desc.to_string(),
                icon_color: crate::theme::green(),
                icon_letter: ':',
                action: Action::RunEx(cmd.name.to_string()),
            });
        }

        // User-defined commands (`:command Name expansion`) — running one
        // goes back through the same `:name` path as a built-in, since
        // `Session::execute_ex` checks user commands before its own table.
        let mut user_cmds: Vec<(String, String)> =
            self.engine.session.user_commands().map(|(name, repl)| (name.to_string(), repl.to_string())).collect();
        user_cmds.sort();
        for (name, repl) in user_cmds {
            items.push(PaletteItem {
                label: format!(":{name}"),
                hint: format!("user command → {repl}"),
                icon_color: crate::theme::red(),
                icon_letter: 'U',
                action: Action::RunEx(name),
            });
        }

        // Plugin-registered commands (`vim.api.ctrlvim_create_user_command`) —
        // a Lua plugin's commands get the same palette visibility as the
        // engine's own, closing the gap a plugin manager would otherwise need.
        let mut plugin_cmds = self.engine.plugin_commands();
        plugin_cmds.sort();
        for (name, desc, source) in plugin_cmds {
            let hint = match (desc.is_empty(), &source) {
                (false, Some(src)) => format!("{desc} — {src}"),
                (false, None) => desc,
                (true, Some(src)) => format!("plugin command — {src}"),
                (true, None) => "plugin command".to_string(),
            };
            items.push(PaletteItem {
                label: format!(":{name}"),
                hint,
                icon_color: crate::theme::orange(),
                // 'L' (Lua) rather than 'P' -- the "Plugin Manager" action
                // below is a distinct entry and shouldn't share a letter with
                // an individual plugin-registered command.
                icon_letter: 'L',
                action: Action::RunPluginCommand(name),
            });
        }

        // Frontend actions.
        if self.active_is_markdown() {
            let (label, letter) = if self.md_render_active() {
                ("Markdown: Show Raw Source".to_string(), 'M')
            } else {
                ("Markdown: Live Render".to_string(), 'M')
            };
            items.push(PaletteItem { label, hint: "toggle markdown render".to_string(), icon_color: crate::theme::purple(), icon_letter: letter, action: Action::ToggleMarkdown });
        }
        items.push(PaletteItem { label: "Find File".into(), hint: "fuzzy file browser".to_string(), icon_color: crate::theme::blue(), icon_letter: 'F', action: Action::OpenFinder });
        items.push(PaletteItem { label: "Plugin Manager".into(), hint: "manage plugins".to_string(), icon_color: crate::theme::orange(), icon_letter: 'P', action: Action::OpenPlugins });
        if self.config.drawer {
            items.push(PaletteItem { label: "Toggle Sidebar".into(), hint: "file drawer".to_string(), icon_color: crate::theme::cyan(), icon_letter: 'S', action: Action::ToggleSidebar });
        }

        // Theme switching (one entry per registered theme).
        for (i, t) in crate::theme::ALL.iter().enumerate() {
            items.push(PaletteItem {
                label: format!("Theme: {}", t.name),
                hint: "color theme".to_string(),
                icon_color: crate::theme::purple(),
                icon_letter: 'T',
                action: Action::SetTheme(i),
            });
        }

        if q.trim().is_empty() {
            items
        } else {
            items
                .into_iter()
                .filter(|it| fuzzy_match(q, &format!("{} {}", it.label, it.hint)))
                .collect()
        }
    }

    pub fn move_palette(&mut self, dir: i32) {
        let len = self.palette_results().len().max(1) as i32;
        let i = self.palette_index as i32;
        self.palette_index = (((i + dir) % len + len) % len) as usize;
    }

    pub fn palette_type(&mut self, c: char) {
        self.palette_query.push(c);
        self.palette_index = 0;
    }

    pub fn palette_backspace(&mut self) {
        self.palette_query.pop();
        self.palette_index = 0;
    }

    pub fn palette_word_backspace(&mut self) {
        delete_word_backward(&mut self.palette_query);
        self.palette_index = 0;
    }

    pub fn palette_clear_to_start(&mut self) {
        self.palette_query.clear();
        self.palette_index = 0;
    }

    fn run_palette(&mut self, idx: usize) {
        let results = self.palette_results();
        if let Some(item) = results.get(idx) {
            let action = item.action.clone();
            self.dispatch(action);
        }
        self.close_palette();
    }

    fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_index = 0;
    }

    /// Confirm the command line (`Enter`).
    ///
    /// A bare command word (letters only — no range, bang, or args) whose
    /// *highlighted* suggestion actually names that command runs whatever's
    /// on screen: what you see focused is what Enter does, so `:cl` picking
    /// `:close` out of the list executes `close`, not some other command
    /// that happens to share the `cl` abbreviation (`:clist`). If the
    /// highlighted row is only a loose/incidental fuzzy hit (matched via its
    /// description, not its name), it isn't trusted as "the" selection.
    ///
    /// Otherwise a recognized Ex command (ranges, `:s/../../`, `!`-args, or a
    /// real command not offered in the palette like `:clist`/`:cc`) runs
    /// verbatim. Failing that, the highlighted fuzzy item runs (themes, Find
    /// File, …); with no match at all, the raw text runs as a freeform Ex
    /// command (`:42`, `:$`, unknown → E492).
    pub fn submit_palette(&mut self) {
        let q = self.palette_query.trim().to_string();
        let results = self.palette_results();
        let bare_word = !q.is_empty() && q.chars().all(|c| c.is_ascii_alphabetic());
        let highlighted_is_strong = results
            .get(self.palette_index.min(results.len().saturating_sub(1)))
            .is_some_and(|item| item.label.trim_start_matches(':').to_lowercase().starts_with(&q.to_lowercase()));

        if bare_word && highlighted_is_strong {
            let idx = self.palette_index.min(results.len() - 1);
            self.run_palette(idx);
            return;
        }
        // A recognized Ex command (incl. ranges/`:s`/`:noh`/…) runs verbatim,
        // so short command names aren't hijacked by a fuzzy palette entry.
        if !q.is_empty() && ctrlvim_core::is_ex_command(&q) {
            self.close_palette();
            self.run_ex_command(&q);
            return;
        }
        // Otherwise pick from the fuzzy list (themes, Find File, …).
        if !results.is_empty() {
            let idx = self.palette_index.min(results.len() - 1);
            self.run_palette(idx);
            return;
        }
        self.close_palette();
        if !q.is_empty() {
            self.run_ex_command(&q);
        }
    }

    // --- fuzzy file browser (finder) --------------------------------------

    /// Open the full-screen file browser, rooted at the active file's directory
    /// (or the project root for non-file buffers).
    pub fn open_finder(&mut self) {
        let dir = self
            .active_buffer()
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        self.finder = Some(Finder { entries: list_dir(&dir), dir, query: String::new(), selected: 0 });
    }

    pub fn close_finder(&mut self) {
        self.finder = None;
    }

    /// Entry indices (into `finder.entries`) that match the current query, as a
    /// case-insensitive subsequence of the entry name.
    pub fn finder_matches(&self) -> Vec<usize> {
        let Some(f) = &self.finder else { return Vec::new() };
        // In command mode (`:cmd …`), keep the whole listing visible and
        // navigable so a bare `:d` can act on the highlighted row — the `:…`
        // text is a command, not a fuzzy filter.
        let filter = if f.query.starts_with(':') { "" } else { f.query.as_str() };
        f.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| fuzzy_match(filter, &e.name))
            .map(|(i, _)| i)
            .collect()
    }

    /// Parse the prompt as a `:`-prefixed browser command, if it is one.
    pub fn finder_command(&self) -> Option<FinderCommand> {
        let f = self.finder.as_ref()?;
        let rest = f.query.strip_prefix(':')?;
        let mut parts = rest.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("").trim();
        let arg = parts.next().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        match cmd {
            "c" | "create" => Some(FinderCommand::Create(arg)),
            "d" | "delete" => Some(FinderCommand::Delete(arg)),
            "dir" | "mkdir" | "create-directory" => Some(FinderCommand::Mkdir(arg)),
            _ => None,
        }
    }

    pub fn finder_move(&mut self, dir: i32) {
        let n = self.finder_matches().len() as i32;
        if let Some(f) = &mut self.finder {
            if n > 0 {
                f.selected = (((f.selected as i32 + dir) % n + n) % n) as usize;
            }
        }
    }

    pub fn finder_type(&mut self, c: char) {
        if let Some(f) = &mut self.finder {
            f.query.push(c);
            f.selected = 0;
        }
    }

    pub fn finder_backspace(&mut self) {
        if let Some(f) = &mut self.finder {
            f.query.pop();
            f.selected = 0;
        }
    }

    pub fn finder_word_backspace(&mut self) {
        if let Some(f) = &mut self.finder {
            delete_word_backward(&mut f.query);
            f.selected = 0;
        }
    }

    pub fn finder_clear_to_start(&mut self) {
        if let Some(f) = &mut self.finder {
            f.query.clear();
            f.selected = 0;
        }
    }

    /// Activate the selected entry: drill into a directory, or open a file.
    /// When the typed name matches nothing, it's treated as a new file to
    /// create in the current directory (telescope `file_browser` style).
    /// A `:`-prefixed prompt runs a browser command instead (see
    /// [`finder_run_command`](Self::finder_run_command)).
    pub fn finder_select(&mut self) {
        if self.finder.as_ref().is_some_and(|f| f.query.starts_with(':')) {
            self.finder_run_command();
            return;
        }
        let matches = self.finder_matches();
        let Some(f) = &self.finder else { return };
        if matches.is_empty() {
            let name = f.query.trim();
            if name.is_empty() {
                return;
            }
            let path = f.dir.join(name);
            self.finder = None;
            self.create_and_open(path);
            return;
        }
        let Some(&ei) = matches.get(f.selected) else { return };
        let entry = &f.entries[ei];
        if entry.is_dir {
            let dir = entry.path.clone();
            self.finder = Some(Finder { entries: list_dir(&dir), dir, query: String::new(), selected: 0 });
        } else {
            let path = entry.path.clone();
            let name = entry.name.clone();
            self.finder = None;
            self.open_path(path, name);
        }
    }

    /// Run the `:`-prefixed browser command in the prompt: create a file,
    /// create a directory, or delete an entry. Directory operations refresh
    /// the listing in place; creating a file opens it.
    fn finder_run_command(&mut self) {
        let Some(cmd) = self.finder_command() else {
            self.message = "E492: not a browser command (try :c, :d, :dir)".into();
            return;
        };
        let dir = match &self.finder {
            Some(f) => f.dir.clone(),
            None => return,
        };
        // Re-list the current directory and reset the prompt, staying open.
        let refresh = |app: &mut Self, dir: PathBuf| {
            app.finder = Some(Finder { entries: list_dir(&dir), dir, query: String::new(), selected: 0 });
        };
        match cmd {
            FinderCommand::Create(name) => {
                let Some(name) = name else {
                    self.message = "usage: :create <name>".into();
                    return;
                };
                let path = dir.join(name.trim());
                self.finder = None;
                self.create_and_open(path);
            }
            FinderCommand::Mkdir(name) => {
                let Some(name) = name else {
                    self.message = "usage: :dir <name>".into();
                    return;
                };
                let name = name.trim();
                match std::fs::create_dir_all(dir.join(name)) {
                    Ok(()) => {
                        self.message = format!("created directory {name}/");
                        refresh(self, dir);
                    }
                    Err(e) => self.message = format!("E739: cannot create directory: {e}"),
                }
            }
            FinderCommand::Delete(name) => {
                // Target: the explicit name, else the highlighted entry.
                let target = match name {
                    Some(n) => dir.join(n.trim()),
                    None => {
                        let matches = self.finder_matches();
                        let Some(f) = &self.finder else { return };
                        let Some(&ei) = matches.get(f.selected) else {
                            self.message = "nothing highlighted to delete".into();
                            return;
                        };
                        let entry = &f.entries[ei];
                        if entry.name == "../" {
                            self.message = "refusing to delete ../".into();
                            return;
                        }
                        entry.path.clone()
                    }
                };
                let label = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.display().to_string());
                let result = if target.is_dir() {
                    std::fs::remove_dir_all(&target)
                } else {
                    std::fs::remove_file(&target)
                };
                match result {
                    Ok(()) => {
                        self.message = format!("deleted {label}");
                        refresh(self, dir);
                    }
                    Err(e) => self.message = format!("E: cannot delete {label}: {e}"),
                }
            }
        }
    }

    // --- find & replace panel ---------------------------------------------

    /// Open the panel (`:Find`, `<leader>S`) seeded with `pattern`, and run the
    /// first search immediately so a seeded word shows its matches at once.
    pub fn open_replace(&mut self, pattern: Option<String>) {
        self.replace = Some(ReplacePanel::new(pattern));
        self.replace_search();
    }

    /// Re-run the project search for the panel's current pattern.
    ///
    /// This is synchronous, like `:vimgrep` — the walk is bounded by
    /// [`crate::data::walk_project`]'s file cap and the result list by
    /// [`MAX_HITS`], so a keystroke stays a keystroke rather than a job.
    pub fn replace_search(&mut self) {
        let Some(panel) = &self.replace else { return };
        let plan = match panel.plan() {
            Some(Ok(plan)) => plan,
            Some(Err(e)) => {
                if let Some(p) = &mut self.replace {
                    p.set_error(e);
                }
                return;
            }
            // An empty Find field: no results, and no error to shout about.
            None => {
                if let Some(p) = &mut self.replace {
                    p.set_hits(Vec::new(), false);
                }
                return;
            }
        };

        let mut hits = Vec::new();
        let mut truncated = false;
        for path in crate::data::walk_project(&self.root) {
            let rel = crate::data::relative_to(&self.root, &path);
            // Search what the *editor* would show: an open buffer's unsaved text
            // wins over what is on disk, so a match can't point at a stale line.
            let text = match self.open_buffer_text(&path) {
                Some(lines) => lines.join("\n"),
                None => match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    // Unreadable or binary files are skipped, not reported.
                    Err(_) => continue,
                },
            };
            hits.extend(plan.hits_in(Path::new(&rel), &text));
            if hits.len() >= MAX_HITS {
                hits.truncate(MAX_HITS);
                truncated = true;
                break;
            }
        }
        if let Some(p) = &mut self.replace {
            p.set_hits(hits, truncated);
        }
    }

    /// The live text of an open buffer for `path`, if there is one. The active
    /// buffer's lives in the engine; the rest sit in their cached `text`.
    fn open_buffer_text(&self, path: &Path) -> Option<Vec<String>> {
        let i = self.buffers.iter().position(|b| b.path.as_deref() == Some(path))?;
        Some(if i == self.active { self.engine.lines() } else { self.buffers[i].text.clone() })
    }

    /// Type into the focused field: a change to Find re-runs the search, while
    /// a change to Replace only re-renders what each match would become.
    pub fn replace_type(&mut self, c: char) {
        let Some(p) = &mut self.replace else { return };
        if p.type_char(c) {
            self.replace_search();
        } else {
            p.refresh_previews();
        }
    }

    /// Backspace in the focused field; same split as [`replace_type`](Self::replace_type).
    pub fn replace_backspace(&mut self) {
        let Some(p) = &mut self.replace else { return };
        if p.backspace() {
            self.replace_search();
        } else {
            p.refresh_previews();
        }
    }

    /// Delete the previous word (Option+Backspace / Ctrl+Backspace); same
    /// split as [`replace_type`](Self::replace_type).
    pub fn replace_word_backspace(&mut self) {
        let Some(p) = &mut self.replace else { return };
        if p.word_backspace() {
            self.replace_search();
        } else {
            p.refresh_previews();
        }
    }

    /// Clear the focused field back to its start (Cmd+Backspace); same split
    /// as [`replace_type`](Self::replace_type).
    pub fn replace_clear_to_start(&mut self) {
        let Some(p) = &mut self.replace else { return };
        if p.clear_to_start() {
            self.replace_search();
        } else {
            p.refresh_previews();
        }
    }

    /// `<Tab>` — move focus Find → Replace → Results → Find.
    pub fn replace_cycle(&mut self) {
        if let Some(p) = &mut self.replace {
            p.focus = p.focus.next();
        }
    }

    /// Move the results selection (`j`/`k`, arrows, or the wheel).
    pub fn replace_move(&mut self, dir: i32) {
        if let Some(p) = &mut self.replace {
            p.move_selection(dir);
        }
    }

    /// Flip `'ignorecase'` for this search and re-run it.
    pub fn replace_toggle_case(&mut self) {
        let Some(p) = &mut self.replace else { return };
        p.ignorecase = !p.ignorecase;
        self.replace_search();
    }

    /// Open the highlighted hit's file at the match and close the panel — the
    /// escape hatch for "this one needs a real edit, not a substitution".
    pub fn replace_jump(&mut self) {
        let Some(hit) = self.replace.as_ref().and_then(|p| p.current()) else { return };
        let (rel, line, col) = (hit.path.display().to_string(), hit.line, hit.col);
        self.replace = None;
        self.quickfix_goto(&rel, line, col);
    }

    /// `y` — replace just the highlighted occurrence.
    ///
    /// The remaining hits are re-derived by searching again rather than by
    /// dropping the row, so counts stay right when the edit changes how many
    /// matches the line holds.
    pub fn replace_accept_one(&mut self) {
        let Some(panel) = &self.replace else { return };
        let Some(Ok(plan)) = panel.plan() else { return };
        let Some(hit) = panel.current() else { return };
        let (rel, line, col) = (hit.path.clone(), hit.line, hit.col);

        let Some(mut lines) = self.file_lines(&rel) else {
            self.message = format!("E: cannot read {}", rel.display());
            return;
        };
        let Some(old) = lines.get(line) else {
            self.message = "E: that match is stale — search again".into();
            return;
        };
        let Some(new) = plan.apply_match_at(old, col) else {
            self.message = "E: that match is stale — search again".into();
            return;
        };
        // A replacement containing `\r` splits the line, as `:s` does.
        let split: Vec<String> = new.split('\n').map(str::to_string).collect();
        lines.splice(line..line + 1, split);

        match self.write_file_lines(&rel, lines) {
            Ok(where_) => self.message = format!("1 replacement in {} ({where_})", rel.display()),
            Err(e) => {
                self.message = e;
                return;
            }
        }
        self.replace_search();
    }

    /// `Y` — replace every occurrence in every matched file.
    pub fn replace_accept_all(&mut self) {
        let Some(panel) = &self.replace else { return };
        let Some(Ok(plan)) = panel.plan() else { return };
        if panel.hits.is_empty() {
            self.message = "no matches to replace".into();
            return;
        }
        // Grouped by file: each file is read once, rewritten once.
        let paths: Vec<PathBuf> = by_file(&panel.hits).into_keys().collect();

        let (mut replaced, mut files, mut failed) = (0usize, 0usize, 0usize);
        for rel in paths {
            let Some(lines) = self.file_lines(&rel) else {
                failed += 1;
                continue;
            };
            let Some((new_lines, n)) = plan.apply_lines(&lines) else { continue };
            match self.write_file_lines(&rel, new_lines) {
                Ok(_) => {
                    replaced += n;
                    files += 1;
                }
                Err(_) => failed += 1,
            }
        }
        self.message = format!(
            "{replaced} replacement{} in {files} file{}{}",
            if replaced == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
            if failed > 0 { format!(" ({failed} failed)") } else { String::new() },
        );
        // The panel stays open on the (now empty) result set, so a mistyped
        // replacement is visible immediately rather than after reopening.
        self.replace_search();
    }

    /// The lines the preview pane draws context from — the same source the
    /// search read, so the surrounding code can't disagree with the match.
    pub fn replace_preview_lines(&self, rel: &Path) -> Option<Vec<String>> {
        self.file_lines(rel)
    }

    /// The current lines of a project-relative path — from its open buffer when
    /// there is one, else from disk.
    fn file_lines(&self, rel: &Path) -> Option<Vec<String>> {
        let abs = self.root.join(rel);
        if let Some(lines) = self.open_buffer_text(&abs) {
            return Some(lines);
        }
        Some(std::fs::read_to_string(&abs).ok()?.lines().map(String::from).collect())
    }

    /// Write `lines` back to a project-relative path, returning where the edit
    /// landed (for the status message) or a user-facing error.
    ///
    /// An open buffer is edited in memory and left **modified** — the change
    /// joins the undo history and `:w` commits it — while a file with no buffer
    /// is written straight to disk, there being nothing to hold it otherwise.
    fn write_file_lines(&mut self, rel: &Path, lines: Vec<String>) -> Result<&'static str, String> {
        let abs = self.root.join(rel);
        match self.buffers.iter().position(|b| b.path.as_deref() == Some(abs.as_path())) {
            Some(i) if i == self.active => {
                // In place, so the buffer keeps its undo history and cursor.
                let count = self.engine.session.editor.cur_buffer().text.line_count();
                self.engine.session.editor.cur_buffer_mut().text.set_lines(0, count, &lines);
                self.engine.session.checkpoint_undo();
                self.engine.set_modified(true);
                let (line, col) = self.editor_cursor();
                self.engine.session.set_cursor_clamped(line, col);
                Ok("buffer")
            }
            Some(i) => {
                self.buffers[i].text = lines;
                self.buffers[i].modified = true;
                Ok("buffer")
            }
            None => {
                let mut text = lines.join("\n");
                text.push('\n');
                std::fs::write(&abs, text)
                    .map(|()| "disk")
                    .map_err(|e| format!("E212: cannot write {}: {e}", rel.display()))
            }
        }
    }

    // --- file drawer (opt-in sidebar) with `/` search ---------------------

    fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        self.drawer_search = false;
        self.drawer_query.clear();
    }

    fn close_sidebar(&mut self) {
        self.sidebar_visible = false;
        self.drawer_search = false;
        self.drawer_query.clear();
    }

    /// Recent-file indices matching the drawer's `/` search (all when empty).
    pub fn drawer_matches(&self) -> Vec<usize> {
        self.project
            .recent_files
            .iter()
            .enumerate()
            .filter(|(_, f)| fuzzy_match(&self.drawer_query, &f.name))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn drawer_start_search(&mut self) {
        self.drawer_search = true;
        self.drawer_query.clear();
    }

    pub fn drawer_type(&mut self, c: char) {
        self.drawer_query.push(c);
        self.clamp_file_index_to_drawer();
    }

    pub fn drawer_backspace(&mut self) {
        self.drawer_query.pop();
        self.clamp_file_index_to_drawer();
    }

    pub fn drawer_word_backspace(&mut self) {
        delete_word_backward(&mut self.drawer_query);
        self.clamp_file_index_to_drawer();
    }

    pub fn drawer_clear_to_start(&mut self) {
        self.drawer_query.clear();
        self.clamp_file_index_to_drawer();
    }

    /// Move the selection within the drawer's filtered results.
    pub fn drawer_move(&mut self, dir: i32) {
        let matches = self.drawer_matches();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&i| i == self.file_index).unwrap_or(0) as i32;
        let n = matches.len() as i32;
        self.file_index = matches[(((pos + dir) % n + n) % n) as usize];
    }

    fn clamp_file_index_to_drawer(&mut self) {
        let matches = self.drawer_matches();
        if !matches.contains(&self.file_index) {
            self.file_index = matches.first().copied().unwrap_or(0);
        }
    }

}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

/// The first line of a (possibly multi-line) error, for the one-row status bar.
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// Strip a line of raw process output down to plain printable text before it
/// reaches a `Span`: drop ANSI escape sequences (some tools colorize even off
/// a tty), expand tabs to 8-column stops, and drop other C0 control bytes.
///
/// ratatui measures a `Span`'s width in *characters*, then the backend writes
/// its bytes as one contiguous run and moves on. An embedded raw `\t` or ESC
/// byte breaks that assumption differently per terminal — Ghostty (confirmed;
/// likely also Kitty/WezTerm/xterm, which follow the same control-sequence
/// handling) advances the real cursor by its own tab-stop/escape rules rather
/// than treating it as a printable cell, so the actual cursor position drifts
/// from what ratatui's diff believes it is. Once that drifts, every later
/// write in the frame lands at the wrong column — which is why unrelated
/// panels elsewhere on screen can end up smeared. Sanitizing here removes the
/// byte sequences no terminal agrees on the meaning of, rather than special-
/// casing any particular emulator.
fn sanitize_for_display(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => {
                // CSI (`ESC [ params... final`) or a bare two-byte escape —
                // either way, swallow through the final byte and move on.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\t' => {
                let next = (col / 8 + 1) * 8;
                out.extend(std::iter::repeat(' ').take(next - col));
                col = next;
            }
            c if (c as u32) < 0x20 => {} // other control bytes: drop
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// Very small glob matcher: `*` matches any suffix, a leading `*.ext` matches by
/// extension, otherwise exact match. Full Vim pattern matching is deferred.
fn pattern_matches(pattern: &str, file: &str) -> bool {
    if pattern == "*" || pattern.is_empty() {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return file.rsplit('.').next() == Some(ext);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return file.starts_with(prefix);
    }
    pattern == file
}

/// Run each `config.toml`-declared plugin once at startup, in order, over the
/// same Lua path as `:luafile` (see `host_source`). A broken script doesn't
/// stop the rest from loading; the first failure becomes the startup status
/// message (empty if everything loaded cleanly).
fn load_startup_plugins(engine: &mut Ctrlvim, plugins: &[PluginEntry]) -> String {
    let mut message = String::new();
    for p in plugins {
        let path = PathBuf::from(expand_tilde(&p.path));
        let source = path.file_stem().and_then(|s| s.to_str());
        let result = std::fs::read_to_string(&path)
            .map_err(|e| format!("E484: Can't open file {}: {e}", path.display()))
            .and_then(|contents| {
                engine.run_lua_as(source, &contents).map_err(|e| format!("E5108: {} ({})", first_line(&e), path.display()))
            });
        if let Err(e) = result {
            if message.is_empty() {
                message = e;
            }
        }
    }
    message
}

/// Case-insensitive subsequence match: every char of `query` appears in
/// `text`, in order. An empty query matches everything.
pub fn fuzzy_match(query: &str, text: &str) -> bool {
    let mut hay = text.chars().flat_map(char::to_lowercase);
    'outer: for qc in query.chars().flat_map(char::to_lowercase) {
        for hc in hay.by_ref() {
            if hc == qc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

pub struct PaletteItem {
    pub label: String,
    pub hint: String,
    pub icon_color: ratatui::style::Color,
    pub icon_letter: char,
    pub action: Action,
}

#[cfg(test)]
mod delete_word_backward_tests {
    use super::delete_word_backward;

    #[test]
    fn deletes_the_trailing_word_and_keeps_the_separator() {
        let mut s = String::from("hello world");
        delete_word_backward(&mut s);
        assert_eq!(s, "hello ");
    }

    #[test]
    fn eats_trailing_whitespace_before_the_word() {
        let mut s = String::from("hello world   ");
        delete_word_backward(&mut s);
        assert_eq!(s, "hello ");
    }

    #[test]
    fn a_single_word_is_cleared_entirely() {
        let mut s = String::from("hello");
        delete_word_backward(&mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn an_empty_string_is_a_no_op() {
        let mut s = String::new();
        delete_word_backward(&mut s);
        assert_eq!(s, "");
    }
}

#[cfg(test)]
mod sanitize_for_display_tests {
    use super::sanitize_for_display;

    #[test]
    fn expands_tabs_to_8_column_stops() {
        // Real `git status` output for a modified file, tab-indented.
        assert_eq!(sanitize_for_display("\tmodified:   job.rs"), "        modified:   job.rs");
        // A tab after some text lands on the next stop, not a fixed offset.
        assert_eq!(sanitize_for_display("ab\tc"), "ab      c");
    }

    #[test]
    fn strips_ansi_escape_sequences() {
        assert_eq!(sanitize_for_display("\x1b[31mred\x1b[0m text"), "red text");
    }

    #[test]
    fn drops_other_control_bytes_but_keeps_printable_unicode() {
        assert_eq!(sanitize_for_display("a\x07b\r"), "ab");
        assert_eq!(sanitize_for_display("héllo → 🎉"), "héllo → 🎉");
    }
}

#[cfg(test)]
mod startup_plugin_tests {
    use super::*;

    /// The repo's config.toml-plugin example, loaded the same way `App::with_root`
    /// loads `config.plugins` at startup.
    fn example_plugin_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/hello.lua")
    }

    #[test]
    fn loads_and_runs_a_declared_plugin() {
        let mut engine = Ctrlvim::new();
        let path = example_plugin_path();
        assert!(path.is_file(), "missing example plugin at {}", path.display());

        let entry = PluginEntry { name: "hello".into(), path: path.to_string_lossy().into(), event: None, enabled: true };
        let message = load_startup_plugins(&mut engine, &[entry]);
        assert!(message.is_empty(), "unexpected startup error: {message}");

        // The plugin's global should now be callable, proving the file actually ran.
        engine.open("first line", Some("scratch"));
        engine.run_lua("Hello.greet()").unwrap();
        assert_eq!(engine.lines(), vec!["Hello from ctrlvim!"]);
    }

    #[test]
    fn a_missing_plugin_reports_the_first_failure_but_does_not_stop_the_rest() {
        let mut engine = Ctrlvim::new();
        let missing = PluginEntry { name: "missing".into(), path: "/nonexistent/ctrlvim-plugin-does-not-exist.lua".into(), event: None, enabled: true };
        let good_path = example_plugin_path();
        let good = PluginEntry { name: "hello".into(), path: good_path.to_string_lossy().into(), event: None, enabled: true };

        let message = load_startup_plugins(&mut engine, &[missing, good]);
        assert!(message.starts_with("E484"), "message: {message}");

        // The second, valid plugin still loaded despite the first one failing.
        engine.open("x", Some("scratch"));
        engine.run_lua("Hello.greet()").unwrap();
        assert_eq!(engine.lines(), vec!["Hello from ctrlvim!"]);
    }
}
