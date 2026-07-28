//! The matcher — a backtracking VM, the counterpart to `nfa_regmatch`.
//!
//! Backtracking rather than simulation is a deliberate choice. A Thompson/Pike
//! simulation is immune to exponential blowup, but it cannot evaluate a
//! backreference (the set of live threads has no single history to compare
//! against) and it cannot run a lookaround as a sub-match. Vim's syntax has
//! both, so the engine backtracks and defends itself with a step budget
//! instead — see [`Limits`].
//!
//! Positions are **character indices** throughout. Byte offsets appear only at
//! the public boundary, because every column in the editor is a character
//! column and converting once at the edge beats converting in the inner loop.

use crate::parse::{Assert, Cmp};
use crate::program::{Inst, Program};

/// The text being searched, with the char/byte mapping precomputed.
pub struct Input<'t> {
    pub text: &'t str,
    chars: Vec<char>,
    /// Byte offset of each character, plus the total length as a final entry,
    /// so `offsets[i]` is valid for `i == chars.len()`.
    offsets: Vec<usize>,
}

impl<'t> Input<'t> {
    pub fn new(text: &'t str) -> Input<'t> {
        let mut chars = Vec::new();
        let mut offsets = Vec::new();
        for (b, c) in text.char_indices() {
            offsets.push(b);
            chars.push(c);
        }
        offsets.push(text.len());
        Input { text, chars, offsets }
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn char_at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    /// Byte offset of character index `i`.
    pub fn byte_of(&self, i: usize) -> usize {
        self.offsets.get(i).copied().unwrap_or(self.text.len())
    }

    /// Character index of byte offset `b`, rounded up to a boundary.
    pub fn char_of_byte(&self, b: usize) -> usize {
        match self.offsets.binary_search(&b) {
            Ok(i) => i,
            Err(i) => i,
        }
    }

    fn slice(&self, from: usize, to: usize) -> &[char] {
        &self.chars[from.min(self.chars.len())..to.min(self.chars.len())]
    }
}

/// Everything an assertion needs that the text alone cannot answer.
///
/// Every field is optional, and an assertion whose data is missing evaluates to
/// **true** — it stops constraining rather than silently failing. That matters
/// because the same compiled pattern is used by `:s` (which knows the line
/// number) and by `:vimgrep` (which does not); the alternative would make
/// `\%23l` quietly match nothing in the second case.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// 1-based line number, for `\%23l`.
    pub line: Option<usize>,
    /// Whether this line is the first/last in the buffer, for `\%^` and `\%$`.
    pub first_line: Option<bool>,
    pub last_line: Option<bool>,
    /// 0-based character column of the cursor, for `\%#`.
    pub cursor_col: Option<usize>,
    /// 0-based character columns of the Visual area on this line, for `\%V`.
    pub visual: Option<(usize, usize)>,
    /// `'tabstop'`, for the virtual columns `\%23v` compares against.
    pub tabstop: usize,
}

impl Context {
    pub fn new() -> Context {
        Context { tabstop: 8, ..Context::default() }
    }
}

/// How much work a single match attempt may do.
///
/// A backtracking engine can be driven exponential by a pattern like
/// `\(a*\)*b` on a long run of `a`s. Vim gets slow there too, but ctrlvim runs
/// this on every keystroke of the find panel, so exhausting the budget reports
/// "no match" rather than freezing the editor. The ceiling is high enough that
/// no reasonable pattern reaches it.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub steps: usize,
    /// How far back a lookbehind will look.
    pub lookbehind: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { steps: 1_000_000, lookbehind: 512 }
    }
}

/// A successful match: capture slots plus the reported boundaries.
#[derive(Debug, Clone)]
pub struct Match {
    /// Slot pairs in character indices; slot `2n`/`2n+1` bracket group `n`.
    pub slots: Vec<Option<usize>>,
    /// Match start, after any `\zs`.
    pub start: usize,
    /// Match end, after any `\ze`.
    pub end: usize,
}

#[derive(Clone)]
struct Thread {
    pc: usize,
    pos: usize,
    slots: Vec<Option<usize>>,
    loops: Vec<usize>,
    zs: Option<usize>,
    ze: Option<usize>,
}

/// Search for the leftmost match at or after character index `from`.
pub fn find_at(
    prog: &Program,
    input: &Input,
    from: usize,
    ctx: &Context,
    ignorecase: bool,
    limits: Limits,
) -> Option<Match> {
    let mut budget = limits.steps;
    let slots = vec![None; prog.slots()];
    // An anchored pattern can only start at column zero, so one attempt is all
    // it ever needs.
    if prog.anchored {
        if from > 0 {
            return None;
        }
        return run(prog, input, 0, &slots, ctx, ignorecase, limits, &mut budget, None)
            .map(|t| finish(t, 0));
    }
    for start in from..=input.len() {
        if let Some(t) = run(prog, input, start, &slots, ctx, ignorecase, limits, &mut budget, None)
        {
            return Some(finish(t, start));
        }
        if budget == 0 {
            return None;
        }
    }
    None
}

