//! Ex command-line line ranges (`:%`, `:1,5`, `:.,+3`, `:'<,'>`, `:/pat/`).
//!
//! Parsing is pure and lives here; resolving an address to a concrete line
//! needs the buffer/cursor/marks/search state and so happens in the session
//! (see `session::resolve_range`). An address is a *base* (`.`, `$`, a number,
//! a mark, a search) plus a signed line offset.

/// The base of a line address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    /// `.` — the current line.
    Current,
    /// `$` — the last line.
    Last,
    /// A 1-based absolute line number.
    Line(usize),
    /// `'x` — the line of mark `x` (incl. `'<` / `'>` visual marks).
    Mark(char),
    /// `/pat/` (forward) or `?pat?` (backward) — the next matching line.
    Search { pattern: String, forward: bool },
}

/// A line address: a base plus a signed offset (`+`/`-` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub base: Addr,
    pub offset: i64,
}

/// A parsed range prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSpec {
    /// No range was given.
    None,
    /// `%` — the whole file (`1,$`).
    Whole,
    /// A single line address.
    One(Address),
    /// A `start,end` pair.
    Pair(Address, Address),
}

/// Split a leading range off `input`, returning it plus the remaining command
/// text (with surrounding whitespace trimmed).
pub(crate) fn parse_range(input: &str) -> (RangeSpec, String) {
    let s = input.trim_start();
    if let Some(rest) = s.strip_prefix('%') {
        return (RangeSpec::Whole, rest.trim().to_string());
    }
    let Some((first, rest)) = parse_address(s) else {
        return (RangeSpec::None, s.trim().to_string());
    };
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix(',').or_else(|| rest.strip_prefix(';')) {
        let after = after.trim_start();
        match parse_address(after) {
            Some((second, r)) => (RangeSpec::Pair(first, second), r.trim().to_string()),
            // `a,` with no second address → second defaults to the current line.
            None => (
                RangeSpec::Pair(first, Address { base: Addr::Current, offset: 0 }),
                after.trim().to_string(),
            ),
        }
    } else {
        (RangeSpec::One(first), rest.trim().to_string())
    }
}

/// Parse one address (base + offsets) off the front of `s`. Returns `None` when
/// `s` doesn't start with an address token.
fn parse_address(s: &str) -> Option<(Address, &str)> {
    let mut rest = s;
    let base = match rest.chars().next()? {
        '.' => {
            rest = &rest[1..];
            Some(Addr::Current)
        }
        '$' => {
            rest = &rest[1..];
            Some(Addr::Last)
        }
        '\'' => {
            let mark = rest[1..].chars().next()?;
            rest = &rest[1 + mark.len_utf8()..];
            Some(Addr::Mark(mark))
        }
        '/' | '?' => {
            let forward = rest.starts_with('/');
            let delim = if forward { '/' } else { '?' };
            let body = &rest[1..];
            let (pat, consumed) = match body.find(delim) {
                Some(i) => (&body[..i], i + 1),
                None => (body, body.len()),
            };
            rest = &body[consumed..];
            Some(Addr::Search { pattern: pat.to_string(), forward })
        }
        c if c.is_ascii_digit() => {
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            let n: usize = rest[..end].parse().ok()?;
            rest = &rest[end..];
            Some(Addr::Line(n))
        }
        // A bare offset (`+3`, `-2`) implies the current line as the base.
        '+' | '-' => Some(Addr::Current),
        _ => None,
    }?;

    // Accumulate `+N` / `-N` offsets (N defaults to 1).
    let mut offset: i64 = 0;
    loop {
        let t = rest.trim_start();
        let sign = match t.chars().next() {
            Some('+') => 1,
            Some('-') => -1,
            _ => break,
        };
        let after = &t[1..];
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        let n: i64 = if end == 0 { 1 } else { after[..end].parse().unwrap_or(1) };
        offset += sign * n;
        rest = &after[end..];
    }

    Some((Address { base, offset }, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(base: Addr, offset: i64) -> Address {
        Address { base, offset }
    }

    #[test]
    fn whole_and_none() {
        assert_eq!(parse_range("%s/a/b/"), (RangeSpec::Whole, "s/a/b/".into()));
        assert_eq!(parse_range("d"), (RangeSpec::None, "d".into()));
    }

    #[test]
    fn single_and_pair() {
        assert_eq!(parse_range("5d"), (RangeSpec::One(addr(Addr::Line(5), 0)), "d".into()));
        assert_eq!(
            parse_range("1,10sort"),
            (RangeSpec::Pair(addr(Addr::Line(1), 0), addr(Addr::Line(10), 0)), "sort".into())
        );
    }

    #[test]
    fn dot_dollar_offsets_marks() {
        assert_eq!(
            parse_range(".,+3d"),
            (RangeSpec::Pair(addr(Addr::Current, 0), addr(Addr::Current, 3)), "d".into())
        );
        assert_eq!(parse_range("$y"), (RangeSpec::One(addr(Addr::Last, 0)), "y".into()));
        assert_eq!(
            parse_range("'<,'>s/x/y/"),
            (RangeSpec::Pair(addr(Addr::Mark('<'), 0), addr(Addr::Mark('>'), 0)), "s/x/y/".into())
        );
        assert_eq!(parse_range(".-2d"), (RangeSpec::One(addr(Addr::Current, -2)), "d".into()));
    }

    #[test]
    fn search_address() {
        assert_eq!(
            parse_range("/foo/d"),
            (RangeSpec::One(addr(Addr::Search { pattern: "foo".into(), forward: true }, 0)), "d".into())
        );
    }
}
