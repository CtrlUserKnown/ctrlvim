//! AST and recursive-descent parser for the Vimscript subset.
//!
//! Statements are line-oriented (as in Vimscript); block statements
//! (`if`/`for`/`while`/`function`) span multiple lines and are parsed by
//! scanning to the matching terminator. Expressions are parsed with a
//! precedence-climbing parser over the [`crate::lexer`] tokens.

use crate::lexer::{lex, Tok};

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Var(String),
    Unary(char, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let { target: Expr, op: char, value: Expr },
    Call(Expr),
    Echo(Vec<Expr>),
    Return(Option<Expr>),
    If { branches: Vec<(Expr, Vec<Stmt>)>, else_body: Option<Vec<Stmt>> },
    For { var: String, iter: Expr, body: Vec<Stmt> },
    While { cond: Expr, body: Vec<Stmt> },
    Function { name: String, params: Vec<String>, body: Vec<Stmt> },
    Break,
    Continue,
    ExprStmt(Expr),
}

/// Parse a whole program (multiple lines) into a statement block.
pub fn parse_program(src: &str) -> Result<Vec<Stmt>, String> {
    let lines: Vec<String> = src.lines().map(strip_comment).collect();
    let mut pos = 0;
    let (stmts, _) = parse_block(&lines, &mut pos, &[])?;
    Ok(stmts)
}

/// Strip a trailing `"` comment (only when not inside a string). Good enough for
/// the subset; a full lexer-level strip is deferred.
fn strip_comment(line: &str) -> String {
    let mut in_str = false;
    let mut out = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    // Leading whitespace + `"` = full-line comment.
    let trimmed = line.trim_start();
    if trimmed.starts_with('"') {
        return String::new();
    }
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            in_str = !in_str;
        }
        if c == '"' && !in_str {
            break;
        }
        out.push(c);
        i += 1;
    }
    out.trim_end().to_string()
}

/// Parse statements until one of `terminators` (a block-ending keyword) is seen
/// at the current line. Returns the statements and the terminator keyword found.
fn parse_block(
    lines: &[String],
    pos: &mut usize,
    terminators: &[&str],
) -> Result<(Vec<Stmt>, String), String> {
    let mut stmts = Vec::new();
    while *pos < lines.len() {
        let raw = lines[*pos].trim();
        if raw.is_empty() {
            *pos += 1;
            continue;
        }
        let kw = first_word(raw);
        if terminators.contains(&kw.as_str()) {
            return Ok((stmts, kw));
        }
        match kw.as_str() {
            "if" => stmts.push(parse_if(lines, pos)?),
            "for" => stmts.push(parse_for(lines, pos)?),
            "while" => stmts.push(parse_while(lines, pos)?),
            "function" | "function!" => stmts.push(parse_function(lines, pos)?),
            _ => {
                stmts.push(parse_simple(raw)?);
                *pos += 1;
            }
        }
    }
    Ok((stmts, String::new()))
}

fn parse_if(lines: &[String], pos: &mut usize) -> Result<Stmt, String> {
    let mut branches = Vec::new();
    let mut else_body = None;
    // current line is `if <cond>`
    let cond = parse_expr_str(rest_after(lines[*pos].trim(), "if"))?;
    *pos += 1;
    let (body, term) = parse_block(lines, pos, &["elseif", "else", "endif"])?;
    branches.push((cond, body));
    let mut term = term;
    loop {
        match term.as_str() {
            "elseif" => {
                let c = parse_expr_str(rest_after(lines[*pos].trim(), "elseif"))?;
                *pos += 1;
                let (b, t) = parse_block(lines, pos, &["elseif", "else", "endif"])?;
                branches.push((c, b));
                term = t;
            }
            "else" => {
                *pos += 1;
                let (b, t) = parse_block(lines, pos, &["endif"])?;
                else_body = Some(b);
                term = t;
            }
            "endif" => {
                *pos += 1;
                break;
            }
            _ => return Err("unterminated if".into()),
        }
    }
    Ok(Stmt::If { branches, else_body })
}

fn parse_for(lines: &[String], pos: &mut usize) -> Result<Stmt, String> {
    // `for x in <expr>`
    let header = rest_after(lines[*pos].trim(), "for");
    let in_idx = header.find(" in ").ok_or("for without 'in'")?;
    let var = header[..in_idx].trim().to_string();
    let iter = parse_expr_str(&header[in_idx + 4..])?;
    *pos += 1;
    let (body, _) = parse_block(lines, pos, &["endfor"])?;
    *pos += 1; // consume endfor
    Ok(Stmt::For { var, iter, body })
}

