//! Text objects — `iw`/`aw`, `i(`/`a(`, `i"`/`a"`, … — the `textobject.c`
//! object-selection layer that operators consume (`diw`, `ci(`, `ya"`).
//!
//! Each function returns an inclusive `(start, end)` character range for the
//! object under the cursor, or `None` when there is no such object. The caller
//! turns that into an inclusive charwise [`crate::operator::OperatorSpan`].

use ctrlvim_text::Buffer;
use ctrlvim_types::Position;

fn line_chars(buf: &Buffer, line: usize) -> Vec<char> {
    buf.line(line).unwrap_or_default().chars().collect()
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Blank,
    Keyword,
    Punct,
}

fn class_of(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Keyword
    } else {
        Class::Punct
    }
}

/// `iw`/`aw` (and `iW`/`aW`): the word under the cursor. `around` (`aw`) also
/// grabs trailing whitespace, or leading whitespace when there is none after.
pub fn word(buf: &Buffer, pos: Position, around: bool, big: bool) -> Option<(Position, Position)> {
    let chars = line_chars(buf, pos.line);
    if chars.is_empty() {
        return None;
    }
    let col = pos.col.min(chars.len() - 1);
    let class = class_of(chars[col], big);

    // Expand over the same class in both directions.
    let mut start = col;
    while start > 0 && class_of(chars[start - 1], big) == class {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < chars.len() && class_of(chars[end + 1], big) == class {
        end += 1;
    }

    if around && class != Class::Blank {
        // Prefer trailing whitespace; fall back to leading.
        let mut e = end;
        while e + 1 < chars.len() && class_of(chars[e + 1], big) == Class::Blank {
            e += 1;
        }
        if e != end {
            end = e;
        } else {
            while start > 0 && class_of(chars[start - 1], big) == Class::Blank {
                start -= 1;
            }
        }
    }
    Some((Position::new(pos.line, start), Position::new(pos.line, end)))
}

/// `i(`/`a(`, `i{`/`a{`, `i[`/`a[`, `i<`/`a<`: the region delimited by a
/// bracket pair surrounding the cursor. `around` includes the brackets.
/// Searches across lines with nesting.
pub fn pair(
    buf: &Buffer,
    pos: Position,
    open: char,
    close: char,
    around: bool,
) -> Option<(Position, Position)> {
    let open_pos = scan_bracket(buf, pos, open, close, false)?;
    // Start the forward scan just after the opener so nesting balances.
    let after = step_forward(buf, open_pos)?;
    let close_pos = scan_bracket(buf, after, open, close, true)?;
    if around {
        Some((open_pos, close_pos))
    } else {
        let inner_start = step_forward(buf, open_pos)?;
        let inner_end = step_back(buf, close_pos)?;
        // Empty pair `()` — nothing inside.
        if (inner_start.line, inner_start.col) > (inner_end.line, inner_end.col) {
            return None;
        }
        Some((inner_start, inner_end))
    }
}

/// `i"`/`a"`, `i'`/`a'`, `` i` ``/`` a` ``: the region between a matching pair
/// of `q` quotes on the cursor's line. `around` includes the quotes.
pub fn quote(buf: &Buffer, pos: Position, q: char, around: bool) -> Option<(Position, Position)> {
    let chars = line_chars(buf, pos.line);
    // Collect quote columns on this line (ignoring escaped ones).
    let quotes: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(i, &c)| c == q && (*i == 0 || chars[i - 1] != '\\'))
        .map(|(i, _)| i)
        .collect();
    // Find the pair (2k, 2k+1) that contains (or starts at) the cursor.
    let mut pair_cols = None;
    let mut i = 0;
    while i + 1 < quotes.len() {
        let (a, b) = (quotes[i], quotes[i + 1]);
        if pos.col <= b {
            pair_cols = Some((a, b));
            break;
        }
        i += 2;
    }
    let (a, b) = pair_cols?;
    if around {
        Some((Position::new(pos.line, a), Position::new(pos.line, b)))
    } else if b > a + 1 {
        Some((Position::new(pos.line, a + 1), Position::new(pos.line, b - 1)))
    } else {
        None // empty quotes ""
    }
}

