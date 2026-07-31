//! The tree-walking interpreter: scopes, statement execution, expression
//! evaluation, and user-function calls.

use crate::builtins;
use crate::parser::{parse_program, BinOp, Expr, Stmt};
use crate::value;
use ctrlvim_types::{Error, Object, Result};
use std::collections::HashMap;

/// Editor operations the editor-aware builtins (`getline`, `setline`, `line`,
/// `col`) need. Implemented for `ctrlvim_editor::Editor` in [`crate::editor_access`].
pub trait EditorAccess {
    fn line_count(&self) -> i64;
    fn get_line(&self, lnum: i64) -> String;
    fn set_line(&mut self, lnum: i64, text: &str);
    /// `(1-based line, 1-based col)`.
    fn cursor(&self) -> (i64, i64);
    /// The current buffer's name/path (`expand('%')`), empty for an unnamed
    /// buffer.
    fn buffer_name(&self) -> String {
        String::new()
    }
}

/// A no-op editor for pure-Vimscript evaluation/tests.
#[derive(Default)]
pub struct NullEditor {
    lines: Vec<String>,
}

impl NullEditor {
    pub fn with_lines(lines: &[&str]) -> Self {
        NullEditor { lines: lines.iter().map(|s| s.to_string()).collect() }
    }
}

impl EditorAccess for NullEditor {
    fn line_count(&self) -> i64 {
        self.lines.len().max(1) as i64
    }
    fn get_line(&self, lnum: i64) -> String {
        self.lines.get((lnum - 1).max(0) as usize).cloned().unwrap_or_default()
    }
    fn set_line(&mut self, lnum: i64, text: &str) {
        let idx = (lnum - 1).max(0) as usize;
        if idx < self.lines.len() {
            self.lines[idx] = text.to_string();
        } else {
            self.lines.resize(idx + 1, String::new());
            self.lines[idx] = text.to_string();
        }
    }
    fn cursor(&self) -> (i64, i64) {
        (1, 1)
    }
}

/// A user-defined function.
#[derive(Clone)]
pub struct FuncDef {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

/// Persistent script state: global variables and user functions. Lives across
/// `:call`/`vim.fn` invocations (in `ApiContext`).
#[derive(Default)]
pub struct ScriptState {
    pub globals: HashMap<String, Object>,
    pub funcs: HashMap<String, FuncDef>,
    /// Text produced by `:echo`, drained by the frontend for the message area.
    pub output: Vec<String>,
}

/// Control-flow result of executing a statement.
enum Flow {
    Normal,
    Break,
    Continue,
    Return(Object),
}

/// One function-call frame: local (`l:`) and argument (`a:`) variables.
#[derive(Default)]
struct Frame {
    locals: HashMap<String, Object>,
    args: HashMap<String, Object>,
}

/// The interpreter borrows the persistent state and an editor for the duration
/// of a run.
pub struct Interp<'e> {
    state: &'e mut ScriptState,
    editor: &'e mut dyn EditorAccess,
    frames: Vec<Frame>,
}

impl<'e> Interp<'e> {
    pub fn new(state: &'e mut ScriptState, editor: &'e mut dyn EditorAccess) -> Self {
        Interp { state, editor, frames: Vec::new() }
    }

    /// Parse and run a whole script.
    pub fn run(&mut self, src: &str) -> Result<()> {
        let prog = parse_program(src).map_err(Error::exception)?;
        self.exec_block(&prog)?;
        Ok(())
    }

    /// Call a function (user-defined or builtin) by name.
    pub fn call_function(&mut self, name: &str, args: Vec<Object>) -> Result<Object> {
        if let Some(def) = self.state.funcs.get(name).cloned() {
            return self.call_user(&def, args);
        }
        builtins::call(name, &args, self.editor).ok_or_else(|| {
            Error::exception(format!("Unknown function: {name}"))
        })?
    }