/// Turn a winning thread into a [`Match`], applying `\zs`/`\ze`.
fn finish(t: Thread, start: usize) -> Match {
    let s = t.zs.or(t.slots.first().copied().flatten()).unwrap_or(start);
    let e = t.ze.or(t.slots.get(1).copied().flatten()).unwrap_or(t.pos);
    let mut slots = t.slots;
    // Group 0 reports the adjusted span, so `\zs` is invisible to callers that
    // only ask for the whole match.
    if slots.len() >= 2 {
        slots[0] = Some(s);
        slots[1] = Some(e);
    }
    Match { slots, start: s, end: e.max(s) }
}

/// Run `prog` anchored at `start`.
///
/// `require_end` makes the run succeed only if it finishes exactly there, which
/// is how a lookbehind asks "does this match end where I am?".
#[allow(clippy::too_many_arguments)]
fn run(
    prog: &Program,
    input: &Input,
    start: usize,
    init_slots: &[Option<usize>],
    ctx: &Context,
    ignorecase: bool,
    limits: Limits,
    budget: &mut usize,
    require_end: Option<usize>,
) -> Option<Thread> {
    let mut stack = vec![Thread {
        pc: 0,
        pos: start,
        slots: init_slots.to_vec(),
        loops: vec![usize::MAX; prog.loops],
        zs: None,
        ze: None,
    }];

    'threads: while let Some(mut t) = stack.pop() {
        loop {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;

            match prog.insts[t.pc] {
                Inst::Char(c) => match input.char_at(t.pos) {
                    Some(got) if got == c || (ignorecase && crate::class::eq_fold(c, got)) => {
                        t.pos += 1;
                        t.pc += 1;
                    }
                    _ => continue 'threads,
                },
                Inst::Any { nl } => match input.char_at(t.pos) {
                    Some('\n') if !nl => continue 'threads,
                    Some(_) => {
                        t.pos += 1;
                        t.pc += 1;
                    }
                    None => continue 'threads,
                },
                Inst::Class { class, nl } => match input.char_at(t.pos) {
                    Some(c) if (nl && c == '\n') || prog.classes[class].matches(c, ignorecase) => {
                        t.pos += 1;
                        t.pc += 1;
                    }
                    _ => continue 'threads,
                },
                Inst::Split(a, b) => {
                    let mut alt = t.clone();
                    alt.pc = b;
                    stack.push(alt);
                    t.pc = a;
                }
                Inst::Jump(a) => t.pc = a,
                Inst::Save(n) => {
                    if n < t.slots.len() {
                        t.slots[n] = Some(t.pos);
                    }
                    t.pc += 1;
                }
                Inst::SetStart => {
                    t.zs = Some(t.pos);
                    t.pc += 1;
                }
                Inst::SetEnd => {
                    t.ze = Some(t.pos);
                    t.pc += 1;
                }
                Inst::LoopStart(id) => {
                    t.loops[id] = t.pos;
                    t.pc += 1;
                }
                Inst::LoopCheck(id) => {
                    // The body matched empty — stop rather than loop forever.
                    if t.loops[id] == t.pos {
                        continue 'threads;
                    }
                    t.pc += 1;
                }
                Inst::Backref(n) => {
                    let (Some(s), Some(e)) =
                        (t.slots.get(n * 2).copied().flatten(), t.slots.get(n * 2 + 1).copied().flatten())
                    else {
                        // An unset group matches the empty string, as in Vim.
                        t.pc += 1;
                        continue;
                    };
                    let want = input.slice(s, e);
                    let got = input.slice(t.pos, t.pos + want.len());
                    if got.len() != want.len() {
                        continue 'threads;
                    }
                    let same = want.iter().zip(got).all(|(a, b)| {
                        a == b || (ignorecase && crate::class::eq_fold(*a, *b))
                    });
                    if !same {
                        continue 'threads;
                    }
                    t.pos += want.len();
                    t.pc += 1;
                }
                Inst::Assert(a) => {
                    if !assert_holds(a, input, t.pos, ctx) {
                        continue 'threads;
                    }
                    t.pc += 1;
                }
                Inst::Look { negate, behind, prog: sub } => {
                    let sub_prog = &prog.subs[sub];
                    let found = if behind {
                        look_behind(sub_prog, input, &t, ctx, ignorecase, limits, budget)
                    } else {
                        run(
                            sub_prog, input, t.pos, &t.slots, ctx, ignorecase, limits, budget, None,
                        )
                    };
                    match (found, negate) {
                        // A positive lookaround keeps what it captured, so
                        // `\(\d\+\)\@=` leaves `\1` readable afterwards.
                        (Some(sub_t), false) => {
                            t.slots = sub_t.slots;
                            t.pc += 1;
                        }
                        (None, true) => t.pc += 1,
                        _ => continue 'threads,
                    }
                }
                Inst::Atomic { prog: sub } => {
                    let sub_prog = &prog.subs[sub];
                    match run(
                        sub_prog, input, t.pos, &t.slots, ctx, ignorecase, limits, budget, None,
                    ) {
                        Some(sub_t) => {
                            // Commit: no backtracking back into the group.
                            t.pos = sub_t.pos;
                            t.slots = sub_t.slots;
                            t.pc += 1;
                        }
                        None => continue 'threads,
                    }
                }
                Inst::Match => {
                    if require_end.is_some_and(|e| e != t.pos) {
                        continue 'threads;
                    }
                    return Some(t);
                }
            }
        }
    }
    None
}

