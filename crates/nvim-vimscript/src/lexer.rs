//! Expression tokenizer for the Vimscript subset.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    /// An identifier, possibly with a scope prefix (`g:foo`, `a:1`, `s:x`) or a
    /// dotted function path (`nvim_buf_get_lines`).
    Ident(String),
    // punctuation / operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Dot,
    Comma,
    Colon,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Not,
    Question,
}

/// Tokenize one line's worth of expression text.
pub fn lex(input: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => i += 1,
            '"' => {
                // A double-quote at expression start is a comment in Vimscript;
                // callers strip trailing comments before lexing, so treat a
                // stray `"` here as end-of-line.
                break;
            }
            '\'' => {
                // Single-quoted string: literal, '' is an escaped quote.
                i += 1;
                let mut s = String::new();
                while i < chars.len() {
                    if chars[i] == '\'' {
                        if i + 1 < chars.len() && chars[i + 1] == '\'' {
                            s.push('\'');
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        s.push(chars[i]);
                        i += 1;
                    }
                }
                out.push(Tok::Str(s));
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i < chars.len() && chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let s: String = chars[start..i].iter().collect();
                    out.push(Tok::Float(s.parse().map_err(|_| "bad float")?));
                } else {
                    let s: String = chars[start..i].iter().collect();
                    out.push(Tok::Int(s.parse().map_err(|_| "bad int")?));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == ':' || chars[i] == '#')
                {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            '+' => { out.push(Tok::Plus); i += 1; }
            '-' => { out.push(Tok::Minus); i += 1; }
            '*' => { out.push(Tok::Star); i += 1; }
            '/' => { out.push(Tok::Slash); i += 1; }
            '%' => { out.push(Tok::Percent); i += 1; }
            '.' => { out.push(Tok::Dot); i += 1; }
            ',' => { out.push(Tok::Comma); i += 1; }
            ':' => { out.push(Tok::Colon); i += 1; }
            '(' => { out.push(Tok::LParen); i += 1; }
            ')' => { out.push(Tok::RParen); i += 1; }
            '[' => { out.push(Tok::LBracket); i += 1; }
            ']' => { out.push(Tok::RBracket); i += 1; }
            '{' => { out.push(Tok::LBrace); i += 1; }
            '}' => { out.push(Tok::RBrace); i += 1; }
            '?' => { out.push(Tok::Question); i += 1; }
            '=' => {
                if next_is(&chars, i, '=') { out.push(Tok::Eq); i += 2; }
                else { out.push(Tok::Assign); i += 1; }
            }
            '!' => {
                if next_is(&chars, i, '=') { out.push(Tok::Ne); i += 2; }
                else { out.push(Tok::Not); i += 1; }
            }
            '<' => {
                if next_is(&chars, i, '=') { out.push(Tok::Le); i += 2; }
                else { out.push(Tok::Lt); i += 1; }
            }
            '>' => {
                if next_is(&chars, i, '=') { out.push(Tok::Ge); i += 2; }
                else { out.push(Tok::Gt); i += 1; }
            }
            '&' => {
                if next_is(&chars, i, '&') { out.push(Tok::And); i += 2; }
                else { return Err("unexpected '&'".into()); }
            }
            '|' => {
                if next_is(&chars, i, '|') { out.push(Tok::Or); i += 2; }
                else { break; } // bar separates commands; stop here
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(out)
}

fn next_is(chars: &[char], i: usize, c: char) -> bool {
    i + 1 < chars.len() && chars[i + 1] == c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_arithmetic() {
        assert_eq!(lex("1 + 2").unwrap(), vec![Tok::Int(1), Tok::Plus, Tok::Int(2)]);
    }

    #[test]
    fn lex_string_and_concat() {
        assert_eq!(
            lex("'a' . 'b'").unwrap(),
            vec![Tok::Str("a".into()), Tok::Dot, Tok::Str("b".into())]
        );
    }

    #[test]
    fn lex_scoped_ident() {
        assert_eq!(lex("g:foo").unwrap(), vec![Tok::Ident("g:foo".into())]);
    }

    #[test]
    fn lex_comparison() {
        assert_eq!(lex("x >= 3").unwrap(), vec![Tok::Ident("x".into()), Tok::Ge, Tok::Int(3)]);
    }
}
