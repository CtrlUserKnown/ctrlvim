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
//! The dashboard's recent-files/git/plugin/LSP data is still static mock data
//! until the engine grows sources for it.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ctrlvim_core::syntax::{self, Filetype};
use ctrlvim_core::{
    grep_text, BufferCmd, Ctrlvim, Event, EventLoop, ExEffect, HlSpan, Jobs, Key, LineBuffer,
    Matcher, OutputParser, QfItem, QuickfixCmd, Selection, TagAddress, TagCmd, TimerService,
};

use crate::config::Config;
use crate::data::{list_dir, FinderEntry};
use crate::data::Project;

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
    OpenFile(usize),
    OpenPlugins,
    OpenDashboard,
    ToggleLsp(usize),
    ToggleMouse,
    CycleIconMode,
    /// Select (and jump to) a quickfix entry by list index.
    QuickfixSelect(usize),
    SetSettingsIndex(usize),
    OpenPalette,
    ClosePalette,
    RunPalette(usize),
    OpenFinder,
    CloseFinder,
    RunFinder(usize),
    ToggleSidebar,
    CloseSidebar,
    ToggleHelp,
    CloseHelp,
    ToggleMarkdown,
    /// Run an Ex command through the engine (e.g. `"w"`, `"q!"`). Carries the
    /// command text without the leading colon.
    RunEx(String),
    /// Switch to the theme at this index in [`theme::ALL`](crate::theme::ALL).
    SetTheme(usize),
    /// Start a new file: opens the file browser where a typed name is created.
    NewFile,
    /// Flip the "open file drawer on startup" config setting (Settings tab).
    ToggleStartupDrawer,
    /// Dismiss the save-as prompt without saving.
    CloseSavePrompt,
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

    pub palette_open: bool,
    pub palette_query: String,
    pub palette_index: usize,

    /// The "save as" prompt: the filename being typed while saving an unnamed
    /// buffer (`Some` while the prompt is open).
    pub save_prompt: Option<String>,

    /// True after `<leader>` (Space) is pressed in the shell, so the next digit
    /// jumps to that tab (the editor handles its own leader via the engine).
    pub leader_pending: bool,

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
    /// Modified time of the tags file when it was last loaded, so a
    /// regenerated one is picked up without a reload command.
    tags_loaded_at: Option<std::time::SystemTime>,

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

