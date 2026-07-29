//! The key-mapping table and its typeahead expansion — Neovim's
//! `:map`/`getchar.c` layer.
//!
//! A mapping is a left-hand key sequence that expands to a right-hand key
//! sequence (`:nnoremap lhs rhs`). Because the right-hand side is re-fed through
//! the session, a `<leader>` chord like `<Space>w` can expand to a real command
//! line such as `:w<CR>` — the same way a Neovim user's config would define it,
//! rather than the frontend hard-coding the chord. Mappings are non-recursive
//! (noremap) for now.
//!
//! ## Ambiguity
//!
//! When one mapping's lhs is a strict prefix of another's — `<leader>q` and
//! `<leader>qq` — an exact match cannot fire immediately, or the longer one
//! could never be typed. [`KeymapMatch::FullAmbiguous`] says "this matches, but
//! hold it": the session parks the expansion and fires it only once
//! `'timeoutlen'` passes with no further key (see
//! [`Session::keymap_timeout`](crate::Session::keymap_timeout)).
//!
//! ## Descriptions
//!
//! Every mapping carries an optional `desc`. It is the *only* source of the
//! text the keybinding help and the which-key popup display, which is why it is
//! settable from every path that can define a mapping — config, `:map
//! <desc=…>`, and `vim.keymap.set(…, { desc = … })`. Anything that rendered a
//! hand-maintained list beside the table would go stale the moment a user
//! rebound something.

use crate::input::Key;

/// One mapping: `lhs` keys expand to `rhs` keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub lhs: Vec<Key>,
    pub rhs: Vec<Key>,
    /// Human-readable description, shown by `?` and the which-key popup.
    pub desc: Option<String>,
}

impl Mapping {
    /// The lhs written back in `<...>` notation, e.g. `<Space>w`.
    pub fn lhs_notation(&self) -> String {
        display_notation(&self.lhs)
    }

    /// The rhs written back in `<...>` notation, e.g. `:w<CR>`.
    pub fn rhs_notation(&self) -> String {
        display_notation(&self.rhs)
    }

    /// What to show for this mapping: its description, or failing that the rhs
    /// it expands to. A mapping with no `desc` is still worth listing —
    /// `:w<CR>` tells the reader more than an omitted row does.
    pub fn label(&self) -> String {
        self.desc.clone().unwrap_or_else(|| self.rhs_notation())
    }
}

/// Render a key sequence back into `<...>` notation.
pub fn notation(keys: &[Key]) -> String {
    keys.iter().map(|k| k.notation()).collect()
}

/// Like [`notation`], but for things a human reads — a `:map` listing or the
/// which-key popup. The only difference is that a literal space is spelled
/// `<Space>`, since `<leader>` is Space by default and a row whose key column
/// is blank tells the reader nothing.
///
/// [`Key::notation`] itself stays literal because it also writes the text of
/// recorded macros, which Vim stores with real spaces in them.
pub fn display_notation(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| if *k == Key::Char(' ') { "<Space>".to_string() } else { k.notation() })
        .collect()
}

/// A mapping reachable from the keys typed so far, for the which-key popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Continuation {
    /// The keys still left to type, in notation — `w` when `<Space>` is
    /// pending and `<Space>w` is mapped.
    pub rest: String,
    /// The whole left-hand side, for listings that aren't chord-relative.
    pub lhs: String,
    /// What it does: `desc` if set, else the rhs.
    pub label: String,
}

/// The result of matching buffered keys against the mapping table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapMatch {
    /// The buffer matches a mapping and nothing longer extends it — expand now.
    Full(Vec<Key>),
    /// The buffer matches a mapping *and* is a prefix of a longer one. Expand
    /// only if `'timeoutlen'` elapses before the next key.
    FullAmbiguous(Vec<Key>),
    /// The buffer is a strict prefix of at least one mapping; wait for more.
    Prefix,
    /// No mapping matches; the keys should be used literally.
    None,
}

/// Which mode a mapping applies in — the `n`/`i`/`v` of `:nmap`/`:imap`/`:vmap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MapMode {
    Normal,
    Insert,
    Visual,
}

impl MapMode {
    /// Parse the mode letter used by `:map` commands and the config's
    /// `[[keymap]] mode` field. Unknown letters fall back to normal, matching
    /// how a bare `:map` behaves.
    pub fn parse(s: &str) -> MapMode {
        match s.trim() {
            "i" | "insert" => MapMode::Insert,
            "v" | "x" | "visual" => MapMode::Visual,
            _ => MapMode::Normal,
        }
    }