/// Try every start position within the lookbehind window, requiring the
/// sub-match to end exactly where the outer match stands.
#[allow(clippy::too_many_arguments)]
fn look_behind(
    sub: &Program,
    input: &Input,
    t: &Thread,
    ctx: &Context,
    ignorecase: bool,
    limits: Limits,
    budget: &mut usize,
) -> Option<Thread> {
    let lowest = t.pos.saturating_sub(limits.lookbehind);
    // Nearest first: Vim prefers the shortest look-behind that works.
    for start in (lowest..=t.pos).rev() {
        if let Some(found) = run(
            sub,
            input,
            start,
            &t.slots,
            ctx,
            ignorecase,
            limits,
            budget,
            Some(t.pos),
        ) {
            return Some(found);
        }
        if *budget == 0 {
            return None;
        }
    }
    None
}

/// Evaluate a zero-width assertion at `pos`.
fn assert_holds(a: Assert, input: &Input, pos: usize, ctx: &Context) -> bool {
    match a {
        Assert::LineStart => pos == 0 || input.char_at(pos - 1) == Some('\n'),
        Assert::LineEnd => pos == input.len() || input.char_at(pos) == Some('\n'),
        Assert::WordStart => {
            let before = pos > 0 && is_word(input.char_at(pos - 1));
            let here = is_word(input.char_at(pos));
            here && !before
        }
        Assert::WordEnd => {
            let before = pos > 0 && is_word(input.char_at(pos - 1));
            let here = is_word(input.char_at(pos));
            before && !here
        }
        // Missing context means "do not constrain" — see [`Context`].
        Assert::BufStart => ctx.first_line.unwrap_or(true) && pos == 0,
        Assert::BufEnd => ctx.last_line.unwrap_or(true) && pos == input.len(),
        Assert::Cursor => ctx.cursor_col.is_none_or(|c| c == pos),
        Assert::VisualArea => ctx.visual.is_none_or(|(s, e)| pos >= s && pos <= e),
        Assert::Line(cmp, n) => ctx.line.is_none_or(|l| compare(cmp, l, n)),
        Assert::Col(cmp, n) => compare(cmp, input.byte_of(pos) + 1, n),
        Assert::VCol(cmp, n) => compare(cmp, vcol(input, pos, ctx.tabstop.max(1)) + 1, n),
    }
}

fn compare(cmp: Cmp, lhs: usize, rhs: usize) -> bool {
    match cmp {
        Cmp::Before => lhs < rhs,
        Cmp::At => lhs == rhs,
        Cmp::After => lhs > rhs,
    }
}

