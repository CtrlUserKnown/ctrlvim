//! The interactive project-wide find & replace panel (`:Find`, `<leader>S`).
//!
//! This is the host half of [`ctrlvim_core::ReplacePlan`]: the engine decides
//! what matches and what the replacement produces, and everything here is the
//! part that needs the filesystem and a screen — walking the project, reading
//! files, showing the before/after, and writing the accepted edits back.
//!
//! **Where an edit lands** depends on whether the file is open:
//! - open as a buffer → the edit is applied to that buffer and left *unsaved*,
//!   so it joins the undo-able work in front of you and `:w` commits it;
//! - not open → written straight to disk, since there is no buffer to hold it.
//!
//! That split is what makes accepting a replacement in the file you are looking
//! at feel like an edit rather than a background rewrite.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ctrlvim_core::{ReplaceHit, ReplacePlan};

/// Which of the panel's three interactive areas has focus. `<Tab>` cycles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Find,
    Replace,
    Results,
}

impl Field {
    /// The next field in the `<Tab>` cycle.
    pub fn next(self) -> Field {
        match self {
            Field::Find => Field::Replace,
            Field::Replace => Field::Results,
            Field::Results => Field::Find,
        }
    }
}

/// A search is capped so a pattern like `.` over a large project can't fill
/// memory or stall the frame. The panel says so when it trims.
pub const MAX_HITS: usize = 2000;

/// Live state of the panel; present only while it is open.
pub struct ReplacePanel {
    /// The Vim pattern being searched for.
    pub find: String,
    /// What each match becomes (`\1`, `&` and `\r` work as in `:s`).
    pub replace: String,
    pub focus: Field,
    /// Matching lines across the project, in path order.
    pub hits: Vec<ReplaceHit>,
    /// Selection index into [`hits`](Self::hits).
    pub selected: usize,
    /// A pattern error to show instead of results (`E486: …`).
    pub error: Option<String>,
    /// Whether the search ignores case (`<C-i>` toggles).
    pub ignorecase: bool,
    /// True when the search stopped at [`MAX_HITS`].
    pub truncated: bool,
    /// Distinct files the hits fall in, for the counts line.
    pub files: usize,
    /// True for the grep entry point (`OpenGrepPrompt`): no Replace field,
    /// and accepting a result opens it instead of rewriting the project. See
    /// `ui/replace.rs` and `input.rs`'s `handle_replace`.
    pub search_only: bool,
}

impl ReplacePanel {
    /// Open the panel seeded with `pattern` (the word under the cursor, or a
    /// `:Find foo` argument). Focus starts on Find so typing refines it.
    pub fn new(pattern: Option<String>) -> ReplacePanel {
        ReplacePanel {
            find: pattern.unwrap_or_default(),
            replace: String::new(),
            focus: Field::Find,
            hits: Vec::new(),
            selected: 0,
            error: None,
            ignorecase: false,
            truncated: false,
            files: 0,
            search_only: false,
        }
    }

    /// Open in grep-only mode — the dashboard's "Find in Files" button.
    pub fn new_grep(pattern: Option<String>) -> ReplacePanel {
        let mut p = ReplacePanel::new(pattern);
        p.search_only = true;
        p
    }

    /// The compiled plan for the current fields, or the reason there isn't one.
    /// `None` (with no error) simply means the Find field is still empty.
    pub fn plan(&self) -> Option<Result<ReplacePlan, String>> {
        if self.find.is_empty() {
            return None;
        }
        Some(ReplacePlan::new(&self.find, &self.replace, self.ignorecase))
    }

    /// The text field with focus, for typing into. `None` while the results
    /// list has focus (where letters are commands, not input).
    fn active_input(&mut self) -> Option<&mut String> {
        match self.focus {
            Field::Find => Some(&mut self.find),
            Field::Replace => Some(&mut self.replace),
            Field::Results => None,
        }
    }

    /// Type a character into the focused field. Returns whether the *search*
    /// needs re-running (only edits to Find change what matches).
    pub fn type_char(&mut self, c: char) -> bool {
        let refind = self.focus == Field::Find;
        if let Some(field) = self.active_input() {
            field.push(c);
        }
        refind
    }