/// Cached highlight spans plus the buffer state they were computed from.
struct SyntaxCache {
    /// Active buffer, a hash of its text, and the visible window (`top`,
    /// `rows`) — an edit or a scroll invalidates.
    key: (usize, u64, usize, usize),
    /// Spans per visible row, indexed from the window's first line.
    lines: Vec<Vec<HlSpan>>,
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
        App {
            engine: Ctrlvim::new(),
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
            palette_open: false,
            palette_query: String::new(),
            palette_index: 0,
            save_prompt: None,
            leader_pending: false,
            message: String::new(),
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
            tags_loaded_at: None,
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
            Action::OpenFile(i) => self.open_file(i),
            Action::OpenPlugins => self.open_plugins(),
            Action::OpenDashboard => self.open_dashboard(),
            Action::ToggleLsp(i) => self.toggle_lsp(i),
            Action::ToggleMouse => self.toggle_mouse(),
            Action::CycleIconMode => self.cycle_icon_mode(),
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
            Action::SetTheme(i) => self.set_theme(i),
            Action::NewFile => self.new_untitled(),
            Action::ToggleStartupDrawer => self.toggle_startup_drawer(),
            Action::CloseSavePrompt => self.close_save_prompt(),
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

    pub fn move_file_selection(&mut self, dir: i32) {
        let n = self.project.recent_files.len() as i32;
        if n == 0 {
            return;
        }
        let i = self.file_index as i32;
        self.file_index = (((i + dir) % n + n) % n) as usize;
    }

    /// Number of Settings rows: the editor options plus each LSP server.
    pub const SETTINGS_EDITOR_OPTIONS: usize = 3; // drawer, mouse, icons

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
            i => self.toggle_lsp(i - Self::SETTINGS_EDITOR_OPTIONS),
        }
    }

    pub fn toggle_lsp(&mut self, i: usize) {
        if let Some(slot) = self.lsp_enabled.get_mut(i) {
            *slot = !*slot;
            self.settings_index = i + Self::SETTINGS_EDITOR_OPTIONS;
        }
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
        self.snapshot_active();
        self.active = idx;
        self.load_active_into_engine();
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
        let key = if lines >= 0 { Key::Char('j') } else { Key::Char('k') };
        for _ in 0..lines.unsigned_abs() {
            self.feed_engine(key);
        }
    }

    /// Feed one key to the engine's editing session, then perform any host
    /// effects the engine requested (`:w`/`:q`/…).
    pub fn feed_engine(&mut self, key: Key) {
        self.engine.session.feed(key);
        self.apply_effects();
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
            }
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
        for event in events {
            match event {
                Event::ProcessOutput { id, data } => {
                    let Some(job) = self.job.as_mut().filter(|j| j.id == id) else { continue };
                    for line in job.lines.push(&data) {
                        if let Some(item) = job.parser.push(&line) {
                            job.items.push(item);
                        }
                    }
                }
                Event::ProcessExit { id, code } => {
                    let Some(job) = self.job.as_mut().filter(|j| j.id == id) else { continue };
                    if let Some(last) = job.lines.flush() {
                        if let Some(item) = job.parser.push(&last) {
                            job.items.push(item);
                        }
                    }
                    finished = self.job.take().map(|j| (j, code));
                }
                // Timers/RPC are not wired into the frontend yet.
                _ => {}
            }
        }
        if let Some((job, code)) = finished {
            let title = format!("{} (exit {code})", job.title);
            self.finish_quickfix(job.items, title);
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
    pub fn palette_results(&self) -> Vec<PaletteItem> {
        let q = &self.palette_query;
        let mut items: Vec<PaletteItem> = Vec::new();

        // Engine-defined Ex commands (`:w`, `:q`, …). The catalog and execution
        // both live in the engine; the palette is only a nicer entry point.
        for cmd in ctrlvim_core::ex_commands() {
            items.push(PaletteItem {
                label: format!(":{}", cmd.name),
                hint: cmd.desc,
                icon_color: crate::theme::green(),
                icon_letter: ':',
                action: Action::RunEx(cmd.name.to_string()),
            });
        }

        // Frontend actions.
        if self.active_is_markdown() {
            let (label, letter) = if self.md_render_active() {
                ("Markdown: Show Raw Source".to_string(), 'M')
            } else {
                ("Markdown: Live Render".to_string(), 'M')
            };
            items.push(PaletteItem { label, hint: "toggle markdown render", icon_color: crate::theme::purple(), icon_letter: letter, action: Action::ToggleMarkdown });
        }
        items.push(PaletteItem { label: "Find File".into(), hint: "fuzzy file browser", icon_color: crate::theme::blue(), icon_letter: 'F', action: Action::OpenFinder });
        items.push(PaletteItem { label: "Plugin Manager".into(), hint: "manage plugins", icon_color: crate::theme::orange(), icon_letter: 'P', action: Action::OpenPlugins });
        if self.config.drawer {
            items.push(PaletteItem { label: "Toggle Sidebar".into(), hint: "file drawer", icon_color: crate::theme::cyan(), icon_letter: 'S', action: Action::ToggleSidebar });
        }

        // Theme switching (one entry per registered theme).
        for (i, t) in crate::theme::ALL.iter().enumerate() {
            items.push(PaletteItem {
                label: format!("Theme: {}", t.name),
                hint: "color theme",
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

    /// Confirm the command line (`Enter`). Command-line semantics take priority
    /// over the fuzzy list: an **exactly** typed command name runs verbatim
    /// (typing `q` runs `:q`, never the `:wq` that fuzzy-matching would rank
    /// first). Otherwise the highlighted item runs; and if nothing matched, the
    /// raw text runs as a freeform Ex command (`:42`, `:$`, unknown → E492).
    pub fn submit_palette(&mut self) {
        let q = self.palette_query.trim().to_string();
        // A recognized Ex command (incl. ranges/`:s`/`:noh`/…) runs verbatim,
        // so short command names aren't hijacked by a fuzzy palette entry.
        if !q.is_empty() && ctrlvim_core::is_ex_command(&q) {
            self.close_palette();
            self.run_ex_command(&q);
            return;
        }
        // Otherwise pick from the fuzzy list (themes, Find File, …).
        let results = self.palette_results();
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
    pub hint: &'static str,
    pub icon_color: ratatui::style::Color,
    pub icon_letter: char,
    pub action: Action,
}