    /// The letter `:map` listings print in the first column.
    pub fn letter(self) -> &'static str {
        match self {
            MapMode::Normal => "n",
            MapMode::Insert => "i",
            MapMode::Visual => "v",
        }
    }

    /// Every mode, for listings that span all of them.
    pub const ALL: [MapMode; 3] = [MapMode::Normal, MapMode::Insert, MapMode::Visual];
}

/// Per-mode mapping tables.
#[derive(Default)]
pub struct Keymap {
    normal: Vec<Mapping>,
    insert: Vec<Mapping>,
    visual: Vec<Mapping>,
    /// What `<leader>` resolves to (`mapleader`). Space, as in most configs.
    leader: char,
}

impl Keymap {
    /// An empty table with the default `<leader>` (Space).
    pub fn new() -> Keymap {
        Keymap { leader: ' ', ..Default::default() }
    }

    fn table(&self, mode: MapMode) -> &Vec<Mapping> {
        match mode {
            MapMode::Normal => &self.normal,
            MapMode::Insert => &self.insert,
            MapMode::Visual => &self.visual,
        }
    }

    fn table_mut(&mut self, mode: MapMode) -> &mut Vec<Mapping> {
        match mode {
            MapMode::Normal => &mut self.normal,
            MapMode::Insert => &mut self.insert,
            MapMode::Visual => &mut self.visual,
        }
    }

    /// What `<leader>` currently expands to.
    pub fn leader(&self) -> char {
        self.leader
    }

    /// Set `mapleader`. Only affects mappings defined *after* this call, as in
    /// Vim — an already-parsed lhs holds real keys, not a leader placeholder.
    pub fn set_leader(&mut self, leader: char) {
        self.leader = leader;
    }

    /// Set a mapping in `mode`, `lhs`/`rhs` written in `<...>` key notation
    /// (`<Space>w` → `:w<CR>`). Replaces any existing mapping with the same lhs.
    ///
    /// The two sides are parsed differently, on purpose:
    ///
    /// - the **lhs** is pure key notation, so an unrecognized tag is an error.
    ///   A mistyped `<A-x>` must not quietly become five literal keystrokes
    ///   sitting in the table waiting for a sequence nobody will type.
    /// - the **rhs** is replayed *text*, which may legitimately contain a `<`
    ///   that isn't key notation — `:m '<-2<CR>` is an Ex range, not a broken
    ///   `<-2<CR>` key. So it falls back to literal characters, as Vim does.
    pub fn set_with_desc(
        &mut self,
        mode: MapMode,
        lhs: &str,
        rhs: &str,
        desc: Option<String>,
    ) -> Result<(), String> {
        let lhs = Key::try_parse_sequence_with_leader(lhs, self.leader)?;
        let rhs = Key::parse_sequence_with_leader(rhs, self.leader);
        if lhs.is_empty() {
            return Err("E5: mapping has an empty left-hand side".to_string());
        }
        let table = self.table_mut(mode);
        table.retain(|m| m.lhs != lhs);
        table.push(Mapping { lhs, rhs, desc });
        Ok(())
    }

    /// [`set_with_desc`](Self::set_with_desc) with no description.
    pub fn set(&mut self, mode: MapMode, lhs: &str, rhs: &str) -> Result<(), String> {
        self.set_with_desc(mode, lhs, rhs, None)
    }

    /// Set a normal-mode mapping.
    pub fn set_normal(&mut self, lhs: &str, rhs: &str) -> Result<(), String> {
        self.set(MapMode::Normal, lhs, rhs)
    }

    /// Remove a mapping (`:unmap`). Returns whether one was there.
    pub fn remove(&mut self, mode: MapMode, lhs: &str) -> bool {
        let Ok(lhs) = Key::try_parse_sequence_with_leader(lhs, self.leader) else {
            return false;
        };
        let table = self.table_mut(mode);
        let before = table.len();
        table.retain(|m| m.lhs != lhs);
        table.len() != before
    }