    /// Backspace in the focused field; same return meaning as [`type_char`](Self::type_char).
    pub fn backspace(&mut self) -> bool {
        let refind = self.focus == Field::Find;
        if let Some(field) = self.active_input() {
            field.pop();
        }
        refind
    }

    /// Delete the previous word in the focused field (Option+Backspace on
    /// macOS, Ctrl+Backspace on Linux); same return meaning as
    /// [`type_char`](Self::type_char).
    pub fn word_backspace(&mut self) -> bool {
        let refind = self.focus == Field::Find;
        if let Some(field) = self.active_input() {
            crate::app::delete_word_backward(field);
        }
        refind
    }

    /// Clear the focused field back to its start (Cmd+Backspace on macOS);
    /// same return meaning as [`type_char`](Self::type_char).
    pub fn clear_to_start(&mut self) -> bool {
        let refind = self.focus == Field::Find;
        if let Some(field) = self.active_input() {
            field.clear();
        }
        refind
    }

    /// Move the results selection by `dir`, saturating at both ends so held
    /// `j` doesn't wrap past the list.
    pub fn move_selection(&mut self, dir: i32) {
        if self.hits.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.hits.len() - 1;
        self.selected = (self.selected as i32 + dir).clamp(0, last as i32) as usize;
    }

    /// The next field in the focus cycle, skipping Replace in grep-only mode
    /// where it isn't shown.
    fn next_focus(&self) -> Field {
        let n = self.focus.next();
        if self.search_only && n == Field::Replace { n.next() } else { n }
    }

    /// `<Tab>` — advance focus one step.
    pub fn cycle_focus(&mut self) {
        self.focus = self.next_focus();
    }

    /// `<S-Tab>` — move focus back one step. Backward is forward twice around
    /// the full three-field cycle; in grep-only mode's two-field cycle,
    /// forward and backward are the same single step.
    pub fn cycle_focus_back(&mut self) {
        self.cycle_focus();
        if !self.search_only {
            self.cycle_focus();
        }
    }

    /// The highlighted hit, if any.
    pub fn current(&self) -> Option<&ReplaceHit> {
        self.hits.get(self.selected)
    }

    /// Install a fresh result set, keeping the selection in range.
    pub fn set_hits(&mut self, hits: Vec<ReplaceHit>, truncated: bool) {
        self.files = distinct_files(&hits);
        self.truncated = truncated;
        self.hits = hits;
        self.error = None;
        self.selected = self.selected.min(self.hits.len().saturating_sub(1));
    }

    /// Recompute every hit's `preview` against the current Replace text.
    ///
    /// Editing the replacement changes what each match *becomes* but not which
    /// lines match, so this is the cheap half of a re-search: it reuses the
    /// text already collected instead of walking the project again.
    pub fn refresh_previews(&mut self) {
        let Some(Ok(plan)) = self.plan() else { return };
        for hit in &mut self.hits {
            if let Some(preview) = plan.apply_line(&hit.text) {
                hit.preview = preview;
            }
        }
    }

    /// Report a pattern error and clear the stale results behind it.
    pub fn set_error(&mut self, message: String) {
        self.hits.clear();
        self.files = 0;
        self.truncated = false;
        self.selected = 0;
        self.error = Some(message);
    }

    /// Total matches across all hits (a line can hold several).
    pub fn match_count(&self) -> usize {
        self.hits.iter().map(|h| h.count).sum()
    }

    /// The status shown under the results list: an error, a count, or a nudge.
    pub fn summary(&self) -> String {
        if let Some(err) = &self.error {
            return err.clone();
        }
        if self.find.is_empty() {
            return "type a pattern to search the project".into();
        }
        if self.hits.is_empty() {
            return "no matches".into();
        }
        let matches = self.match_count();
        let files = self.files;
        let mut s = format!(
            "{matches} match{} in {files} file{}",
            if matches == 1 { "" } else { "es" },
            if files == 1 { "" } else { "s" },
        );
        if self.truncated {
            s.push_str(&format!(" (stopped at {MAX_HITS})"));
        }
        s
    }
}