    fn call_user(&mut self, def: &FuncDef, args: Vec<Object>) -> Result<Object> {
        let mut frame = Frame::default();
        for (i, param) in def.params.iter().enumerate() {
            if param == "..." {
                break;
            }
            frame.args.insert(param.clone(), args.get(i).cloned().unwrap_or(Object::Nil));
        }
        frame.args.insert("0".to_string(), Object::Integer(args.len() as i64));
        self.frames.push(frame);
        let result = self.exec_block(&def.body);
        self.frames.pop();
        match result? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Object::Nil),
        }
    }

    fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Flow> {
        for stmt in stmts {
            match self.exec_stmt(stmt)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Flow> {
        match stmt {
            Stmt::Let { target, op, value } => {
                let rhs = self.eval(value)?;
                self.assign(target, *op, rhs)?;
                Ok(Flow::Normal)
            }
            Stmt::Call(expr) => {
                self.eval(expr)?;
                Ok(Flow::Normal)
            }
            Stmt::Echo(exprs) => {
                // Evaluate each argument and record the joined text so a frontend
                // can surface it in the message area.
                let mut parts = Vec::with_capacity(exprs.len());
                for e in exprs {
                    parts.push(crate::value::to_string(&self.eval(e)?));
                }
                self.state.output.push(parts.join(" "));
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Object::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If { branches, else_body } => {
                for (cond, body) in branches {
                    if value::is_truthy(&self.eval(cond)?) {
                        return self.exec_block(body);
                    }
                }
                if let Some(body) = else_body {
                    return self.exec_block(body);
                }
                Ok(Flow::Normal)
            }
            Stmt::For { var, iter, body } => {
                let seq = self.eval(iter)?;
                let items = match seq {
                    Object::Array(items) => items,
                    _ => return Err(Error::exception("for expects a list")),
                };
                for item in items {
                    self.set_var(var, item);
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ Flow::Return(_) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::While { cond, body } => {
                while value::is_truthy(&self.eval(cond)?) {
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ Flow::Return(_) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Function { name, params, body } => {
                self.state.funcs.insert(
                    name.clone(),
                    FuncDef { params: params.clone(), body: body.clone() },
                );
                Ok(Flow::Normal)
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::ExprStmt(e) => {
                self.eval(e)?;
                Ok(Flow::Normal)
            }
        }
    }

    fn assign(&mut self, target: &Expr, op: char, rhs: Object) -> Result<()> {
        match target {
            Expr::Var(name) => {
                let new = if op == '=' {
                    rhs
                } else {
                    let cur = self.get_var(name).unwrap_or(Object::Integer(0));
                    match op {
                        '.' => Object::str(format!("{}{}", value::to_string(&cur), value::to_string(&rhs))),
                        _ => value::arith(op, &cur, &rhs)?,
                    }
                };
                self.set_var(name, new);
                Ok(())
            }
            Expr::Index(base, idx) => {
                // let l[i] = v  /  let d[k] = v
                let key = self.eval(idx)?;
                if let Expr::Var(name) = base.as_ref() {
                    let mut container = self.get_var(name).unwrap_or(Object::Array(vec![]));
                    set_index(&mut container, &key, rhs)?;
                    self.set_var(name, container);
                    Ok(())
                } else {
                    Err(Error::exception("unsupported assignment target"))
                }
            }
            _ => Err(Error::exception("invalid assignment target")),
        }
    }

    fn eval(&mut self, expr: &Expr) -> Result<Object> {
        Ok(match expr {
            Expr::Int(i) => Object::Integer(*i),
            Expr::Float(f) => Object::Float(*f),
            Expr::Str(s) => Object::str(s.clone()),
            Expr::List(items) => {
                let mut out = Vec::new();
                for e in items {
                    out.push(self.eval(e)?);
                }
                Object::Array(out)
            }
            Expr::Dict(pairs) => {
                let mut map = std::collections::BTreeMap::new();
                for (k, v) in pairs {
                    let key = value::to_string(&self.eval(k)?);
                    map.insert(key, self.eval(v)?);
                }
                Object::Dict(map)
            }
            Expr::Var(name) => self
                .get_var(name)
                .ok_or_else(|| Error::exception(format!("Undefined variable: {name}")))?,
            Expr::Unary(op, e) => {
                let v = self.eval(e)?;
                match op {
                    '-' => value::arith('-', &Object::Integer(0), &v)?,
                    '!' => Object::Integer(if value::is_truthy(&v) { 0 } else { 1 }),
                    _ => return Err(Error::exception("bad unary op")),
                }
            }
            Expr::Binary(op, a, b) => self.eval_binary(*op, a, b)?,
            Expr::Call(name, args) => {
                let mut argv = Vec::new();
                for a in args {
                    argv.push(self.eval(a)?);
                }
                self.call_function(name, argv)?
            }
            Expr::Index(base, idx) => {
                let b = self.eval(base)?;
                let i = self.eval(idx)?;
                index_value(&b, &i)?
            }
            Expr::Ternary(c, t, f) => {
                if value::is_truthy(&self.eval(c)?) {
                    self.eval(t)?
                } else {
                    self.eval(f)?
                }
            }
        })
    }

    fn eval_binary(&mut self, op: BinOp, a: &Expr, b: &Expr) -> Result<Object> {
        // Short-circuit logical operators.
        match op {
            BinOp::And => {
                let l = self.eval(a)?;
                if !value::is_truthy(&l) {
                    return Ok(Object::Integer(0));
                }
                return Ok(Object::Integer(value::is_truthy(&self.eval(b)?) as i64));
            }
            BinOp::Or => {
                let l = self.eval(a)?;
                if value::is_truthy(&l) {
                    return Ok(Object::Integer(1));
                }
                return Ok(Object::Integer(value::is_truthy(&self.eval(b)?) as i64));
            }
            _ => {}
        }
        let l = self.eval(a)?;
        let r = self.eval(b)?;
        Ok(match op {
            BinOp::Add => value::arith('+', &l, &r)?,
            BinOp::Sub => value::arith('-', &l, &r)?,
            BinOp::Mul => value::arith('*', &l, &r)?,
            BinOp::Div => value::arith('/', &l, &r)?,
            BinOp::Mod => value::arith('%', &l, &r)?,
            BinOp::Concat => Object::str(format!("{}{}", value::to_string(&l), value::to_string(&r))),
            BinOp::Eq => Object::Integer((compare(&l, &r) == 0) as i64),
            BinOp::Ne => Object::Integer((compare(&l, &r) != 0) as i64),
            BinOp::Lt => Object::Integer((compare(&l, &r) < 0) as i64),
            BinOp::Le => Object::Integer((compare(&l, &r) <= 0) as i64),
            BinOp::Gt => Object::Integer((compare(&l, &r) > 0) as i64),
            BinOp::Ge => Object::Integer((compare(&l, &r) >= 0) as i64),
            BinOp::And | BinOp::Or => unreachable!(),
        })
    }

    // --- variable scoping ---

    fn get_var(&self, name: &str) -> Option<Object> {
        if let Some(rest) = name.strip_prefix("a:") {
            return self.frames.last().and_then(|f| f.args.get(rest).cloned());
        }
        if let Some(rest) = name.strip_prefix("g:") {
            return self.state.globals.get(rest).cloned();
        }
        if let Some(rest) = name.strip_prefix("v:") {
            return vvar(rest);
        }
        if let Some(rest) = name.strip_prefix("l:") {
            return self.frames.last().and_then(|f| f.locals.get(rest).cloned());
        }
        // Unscoped: local inside a function, else global.
        if let Some(frame) = self.frames.last() {
            if let Some(v) = frame.locals.get(name) {
                return Some(v.clone());
            }
            if let Some(v) = frame.args.get(name) {
                return Some(v.clone());
            }
        }
        self.state.globals.get(name).cloned()
    }

    fn set_var(&mut self, name: &str, value: Object) {
        if let Some(rest) = name.strip_prefix("g:") {
            self.state.globals.insert(rest.to_string(), value);
            return;
        }
        if let Some(rest) = name.strip_prefix("l:") {
            if let Some(frame) = self.frames.last_mut() {
                frame.locals.insert(rest.to_string(), value);
            }
            return;
        }
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.insert(name.to_string(), value);
        } else {
            self.state.globals.insert(name.to_string(), value);
        }
    }
}

fn vvar(name: &str) -> Option<Object> {
    Some(match name {
        "true" => Object::Integer(1),
        "false" => Object::Integer(0),
        "null" => Object::Nil,
        "t_number" => Object::Integer(0),
        "t_string" => Object::Integer(1),
        "t_func" => Object::Integer(2),
        "t_list" => Object::Integer(3),
        "t_dict" => Object::Integer(4),
        "t_float" => Object::Integer(5),
        _ => return None,
    })
}

/// Vimscript comparison: numbers numerically, strings lexically, mixed via
/// number coercion.
fn compare(a: &Object, b: &Object) -> i32 {
    match (a, b) {
        (Object::String(_), Object::String(_)) => {
            value::to_string(a).cmp(&value::to_string(b)) as i32
        }
        _ => {
            let x = value::to_number(a);
            let y = value::to_number(b);
            x.cmp(&y) as i32
        }
    }
}

fn index_value(base: &Object, idx: &Object) -> Result<Object> {
    match base {
        Object::Array(items) => {
            let mut i = value::to_number(idx);
            if i < 0 {
                i += items.len() as i64;
            }
            Ok(items.get(i.max(0) as usize).cloned().unwrap_or(Object::Nil))
        }
        Object::Dict(map) => Ok(map.get(&value::to_string(idx)).cloned().unwrap_or(Object::Nil)),
        Object::String(bytes) => {
            let s = String::from_utf8_lossy(bytes);
            let i = value::to_number(idx).max(0) as usize;
            Ok(Object::str(s.chars().nth(i).map(|c| c.to_string()).unwrap_or_default()))
        }
        _ => Err(Error::exception("cannot index this value")),
    }
}

fn set_index(container: &mut Object, key: &Object, val: Object) -> Result<()> {
    match container {
        Object::Array(items) => {
            let mut i = value::to_number(key);
            if i < 0 {
                i += items.len() as i64;
            }
            let i = i.max(0) as usize;
            if i < items.len() {
                items[i] = val;
            } else {
                items.resize(i + 1, Object::Nil);
                items[i] = val;
            }
            Ok(())
        }
        Object::Dict(map) => {
            map.insert(value::to_string(key), val);
            Ok(())
        }
        _ => Err(Error::exception("cannot index-assign this value")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> ScriptState {
        let mut state = ScriptState::default();
        let mut ed = NullEditor::default();
        {
            let mut interp = Interp::new(&mut state, &mut ed);
            interp.run(src).unwrap();
        }
        state
    }

    fn eval_g(src: &str, var: &str) -> Object {
        run(src).globals.get(var).cloned().unwrap_or(Object::Nil)
    }

    #[test]
    fn let_and_arithmetic() {
        assert_eq!(eval_g("let g:x = 2 + 3 * 4", "x"), Object::Integer(14));
    }

    #[test]
    fn string_concat() {
        assert_eq!(eval_g("let g:s = 'foo' . 'bar'", "s"), Object::str("foobar"));
    }

    #[test]
    fn if_else() {
        let src = "let g:r = 0\nif 3 > 2\n  let g:r = 10\nelse\n  let g:r = 20\nendif";
        assert_eq!(eval_g(src, "r"), Object::Integer(10));
    }

    #[test]
    fn for_loop_sum() {
        let src = "let g:sum = 0\nfor i in [1, 2, 3, 4]\n  let g:sum += i\nendfor";
        assert_eq!(eval_g(src, "sum"), Object::Integer(10));
    }

    #[test]
    fn while_loop() {
        let src = "let g:n = 0\nlet g:i = 0\nwhile g:i < 5\n  let g:n += g:i\n  let g:i += 1\nendwhile";
        assert_eq!(eval_g(src, "n"), Object::Integer(10));
    }

    #[test]
    fn user_function_call() {
        let src = "function Add(a, b)\n  return a:a + a:b\nendfunction\nlet g:r = Add(3, 4)";
        assert_eq!(eval_g(src, "r"), Object::Integer(7));
    }

    #[test]
    fn recursion_factorial() {
        let src = "function Fact(n)\n  if a:n <= 1\n    return 1\n  endif\n  return a:n * Fact(a:n - 1)\nendfunction\nlet g:r = Fact(5)";
        assert_eq!(eval_g(src, "r"), Object::Integer(120));
    }

    #[test]
    fn builtin_len_and_list_index() {
        assert_eq!(eval_g("let g:n = len([10, 20, 30])", "n"), Object::Integer(3));
        assert_eq!(eval_g("let g:v = [10, 20, 30][1]", "v"), Object::Integer(20));
    }

    #[test]
    fn dict_literal_and_index() {
        assert_eq!(eval_g("let g:v = {'a': 1, 'b': 2}['b']", "v"), Object::Integer(2));
    }

    #[test]
    fn ternary() {
        assert_eq!(eval_g("let g:r = 5 > 3 ? 'yes' : 'no'", "r"), Object::str("yes"));
    }
}