/// Virtual column of `pos`: like a character column, except a tab advances to
/// the next multiple of `'tabstop'`.
fn vcol(input: &Input, pos: usize, tabstop: usize) -> usize {
    let mut v = 0usize;
    for i in 0..pos {
        match input.char_at(i) {
            Some('\t') => v += tabstop - (v % tabstop),
            Some(_) => v += 1,
            None => break,
        }
    }
    v
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, Magic};
    use crate::program::compile;

    fn find(pat: &str, text: &str) -> Option<(usize, usize)> {
        let p = parse(pat, Magic::Magic, "").expect("parses");
        let prog = compile(&p.ast, p.groups);
        let input = Input::new(text);
        find_at(&prog, &input, 0, &Context::new(), false, Limits::default())
            .map(|m| (m.start, m.end))
    }

    #[test]
    fn a_backreference_matches_what_the_group_captured() {
        assert_eq!(find(r"\(ab\)\1", "xxabab"), Some((2, 6)));
        assert_eq!(find(r"\(ab\)\1", "xxabcd"), None);
        // The classic doubled-word search.
        assert_eq!(find(r"\<\(\w\+\)\s\+\1\>", "the the end"), Some((0, 7)));
    }

    #[test]
    fn zs_and_ze_move_the_reported_boundaries() {
        // `foo\zsbar` matches "bar" but only after "foo".
        assert_eq!(find(r"foo\zsbar", "foobar"), Some((3, 6)));
        assert_eq!(find(r"foo\zsbar", "xxxbar"), None);
        // `\ze` trims the tail off the match.
        assert_eq!(find(r"foo\zebar", "foobar"), Some((0, 3)));
    }

    #[test]
    fn lookahead_constrains_without_consuming() {
        assert_eq!(find(r"foo\(bar\)\@=", "foobar"), Some((0, 3)));
        assert_eq!(find(r"foo\(bar\)\@=", "foobaz"), None);
        assert_eq!(find(r"foo\(bar\)\@!", "foobaz"), Some((0, 3)));
        assert_eq!(find(r"foo\(bar\)\@!", "foobar"), None);
    }

    #[test]
    fn lookbehind_constrains_what_precedes() {
        assert_eq!(find(r"\(foo\)\@<=bar", "foobar"), Some((3, 6)));
        assert_eq!(find(r"\(foo\)\@<=bar", "bazbar"), None);
        assert_eq!(find(r"\(foo\)\@<!bar", "bazbar"), Some((3, 6)));
        assert_eq!(find(r"\(foo\)\@<!bar", "foobar"), None);
    }

    #[test]
    fn an_atomic_group_does_not_give_text_back() {
        // `\(a*\)\@>a` can never match: the atomic group takes every `a`.
        assert_eq!(find(r"\(a*\)\@>a", "aaa"), None);
        // Without the atomic marker the same shape matches.
        assert_eq!(find(r"\(a*\)a", "aaa"), Some((0, 3)));
    }

    #[test]
    fn an_empty_loop_body_terminates() {
        // The guard in `emit_star` is what stops this from hanging.
        assert_eq!(find(r"\(x*\)*y", "y"), Some((0, 1)));
        assert_eq!(find(r"\(\)*a", "a"), Some((0, 1)));
    }

    #[test]
    fn lazy_repeats_prefer_the_shortest_match() {
        assert_eq!(find(r"a.\{-}b", "axxbxxb"), Some((0, 4)));
        assert_eq!(find(r"a.*b", "axxbxxb"), Some((0, 7)));
    }

    #[test]
    fn alternation_takes_the_first_branch_that_matches() {
        assert_eq!(find(r"foo\|foobar", "foobar"), Some((0, 3)));
        assert_eq!(find(r"foobar\|foo", "foobar"), Some((0, 6)));
    }

    #[test]
    fn word_boundaries_use_the_keyword_set() {
        assert_eq!(find(r"\<fn\>", "fn main"), Some((0, 2)));
        assert_eq!(find(r"\<fn\>", "effner"), None);
    }

    #[test]
    fn the_match_is_leftmost() {
        assert_eq!(find("b", "abcb"), Some((1, 2)));
    }

    #[test]
    fn positions_are_characters_not_bytes() {
        // Four 3-byte characters, then the target.
        assert_eq!(find("x", "日本語版x"), Some((4, 5)));
    }

    #[test]
    fn a_runaway_pattern_gives_up_instead_of_hanging() {
        let p = parse(r"\(a*\)*b", Magic::Magic, "").expect("parses");
        let prog = compile(&p.ast, p.groups);
        let text = "a".repeat(60);
        let input = Input::new(&text);
        // Exponential on paper; bounded here, and it returns rather than hangs.
        let limits = Limits { steps: 50_000, lookbehind: 512 };
        assert!(find_at(&prog, &input, 0, &Context::new(), false, limits).is_none());
    }

    #[test]
    fn line_assertions_constrain_only_when_context_is_supplied() {
        let p = parse(r"\%2lfoo", Magic::Magic, "").expect("parses");
        let prog = compile(&p.ast, p.groups);
        let input = Input::new("foo");
        let hit = |line| {
            let ctx = Context { line, ..Context::new() };
            find_at(&prog, &input, 0, &ctx, false, Limits::default()).is_some()
        };
        assert!(hit(Some(2)));
        assert!(!hit(Some(3)));
        // No line number known: the assertion stops constraining.
        assert!(hit(None));
    }

    #[test]
    fn virtual_columns_account_for_tabstops() {
        let p = parse(r"\%9vx", Magic::Magic, "").expect("parses");
        let prog = compile(&p.ast, p.groups);
        // One tab, then `x` — with tabstop 8 the `x` sits at virtual column 9.
        let input = Input::new("\tx");
        let ctx = Context { tabstop: 8, ..Context::new() };
        assert!(find_at(&prog, &input, 0, &ctx, false, Limits::default()).is_some());
    }
}
