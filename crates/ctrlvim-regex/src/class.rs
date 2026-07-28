//! Character classes — Vim's `\d`/`\w`/… atoms and `[…]` collections.
//!
//! Two things live here because they answer the same question ("does this
//! character belong to this set?") from different syntax: the single-letter
//! class atoms, and the bracket collection with its ranges and POSIX names.
//!
//! One Vim rule worth stating up front: the *lowercase* class atoms never match
//! a newline, while their uppercase negations do. ctrlvim matches a line at a
//! time, so the distinction rarely surfaces — but `\_.` and friends would need
//! it, which is why [`Named::matches`] is written to respect it rather than
//! folding newline handling into the caller.

/// A named character class — the set behind `\d`, `[:alpha:]`, and their kin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Digit,
    Hex,
    Octal,
    /// `\w` — word character. Fixed at `[0-9A-Za-z_]`, as Vim's is.
    Word,
    /// `\h` — "head of word": a word character that cannot start a number.
    Head,
    Alpha,
    Alnum,
    Lower,
    Upper,
    /// `\s` — Vim's is space and tab only, *not* the Unicode whitespace set.
    Space,
    /// `\_s`-adjacent: POSIX `[:space:]`, which does include newline and form feed.
    PosixSpace,
    /// `\i` — identifier character (`'isident'`). We use the option's default.
    Ident,
    /// `\k` — keyword character (`'iskeyword'`). Default plus non-ASCII letters.
    Keyword,
    /// `\f` — filename character (`'isfname'`).
    File,
    /// `\p` — printable (`'isprint'`).
    Printable,
    Punct,
    Cntrl,
    Graph,
    /// POSIX `[:blank:]` — space and tab.
    Blank,
}

impl Named {
    /// Whether `c` belongs to the class.
    ///
    /// A newline is excluded from every class here; the negated forms add it
    /// back in [`Class::matches`], which is where Vim draws the same line.
    pub fn matches(self, c: char) -> bool {
        if c == '\n' && self != Named::PosixSpace {
            return false;
        }
        match self {
            Named::Digit => c.is_ascii_digit(),
            Named::Hex => c.is_ascii_hexdigit(),
            Named::Octal => ('0'..='7').contains(&c),
            Named::Word => c.is_ascii_alphanumeric() || c == '_',
            Named::Head => c.is_ascii_alphabetic() || c == '_',
            Named::Alpha => c.is_ascii_alphabetic(),
            Named::Alnum => c.is_ascii_alphanumeric(),
            Named::Lower => c.is_lowercase(),
            Named::Upper => c.is_uppercase(),
            Named::Space => c == ' ' || c == '\t',
            Named::PosixSpace => c.is_whitespace(),
            // `'isident'` defaults to `@,48-57,_,192-255`: letters, digits,
            // underscore, and the Latin-1 high range.
            Named::Ident => c.is_alphanumeric() || c == '_',
            // `'iskeyword'` defaults the same way, and is what `\<`/`\>` and
            // the `w` motion agree on.
            Named::Keyword => c.is_alphanumeric() || c == '_',
            // `'isfname'` is deliberately generous — anything that is not a
            // separator can appear in a path.
            Named::File => {
                c.is_alphanumeric()
                    || "/\\.-_+,#$%~=".contains(c)
                    || (!c.is_ascii() && !c.is_whitespace())
            }
            Named::Printable => !c.is_control(),
            Named::Punct => c.is_ascii_punctuation(),
            Named::Cntrl => c.is_control(),
            Named::Graph => c.is_ascii_graphic(),
            Named::Blank => c == ' ' || c == '\t',
        }
    }

    /// Resolve a POSIX `[:name:]` spelling.
    pub fn from_posix(name: &str) -> Option<Named> {
        Some(match name {
            "alpha" => Named::Alpha,
            "digit" => Named::Digit,
            "alnum" => Named::Alnum,
            "lower" => Named::Lower,
            "upper" => Named::Upper,
            "space" => Named::PosixSpace,
            "blank" => Named::Blank,
            "punct" => Named::Punct,
            "cntrl" => Named::Cntrl,
            "graph" => Named::Graph,
            "print" => Named::Printable,
            "xdigit" => Named::Hex,
            "word" => Named::Word,
            _ => return None,
        })
    }
}

/// One member of a `[…]` collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Char(char),
    Range(char, char),
    Named(Named),
}

/// A set of characters: either a bracket collection or a single class atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub negated: bool,
    pub items: Vec<Item>,
}

impl Class {
    /// A class holding exactly one named set — how `\d` and `\D` are built.
    pub fn named(n: Named, negated: bool) -> Class {
        Class { negated, items: vec![Item::Named(n)] }
    }

    /// Whether `c` is in the set.
    ///
    /// `ignorecase` folds both the literal members and the range endpoints, so
    /// `[a-z]` under `\c` matches `Q` — which is what Vim does, and what a user
    /// who typed `:set ignorecase` expects of a collection.
    pub fn matches(&self, c: char, ignorecase: bool) -> bool {
        // A negated collection never matches a newline: `[^x]` stops at the end
        // of the line rather than swallowing it.
        if self.negated && c == '\n' {
            return false;
        }
        let hit = self.items.iter().any(|item| match *item {
            Item::Char(m) => m == c || (ignorecase && eq_fold(m, c)),
            Item::Range(lo, hi) => {
                (lo..=hi).contains(&c)
                    || (ignorecase && (in_range_fold(lo, hi, c) || in_range_fold_up(lo, hi, c)))
            }
            Item::Named(n) => n.matches(c),
        });
        hit != self.negated
    }
}

/// Case-insensitive character equality, via simple case folding.
pub fn eq_fold(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn in_range_fold(lo: char, hi: char, c: char) -> bool {
    c.to_lowercase().any(|f| (lo..=hi).contains(&f))
}

fn in_range_fold_up(lo: char, hi: char, c: char) -> bool {
    c.to_uppercase().any(|f| (lo..=hi).contains(&f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vims_space_class_is_space_and_tab_not_unicode_whitespace() {
        assert!(Named::Space.matches(' '));
        assert!(Named::Space.matches('\t'));
        // A no-break space is whitespace to Unicode but not to Vim's `\s`.
        assert!(!Named::Space.matches('\u{a0}'));
    }

    #[test]
    fn head_of_word_excludes_digits() {
        assert!(Named::Head.matches('_'));
        assert!(Named::Head.matches('x'));
        assert!(!Named::Head.matches('7'));
        assert!(Named::Word.matches('7'));
    }

    #[test]
    fn a_negated_collection_does_not_swallow_a_newline() {
        let c = Class { negated: true, items: vec![Item::Char('x')] };
        assert!(c.matches('y', false));
        assert!(!c.matches('\n', false));
    }

    #[test]
    fn ranges_fold_when_ignoring_case() {
        let c = Class { negated: false, items: vec![Item::Range('a', 'z')] };
        assert!(!c.matches('Q', false));
        assert!(c.matches('Q', true));
    }

    #[test]
    fn posix_names_resolve() {
        assert_eq!(Named::from_posix("xdigit"), Some(Named::Hex));
        assert_eq!(Named::from_posix("nope"), None);
    }
}
