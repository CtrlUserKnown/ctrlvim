//! The pattern parser — the front half of `regexp.c`'s `nfa_regcomp`.
//!
//! Vim's regex syntax is not one language but four, selected by the "magic"
//! level: `\v` (very magic, PCRE-like), `\m` (magic, the default), `\M`
//! (nomagic) and `\V` (very nomagic). They differ only in *which characters
//! need a backslash*, never in what the constructs mean, so this parser keeps a
//! single grammar and consults [`Magic`] at exactly one place: deciding whether
//! a bare character is an operator or a literal.
//!
//! The rule that makes that work is worth stating, because it collapses four
//! syntaxes into one table. A character is an operator when it appears *bare*
//! and the level says it is special; a backslash **inverts** that. So `(` is a
//! group opener bare in `\v` and backslashed everywhere else, and the single
//! predicate [`Magic::bare_special`] decides both directions.
//!
//! The grammar itself follows `:h pattern`:
//!
//! ```text
//! pattern ::= branch  \|  branch  …      (alternation, first match wins)
//! branch  ::= concat  \&  concat  …      (all must match here; last one counts)
//! concat  ::= piece piece …
//! piece   ::= atom multi?                (multi = * \+ \? \{n,m} \@= …)
//! ```
//!
//! `\&` is the odd one: `foo\&..` matches the first two characters of a line
//! only when `foo` also matches there. That is precisely a positive lookahead,
//! so it is desugared into one rather than given its own node type.

use crate::class::{Class, Item, Named};

/// A parse failure, carrying the `E…` code Vim would report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// The magic level — how much backslashing the pattern needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Magic {
    /// `\v` — every non-alphanumeric character is an operator.
    Very,
    /// `\m` — Vim's default.
    Magic,
    /// `\M` — `.` `*` `[` `~` become literal.
    No,
    /// `\V` — only a backslash is special.
    VeryNo,
}

impl Magic {
    /// Whether `c` acts as an operator when written without a backslash.
    ///
    /// `^` and `$` are deliberately absent: they anchor only in certain
    /// positions, which [`Parser::parse_atom`] decides with context this
    /// predicate does not have.
    fn bare_special(self, c: char) -> bool {
        match self {
            Magic::Very => !c.is_alphanumeric() && c != '_',
            Magic::Magic => matches!(c, '.' | '*' | '[' | '~'),
            Magic::No | Magic::VeryNo => false,
        }
    }
}

/// A parsed pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast {
    /// Matches at any position without consuming — the empty branch in `a\|`.
    Empty,
    Literal(char),
    /// `.` — any character except a newline (`\_.` clears that exception).
    Any {
        nl: bool,
    },
    Class {
        class: Class,
        /// `\_` prefix: the class additionally matches a newline.
        nl: bool,
    },
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Group {
        /// `Some(n)` for `\(…\)`, `None` for the non-capturing `\%(…\)`.
        index: Option<usize>,
        node: Box<Ast>,
    },
    Repeat {
        node: Box<Ast>,
        min: u32,
        max: Option<u32>,
        /// `false` for the `\{-…}` forms, which prefer the shortest match.
        greedy: bool,
    },
    /// `\1`–`\9` — matches the text a previous group captured.
    Backref(usize),
    Assert(Assert),
    /// `\zs` — everything before this is context, not part of the match.
    MatchStart,
    /// `\ze` — everything after this is context.
    MatchEnd,
    /// `\@=`, `\@!`, `\@<=`, `\@<!` — match without consuming.
    Look {
        node: Box<Ast>,
        negate: bool,
        behind: bool,
    },
    /// `\@>` — match once and never give the text back.
    Atomic(Box<Ast>),
}

/// A zero-width condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assert {
    /// `^`
    LineStart,
    /// `$`
    LineEnd,
    /// `\<`
    WordStart,
    /// `\>`
    WordEnd,
    /// `\%^`
    BufStart,
    /// `\%$`
    BufEnd,
    /// `\%#` — the cursor position.
    Cursor,
    /// `\%V` — inside the last Visual selection.
    VisualArea,
    /// `\%23l` and its `\%<23l` / `\%>23l` comparisons.
    Line(Cmp, usize),
    /// `\%23c` — byte column.
    Col(Cmp, usize),
    /// `\%23v` — virtual (screen) column.
    VCol(Cmp, usize),
}

