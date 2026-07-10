//! Registers — the yank/delete/paste storage (`register.c`).
//!
//! Neovim keeps a flat `yankreg_T y_regs[NUM_REGISTERS]` array. We model the
//! same: named (`a`-`z`), numbered (`0`-`9`), and a handful of special
//! registers, each holding a list of lines plus a [`MotionType`] tag. Clipboard
//! provider and ShaDa persistence are deferred (they need the Lua/RPC layer).

use std::collections::HashMap;

/// How yanked/deleted text behaves on paste. Mirrors Neovim's `MotionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionType {
    /// Charwise: pasted inline at the cursor.
    Char,
    /// Linewise: pasted as whole lines below/above.
    Line,
    /// Blockwise: pasted as a rectangular block.
    Block,
}

/// One register's contents.
#[derive(Debug, Clone)]
pub struct YankReg {
    pub lines: Vec<String>,
    pub motion: MotionType,
}

impl YankReg {
    pub fn new(lines: Vec<String>, motion: MotionType) -> Self {
        YankReg { lines, motion }
    }
}

/// The full register file.
#[derive(Debug, Default)]
pub struct Registers {
    regs: HashMap<char, YankReg>,
    /// Name of the register `""` currently points at (the unnamed register
    /// tracks the most recent yank/delete, Neovim's `y_previous`).
    unnamed_points_to: Option<char>,
}

impl Registers {
    pub fn new() -> Self {
        Registers::default()
    }

    /// Resolve a register name to a normalized key. Uppercase `A`-`Z` map to
    /// their lowercase slot (with append semantics handled in [`Self::write`]).
    fn normalize(name: char) -> char {
        name.to_ascii_lowercase()
    }

    /// Write to a register. Uppercase names append to the existing contents
    /// (Neovim's `A`-`Z` semantics). Writing to a named/numbered register also
    /// updates the unnamed register `"` to point at it.
    pub fn write(&mut self, name: char, reg: YankReg) {
        let append = name.is_ascii_uppercase();
        let key = Self::normalize(name);

        if append {
            if let Some(existing) = self.regs.get_mut(&key) {
                // Append: join at the boundary for charwise, else concat lines.
                existing.lines.extend(reg.lines);
                existing.motion = reg.motion;
            } else {
                self.regs.insert(key, reg);
            }
        } else {
            self.regs.insert(key, reg);
        }

        if key != '_' {
            self.unnamed_points_to = Some(key);
        }
    }

    /// Perform a yank: writes register `0` (the yank register) plus the target
    /// register if named, mirroring Neovim's default yank routing.
    pub fn yank(&mut self, name: Option<char>, reg: YankReg) {
        match name {
            Some('_') => { /* blackhole: discard */ }
            Some(n) => self.write(n, reg),
            None => {
                self.regs.insert('0', reg.clone());
                self.unnamed_points_to = Some('0');
            }
        }
    }

    /// Perform a delete: shifts the numbered ring `1`..`9` for linewise/multi-
    /// line deletes, or writes the small-delete register `-` for charwise
    /// single-line deletes (`shift_delete_registers`).
    pub fn delete(&mut self, name: Option<char>, reg: YankReg) {
        match name {
            Some('_') => return,
            Some(n) => {
                self.write(n, reg);
                return;
            }
            None => {}
        }
        let small = reg.motion == MotionType::Char && reg.lines.len() <= 1;
        if small {
            self.regs.insert('-', reg);
            self.unnamed_points_to = Some('-');
        } else {
            // Shift 1->2, ..., 8->9, then store into 1.
            for i in (1..9).rev() {
                let from = char::from_digit(i, 10).unwrap();
                let to = char::from_digit(i + 1, 10).unwrap();
                if let Some(r) = self.regs.get(&from).cloned() {
                    self.regs.insert(to, r);
                }
            }
            self.regs.insert('1', reg);
            self.unnamed_points_to = Some('1');
        }
    }

    /// Read a register by name. `"` reads whatever the unnamed register points
    /// at.
    pub fn read(&self, name: char) -> Option<&YankReg> {
        let key = if name == '"' {
            self.unnamed_points_to?
        } else {
            Self::normalize(name)
        };
        self.regs.get(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(lines: &[&str], m: MotionType) -> YankReg {
        YankReg::new(lines.iter().map(|s| s.to_string()).collect(), m)
    }

    #[test]
    fn yank_updates_zero_and_unnamed() {
        let mut r = Registers::new();
        r.yank(None, reg(&["hello"], MotionType::Char));
        assert_eq!(r.read('0').unwrap().lines, vec!["hello"]);
        assert_eq!(r.read('"').unwrap().lines, vec!["hello"]);
    }

    #[test]
    fn delete_shifts_numbered_ring() {
        let mut r = Registers::new();
        r.delete(None, reg(&["one", "two"], MotionType::Line));
        r.delete(None, reg(&["three", "four"], MotionType::Line));
        assert_eq!(r.read('1').unwrap().lines, vec!["three", "four"]);
        assert_eq!(r.read('2').unwrap().lines, vec!["one", "two"]);
    }

    #[test]
    fn small_delete_uses_dash_register() {
        let mut r = Registers::new();
        r.delete(None, reg(&["x"], MotionType::Char));
        assert_eq!(r.read('-').unwrap().lines, vec!["x"]);
    }

    #[test]
    fn uppercase_appends() {
        let mut r = Registers::new();
        r.write('a', reg(&["one"], MotionType::Line));
        r.write('A', reg(&["two"], MotionType::Line));
        assert_eq!(r.read('a').unwrap().lines, vec!["one", "two"]);
    }

    #[test]
    fn blackhole_discards() {
        let mut r = Registers::new();
        r.yank(Some('_'), reg(&["gone"], MotionType::Char));
        assert!(r.read('_').is_none());
    }
}
