//! Lowering the AST to a program — the back half of `nfa_regcomp`.
//!
//! The instruction set is the classic Thompson layout (`Split` for a choice,
//! `Jump` for a loop, `Save` for a capture slot) with the additions Vim's
//! syntax forces: [`Inst::Backref`], [`Inst::Look`] and [`Inst::Atomic`], none
//! of which a DFA can express. That is the whole reason this crate exists —
//! the previous engine translated Vim patterns onto a DFA library, where
//! `\1` and `\@<=` were not unimplemented but *impossible*.
//!
//! Counted repeats are expanded by duplication (`a\{2,4}` becomes
//! `aa\(a\(a\)\?\)\?`) rather than given a counter instruction. Duplication is
//! simpler and it keeps the VM's state per backtrack point small, which matters
//! more here because that state is cloned on every choice point.

use crate::class::Class;
use crate::parse::{Assert, Ast};

/// One VM instruction. Jump targets are absolute indices into
/// [`Program::insts`].
#[derive(Debug, Clone)]
pub enum Inst {
    Char(char),
    Any {
        nl: bool,
    },
    Class {
        class: usize,
        nl: bool,
    },
    /// Try `0` first, keep `1` as a backtrack point.
    Split(usize, usize),
    Jump(usize),
    /// Record the current position in capture slot `0`.
    Save(usize),
    Backref(usize),
    Assert(Assert),
    /// `\zs` — the reported match starts here instead.
    SetStart,
    /// `\ze` — the reported match ends here instead.
    SetEnd,
    /// Enter a sub-program without consuming input.
    Look {
        negate: bool,
        behind: bool,
        prog: usize,
    },
    /// `\@>` — run the sub-program once and keep the result.
    Atomic {
        prog: usize,
    },
    /// Remember the position, so an unbounded loop can tell it made progress.
    LoopStart(usize),
    /// Fail if the loop body consumed nothing since [`Inst::LoopStart`].
    LoopCheck(usize),
    Match,
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Program {
    pub insts: Vec<Inst>,
    pub classes: Vec<Class>,
    /// Bodies of lookarounds and atomic groups, referenced by index.
    pub subs: Vec<Program>,
    /// Number of capturing groups, excluding the whole-match group 0.
    pub groups: usize,
    /// How many `LoopStart`/`LoopCheck` pairs need state.
    pub loops: usize,
    /// Set when the pattern can only match at the start of a line, so the
    /// search does not retry at every position.
    pub anchored: bool,
}

impl Program {
    /// Capture slots the VM must carry: two per group, plus group 0.
    pub fn slots(&self) -> usize {
        (self.groups + 1) * 2
    }
}

/// Compile a parsed pattern into a runnable program.
pub fn compile(ast: &Ast, groups: usize) -> Program {
    let mut c = Compiler { insts: Vec::new(), classes: Vec::new(), subs: Vec::new(), loops: 0 };
    // Slot 0/1 bracket the whole match; `\zs`/`\ze` override them later.
    c.push(Inst::Save(0));
    c.emit(ast);
    c.push(Inst::Save(1));
    c.push(Inst::Match);
    let anchored = starts_anchored(ast);
    Program {
        insts: c.insts,
        classes: c.classes,
        subs: c.subs,
        groups,
        loops: c.loops,
        anchored,
    }
}

/// Whether every branch begins with `^`, which lets the search skip straight to
/// column zero instead of retrying at each character.
fn starts_anchored(ast: &Ast) -> bool {
    match ast {
        Ast::Assert(Assert::LineStart) => true,
        Ast::Concat(parts) => parts.iter().find(|p| !is_zero_width(p)).is_some_and(starts_anchored),
        Ast::Alt(branches) => branches.iter().all(starts_anchored),
        Ast::Group { node, .. } => starts_anchored(node),
        _ => false,
    }
}

/// Nodes that neither consume input nor decide where a match may start.
fn is_zero_width(ast: &Ast) -> bool {
    matches!(ast, Ast::Empty | Ast::MatchStart | Ast::MatchEnd)
}

struct Compiler {
    insts: Vec<Inst>,
    classes: Vec<Class>,
    subs: Vec<Program>,
    loops: usize,
}

impl Compiler {
    fn push(&mut self, inst: Inst) -> usize {
        self.insts.push(inst);
        self.insts.len() - 1
    }

