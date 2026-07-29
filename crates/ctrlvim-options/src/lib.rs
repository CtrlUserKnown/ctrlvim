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