fn parse_while(lines: &[String], pos: &mut usize) -> Result<Stmt, String> {
    let cond = parse_expr_str(rest_after(lines[*pos].trim(), "while"))?;
    *pos += 1;
    let (body, _) = parse_block(lines, pos, &["endwhile"])?;
    *pos += 1;
    Ok(Stmt::While { cond, body })
}

fn parse_function(lines: &[String], pos: &mut usize) -> Result<Stmt, String> {
    // `function[!] Name(a, b, ...)`
    let header = lines[*pos].trim();
    let header = header.strip_prefix("function!").or_else(|| header.strip_prefix("function")).unwrap().trim();
    let lp = header.find('(').ok_or("function without (")?;
    let rp = header.rfind(')').ok_or("function without )")?;
    let name = header[..lp].trim().to_string();
    let params: Vec<String> = header[lp + 1..rp]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    *pos += 1;
    let (body, _) = parse_block(lines, pos, &["endfunction", "endfunc"])?;
    *pos += 1;
    Ok(Stmt::Function { name, params, body })
}

fn parse_simple(line: &str) -> Result<Stmt, String> {
    let kw = first_word(line);
    match kw.as_str() {
        "let" => {
            let rest = rest_after(line, "let");
            // Find the assignment operator (=, +=, -=, .=, *=, /=).
            let (target_str, op, value_str) = split_assign(rest)?;
            let target = parse_expr_str(target_str)?;
            let value = parse_expr_str(value_str)?;
            Ok(Stmt::Let { target, op, value })
        }
        "call" => Ok(Stmt::Call(parse_expr_str(rest_after(line, "call"))?)),
        "echo" | "echom" | "echomsg" => {
            let rest = rest_after(line, &kw);
            // echo takes space-separated expressions; parse the whole thing as
            // one expression list is complex — parse as a single expression for
            // the common case, else split on top-level spaces is hard. Keep it
            // to one expression here.
            Ok(Stmt::Echo(vec![parse_expr_str(rest)?]))
        }
        "return" => {
            let rest = rest_after(line, "return");
            if rest.trim().is_empty() {
                Ok(Stmt::Return(None))
            } else {
                Ok(Stmt::Return(Some(parse_expr_str(rest)?)))
            }
        }
        "break" => Ok(Stmt::Break),
        "continue" => Ok(Stmt::Continue),
        _ => Ok(Stmt::ExprStmt(parse_expr_str(line)?)),
    }
}

fn split_assign(s: &str) -> Result<(&str, char, &str), String> {
    // Look for compound assignment first.
    for (pat, op) in [(" += ", '+'), (" -= ", '-'), (" .= ", '.'), (" *= ", '*'), (" /= ", '/')] {
        if let Some(idx) = s.find(pat) {
            return Ok((&s[..idx], op, &s[idx + pat.len()..]));
        }
    }
    let idx = s.find('=').ok_or("let without =")?;
    Ok((&s[..idx], '=', &s[idx + 1..]))
}

fn first_word(s: &str) -> String {
    s.split_whitespace().next().unwrap_or("").to_string()
}

fn rest_after<'a>(line: &'a str, kw: &str) -> &'a str {
    line.trim().strip_prefix(kw).unwrap_or(line).trim()
}

// --- expression parser (precedence climbing) ---

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