    fn here(&self) -> usize {
        self.insts.len()
    }

    fn emit(&mut self, ast: &Ast) {
        match ast {
            Ast::Empty => {}
            Ast::Literal(c) => {
                self.push(Inst::Char(*c));
            }
            Ast::Any { nl } => {
                self.push(Inst::Any { nl: *nl });
            }
            Ast::Class { class, nl } => {
                self.classes.push(class.clone());
                let idx = self.classes.len() - 1;
                self.push(Inst::Class { class: idx, nl: *nl });
            }
            Ast::Concat(parts) => {
                for p in parts {
                    self.emit(p);
                }
            }
            Ast::Alt(branches) => self.emit_alt(branches),
            Ast::Group { index, node } => {
                // Slots 0 and 1 belong to the whole match, so group n uses 2n.
                match index {
                    Some(n) => {
                        self.push(Inst::Save(n * 2));
                        self.emit(node);
                        self.push(Inst::Save(n * 2 + 1));
                    }
                    None => self.emit(node),
                }
            }
            Ast::Repeat { node, min, max, greedy } => self.emit_repeat(node, *min, *max, *greedy),
            Ast::Backref(n) => {
                self.push(Inst::Backref(*n));
            }
            Ast::Assert(a) => {
                self.push(Inst::Assert(*a));
            }
            Ast::MatchStart => {
                self.push(Inst::SetStart);
            }
            Ast::MatchEnd => {
                self.push(Inst::SetEnd);
            }
            Ast::Look { node, negate, behind } => {
                let prog = self.sub_program(node);
                self.push(Inst::Look { negate: *negate, behind: *behind, prog });
            }
            Ast::Atomic(node) => {
                let prog = self.sub_program(node);
                self.push(Inst::Atomic { prog });
            }
        }
    }

    /// Compile `node` as a standalone program a lookaround can run.
    ///
    /// Sub-programs share the parent's slot numbering so a capture made inside
    /// a positive lookahead survives into the outer match — `\(\d\+\)\@=` is
    /// only useful if `\1` can be read afterwards.
    fn sub_program(&mut self, node: &Ast) -> usize {
        let mut c = Compiler { insts: Vec::new(), classes: Vec::new(), subs: Vec::new(), loops: 0 };
        c.emit(node);
        c.push(Inst::Match);
        self.subs.push(Program {
            insts: c.insts,
            classes: c.classes,
            subs: c.subs,
            groups: 0,
            loops: c.loops,
            anchored: false,
        });
        self.subs.len() - 1
    }

    fn emit_alt(&mut self, branches: &[Ast]) {
        // Chain of two-way splits, so the first branch that matches wins —
        // Vim's alternation is ordered, not longest-wins.
        let mut jumps = Vec::new();
        for (i, branch) in branches.iter().enumerate() {
            if i + 1 == branches.len() {
                self.emit(branch);
                break;
            }
            let split = self.push(Inst::Split(0, 0));
            let body = self.here();
            self.emit(branch);
            jumps.push(self.push(Inst::Jump(0)));
            let next = self.here();
            self.insts[split] = Inst::Split(body, next);
        }
        let end = self.here();
        for j in jumps {
            self.insts[j] = Inst::Jump(end);
        }
    }