/// The comparison in a positional assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Before,
    At,
    After,
}

/// What a parse produced, alongside the flags the pattern itself set.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub ast: Ast,
    pub groups: usize,
    /// `Some` when the pattern contained `\c` or `\C`, which override the
    /// caller's `'ignorecase'` entirely — that is the whole point of them.
    pub force_ignorecase: Option<bool>,
}

/// Parse `pat` at the given starting magic level.
///
/// `last_sub` is the text `~` stands for (the previous `:s` replacement); an
/// empty string leaves `~` matching itself, which is friendlier than failing.
pub fn parse(pat: &str, magic: Magic, last_sub: &str) -> Result<Parsed> {
    let mut p = Parser {
        chars: pat.chars().collect(),
        i: 0,
        magic,
        groups: 0,
        force_ignorecase: None,
        last_sub: last_sub.chars().collect(),
        depth: 0,
        branch_start: true,
    };
    let ast = p.parse_alt()?;
    if p.i < p.chars.len() {
        // The only way to stop early is a `\)` with no opener.
        return Err(Error("E55: Unmatched \\)".into()));
    }
    Ok(Parsed { ast, groups: p.groups, force_ignorecase: p.force_ignorecase })
}

/// How deeply groups may nest before we refuse. Vim has a similar ceiling; the
/// point is to fail on a pathological pattern rather than exhaust the stack.
const MAX_DEPTH: usize = 100;

struct Parser {
    chars: Vec<char>,
    i: usize,
    magic: Magic,
    groups: usize,
    force_ignorecase: Option<bool>,
    last_sub: Vec<char>,
    depth: usize,
    /// Whether nothing consuming has been parsed in the current concat, which
    /// is exactly where `^` anchors instead of standing for itself.
    branch_start: bool,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    /// Consume `s` if it is next, reporting whether it was.
    fn eat(&mut self, s: &str) -> bool {
        let want: Vec<char> = s.chars().collect();
        if self.chars[self.i..].starts_with(&want) {
            self.i += want.len();
            true
        } else {
            false
        }
    }

    /// Whether the operator `c` is next, in whichever spelling this magic level
    /// requires — bare in `\v`, backslashed otherwise.
    fn at_op(&self, c: char) -> bool {
        if self.magic.bare_special(c) {
            self.peek() == Some(c)
        } else {
            self.peek() == Some('\\') && self.peek_at(1) == Some(c)
        }
    }

    fn eat_op(&mut self, c: char) -> bool {
        if !self.at_op(c) {
            return false;
        }
        self.i += if self.magic.bare_special(c) { 1 } else { 2 };
        true
    }

    // --- grammar ---

    fn parse_alt(&mut self) -> Result<Ast> {
        let mut branches = vec![self.parse_branch()?];
        while self.eat_op('|') {
            branches.push(self.parse_branch()?);
        }
        Ok(if branches.len() == 1 { branches.pop().expect("just checked") } else { Ast::Alt(branches) })
    }

    /// A branch is `concat \& concat …`. Every concat but the last must match
    /// at this position without consuming, so they become lookaheads.
    fn parse_branch(&mut self) -> Result<Ast> {
        let mut parts = vec![self.parse_concat()?];
        while self.eat_op('&') {
            parts.push(self.parse_concat()?);
        }
        if parts.len() == 1 {
            return Ok(parts.pop().expect("just checked"));
        }
        let last = parts.pop().expect("just checked");
        let mut seq: Vec<Ast> = parts
            .into_iter()
            .map(|p| Ast::Look { node: Box::new(p), negate: false, behind: false })
            .collect();
        seq.push(last);
        Ok(Ast::Concat(seq))
    }

    fn parse_concat(&mut self) -> Result<Ast> {
        let mut pieces = Vec::new();
        // A fresh concat is a fresh place for `^` to anchor.
        self.branch_start = true;
        while self.peek().is_some() && !self.at_op('|') && !self.at_op('&') && !self.at_op(')') {
            let piece = self.parse_piece()?;
            // Atoms that produce nothing — `\c`, `\v` and friends — leave `^`
            // still able to anchor, which is why `\v^foo` works.
            if piece != Ast::Empty {
                self.branch_start = false;
            }
            pieces.push(piece);
        }
        Ok(match pieces.len() {
            0 => Ast::Empty,
            1 => pieces.pop().expect("just checked"),
            _ => Ast::Concat(pieces),
        })
    }

