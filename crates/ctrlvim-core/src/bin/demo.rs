//! ctrlvim milestone demo — exercises M1–M5 without any UI (the UI is the
//! user's Ratatui frontend). Run with `cargo run -p ctrlvim-core --bin ctrlvim-demo`.

use std::time::Duration;

use ctrlvim_core::{Host, Ctrlvim};
use ctrlvim_editor::Editor;

fn rule(title: &str) {
    println!("\n\x1b[1m== {title} ==\x1b[0m");
}

fn show(ctrlvim: &Ctrlvim) {
    let (row, col) = ctrlvim.cursor();
    println!("  mode={} cursor=({},{})", ctrlvim.mode(), row, col);
    for (i, line) in ctrlvim.lines().iter().enumerate() {
        println!("  {:>2} | {}", i + 1, line);
    }
}

fn main() -> mlua::Result<()> {
    // ---- M1: open a file and move/edit with modal keys ----
    rule("M1 — buffer + modal editing (no Lua)");
    let mut ctrlvim = Ctrlvim::new();
    ctrlvim.open("The quick brown fox\njumps over\nthe lazy dog", Some("demo.txt"));
    println!("opened demo.txt:");
    show(&ctrlvim);

    println!("\nfeed: `wdw` (next word, delete word)");
    ctrlvim.feed("wdw");
    show(&ctrlvim);

    println!("\nfeed: `Gonew last line<Esc>` (append line at bottom)");
    ctrlvim.feed("Gonew last line<Esc>");
    show(&ctrlvim);

    println!("\nfeed: `ggyyp` (duplicate first line)");
    ctrlvim.feed("ggyyp");
    show(&ctrlvim);

    // ---- M2: undo tree ----
    rule("M2 — undo/redo");
    println!("feed: `u` (undo the duplicate)");
    ctrlvim.feed("u");
    show(&ctrlvim);

    // ---- M4/M5: Lua runtime with vim.api, autocmds, and vim.uv timers ----
    rule("M4 — Lua: vim.api");
    let mut ed = Editor::new();
    ed.load_str("lua sees this line", Some("lua.txt"));
    let host = Host::new(ed)?;
    let line = host.eval_string("vim.api.ctrlvim_get_current_line()")?;
    println!("  vim.api.ctrlvim_get_current_line() => {line:?}");

    host.exec("vim.api.ctrlvim_set_current_line('rewritten by lua')")?;
    let line = host.eval_string("vim.api.ctrlvim_get_current_line()")?;
    println!("  after ctrlvim_set_current_line   => {line:?}");

    rule("M4 — Lua: autocmd callback");
    host.exec(
        r#"
        vim.api.ctrlvim_create_autocmd('BufWritePre', {
          pattern = '*',
          callback = function(ev)
            _G.saved_file = ev.file
          end,
        })
        "#,
    )?;
    println!("  registered BufWritePre autocmd; firing for 'demo.txt'...");
    host.fire_autocmd("BufWritePre", "demo.txt")?;
    println!("  callback saw file => {:?}", host.eval_string("_G.saved_file")?);

    rule("M5 — Lua: vim.uv timer (tokio-backed)");
    host.exec(
        r#"
        _G.tick = 0
        local t = vim.uv.new_timer()
        t:start(15, 0, function() _G.tick = _G.tick + 1 end)
        "#,
    )?;
    println!("  scheduled 15ms one-shot timer; tick={}", host.eval_string("_G.tick")?);
    let fired = host.run_events(Duration::from_secs(2))?;
    println!("  ran event loop, invoked {fired} callback(s); tick={}", host.eval_string("_G.tick")?);

    rule("M5 — msgpack-RPC (same dispatch as Lua)");
    let req = ctrlvim_async::rpc::encode(&ctrlvim_async::rpc::Message::Request {
        msgid: 1,
        method: "ctrlvim_get_current_line".into(),
        params: vec![],
    });
    if let Some(resp) = host.handle_rpc(&req).map_err(mlua::Error::RuntimeError)? {
        if let ctrlvim_async::rpc::Message::Response { result, .. } =
            ctrlvim_async::rpc::decode(&resp).map_err(mlua::Error::RuntimeError)?
        {
            println!("  RPC ctrlvim_get_current_line => {:?}", result.as_str());
        }
    }

    // ---- M6: Vimscript + vim.fn ----
    rule("M6 — Vimscript interpreter + vim.fn");
    host.exec_vimscript(
        "function! Square(n)\n  return a:n * a:n\nendfunction\nlet g:answer = Square(7) + 1",
    )?;
    println!("  ran Vimscript; Square(7)+1 => {}", host.eval_string("vim.fn.Square(7) + 1")?);
    println!("  vim.fn.join(vim.fn.range(1,5), '+') => {}", host.eval_string("vim.fn.join(vim.fn.range(1, 5), '+')")?);

    // ---- M7: treesitter ----
    rule("M7 — treesitter query (JSON grammar)");
    host.register_ts_language("json", tree_sitter_json::LANGUAGE.into());
    host.exec(
        r#"
        local caps = vim.treesitter.query('json', '{"name": "ada", "age": 36}', '(number) @n (string) @s')
        _G.n_strings = 0
        for _, c in ipairs(caps) do if c.name == 's' then _G.n_strings = _G.n_strings + 1 end end
        "#,
    )?;
    println!("  string nodes found via treesitter query => {}", host.eval_string("_G.n_strings")?);

    // ---- M8: plugin-style integration + keymap ----
    rule("M8 — plugin-style keymap + api + treesitter");
    host.exec(
        r#"
        vim.keymap.set('n', '<leader>c', function()
          local src = vim.api.ctrlvim_get_current_line()
          _G.count = #vim.treesitter.query('json', src, '(string) @s')
        end)
        "#,
    )?;
    host.with_editor_mut(|ed| ed.load_str(r#"{"a": "x", "b": "y"}"#, Some("data.json")));
    host.trigger_keymap("n", "<leader>c")?;
    println!("  keymap ran treesitter query on buffer => {} strings", host.eval_string("_G.count")?);

    // ---- M9: windows/splits as data ----
    rule("M9 — windows/splits as a data model");
    host.feed_keys("<C-w>v<C-w>s"); // vertical then horizontal split
    let (count, rects) = host.with_editor(|ed| (ed.window_count(), ed.layout_rects(80, 24)));
    println!("  window count after <C-w>v<C-w>s => {count}");
    for (id, x, y, w, h) in rects {
        println!("    win {} rect = x:{x} y:{y} w:{w} h:{h}", id.raw());
    }

    println!("\n\x1b[1;32mAll milestone demos (M1–M9) completed.\x1b[0m");
    Ok(())
}