/// Scan for an unmatched `open`/`close` bracket from `pos` (inclusive) in the
/// given direction, respecting nesting. `forward=true` looks for the closer.
fn scan_bracket(buf: &Buffer, pos: Position, open: char, close: char, forward: bool) -> Option<Position> {
    let total = buf.line_count();
    let (mut line, mut col) = (pos.line, pos.col);
    let mut depth = 0i32;
    loop {
        let chars = line_chars(buf, line);
        if col < chars.len() {
            let c = chars[col];
            if forward {
                if c == open {
                    depth += 1;
                } else if c == close {
                    if depth == 0 {
                        return Some(Position::new(line, col));
                    }
                    depth -= 1;
                }
            } else if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    return Some(Position::new(line, col));
                }
                depth -= 1;
            }
        }
        if forward {
            col += 1;
            if col >= chars.len() {
                line += 1;
                col = 0;
                if line >= total {
                    return None;
                }
            }
        } else {
            if col == 0 {
                if line == 0 {
                    return None;
                }
                line -= 1;
                col = line_chars(buf, line).len();
                if col == 0 {
                    continue;
                }
                col -= 1;
            } else {
                col -= 1;
            }
        }
    }
}

fn step_forward(buf: &Buffer, pos: Position) -> Option<Position> {
    let len = line_chars(buf, pos.line).len();
    if pos.col + 1 < len {
        Some(Position::new(pos.line, pos.col + 1))
    } else if pos.line + 1 < buf.line_count() {
        Some(Position::new(pos.line + 1, 0))
    } else {
        Some(Position::new(pos.line, pos.col)) // clamp at EOF
    }
}

fn step_back(buf: &Buffer, pos: Position) -> Option<Position> {
    if pos.col > 0 {
        Some(Position::new(pos.line, pos.col - 1))
    } else if pos.line > 0 {
        let prev = line_chars(buf, pos.line - 1).len().saturating_sub(1);
        Some(Position::new(pos.line - 1, prev))
    } else {
        Some(Position::new(pos.line, pos.col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(lines: &[&str]) -> Buffer {
        Buffer::from_lines(lines)
    }

    #[test]
    fn word_inner_and_around() {
        let buf = b(&["foo bar baz"]);
        // iw on "bar"
        assert_eq!(word(&buf, Position::new(0, 5), false, false).unwrap(), (Position::new(0, 4), Position::new(0, 6)));
        // aw grabs the trailing space
        assert_eq!(word(&buf, Position::new(0, 5), true, false).unwrap(), (Position::new(0, 4), Position::new(0, 7)));
    }

    #[test]
    fn pair_inner_and_around() {
        let buf = b(&["x(a, b)y"]);
        // i( -> "a, b" (cols 2..5)
        assert_eq!(pair(&buf, Position::new(0, 3), '(', ')', false).unwrap(), (Position::new(0, 2), Position::new(0, 5)));
        // a( -> "(a, b)" (cols 1..6)
        assert_eq!(pair(&buf, Position::new(0, 3), '(', ')', true).unwrap(), (Position::new(0, 1), Position::new(0, 6)));
    }

    #[test]
    fn pair_nested_and_multiline() {
        let buf = b(&["f(a(b)c)"]);
        // From inside inner pair, i( picks the inner.
        assert_eq!(pair(&buf, Position::new(0, 4), '(', ')', false).unwrap(), (Position::new(0, 4), Position::new(0, 4)));
        let ml = b(&["{", "  x", "}"]);
        assert_eq!(pair(&ml, Position::new(1, 1), '{', '}', true).unwrap(), (Position::new(0, 0), Position::new(2, 0)));
    }

    #[test]
    fn quote_inner_and_around() {
        let buf = b(&["say \"hi\" now"]);
        assert_eq!(quote(&buf, Position::new(0, 6), '"', false).unwrap(), (Position::new(0, 5), Position::new(0, 6)));
        assert_eq!(quote(&buf, Position::new(0, 6), '"', true).unwrap(), (Position::new(0, 4), Position::new(0, 7)));
    }
}
