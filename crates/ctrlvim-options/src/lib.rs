//! Options system — the replacement for `option.c` + `options.lua`.
//!
//! Neovim generates a static `vimoption_T options[]` array from the 11k-line
//! declarative `options.lua` via `gen_options.lua`. The three-tier scoping model
//! it encodes — global / buffer-local / window-local / "global with local
//! override" — maps cleanly onto Rust's own `Option<T>`: a local override is
//! `Some(v)`, "inherit the global" is `None`. That is exactly the sentinel-value
//! hack (`-1`, empty string) the C code emulates by hand, expressed natively.
//!
//! This module hand-writes a representative core set of options with the right
//! scoping. A `build.rs` generator consuming an `options.lua`-equivalent data
//! file can later replace the field definitions here without changing callers;
//! the resolution logic ([`Options::resolve`]-style getters) stays identical.

/// Where a window's folds come from (`'foldmethod'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldMethod {
    /// Folds exist only where the user made them (`zf`). Neovim's default.
    #[default]
    Manual,
    /// Folds are derived from indentation.
    Indent,
}

impl FoldMethod {
    pub fn parse(s: &str) -> Option<FoldMethod> {
        match s {
            "manual" => Some(FoldMethod::Manual),
            "indent" => Some(FoldMethod::Indent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FoldMethod::Manual => "manual",
            FoldMethod::Indent => "indent",
        }
    }
}

/// The shape a terminal cursor takes, as named by `'guicursor'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// `block` — fills the cell.
    #[default]
    Block,
    /// `ver{N}` — a vertical bar down the left of the cell.
    Vertical,
    /// `hor{N}` — a horizontal bar along the bottom of the cell.
    Horizontal,
}

/// A resolved `'guicursor'` entry: how the cursor should look in one mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    /// The `N` in `ver25`/`hor20`: percent of the cell the bar covers, and
    /// always 100 for a block. A terminal cursor can't actually be sized, so
    /// nothing renders this yet — it is parsed and kept because dropping it
    /// would make `:set guicursor?` echo back something the user never typed.
    pub percent: u8,
    /// Whether the spec asked for blinking (`blinkon`/`blinkoff` both
    /// non-zero). Vim's default spec asks for no blinking at all.
    pub blink: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        CursorStyle { shape: CursorShape::Block, percent: 100, blink: false }
    }
}

/// Neovim's own `'guicursor'` default.
pub const DEFAULT_GUICURSOR: &str = "n-v-c-sm:block,i-ci-ve:ver25,r-cr-o:hor20";

/// Resolve `'guicursor'` for one mode.
///
/// `mode` is a [`Mode::short_name`](../ctrlvim_editor/mode/enum.Mode.html)-style
/// short name; the visual variants (`V`, CTRL-V) are folded onto `v` since
/// `'guicursor'` has no separate entries for them.
///
/// Entries are applied in order so a later one wins, which is what lets the
/// idiomatic `:set guicursor=a:block,i:ver25` work: `a:` seeds every mode and
/// the specific entry then overrides it. Unparseable arguments — including the
/// highlight-group names Vim allows here, which have no meaning for a terminal
/// cursor — are skipped rather than failing the whole spec.
pub fn resolve_guicursor(spec: &str, mode: &str) -> CursorStyle {
    let mode = match mode {
        "V" | "\u{16}" => "v",
        m => m,
    };
    let mut style = CursorStyle::default();
    // Blink is only on when a spec asks for it *and* neither phase is zero;
    // tracked separately because the two args can arrive in either order.
    let (mut blink_on, mut blink_off) = (None::<u32>, None::<u32>);
    for entry in spec.split(',') {
        let Some((modes, args)) = entry.split_once(':') else { continue };
        if !modes.split('-').any(|m| m == "a" || m == mode) {
            continue;
        }
        for arg in args.split('-') {
            if arg == "block" {
                style.shape = CursorShape::Block;
                style.percent = 100;
            } else if let Some(n) = arg.strip_prefix("ver").and_then(|n| n.parse().ok()) {
                style.shape = CursorShape::Vertical;
                style.percent = n;
            } else if let Some(n) = arg.strip_prefix("hor").and_then(|n| n.parse().ok()) {
                style.shape = CursorShape::Horizontal;
                style.percent = n;
            } else if let Some(n) = arg.strip_prefix("blinkon").and_then(|n| n.parse().ok()) {
                blink_on = Some(n);
            } else if let Some(n) = arg.strip_prefix("blinkoff").and_then(|n| n.parse().ok()) {
                blink_off = Some(n);
            }
            // `blinkwait` and highlight-group names are accepted and ignored.
        }
    }
    style.blink = matches!((blink_on, blink_off), (Some(on), Some(off)) if on > 0 && off > 0)
        || matches!((blink_on, blink_off), (Some(on), None) if on > 0)
        || matches!((blink_on, blink_off), (None, Some(off)) if off > 0);
    style
}