    /// An atom plus any postfix operators applied to it.
    fn parse_piece(&mut self) -> Result<Ast> {
        let mut atom = self.parse_atom()?;
        loop {
            match self.parse_multi(atom)? {
                Postfix::Applied(a) => atom = a,
                Postfix::None(a) => return Ok(a),
            }
        }
    }

    /// Try to apply one postfix operator to `atom`.
    fn parse_multi(&mut self, atom: Ast) -> Result<Postfix> {
        // `*` is the one multi that is bare at the default magic level.
        if self.at_op('*') {
            self.i += if self.magic.bare_special('*') { 1 } else { 2 };
            return Ok(Postfix::Applied(repeat(atom, 0, None, true)));
        }
        if self.eat_op('+') {
            return Ok(Postfix::Applied(repeat(atom, 1, None, true)));
        }
        if self.eat_op('?') || self.eat_op('=') {
            return Ok(Postfix::Applied(repeat(atom, 0, Some(1), true)));
        }
        if self.at_op('{') {
            self.i += if self.magic.bare_special('{') { 1 } else { 2 };
            let (min, max, greedy) = self.parse_brace()?;
            return Ok(Postfix::Applied(repeat(atom, min, max, greedy)));
        }
        if self.at_op('@') {
            self.i += if self.magic.bare_special('@') { 1 } else { 2 };
            return Ok(Postfix::Applied(self.parse_lookaround(atom)?));
        }
        Ok(Postfix::None(atom))
    }

    /// The body of `\{…}`, with the opening brace already consumed.
    ///
    /// Vim accepts a lot of shapes here: `\{n}`, `\{n,}`, `\{,m}`, `\{n,m}`,
    /// the bare `\{}` meaning `*`, and a leading `-` on any of them to make the
    /// repeat prefer the *shortest* match. The closing brace may be written `}`
    /// or `\}`.
    fn parse_brace(&mut self) -> Result<(u32, Option<u32>, bool)> {
        let greedy = !self.eat("-");
        let lo = self.parse_number();
        let has_comma = self.eat(",");
        let hi = if has_comma { self.parse_number() } else { None };
        if !self.eat("}") && !self.eat("\\}") {
            return Err(Error("E554: Syntax error in \\{...}".into()));
        }
        // The shapes differ in what a *missing* number means, so the bare
        // `\{}` (and `\{-}`) has to stay unbounded rather than collapse to
        // "exactly zero".
        let (min, max) = match (lo, has_comma, hi) {
            (None, false, _) => (0, None),          // `\{}`  — same as `*`
            (Some(n), false, _) => (n, Some(n)),    // `\{n}`
            (Some(n), true, Some(m)) => (n, Some(m)), // `\{n,m}`
            (Some(n), true, None) => (n, None),     // `\{n,}`
            (None, true, Some(m)) => (0, Some(m)),  // `\{,m}`
            (None, true, None) => (0, None),        // `\{,}`
        };
        if let Some(m) = max {
            if m < min {
                return Err(Error("E554: Syntax error in \\{...}".into()));
            }
            if m > MAX_REPEAT {
                return Err(Error(format!("E554: repeat count too large: {m}")));
            }
        }
        Ok((min, max, greedy))
    }