/// How many distinct files a hit list touches.
fn distinct_files(hits: &[ReplaceHit]) -> usize {
    let mut paths: Vec<&PathBuf> = hits.iter().map(|h| &h.path).collect();
    paths.sort();
    paths.dedup();
    paths.len()
}

/// Group hits by file, preserving path order and each file's line order — the
/// shape "accept all" needs, since it rewrites one file at a time.
pub fn by_file(hits: &[ReplaceHit]) -> BTreeMap<PathBuf, Vec<&ReplaceHit>> {
    let mut map: BTreeMap<PathBuf, Vec<&ReplaceHit>> = BTreeMap::new();
    for hit in hits {
        map.entry(hit.path.clone()).or_default().push(hit);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn hit(path: &str, line: usize, count: usize) -> ReplaceHit {
        ReplaceHit {
            path: PathBuf::from(path),
            line,
            col: 0,
            text: "foo".into(),
            preview: "bar".into(),
            count,
        }
    }

    #[test]
    fn tab_cycles_the_three_fields() {
        assert_eq!(Field::Find.next(), Field::Replace);
        assert_eq!(Field::Replace.next(), Field::Results);
        assert_eq!(Field::Results.next(), Field::Find);
    }

    #[test]
    fn typing_only_re_searches_from_the_find_field() {
        let mut p = ReplacePanel::new(None);
        assert!(p.type_char('f'), "editing Find changes what matches");
        assert_eq!(p.find, "f");
        p.focus = Field::Replace;
        assert!(!p.type_char('x'), "editing Replace only changes the preview");
        assert_eq!((p.find.as_str(), p.replace.as_str()), ("f", "x"));
        // Letters in the results list are commands, not input.
        p.focus = Field::Results;
        assert!(!p.type_char('y'));
        assert_eq!((p.find.as_str(), p.replace.as_str()), ("f", "x"));
    }

    #[test]
    fn backspace_edits_the_focused_field_too() {
        let mut p = ReplacePanel::new(Some("foo".into()));
        assert!(p.backspace());
        assert_eq!(p.find, "fo");
        p.focus = Field::Results;
        assert!(!p.backspace());
        assert_eq!(p.find, "fo", "the results list swallows backspace");
    }

    #[test]
    fn word_backspace_deletes_the_last_word_of_the_focused_field() {
        let mut p = ReplacePanel::new(Some("hello world".into()));
        assert!(p.word_backspace());
        assert_eq!(p.find, "hello ");
        p.focus = Field::Results;
        assert!(!p.word_backspace());
        assert_eq!(p.find, "hello ", "the results list swallows it");
    }

    #[test]
    fn clear_to_start_empties_the_focused_field() {
        let mut p = ReplacePanel::new(Some("hello world".into()));
        p.focus = Field::Replace;
        p.type_char('x');
        assert!(!p.clear_to_start(), "editing Replace doesn't re-search");
        assert_eq!((p.find.as_str(), p.replace.as_str()), ("hello world", ""));
    }

    #[test]
    fn the_panel_opens_seeded_with_the_word_under_the_cursor() {
        let p = ReplacePanel::new(Some("widget".into()));
        assert_eq!(p.find, "widget");
        assert_eq!(p.focus, Field::Find, "so typing refines the seed");
    }

    #[test]
    fn selection_saturates_at_both_ends() {
        let mut p = ReplacePanel::new(None);
        p.set_hits(vec![hit("a.rs", 0, 1), hit("a.rs", 4, 1), hit("b.rs", 2, 1)], false);
        p.move_selection(-1);
        assert_eq!(p.selected, 0, "before the first clamps");
        p.move_selection(9);
        assert_eq!(p.selected, 2, "past the last clamps");
        assert_eq!(p.current().unwrap().path, PathBuf::from("b.rs"));
    }

    #[test]
    fn a_shrinking_result_set_pulls_the_selection_back_in_range() {
        let mut p = ReplacePanel::new(None);
        p.set_hits(vec![hit("a.rs", 0, 1), hit("a.rs", 1, 1), hit("a.rs", 2, 1)], false);
        p.move_selection(2);
        assert_eq!(p.selected, 2);
        p.set_hits(vec![hit("a.rs", 0, 1)], false);
        assert_eq!(p.selected, 0, "no dangling selection past the end");
        assert!(p.current().is_some());
    }

    #[test]
    fn the_summary_counts_matches_and_files_not_lines() {
        let mut p = ReplacePanel::new(Some("foo".into()));
        p.set_hits(vec![hit("a.rs", 0, 2), hit("a.rs", 4, 1), hit("b.rs", 2, 1)], false);
        assert_eq!(p.match_count(), 4);
        assert_eq!(p.summary(), "4 matches in 2 files");
        // Singulars read correctly.
        p.set_hits(vec![hit("a.rs", 0, 1)], false);
        assert_eq!(p.summary(), "1 match in 1 file");
    }

    #[test]
    fn the_summary_explains_an_empty_result() {
        let mut p = ReplacePanel::new(None);
        assert_eq!(p.summary(), "type a pattern to search the project");
        p.find = "zzz".into();
        p.set_hits(Vec::new(), false);
        assert_eq!(p.summary(), "no matches");
        p.set_error("E486: invalid pattern".into());
        assert_eq!(p.summary(), "E486: invalid pattern");
    }

    #[test]
    fn a_pattern_error_clears_the_results_behind_it() {
        let mut p = ReplacePanel::new(Some("foo".into()));
        p.set_hits(vec![hit("a.rs", 0, 1)], false);
        p.set_error("E486: invalid pattern".into());
        assert!(p.hits.is_empty(), "stale hits would be replaced by the wrong thing");
        assert!(p.current().is_none());
    }

    #[test]
    fn an_empty_find_field_compiles_to_nothing_rather_than_erroring() {
        let p = ReplacePanel::new(None);
        assert!(p.plan().is_none(), "an empty field is not an error to show");
        // An unclosed group is a genuine Vim error (E54). An unclosed `[` is
        // not: Vim reads a bracket that starts no valid collection as a
        // literal, so `/[` searches for a bracket.
        let mut p = ReplacePanel::new(Some(r"\(unclosed".into()));
        assert!(p.plan().unwrap().is_err());
        p.find = "[unclosed".into();
        assert!(p.plan().unwrap().is_ok(), "a bare [ is a literal, not an error");
        p.find = "foo".into();
        assert!(p.plan().unwrap().is_ok());
    }

    #[test]
    fn the_ignorecase_toggle_reaches_the_compiled_plan() {
        let mut p = ReplacePanel::new(Some("FOO".into()));
        let plan = p.plan().unwrap().unwrap();
        assert!(plan.hits_in(Path::new("a.rs"), "foo").is_empty());
        p.ignorecase = true;
        let plan = p.plan().unwrap().unwrap();
        assert_eq!(plan.hits_in(Path::new("a.rs"), "foo").len(), 1);
    }

    #[test]
    fn editing_the_replacement_refreshes_the_previews_in_place() {
        let mut p = ReplacePanel::new(Some("foo".into()));
        let mut h = hit("a.rs", 0, 1);
        h.text = "let foo = 1;".into();
        p.set_hits(vec![h], false);
        p.focus = Field::Replace;

        p.type_char('b');
        p.refresh_previews();
        assert_eq!(p.hits[0].preview, "let b = 1;");
        p.type_char('z');
        p.refresh_previews();
        assert_eq!(p.hits[0].preview, "let bz = 1;", "a stale preview would lie about the edit");
        // The result set itself is untouched — only what each match becomes.
        assert_eq!(p.hits.len(), 1);
        assert_eq!(p.hits[0].text, "let foo = 1;");
    }

    #[test]
    fn hits_group_by_file_keeping_line_order() {
        let hits = vec![hit("b.rs", 2, 1), hit("a.rs", 9, 1), hit("a.rs", 1, 1)];
        let grouped = by_file(&hits);
        assert_eq!(grouped.len(), 2);
        let a: Vec<usize> = grouped[Path::new("a.rs")].iter().map(|h| h.line).collect();
        assert_eq!(a, vec![9, 1], "insertion order within a file is preserved");
    }
}