/// Parse a standalone expression string.
pub fn parse_expr_str(s: &str) -> Result<Expr, String> {
    let toks = lex(s)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_expr(0)?;
    if p.pos != p.toks.len() {
        return Err(format!("trailing tokens in expression: {s:?}"));
    }
    Ok(e)
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> Result<(), String> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {t:?}, found {:?}", self.peek()))
        }
    }

    /// Precedence-climbing. Binary precedence: `||`(1) `&&`(2) comparisons(3)
    /// `.`concat/`+`/`-`(4) `*`/`/`/`%`(5).
    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let (op, bp) = match self.peek() {
                Some(Tok::Or) => (BinOp::Or, 1),
                Some(Tok::And) => (BinOp::And, 2),
                Some(Tok::Eq) => (BinOp::Eq, 3),
                Some(Tok::Ne) => (BinOp::Ne, 3),
                Some(Tok::Lt) => (BinOp::Lt, 3),
                Some(Tok::Le) => (BinOp::Le, 3),
                Some(Tok::Gt) => (BinOp::Gt, 3),
                Some(Tok::Ge) => (BinOp::Ge, 3),
                Some(Tok::Plus) => (BinOp::Add, 4),
                Some(Tok::Minus) => (BinOp::Sub, 4),
                Some(Tok::Dot) => (BinOp::Concat, 4),
                Some(Tok::Star) => (BinOp::Mul, 5),
                Some(Tok::Slash) => (BinOp::Div, 5),
                Some(Tok::Percent) => (BinOp::Mod, 5),
                _ => break,
            };
            if bp < min_bp {
                break;
            }
            self.next();
            let rhs = self.parse_expr(bp + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        // Ternary (lowest precedence, right associative).
        if min_bp == 0 {
            if let Some(Tok::Question) = self.peek() {
                self.next();
                let then_e = self.parse_expr(0)?;
                self.eat(&Tok::Colon)?;
                let else_e = self.parse_expr(0)?;
                lhs = Expr::Ternary(Box::new(lhs), Box::new(then_e), Box::new(else_e));
            }
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Minus) => Ok(Expr::Unary('-', Box::new(self.parse_prefix()?))),
            Some(Tok::Not) => Ok(Expr::Unary('!', Box::new(self.parse_prefix()?))),
            Some(Tok::Int(i)) => self.parse_postfix(Expr::Int(i)),
            Some(Tok::Float(f)) => self.parse_postfix(Expr::Float(f)),
            Some(Tok::Str(s)) => self.parse_postfix(Expr::Str(s)),
            Some(Tok::Ident(name)) => {
                // Function call?
                if self.peek() == Some(&Tok::LParen) {
                    self.next();
                    let args = self.parse_arg_list()?;
                    self.eat(&Tok::RParen)?;
                    self.parse_postfix(Expr::Call(name, args))
                } else {
                    self.parse_postfix(Expr::Var(name))
                }
            }
            Some(Tok::LParen) => {
                let e = self.parse_expr(0)?;
                self.eat(&Tok::RParen)?;
                self.parse_postfix(e)
            }
            Some(Tok::LBracket) => {
                let items = self.parse_arg_list_until(&Tok::RBracket)?;
                self.eat(&Tok::RBracket)?;
                self.parse_postfix(Expr::List(items))
            }
            Some(Tok::LBrace) => {
                let mut pairs = Vec::new();
                while self.peek() != Some(&Tok::RBrace) {
                    let k = self.parse_expr(0)?;
                    self.eat(&Tok::Colon)?;
                    let v = self.parse_expr(0)?;
                    pairs.push((k, v));
                    if self.peek() == Some(&Tok::Comma) {
                        self.next();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RBrace)?;
                self.parse_postfix(Expr::Dict(pairs))
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }

    fn parse_postfix(&mut self, mut e: Expr) -> Result<Expr, String> {
        loop {
            match self.peek() {
                Some(Tok::LBracket) => {
                    self.next();
                    let idx = self.parse_expr(0)?;
                    self.eat(&Tok::RBracket)?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, String> {
        self.parse_arg_list_until(&Tok::RParen)
    }

    fn parse_arg_list_until(&mut self, end: &Tok) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        while self.peek() != Some(end) && self.peek().is_some() {
            args.push(self.parse_expr(0)?);
            if self.peek() == Some(&Tok::Comma) {
                self.next();
            } else {
                break;
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_binary_precedence() {
        let e = parse_expr_str("1 + 2 * 3").unwrap();
        assert_eq!(
            e,
            Expr::Binary(
                BinOp::Add,
                Box::new(Expr::Int(1)),
                Box::new(Expr::Binary(BinOp::Mul, Box::new(Expr::Int(2)), Box::new(Expr::Int(3))))
            )
        );
    }

    #[test]
    fn parse_let_statement() {
        let prog = parse_program("let g:x = 5").unwrap();
        assert_eq!(prog.len(), 1);
        matches!(&prog[0], Stmt::Let { .. });
    }

    #[test]
    fn parse_if_block() {
        let prog = parse_program("if x > 0\n  let y = 1\nelse\n  let y = 2\nendif").unwrap();
        assert_eq!(prog.len(), 1);
        match &prog[0] {
            Stmt::If { branches, else_body } => {
                assert_eq!(branches.len(), 1);
                assert!(else_body.is_some());
            }
            _ => panic!("expected if"),
        }
    }

    #[test]
    fn parse_function_def() {
        let prog = parse_program("function Add(a, b)\n  return a:a + a:b\nendfunction").unwrap();
        match &prog[0] {
            Stmt::Function { name, params, body } => {
                assert_eq!(name, "Add");
                assert_eq!(params, &["a", "b"]);
                assert_eq!(body.len(), 1);
            }
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn parse_list_and_index() {
        let e = parse_expr_str("[1, 2, 3][0]").unwrap();
        matches!(e, Expr::Index(_, _));
    }
}
