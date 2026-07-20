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

use std::path::PathBuf;
use std::time::Instant;

use ctrlvim_core::{Ctrlvim, Key};

use crate::data::Project;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DashboardSection {
    Workspace,
    Settings,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Columns,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelId {
    RecentFiles,
    Git,
    Plugins,
    Keybindings,
}

/// What kind of screen a buffer/tab shows.
#[derive(Clone, PartialEq, Eq)]
pub enum BufferKind {
    Dashboard,
    File(usize), // index into model::FILES
    Plugins,
}

pub struct Buffer {
    pub label: String,
    pub kind: BufferKind,
    /// Cached text for a File buffer, so edits survive switching away and back
    /// while the engine facade only holds one working buffer. Empty for
    /// non-file buffers.
    pub text: Vec<String>,
    /// When this is a markdown File buffer, whether live rendering is on. Only
    /// meaningful for markdown files; auto-enabled when such a file is opened.
    pub render_md: bool,
}

impl Buffer {
    fn dashboard() -> Self {
        Buffer { label: "Dashboard".into(), kind: BufferKind::Dashboard, text: Vec::new(), render_md: false }
    }
    pub fn closable(&self) -> bool {
        !matches!(self.kind, BufferKind::Dashboard)
    }
    /// True when this buffer's file is markdown (by extension).
    pub fn is_markdown(&self) -> bool {
        matches!(self.kind, BufferKind::File(_)) && is_markdown_name(&self.label)
    }
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
    SetLayout(Layout),
    TogglePanel(PanelId),
    OpenFile(usize),
    OpenPlugins,
    ToggleLsp(usize),
    SetLspIndex(usize),
    OpenPalette,
    ClosePalette,
    RunPalette(usize),
    ToggleSidebar,
    CloseSidebar,
    ToggleHelp,
    CloseHelp,
    ToggleMarkdown,
}

pub struct App {
    pub engine: Ctrlvim,

    /// Real data for the project the editor was launched in.
    pub project: Project,
    /// The project root (current working directory), used to open files.
    pub root: PathBuf,

    pub buffers: Vec<Buffer>,
    pub active: usize,

    pub layout: Layout,
    pub section: DashboardSection,

    pub sidebar_visible: bool,
    pub file_index: usize, // selection in FILES, also highlights Recent Files

    pub palette_open: bool,
    pub palette_query: String,
    pub palette_index: usize,

    pub expand_recent_files: bool,
    pub expand_git: bool,
    pub expand_plugins: bool,
    pub expand_keybindings: bool,

    pub lsp_enabled: Vec<bool>,
    pub lsp_index: usize,

    pub help_open: bool,

    pub should_quit: bool,
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
        App {
            engine: Ctrlvim::new(),
            project,
            root,
            buffers: vec![Buffer::dashboard()],
            active: 0,
            layout: Layout::Grid, // grid is the default
            section: DashboardSection::Workspace,
            sidebar_visible: false,
            file_index: 0,
            palette_open: false,
            palette_query: String::new(),
            palette_index: 0,
            expand_recent_files: false,
            expand_git: false,
            expand_plugins: false,
            expand_keybindings: false,
            lsp_enabled,
            lsp_index: 0,
            help_open: false,
            should_quit: false,
        }
    }

    // --- queries -----------------------------------------------------------

    pub fn active_buffer(&self) -> &Buffer {
        &self.buffers[self.active]
    }
    pub fn is_dashboard(&self) -> bool {
        matches!(self.active_buffer().kind, BufferKind::Dashboard)
    }
    pub fn active_file(&self) -> Option<usize> {
        match self.active_buffer().kind {
            BufferKind::File(i) => Some(i),
            _ => None,
        }
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
            PanelId::RecentFiles => self.expand_recent_files,
            PanelId::Git => self.expand_git,
            PanelId::Plugins => self.expand_plugins,
            PanelId::Keybindings => self.expand_keybindings,
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
            Action::SetLayout(l) => {
                // palette layout actions also snap back to the dashboard
                self.focus_dashboard();
                self.layout = l;
            }
            Action::TogglePanel(p) => self.toggle_panel(p),
            Action::OpenFile(i) => self.open_file(i),
            Action::OpenPlugins => self.open_plugins(),
            Action::ToggleLsp(i) => self.toggle_lsp(i),
            Action::SetLspIndex(i) => {
                if i < self.project.lsp.len() {
                    self.lsp_index = i;
                }
            }
            Action::OpenPalette => {
                self.palette_open = true;
                self.palette_query.clear();
                self.palette_index = 0;
            }
            Action::ClosePalette => self.palette_open = false,
            Action::RunPalette(i) => self.run_palette(i),
            Action::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Action::CloseSidebar => self.sidebar_visible = false,
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::CloseHelp => self.help_open = false,
            Action::ToggleMarkdown => self.toggle_md_render(),
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
        let slot = match p {
            PanelId::RecentFiles => &mut self.expand_recent_files,
            PanelId::Git => &mut self.expand_git,
            PanelId::Plugins => &mut self.expand_plugins,
            PanelId::Keybindings => &mut self.expand_keybindings,
        };
        *slot = !*slot;
    }

    pub fn move_file_selection(&mut self, dir: i32) {
        let n = self.project.recent_files.len() as i32;
        if n == 0 {
            return;
        }
        let i = self.file_index as i32;
        self.file_index = (((i + dir) % n + n) % n) as usize;
    }

    pub fn move_lsp_selection(&mut self, dir: i32) {
        let n = self.project.lsp.len() as i32;
        if n == 0 {
            return;
        }
        let i = self.lsp_index as i32;
        self.lsp_index = (((i + dir) % n + n) % n) as usize;
    }

    pub fn toggle_lsp(&mut self, i: usize) {
        if let Some(slot) = self.lsp_enabled.get_mut(i) {
            *slot = !*slot;
            self.lsp_index = i;
        }
    }

    fn focus_dashboard(&mut self) {
        if let Some(i) = self
            .buffers
            .iter()
            .position(|b| matches!(b.kind, BufferKind::Dashboard))
        {
            self.set_active(i);
        }
    }

    /// Open (or focus) a recent file. New buffers are seeded by reading the
    /// real file from disk; [`set_active`](Self::set_active) loads that text
    /// into the engine.
    pub fn open_file(&mut self, file_idx: usize) {
        let Some(f) = self.project.recent_files.get(file_idx) else { return };
        let name = f.name.clone();
        let path = self.root.join(&f.path);
        match self.buffers.iter().position(|b| b.kind == BufferKind::File(file_idx)) {
            Some(i) => self.set_active(i),
            None => {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .lines()
                    .map(String::from)
                    .collect();
                let render_md = is_markdown_name(&name); // live-render markdown by default
                self.buffers.push(Buffer { label: name, kind: BufferKind::File(file_idx), text, render_md });
                self.set_active(self.buffers.len() - 1);
            }
        }
    }

    pub fn open_plugins(&mut self) {
        if let Some(i) = self.buffers.iter().position(|b| b.kind == BufferKind::Plugins) {
            self.set_active(i);
        } else {
            self.buffers.push(Buffer { label: "Plugin Manager".into(), kind: BufferKind::Plugins, text: Vec::new(), render_md: false });
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

    /// Save the engine's current text back into the active file buffer's cache.
    fn snapshot_active(&mut self) {
        if matches!(self.active_buffer().kind, BufferKind::File(_)) {
            self.buffers[self.active].text = self.engine.lines();
        }
    }

    /// Load the active file buffer's cached text into the engine.
    fn load_active_into_engine(&mut self) {
        if matches!(self.active_buffer().kind, BufferKind::File(_)) {
            let text = self.buffers[self.active].text.join("\n");
            let label = self.buffers[self.active].label.clone();
            self.engine.open(&text, Some(&label));
        }
    }

    /// Feed one key to the engine's editing session.
    pub fn feed_engine(&mut self, key: Key) {
        self.engine.session.feed(key);
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

    /// True when a File buffer is focused and no overlay is capturing input —
    /// i.e. keystrokes should drive the editor.
    pub fn editor_focus(&self) -> bool {
        self.active_file().is_some() && !self.palette_open && !self.help_open && !self.sidebar_visible
    }

    // --- command palette ---------------------------------------------------

    pub fn palette_results(&self) -> Vec<PaletteItem> {
        let q = self.palette_query.to_lowercase();
        let mut items: Vec<PaletteItem> = Vec::new();
        for (i, f) in self.project.recent_files.iter().enumerate() {
            items.push(PaletteItem {
                label: f.path.clone(),
                hint: "open file",
                icon_color: f.icon_color,
                icon_letter: f.icon_letter,
                action: Action::OpenFile(i),
            });
        }
        if self.active_is_markdown() {
            let (label, letter) = if self.md_render_active() {
                ("Markdown: Show Raw Source".to_string(), 'M')
            } else {
                ("Markdown: Live Render".to_string(), 'M')
            };
            items.push(PaletteItem { label, hint: "action", icon_color: crate::theme::PURPLE, icon_letter: letter, action: Action::ToggleMarkdown });
        }
        items.push(PaletteItem { label: "Plugin Manager".into(), hint: "action", icon_color: crate::theme::ORANGE, icon_letter: 'P', action: Action::OpenPlugins });
        items.push(PaletteItem { label: "Toggle Sidebar".into(), hint: "action", icon_color: crate::theme::CYAN, icon_letter: 'S', action: Action::ToggleSidebar });
        items.push(PaletteItem { label: "Dashboard Layout: Two Column".into(), hint: "action", icon_color: crate::theme::BLUE, icon_letter: '2', action: Action::SetLayout(Layout::Columns) });
        items.push(PaletteItem { label: "Dashboard Layout: Grid".into(), hint: "action", icon_color: crate::theme::BLUE, icon_letter: '3', action: Action::SetLayout(Layout::Grid) });
        if q.is_empty() {
            items
        } else {
            items.into_iter().filter(|it| it.label.to_lowercase().contains(&q)).collect()
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
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_index = 0;
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

pub struct PaletteItem {
    pub label: String,
    pub hint: &'static str,
    pub icon_color: ratatui::style::Color,
    pub icon_letter: char,
    pub action: Action,
}
