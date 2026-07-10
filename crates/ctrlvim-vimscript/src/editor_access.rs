//! Bridge the interpreter's [`EditorAccess`] onto the real `ctrlvim_editor::Editor`.

use crate::interp::EditorAccess;
use ctrlvim_editor::Editor;
use ctrlvim_types::Position;

impl EditorAccess for Editor {
    fn line_count(&self) -> i64 {
        self.cur_buffer().text.line_count() as i64
    }

    fn get_line(&self, lnum: i64) -> String {
        let idx = (lnum - 1).max(0) as usize;
        self.cur_buffer().text.line(idx).unwrap_or_default()
    }

    fn set_line(&mut self, lnum: i64, text: &str) {
        let idx = (lnum - 1).max(0) as usize;
        self.cur_buffer_mut().text.replace_line(idx, text);
    }

    fn cursor(&self) -> (i64, i64) {
        let Position { line, col } = self.cursor();
        (line as i64 + 1, col as i64 + 1)
    }
}