    /// Match `pending` against `mode`'s mappings.
    pub fn match_mode(&self, mode: MapMode, pending: &[Key]) -> KeymapMatch {
        let mut exact: Option<&Vec<Key>> = None;
        let mut has_longer = false;
        for m in self.table(mode) {
            if m.lhs == pending {
                exact = Some(&m.rhs);
            } else if m.lhs.len() > pending.len() && m.lhs.starts_with(pending) {
                has_longer = true;
            }
        }
        match (exact, has_longer) {
            // `<leader>q` typed while `<leader>qq` also exists: it *is* a
            // match, but firing now would make the longer one untypable.
            (Some(rhs), true) => KeymapMatch::FullAmbiguous(rhs.clone()),
            (Some(rhs), false) => KeymapMatch::Full(rhs.clone()),
            (None, true) => KeymapMatch::Prefix,
            (None, false) => KeymapMatch::None,
        }
    }

    /// Match `pending` against the normal-mode mappings.
    pub fn match_normal(&self, pending: &[Key]) -> KeymapMatch {
        self.match_mode(MapMode::Normal, pending)
    }

    /// True when `key` on its own could begin a mapping in `mode`.
    pub fn can_start(&self, mode: MapMode, key: Key) -> bool {
        !matches!(self.match_mode(mode, &[key]), KeymapMatch::None)
    }

    /// True when `key` on its own could begin a normal-mode mapping.
    pub fn can_start_normal(&self, key: Key) -> bool {
        self.can_start(MapMode::Normal, key)
    }

    /// Every mapping in `mode`, for `:map` with no arguments.
    pub fn list(&self, mode: MapMode) -> &[Mapping] {
        self.table(mode)
    }