/// Global option values (the fallback layer).
#[derive(Debug, Clone)]
pub struct GlobalOptions {
    pub tabstop: i64,
    pub shiftwidth: i64,
    pub expandtab: bool,
    pub number: bool,
    pub relativenumber: bool,
    pub wrap: bool,
    pub scrolloff: i64,
    /// Highlight the line the cursor is on (`'cursorline'`).
    pub cursorline: bool,
    /// Per-mode cursor shape (`'guicursor'`). Stored as the raw spec string so
    /// `:set guicursor?` round-trips; parsed on demand by
    /// [`resolve_guicursor`].
    pub guicursor: String,
    pub iskeyword: String,
    pub foldenable: bool,
    pub foldmethod: FoldMethod,
    pub foldcolumn: i64,
    pub autoindent: bool,
    // Search behaviour. `'hlsearch'` is the master switch; whether matches are
    // *currently* lit is separate transient state owned by the session (`:noh`).
    pub ignorecase: bool,
    pub smartcase: bool,
    pub hlsearch: bool,
    // Where `:split`/`:vsplit` put the new window.
    pub splitbelow: bool,
    pub splitright: bool,
    /// Milliseconds to wait for the rest of a mapped key sequence before
    /// giving up on it — Vim's `'timeoutlen'`. This is what lets `<leader>q`
    /// and `<leader>qq` coexist: the shorter one fires only once this elapses.
    pub timeoutlen: i64,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        // Neovim defaults.
        GlobalOptions {
            tabstop: 8,
            shiftwidth: 8,
            expandtab: false,
            number: false,
            relativenumber: false,
            wrap: true,
            scrolloff: 0,
            cursorline: false,
            guicursor: DEFAULT_GUICURSOR.to_string(),
            iskeyword: "@,48-57,_,192-255".to_string(),
            foldenable: true,
            foldmethod: FoldMethod::Manual,
            foldcolumn: 0,
            autoindent: false,
            ignorecase: false,
            smartcase: false,
            // Neovim defaults 'hlsearch' on; Vim defaults it off.
            hlsearch: true,
            splitbelow: false,
            splitright: false,
            // Neovim's default is 1000ms, which feels sluggish on a chord that
            // is *only* waiting to be disambiguated. 500 is what most configs
            // (charvim included) set it to anyway.
            timeoutlen: 500,
        }
    }
}

/// Buffer-local option overrides. `None` means "inherit from global".
#[derive(Debug, Clone, Default)]
pub struct BufferOptions {
    pub tabstop: Option<i64>,
    pub shiftwidth: Option<i64>,
    pub expandtab: Option<bool>,
    pub iskeyword: Option<String>,
    pub autoindent: Option<bool>,
}