    fn parse_number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        self.chars[start..self.i].iter().collect::<String>().parse().ok()
    }

    /// The body of `\@…`, with `\@` already consumed.
    fn parse_lookaround(&mut self, atom: Ast) -> Result<Ast> {
        let node = Box::new(atom);
        if self.eat("=") {
            return Ok(Ast::Look { node, negate: false, behind: false });
        }
        if self.eat("!") {
            return Ok(Ast::Look { node, negate: true, behind: false });
        }
        if self.eat(">") {
            return Ok(Ast::Atomic(node));
        }
        if self.eat("<=") {
            return Ok(Ast::Look { node, negate: false, behind: true });
        }
        if self.eat("<!") {
            return Ok(Ast::Look { node, negate: true, behind: true });
        }
        Err(Error("E64: \\@ follows nothing".into()))
    }

    // --- atoms ---

    fn parse_atom(&mut self) -> Result<Ast> {
        let Some(c) = self.next() else {
            return Ok(Ast::Empty);
        };
        if c == '\\' {
            let Some(e) = self.next() else {
                return Err(Error("E68: Invalid character after \\".into()));
            };
            // A backslash inverts the level: what is special bare is literal
            // escaped, and everything else gets looked up as an escape.
            if self.magic.bare_special(e) {
                return Ok(Ast::Literal(e));
            }
            return self.escaped_atom(e);
        }
        // `^` anchors only where a match can start; elsewhere it is a literal.
        if c == '^' {
            return Ok(if self.branch_start {
                Ast::Assert(Assert::LineStart)
            } else {
                Ast::Literal('^')
            });
        }
        // `$` anchors only at the end of a branch.
        if c == '$' && self.at_end_of_branch() {
            return Ok(Ast::Assert(Assert::LineEnd));
        }
        if self.magic.bare_special(c) {
            return self.special_atom(c);
        }
        Ok(Ast::Literal(c))
    }

    /// Whether the `$` just consumed sat at the end of a branch: the very end,
    /// or straight before `\)`, `\|` or `\&`.
    ///
    /// Zero-width switches are skipped first, so the `$` in `/foo$\c` still
    /// anchors.
    fn at_end_of_branch(&self) -> bool {
        let mut i = self.i;
        while i + 1 < self.chars.len()
            && self.chars[i] == '\\'
            && matches!(self.chars[i + 1], 'c' | 'C' | 'v' | 'm' | 'M' | 'V')
        {
            i += 2;
        }
        if i >= self.chars.len() {
            return true;
        }
        let c = self.chars[i];
        if self.magic == Magic::Very {
            return matches!(c, ')' | '|' | '&');
        }
        c == '\\' && self.chars.get(i + 1).is_some_and(|n| matches!(n, ')' | '|' | '&'))
    }

    /// An operator written bare (very magic) — dispatched to the same handlers
    /// the backslashed spelling reaches.
    fn special_atom(&mut self, c: char) -> Result<Ast> {
        match c {
            '.' => Ok(Ast::Any { nl: false }),
            '[' => self.parse_collection(false),
            '~' => Ok(self.last_sub_atom()),
            '(' => self.parse_group(Some(())),
            ')' => Err(Error("E55: Unmatched \\)".into())),
            '%' => self.percent_atom(),
            '<' => Ok(Ast::Assert(Assert::WordStart)),
            '>' => Ok(Ast::Assert(Assert::WordEnd)),
            '_' => self.underscore_atom(),
            // A multi with nothing in front of it. Vim treats a leading `*` as
            // a literal rather than an error, and the others as errors.
            '*' => Ok(Ast::Literal('*')),
            '+' | '?' | '=' | '@' | '{' => Err(Error(format!("E64: {c} follows nothing"))),
            '|' | '&' => Err(Error(format!("E64: {c} follows nothing"))),
            _ => Ok(Ast::Literal(c)),
        }
    }

    /// The meaning of `\c` for whichever `c` is not bare-special here.
    fn escaped_atom(&mut self, e: char) -> Result<Ast> {
        // Operators, in their backslashed spelling.
        match e {
            '(' => return self.parse_group(Some(())),
            ')' => return Err(Error("E55: Unmatched \\)".into())),
            '.' => return Ok(Ast::Any { nl: false }),
            '[' => return self.parse_collection(false),
            '~' => return Ok(self.last_sub_atom()),
            '%' => return self.percent_atom(),
            '<' => return Ok(Ast::Assert(Assert::WordStart)),
            '>' => return Ok(Ast::Assert(Assert::WordEnd)),
            '_' => return self.underscore_atom(),
            '^' => return Ok(Ast::Literal('^')),
            '$' => return Ok(Ast::Literal('$')),
            _ => {}
        }
        // `\zs` / `\ze` — move the reported match boundary.
        if e == 'z' {
            return match self.next() {
                Some('s') => Ok(Ast::MatchStart),
                Some('e') => Ok(Ast::MatchEnd),
                // `\z1`…`\z9` are syntax-highlighting atoms with no meaning in
                // a search; treating them as an error is more honest than
                // silently matching something else.
                _ => Err(Error("E68: \\z is only valid in a syntax script".into())),
            };
        }
        // Case-sensitivity overrides apply to the whole pattern wherever they
        // appear, so they parse to nothing.
        if e == 'c' {
            self.force_ignorecase = Some(true);
            return Ok(Ast::Empty);
        }
        if e == 'C' {
            self.force_ignorecase = Some(false);
            return Ok(Ast::Empty);
        }
        // The magic level can be changed anywhere, and applies from that point
        // to the end of the pattern.
        if let Some(m) = magic_switch(e) {
            self.magic = m;
            return Ok(Ast::Empty);
        }
        if let Some('1'..='9') = Some(e) {
            let n = e as usize - '0' as usize;
            if n > self.groups {
                return Err(Error(format!("E65: Illegal back reference: \\{n}")));
            }
            return Ok(Ast::Backref(n));
        }
        if let Some(class) = named_escape(e) {
            return Ok(Ast::Class { class, nl: false });
        }
        Ok(Ast::Literal(control_escape(e)))
    }

    /// `\_x` — the same atom, but also matching a newline.
    fn underscore_atom(&mut self) -> Result<Ast> {
        match self.next() {
            Some('.') => Ok(Ast::Any { nl: true }),
            Some('[') => self.parse_collection(true),
            Some('^') => Ok(Ast::Assert(Assert::LineStart)),
            Some('$') => Ok(Ast::Assert(Assert::LineEnd)),
            Some(c) => match named_escape(c) {
                Some(class) => Ok(Ast::Class { class, nl: true }),
                None => Err(Error(format!("E63: invalid use of \\_{c}"))),
            },
            None => Err(Error("E63: invalid use of \\_".into())),
        }
    }

    /// `~` — the previous `:s` replacement, as a literal sequence.
    fn last_sub_atom(&self) -> Ast {
        if self.last_sub.is_empty() {
            return Ast::Literal('~');
        }
        Ast::Concat(self.last_sub.iter().map(|&c| Ast::Literal(c)).collect())
    }

    /// A group. `capture` is `None` for the non-capturing `\%(…\)`.
    ///
    /// The index is claimed *before* the body is parsed so that `\(a\1\)` sees
    /// its own number — Vim numbers by opening paren, not by closing one.
    fn parse_group(&mut self, capture: Option<()>) -> Result<Ast> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error("E51: Too many nested groups".into()));
        }
        let index = capture.map(|()| {
            self.groups += 1;
            self.groups
        });
        let node = self.parse_alt()?;
        if !self.eat_op(')') {
            return Err(Error("E54: Unmatched \\(".into()));
        }
        self.depth -= 1;
        Ok(Ast::Group { index, node: Box::new(node) })
    }

    /// The `\%…` family: non-capturing groups, positional assertions, optional
    /// sequences, and character codes.
    fn percent_atom(&mut self) -> Result<Ast> {
        // `\%(` — group without a capture slot.
        if self.eat_op('(') || self.eat("(") {
            return self.parse_group(None);
        }
        if self.eat("[") {
            return self.optional_sequence();
        }
        match self.peek() {
            Some('^') => {
                self.i += 1;
                return Ok(Ast::Assert(Assert::BufStart));
            }
            Some('$') => {
                self.i += 1;
                return Ok(Ast::Assert(Assert::BufEnd));
            }
            Some('#') => {
                self.i += 1;
                return Ok(Ast::Assert(Assert::Cursor));
            }
            Some('V') => {
                self.i += 1;
                return Ok(Ast::Assert(Assert::VisualArea));
            }
            _ => {}
        }
        // `\%d123`, `\%x2a`, `\%o40`, `\%u20ac`, `\%U0001f600`.
        if let Some(radix_char) = self.peek() {
            let radix = match radix_char {
                'd' => Some(10),
                'x' | 'u' | 'U' => Some(16),
                'o' => Some(8),
                _ => None,
            };
            if let Some(radix) = radix {
                self.i += 1;
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_digit(radix)) {
                    self.i += 1;
                }
                if self.i == start {
                    return Err(Error("E678: Invalid character after \\%[dxouU]".into()));
                }
                let digits: String = self.chars[start..self.i].iter().collect();
                let code = u32::from_str_radix(&digits, radix)
                    .map_err(|_| Error("E678: Invalid character code".into()))?;
                let c = char::from_u32(code)
                    .ok_or_else(|| Error("E678: Invalid character code".into()))?;
                return Ok(Ast::Literal(c));
            }
        }
        // `\%23l` / `\%<23c` / `\%>23v`.
        let cmp = if self.eat("<") {
            Cmp::Before
        } else if self.eat(">") {
            Cmp::After
        } else {
            Cmp::At
        };
        let Some(n) = self.parse_number() else {
            return Err(Error("E71: Invalid character after \\%".into()));
        };
        match self.next() {
            Some('l') => Ok(Ast::Assert(Assert::Line(cmp, n as usize))),
            Some('c') => Ok(Ast::Assert(Assert::Col(cmp, n as usize))),
            Some('v') => Ok(Ast::Assert(Assert::VCol(cmp, n as usize))),
            _ => Err(Error("E71: Invalid character after \\%".into())),
        }
    }

    /// `\%[abc]` — "match as much of this sequence as is there".
    ///
    /// Desugared to nested optionals: `a\(b\(c\)\?\)\?`. That is exactly the
    /// semantics (each character optional only if every earlier one matched)
    /// and it costs no new node type.
    fn optional_sequence(&mut self) -> Result<Ast> {
        let mut atoms = Vec::new();
        loop {
            match self.peek() {
                None => return Err(Error("E69: Missing ] after \\%[".into())),
                Some(']') => {
                    self.i += 1;
                    break;
                }
                Some(_) => atoms.push(self.parse_atom()?),
            }
        }
        let mut node = Ast::Empty;
        for atom in atoms.into_iter().rev() {
            let mut seq = vec![atom];
            if node != Ast::Empty {
                seq.push(node);
            }
            node = repeat(Ast::Concat(seq), 0, Some(1), true);
        }
        Ok(node)
    }

    /// A `[…]` collection, with the bracket already consumed.
    ///
    /// Vim's escape hatch: a `[` that is not followed by a well-formed
    /// collection is a literal `[`. So a failure here rewinds rather than
    /// erroring, which is why `/[/` searches for a bracket instead of
    /// complaining.
    fn parse_collection(&mut self, nl: bool) -> Result<Ast> {
        let open = self.i;
        match self.try_collection() {
            Some(class) => Ok(Ast::Class { class, nl }),
            None => {
                self.i = open;
                Ok(Ast::Literal('['))
            }
        }
    }

    fn try_collection(&mut self) -> Option<Class> {
        let negated = self.eat("^");
        let mut items = Vec::new();
        // A `]` in first position is a literal, not the terminator.
        if self.peek() == Some(']') {
            self.i += 1;
            items.push(Item::Char(']'));
        }
        loop {
            let c = self.next()?;
            if c == ']' {
                return Some(Class { negated, items });
            }
            // POSIX `[:alpha:]`.
            if c == '[' && self.peek() == Some(':') {
                let save = self.i;
                self.i += 1;
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    self.i += 1;
                }
                let name: String = self.chars[start..self.i].iter().collect();
                if self.eat(":]") {
                    if let Some(n) = Named::from_posix(&name) {
                        items.push(Item::Named(n));
                        continue;
                    }
                    return None;
                }
                self.i = save;
            }
            let lo = if c == '\\' {
                // Inside a collection Vim honors only a few escapes; anything
                // else keeps the backslash as a literal member.
                match self.peek() {
                    Some(e @ ('\\' | ']' | '^' | '-' | 'n' | 'r' | 't' | 'e' | 'b')) => {
                        self.i += 1;
                        control_escape(e)
                    }
                    _ => '\\',
                }
            } else {
                c
            };
            // A range, unless the `-` is the last character before `]`.
            if self.peek() == Some('-') && self.peek_at(1) != Some(']') && self.peek_at(1).is_some() {
                self.i += 1;
                let hi_raw = self.next()?;
                let hi = if hi_raw == '\\' {
                    match self.peek() {
                        Some(e @ ('\\' | ']' | '^' | '-' | 'n' | 'r' | 't' | 'e' | 'b')) => {
                            self.i += 1;
                            control_escape(e)
                        }
                        _ => '\\',
                    }
                } else {
                    hi_raw
                };
                if hi < lo {
                    return None;
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Char(lo));
            }
        }
    }
}