    /// Every mapping that `pending` is a prefix of, as the keys still left to
    /// type plus what they do. This is what the which-key popup renders — it
    /// reads the live table, so a user's mapping shows up the moment it is
    /// defined and a changed default can never go stale.
    ///
    /// An exact match is included too (with an empty `rest`), so the ambiguous
    /// `<leader>q`/`<leader>qq` case shows both rows.
    pub fn continuations(&self, mode: MapMode, pending: &[Key]) -> Vec<Continuation> {
        let mut out: Vec<Continuation> = self
            .table(mode)
            .iter()
            .filter(|m| m.lhs.starts_with(pending))
            .map(|m| Continuation {
                rest: display_notation(&m.lhs[pending.len()..]),
                lhs: m.lhs_notation(),
                label: m.label(),
            })
            .collect();
        out.sort_by(|a, b| a.rest.cmp(&b.rest));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<Key> {
        Key::parse_sequence(s)
    }

    fn km() -> Keymap {
        Keymap::new()
    }

    #[test]
    fn full_prefix_none() {
        let mut km = km();
        km.set_normal("<Space>w", ":w<CR>").unwrap();
        km.set_normal("<Space>ff", ":Files<CR>").unwrap();

        assert_eq!(km.match_normal(&parse("<Space>")), KeymapMatch::Prefix);
        assert_eq!(km.match_normal(&parse("<Space>f")), KeymapMatch::Prefix);
        assert_eq!(km.match_normal(&parse("<Space>w")), KeymapMatch::Full(parse(":w<CR>")));
        assert_eq!(km.match_normal(&parse("<Space>x")), KeymapMatch::None);
        assert_eq!(km.match_normal(&parse("x")), KeymapMatch::None);
    }

    #[test]
    fn an_exact_match_that_is_also_a_prefix_is_ambiguous() {
        // charvim binds both `<leader>q` and `<leader>qq`. Firing `<leader>q`
        // on sight would make `<leader>qq` unreachable.
        let mut km = km();
        km.set_normal("<Space>q", ":wq<CR>").unwrap();
        km.set_normal("<Space>qq", ":q!<CR>").unwrap();

        assert_eq!(
            km.match_normal(&parse("<Space>q")),
            KeymapMatch::FullAmbiguous(parse(":wq<CR>")),
        );
        // The longer one is unambiguous once fully typed.
        assert_eq!(km.match_normal(&parse("<Space>qq")), KeymapMatch::Full(parse(":q!<CR>")));
    }

    #[test]
    fn removing_the_longer_mapping_makes_the_shorter_unambiguous() {
        let mut km = km();
        km.set_normal("<Space>q", ":wq<CR>").unwrap();
        km.set_normal("<Space>qq", ":q!<CR>").unwrap();
        assert!(km.remove(MapMode::Normal, "<Space>qq"));
        assert_eq!(km.match_normal(&parse("<Space>q")), KeymapMatch::Full(parse(":wq<CR>")));
    }

    #[test]
    fn modes_have_independent_tables() {
        let mut km = km();
        km.set(MapMode::Insert, "jk", "<Esc>").unwrap();
        // The insert mapping must not leak into normal mode.
        assert_eq!(km.match_normal(&parse("jk")), KeymapMatch::None);
        assert_eq!(
            km.match_mode(MapMode::Insert, &parse("jk")),
            KeymapMatch::Full(parse("<Esc>")),
        );
    }

    #[test]
    fn mode_letters_parse() {
        assert_eq!(MapMode::parse("i"), MapMode::Insert);
        assert_eq!(MapMode::parse("v"), MapMode::Visual);
        assert_eq!(MapMode::parse("x"), MapMode::Visual);
        assert_eq!(MapMode::parse("n"), MapMode::Normal);
        assert_eq!(MapMode::parse("garbage"), MapMode::Normal);
    }

    #[test]
    fn unmap_removes_a_mapping() {
        let mut km = km();
        km.set_normal("<Space>w", ":w<CR>").unwrap();
        assert!(km.remove(MapMode::Normal, "<Space>w"));
        assert!(!km.remove(MapMode::Normal, "<Space>w"), "already gone");
        assert_eq!(km.match_normal(&parse("<Space>w")), KeymapMatch::None);
    }

    #[test]
    fn set_normal_replaces() {
        let mut km = km();
        km.set_normal("<Space>w", ":w<CR>").unwrap();
        km.set_normal("<Space>w", ":wq<CR>").unwrap();
        assert_eq!(km.match_normal(&parse("<Space>w")), KeymapMatch::Full(parse(":wq<CR>")));
        assert_eq!(km.list(MapMode::Normal).len(), 1, "replaced, not appended");
    }

    #[test]
    fn a_bad_key_notation_is_rejected_rather_than_taken_literally() {
        let mut km = km();
        let e = km.set_normal("<Nope>", ":w<CR>").unwrap_err();
        assert!(e.contains("<Nope>"), "{e}");
        assert!(km.list(MapMode::Normal).is_empty(), "nothing was stored");
    }

    #[test]
    fn modifier_mappings_round_trip() {
        let mut km = km();
        km.set_normal("<A-j>", ":m .+1<CR>==").unwrap();
        km.set_normal("<C-S-k>", ":t .-1<CR>==").unwrap();
        assert_eq!(km.match_normal(&parse("<A-j>")), KeymapMatch::Full(parse(":m .+1<CR>==")));
        // `<C-k>` and `<C-S-k>` are different keys, so one must not match the other.
        assert_eq!(km.match_normal(&parse("<C-k>")), KeymapMatch::None);
        assert_eq!(km.list(MapMode::Normal)[0].lhs_notation(), "<A-j>");
        assert_eq!(km.list(MapMode::Normal)[1].lhs_notation(), "<C-S-k>");
    }

    #[test]
    fn leader_is_configurable_and_applies_at_definition_time() {
        let mut km = km();
        km.set_leader(',');
        km.set_normal("<leader>w", ":w<CR>").unwrap();
        assert_eq!(km.match_normal(&parse(",w")), KeymapMatch::Full(parse(":w<CR>")));
        assert_eq!(km.match_normal(&parse(" w")), KeymapMatch::None, "not Space any more");
    }

    #[test]
    fn continuations_describe_what_can_follow() {
        let mut km = km();
        km.set_with_desc(MapMode::Normal, "<Space>w", ":w<CR>", Some("Save file".into())).unwrap();
        km.set_with_desc(MapMode::Normal, "<Space>q", ":wq<CR>", Some("Save and quit".into()))
            .unwrap();
        // No desc: the label falls back to the rhs rather than being dropped.
        km.set_normal("<Space>d", ":dash<CR>").unwrap();
        km.set_normal("zz", "zz").unwrap();

        let c = km.continuations(MapMode::Normal, &parse("<Space>"));
        assert_eq!(
            c,
            vec![
                Continuation { rest: "d".into(), lhs: "<Space>d".into(), label: ":dash<CR>".into() },
                Continuation { rest: "q".into(), lhs: "<Space>q".into(), label: "Save and quit".into() },
                Continuation { rest: "w".into(), lhs: "<Space>w".into(), label: "Save file".into() },
            ],
            "only the <Space> chords, sorted, each labelled from the table"
        );
    }

    #[test]
    fn continuations_include_an_exact_match() {
        let mut km = km();
        km.set_normal("<Space>q", ":wq<CR>").unwrap();
        km.set_normal("<Space>qq", ":q!<CR>").unwrap();
        let c = km.continuations(MapMode::Normal, &parse("<Space>q"));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].rest, "", "the exact match itself");
        assert_eq!(c[1].rest, "q");
    }
}