/// Window-local option overrides. `None` means "inherit from global".
#[derive(Debug, Clone, Default)]
pub struct WindowOptions {
    pub number: Option<bool>,
    pub relativenumber: Option<bool>,
    pub wrap: Option<bool>,
    pub scrolloff: Option<i64>,
    pub cursorline: Option<bool>,
    pub foldenable: Option<bool>,
    pub foldmethod: Option<FoldMethod>,
    pub foldcolumn: Option<i64>,
}

/// Resolves an effective option value from the global + local layers. Held by
/// the editor; the buffer/window-local structs live on their respective owners.
pub struct OptionContext<'a> {
    pub global: &'a GlobalOptions,
    pub buffer: &'a BufferOptions,
    pub window: &'a WindowOptions,
}

impl OptionContext<'_> {
    // Buffer-local-with-global-fallback.
    pub fn tabstop(&self) -> i64 {
        self.buffer.tabstop.unwrap_or(self.global.tabstop)
    }
    pub fn shiftwidth(&self) -> i64 {
        // Neovim: shiftwidth of 0 means "use tabstop".
        let sw = self.buffer.shiftwidth.unwrap_or(self.global.shiftwidth);
        if sw == 0 {
            self.tabstop()
        } else {
            sw
        }
    }
    pub fn expandtab(&self) -> bool {
        self.buffer.expandtab.unwrap_or(self.global.expandtab)
    }
    pub fn iskeyword(&self) -> &str {
        self.buffer
            .iskeyword
            .as_deref()
            .unwrap_or(&self.global.iskeyword)
    }
    pub fn autoindent(&self) -> bool {
        self.buffer.autoindent.unwrap_or(self.global.autoindent)
    }

    // Global-only.
    pub fn ignorecase(&self) -> bool {
        self.global.ignorecase
    }
    pub fn smartcase(&self) -> bool {
        self.global.smartcase
    }
    pub fn hlsearch(&self) -> bool {
        self.global.hlsearch
    }
    pub fn splitbelow(&self) -> bool {
        self.global.splitbelow
    }
    pub fn splitright(&self) -> bool {
        self.global.splitright
    }

    // Window-local-with-global-fallback.
    pub fn number(&self) -> bool {
        self.window.number.unwrap_or(self.global.number)
    }
    pub fn relativenumber(&self) -> bool {
        self.window
            .relativenumber
            .unwrap_or(self.global.relativenumber)
    }
    pub fn wrap(&self) -> bool {
        self.window.wrap.unwrap_or(self.global.wrap)
    }
    pub fn scrolloff(&self) -> i64 {
        self.window.scrolloff.unwrap_or(self.global.scrolloff)
    }
    pub fn cursorline(&self) -> bool {
        self.window.cursorline.unwrap_or(self.global.cursorline)
    }
    /// The raw `'guicursor'` spec. Global-only, as in Vim.
    pub fn guicursor(&self) -> &str {
        &self.global.guicursor
    }
    /// The cursor style `'guicursor'` asks for in `mode`.
    pub fn cursor_style(&self, mode: &str) -> CursorStyle {
        resolve_guicursor(&self.global.guicursor, mode)
    }
    pub fn foldenable(&self) -> bool {
        self.window.foldenable.unwrap_or(self.global.foldenable)
    }
    pub fn foldmethod(&self) -> FoldMethod {
        self.window.foldmethod.unwrap_or(self.global.foldmethod)
    }
    pub fn foldcolumn(&self) -> i64 {
        self.window.foldcolumn.unwrap_or(self.global.foldcolumn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_guicursor_matches_neovims_modes() {
        let g = |m| resolve_guicursor(DEFAULT_GUICURSOR, m);
        // n-v-c-sm:block
        assert_eq!(g("n").shape, CursorShape::Block);
        assert_eq!(g("v").shape, CursorShape::Block);
        assert_eq!(g("c").shape, CursorShape::Block);
        // i-ci-ve:ver25
        assert_eq!(g("i").shape, CursorShape::Vertical);
        assert_eq!(g("i").percent, 25);
        // r-cr-o:hor20
        assert_eq!(g("r").shape, CursorShape::Horizontal);
        assert_eq!(g("r").percent, 20);
        // Nothing in the default spec asks to blink.
        assert!(!g("n").blink);
    }

    #[test]
    fn visual_line_and_block_share_the_visual_entry() {
        // `'guicursor'` has no `V` or CTRL-V entry, so both fold onto `v`.
        let spec = "n:block,v:ver25";
        assert_eq!(resolve_guicursor(spec, "V").shape, CursorShape::Vertical);
        assert_eq!(resolve_guicursor(spec, "\u{16}").shape, CursorShape::Vertical);
    }

    #[test]
    fn a_seeds_every_mode_and_a_later_entry_overrides_it() {
        let spec = "a:block,i:ver25";
        assert_eq!(resolve_guicursor(spec, "n").shape, CursorShape::Block);
        assert_eq!(resolve_guicursor(spec, "i").shape, CursorShape::Vertical);
    }

    #[test]
    fn blink_needs_a_nonzero_phase() {
        assert!(resolve_guicursor("n:block-blinkon500-blinkoff500", "n").blink);
        // Vim's documented way to switch blinking off is a zero phase.
        assert!(!resolve_guicursor("n:block-blinkon0-blinkoff0", "n").blink);
        assert!(!resolve_guicursor("n:block-blinkwait700", "n").blink);
    }

    #[test]
    fn unknown_arguments_are_skipped_not_fatal() {
        // `Cursor`/`lCursor` are highlight groups — legal in Vim, meaningless
        // for a terminal cursor. The shape in the same entry must still apply.
        let s = resolve_guicursor("n-v-c:block-Cursor/lCursor,i:ver25-lCursor", "i");
        assert_eq!(s.shape, CursorShape::Vertical);
        assert_eq!(s.percent, 25);
        // A malformed entry with no colon is ignored rather than poisoning it.
        assert_eq!(resolve_guicursor("garbage,i:ver25", "i").shape, CursorShape::Vertical);
    }

    #[test]
    fn a_mode_the_spec_never_mentions_falls_back_to_a_block() {
        assert_eq!(resolve_guicursor("i:ver25", "n"), CursorStyle::default());
    }

    #[test]
    fn cursorline_is_window_local_over_global() {
        let mut g = GlobalOptions::default();
        let b = BufferOptions::default();
        let mut w = WindowOptions::default();
        assert!(!OptionContext { global: &g, buffer: &b, window: &w }.cursorline());
        g.cursorline = true;
        assert!(OptionContext { global: &g, buffer: &b, window: &w }.cursorline());
        w.cursorline = Some(false);
        assert!(!OptionContext { global: &g, buffer: &b, window: &w }.cursorline());
    }

    #[test]
    fn buffer_local_overrides_global() {
        let g = GlobalOptions::default();
        let mut b = BufferOptions::default();
        let w = WindowOptions::default();
        assert_eq!(OptionContext { global: &g, buffer: &b, window: &w }.tabstop(), 8);
        b.tabstop = Some(4);
        assert_eq!(OptionContext { global: &g, buffer: &b, window: &w }.tabstop(), 4);
    }

    #[test]
    fn shiftwidth_zero_falls_back_to_tabstop() {
        let g = GlobalOptions::default();
        let b = BufferOptions { shiftwidth: Some(0), tabstop: Some(2), ..Default::default() };
        let w = WindowOptions::default();
        let ctx = OptionContext { global: &g, buffer: &b, window: &w };
        assert_eq!(ctx.shiftwidth(), 2);
    }

    #[test]
    fn window_local_override() {
        let g = GlobalOptions::default();
        let b = BufferOptions::default();
        let w = WindowOptions { number: Some(true), ..Default::default() };
        assert!(OptionContext { global: &g, buffer: &b, window: &w }.number());
    }
}