/// The outcome of trying to apply a postfix operator.
enum Postfix {
    Applied(Ast),
    None(Ast),
}

/// Largest `\{n,m}` bound we will expand. The compiler duplicates the body per
/// repetition, so an unbounded count would be an easy way to exhaust memory.
const MAX_REPEAT: u32 = 1000;

fn repeat(node: Ast, min: u32, max: Option<u32>, greedy: bool) -> Ast {
    Ast::Repeat { node: Box::new(node), min, max, greedy }
}

/// The single-letter class escapes. Uppercase is the negation of lowercase,
/// which is the one regularity in Vim's class naming.
fn named_escape(c: char) -> Option<Class> {
    let (named, negated) = match c {
        'd' => (Named::Digit, false),
        'D' => (Named::Digit, true),
        'w' => (Named::Word, false),
        'W' => (Named::Word, true),
        's' => (Named::Space, false),
        'S' => (Named::Space, true),
        'a' => (Named::Alpha, false),
        'A' => (Named::Alpha, true),
        'l' => (Named::Lower, false),
        'L' => (Named::Lower, true),
        'u' => (Named::Upper, false),
        'U' => (Named::Upper, true),
        'x' => (Named::Hex, false),
        'X' => (Named::Hex, true),
        'o' => (Named::Octal, false),
        'O' => (Named::Octal, true),
        'h' => (Named::Head, false),
        'H' => (Named::Head, true),
        'i' => (Named::Ident, false),
        'I' => (Named::Ident, true),
        'k' => (Named::Keyword, false),
        'K' => (Named::Keyword, true),
        'f' => (Named::File, false),
        'F' => (Named::File, true),
        'p' => (Named::Printable, false),
        'P' => (Named::Printable, true),
        _ => return None,
    };
    Some(Class::named(named, negated))
}

