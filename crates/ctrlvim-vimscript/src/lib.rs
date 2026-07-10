//! A minimal Vimscript interpreter — the "interpreter-lite" from the plan's M6.
//!
//! Enough to load `ftplugin`/`indent` scripts and back `vim.fn.*`: variables and
//! scopes (`g:`/`l:`/`a:`/`v:`), arithmetic/string/comparison/logical
//! expressions, lists and dicts, `if`/`for`/`while`, user `function`s with
//! recursion, and a representative set of builtin functions. Full legacy
//! fidelity (`:execute`, exceptions, autoload, closures) is deferred until a
//! real plugin needs it.
//!
//! Values reuse [`ctrlvim_types::Object`] so results flow straight to Lua/RPC.

pub mod builtins;
pub mod editor_access;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod value;

pub use interp::{EditorAccess, Interp, NullEditor, ScriptState};

#[cfg(test)]
mod integration_tests {
    use super::*;
    use ctrlvim_types::Object;

    /// A small realistic script exercising the common surface together.
    #[test]
    fn ftplugin_style_script() {
        let src = r#"
            " set up a helper and compute an indent-like value
            let g:shiftwidth = 4
            function! ComputeIndent(level)
              return a:level * g:shiftwidth
            endfunction

            let g:levels = [0, 1, 2, 3]
            let g:indents = []
            for lvl in g:levels
              let g:indents = add(g:indents, ComputeIndent(lvl))
            endfor
            let g:total = 0
            for x in g:indents
              let g:total += x
            endfor
        "#;
        let mut state = ScriptState::default();
        let mut ed = NullEditor::default();
        {
            let mut interp = Interp::new(&mut state, &mut ed);
            interp.run(src).unwrap();
        }
        assert_eq!(state.globals.get("total"), Some(&Object::Integer(0 + 4 + 8 + 12)));
        assert_eq!(
            state.globals.get("indents"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(4),
                Object::Integer(8),
                Object::Integer(12)
            ]))
        );
    }
}