    fn emit_repeat(&mut self, node: &Ast, min: u32, max: Option<u32>, greedy: bool) {
        match (min, max) {
            // `x*`
            (0, None) => self.emit_star(node, greedy),
            // `x\+` — one mandatory copy, then a star.
            (1, None) => {
                self.emit(node);
                self.emit_star(node, greedy);
            }
            // `x\?`
            (0, Some(1)) => {
                let split = self.push(Inst::Split(0, 0));
                let body = self.here();
                self.emit(node);
                let end = self.here();
                self.insts[split] =
                    if greedy { Inst::Split(body, end) } else { Inst::Split(end, body) };
            }
            (n, None) => {
                for _ in 0..n {
                    self.emit(node);
                }
                self.emit_star(node, greedy);
            }
            (n, Some(m)) => {
                for _ in 0..n {
                    self.emit(node);
                }
                // The optional tail nests so that `a\{2,4}` cannot match a
                // fourth `a` without having matched the third.
                let mut splits = Vec::new();
                for _ in n..m {
                    let split = self.push(Inst::Split(0, 0));
                    let body = self.here();
                    splits.push((split, body));
                    self.emit(node);
                }
                let end = self.here();
                for (split, body) in splits {
                    self.insts[split] =
                        if greedy { Inst::Split(body, end) } else { Inst::Split(end, body) };
                }
            }
        }
    }

    /// The unbounded loop, guarded against a body that can match empty.
    ///
    /// Without the guard, `\(\)*` spins forever: the body succeeds, consumes
    /// nothing, and the jump returns to the same state. [`Inst::LoopCheck`]
    /// fails the iteration when the position has not moved, which ends the loop
    /// the way Vim's `nfa_regmatch` does.
    fn emit_star(&mut self, node: &Ast, greedy: bool) {
        let id = self.loops;
        self.loops += 1;
        let split = self.push(Inst::Split(0, 0));
        let body = self.here();
        self.push(Inst::LoopStart(id));
        self.emit(node);
        self.push(Inst::LoopCheck(id));
        self.push(Inst::Jump(split));
        let end = self.here();
        self.insts[split] = if greedy { Inst::Split(body, end) } else { Inst::Split(end, body) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, Magic};

    fn prog(pat: &str) -> Program {
        let p = parse(pat, Magic::Magic, "").expect("parses");
        compile(&p.ast, p.groups)
    }

    #[test]
    fn a_group_gets_the_slot_pair_after_the_whole_match() {
        let p = prog(r"\(a\)");
        let saves: Vec<usize> = p
            .insts
            .iter()
            .filter_map(|i| match i {
                Inst::Save(n) => Some(*n),
                _ => None,
            })
            .collect();
        // 0 and 1 bracket the match; 2 and 3 bracket group 1.
        assert_eq!(saves, vec![0, 2, 3, 1]);
        assert_eq!(p.slots(), 4);
    }

    #[test]
    fn an_unbounded_loop_is_guarded_against_an_empty_body() {
        let p = prog(r"\(a\)*");
        assert!(p.insts.iter().any(|i| matches!(i, Inst::LoopStart(_))));
        assert!(p.insts.iter().any(|i| matches!(i, Inst::LoopCheck(_))));
        assert_eq!(p.loops, 1);
    }

    #[test]
    fn a_counted_repeat_is_expanded_by_duplication() {
        let p = prog(r"a\{2,4}");
        let chars = p.insts.iter().filter(|i| matches!(i, Inst::Char('a'))).count();
        assert_eq!(chars, 4);
        // Two of them are behind splits, one split per optional copy.
        assert_eq!(p.insts.iter().filter(|i| matches!(i, Inst::Split(..))).count(), 2);
    }

    #[test]
    fn an_anchored_pattern_is_detected() {
        assert!(prog("^foo").anchored);
        assert!(prog(r"^a\|^b").anchored);
        // One unanchored branch is enough to force the full scan.
        assert!(!prog(r"^a\|b").anchored);
        assert!(!prog("foo").anchored);
    }

    #[test]
    fn lookaround_compiles_to_a_sub_program() {
        let p = prog(r"\(foo\)\@=bar");
        assert_eq!(p.subs.len(), 1);
        assert!(p.insts.iter().any(|i| matches!(i, Inst::Look { behind: false, .. })));
    }
}