/// `\v`, `\m`, `\M`, `\V` — the mid-pattern magic-level switches.
fn magic_switch(c: char) -> Option<Magic> {
    Some(match c {
        'v' => Magic::Very,
        'm' => Magic::Magic,
        'M' => Magic::No,
        'V' => Magic::VeryNo,
        _ => return None,
    })
}

/// `\n`, `\t` and friends. Anything else stands for itself.
fn control_escape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        'e' => '\x1b',
        'b' => '\x08',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast(pat: &str) -> Ast {
        parse(pat, Magic::Magic, "").expect("parses").ast
    }

    fn very(pat: &str) -> Ast {
        parse(pat, Magic::Very, "").expect("parses").ast
    }

    #[test]
    fn a_backslash_inverts_whichever_level_is_active() {
        // Magic: `\(` groups, `(` is a literal.
        assert!(matches!(ast(r"\(a\)"), Ast::Group { index: Some(1), .. }));
        assert_eq!(ast("("), Ast::Literal('('));
        // Very magic: exactly the other way round.
        assert!(matches!(very("(a)"), Ast::Group { index: Some(1), .. }));
        assert_eq!(very(r"\("), Ast::Literal('('));
    }

    #[test]
    fn dot_is_literal_once_magic_is_off() {
        assert_eq!(ast("."), Ast::Any { nl: false });
        assert_eq!(parse(".", Magic::No, "").unwrap().ast, Ast::Literal('.'));
        assert_eq!(parse(r"\.", Magic::No, "").unwrap().ast, Ast::Any { nl: false });
        assert_eq!(parse(".", Magic::VeryNo, "").unwrap().ast, Ast::Literal('.'));
    }

    #[test]
    fn caret_anchors_only_where_a_match_can_start() {
        assert_eq!(ast("^a"), Ast::Concat(vec![Ast::Assert(Assert::LineStart), Ast::Literal('a')]));
        // Mid-pattern it is just a character.
        assert_eq!(ast("a^b"), Ast::Concat(vec![Ast::Literal('a'), Ast::Literal('^'), Ast::Literal('b')]));
    }

    #[test]
    fn dollar_anchors_only_at_the_end_of_a_branch() {
        assert_eq!(ast("a$"), Ast::Concat(vec![Ast::Literal('a'), Ast::Assert(Assert::LineEnd)]));
        assert_eq!(ast("a$b"), Ast::Concat(vec![Ast::Literal('a'), Ast::Literal('$'), Ast::Literal('b')]));
    }

    #[test]
    fn the_lazy_brace_forms_parse() {
        assert_eq!(ast(r"a\{-}"), repeat(Ast::Literal('a'), 0, None, false));
        assert_eq!(ast(r"a\{2,5}"), repeat(Ast::Literal('a'), 2, Some(5), true));
        assert_eq!(ast(r"a\{-1,3}"), repeat(Ast::Literal('a'), 1, Some(3), false));
        assert_eq!(ast(r"a\{3}"), repeat(Ast::Literal('a'), 3, Some(3), true));
        assert_eq!(ast(r"a\{2,}"), repeat(Ast::Literal('a'), 2, None, true));
    }

    #[test]
    fn lookaround_reads_its_four_spellings() {
        assert!(matches!(ast(r"\(a\)\@="), Ast::Look { negate: false, behind: false, .. }));
        assert!(matches!(ast(r"\(a\)\@!"), Ast::Look { negate: true, behind: false, .. }));
        assert!(matches!(ast(r"\(a\)\@<="), Ast::Look { negate: false, behind: true, .. }));
        assert!(matches!(ast(r"\(a\)\@<!"), Ast::Look { negate: true, behind: true, .. }));
        assert!(matches!(ast(r"\(a\)\@>"), Ast::Atomic(_)));
    }

    #[test]
    fn a_backreference_must_name_a_group_that_exists() {
        assert!(matches!(ast(r"\(a\)\1"), Ast::Concat(_)));
        assert!(parse(r"\1", Magic::Magic, "").is_err());
    }

    #[test]
    fn case_atoms_are_pattern_wide_and_consume_nothing() {
        let p = parse(r"foo\c", Magic::Magic, "").unwrap();
        assert_eq!(p.force_ignorecase, Some(true));
        let p = parse(r"\Cfoo", Magic::Magic, "").unwrap();
        assert_eq!(p.force_ignorecase, Some(false));
    }

    #[test]
    fn the_and_operator_becomes_a_lookahead() {
        // `foo\&..` — the first concat must match here, the last one is what counts.
        let a = ast(r"foo\&..");
        let Ast::Concat(parts) = a else { panic!("expected a concat") };
        assert!(matches!(parts[0], Ast::Look { negate: false, behind: false, .. }));
    }

    #[test]
    fn an_optional_sequence_nests_its_optionals() {
        // `\%[foo]` matches "", "f", "fo", "foo" — never "fo" out of order.
        assert!(matches!(ast(r"\%[foo]"), Ast::Repeat { min: 0, max: Some(1), .. }));
    }

    #[test]
    fn positional_assertions_carry_their_comparison() {
        assert_eq!(ast(r"\%23l"), Ast::Assert(Assert::Line(Cmp::At, 23)));
        assert_eq!(ast(r"\%<5c"), Ast::Assert(Assert::Col(Cmp::Before, 5)));
        assert_eq!(ast(r"\%>9v"), Ast::Assert(Assert::VCol(Cmp::After, 9)));
    }

    #[test]
    fn character_codes_decode() {
        assert_eq!(ast(r"\%d65"), Ast::Literal('A'));
        assert_eq!(ast(r"\%x41"), Ast::Literal('A'));
        assert_eq!(ast(r"\%o101"), Ast::Literal('A'));
        assert_eq!(ast(r"\%u20ac"), Ast::Literal('€'));
    }

    #[test]
    fn a_lone_bracket_is_a_literal_rather_than_an_error() {
        assert_eq!(ast("["), Ast::Literal('['));
        assert!(matches!(ast("[a-z]"), Ast::Class { .. }));
    }

    #[test]
    fn a_closing_bracket_in_first_position_is_a_member() {
        let Ast::Class { class, .. } = ast("[]a]") else { panic!("expected a class") };
        assert!(class.matches(']', false));
        assert!(class.matches('a', false));
        assert!(!class.matches('b', false));
    }

    #[test]
    fn zs_and_ze_parse_to_boundary_markers() {
        assert_eq!(ast(r"\zs"), Ast::MatchStart);
        assert_eq!(ast(r"\ze"), Ast::MatchEnd);
    }

    #[test]
    fn tilde_stands_for_the_last_substitute() {
        assert_eq!(parse("~", Magic::Magic, "").unwrap().ast, Ast::Literal('~'));
        let a = parse("~", Magic::Magic, "hi").unwrap().ast;
        assert_eq!(a, Ast::Concat(vec![Ast::Literal('h'), Ast::Literal('i')]));
    }

    #[test]
    fn unmatched_delimiters_are_reported_not_panicked() {
        assert!(parse(r"\(a", Magic::Magic, "").is_err());
        assert!(parse(r"a\)", Magic::Magic, "").is_err());
        assert!(parse("ab\\", Magic::Magic, "").is_err());
    }
}
