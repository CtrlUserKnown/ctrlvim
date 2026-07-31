//! The project-wide find & replace panel.
//!
//! A centered floating popup in the same telescope idiom as the file browser
//! ([`super::finder`]) — rounded boxes with centered border titles, over a
//! screen that stays visible around it. It is an overlay you summon, not a mode
//! you navigate into, and the framing is what says so.
//!
//! Inside, it is split the way the operation itself is: the *inputs and the
//! list* on the left (Find, Replace, then every matching line grouped by file),
//! and the *consequences* on the right — the highlighted match in its
//! surrounding code, with the line it would become shown beneath the line it is
//! now, diff-style. Seeing the before and after together is the point: the
//! preview is produced by the same [`ctrlvim_core::ReplacePlan`] that performs
//! the edit, so reviewing it is reviewing the real change.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use std::path::Path;

use crate::app::{Action, App};
use crate::replace::{Field, ReplacePanel};
use ctrlvim_core::{display_width, ReplaceHit};
use crate::theme;

use super::{centered, row_style, selection_bar, Zones};

/// Lines of context shown either side of the match in the preview pane.
const CONTEXT: usize = 6;

pub fn screen(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let Some(panel) = &app.replace else { return };
    // Click anywhere outside the panel closes it.
    zones.push(area, Action::CloseReplace);
    if area.width < 40 || area.height < 12 {
        return;
    }

    // A centered floating popup, sized like the file browser's — the screen
    // behind it stays visible, so this reads as an overlay rather than a
    // full-screen mode you have navigated into.
    // Wider than the file browser because of the second column, but kept short
    // enough that the screen behind it frames it on every side.
    let w = area.width.saturating_sub(8).min(112);
    let h = area.height.saturating_sub(6).min(28);
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    // One background across the whole popup, so the boxes inside read as parts
    // of a single floating surface.
    f.render_widget(Block::default().style(Style::default().bg(theme::bg())), popup);
    // Swallow clicks inside the panel so they don't fall through to close.
    zones.push(popup, Action::None);

    // Body over a one-row hint bar.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(1)])
        .split(popup);

    // Left column (inputs + results) beside the preview. The column is a fixed
    // width so the preview gets every remaining cell — code needs the room more
    // than a file list does. On a narrow terminal the preview is dropped
    // entirely rather than squeezed into uselessness.
    let left_w = 52.min(rows[0].width);
    let show_preview = rows[0].width >= left_w + 30;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_preview {
            [Constraint::Length(left_w), Constraint::Min(20)]
        } else {
            [Constraint::Min(20), Constraint::Length(0)]
        })
        .split(rows[0]);

    // Grep-only mode drops the Replace box entirely — there is nothing to
    // replace — and hands the freed row to the results list.
    let (find_area, replace_area, results_area) = if panel.search_only {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(cols[0]);
        (rows[0], None, rows[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Min(3)])
            .split(cols[0]);
        (rows[0], Some(rows[1]), rows[2])
    };

    let find_title = if panel.search_only { " 󰍉 Grep " } else { " 󰍉 Find " };
    input_box(f, panel, find_area, Field::Find, find_title, &panel.find, zones);
    if let Some(area) = replace_area {
        input_box(f, panel, area, Field::Replace, " 󰛔 Replace ", &panel.replace, zones);
    }
    results(f, panel, results_area, zones);
    if show_preview {
        preview(f, app, panel, cols[1]);
    }
    hints(f, panel, rows[1]);
}

/// One of the two text fields, its border lit when focused.
fn input_box(
    f: &mut Frame,
    panel: &ReplacePanel,
    area: Rect,
    field: Field,
    title: &str,
    value: &str,
    zones: &mut Zones,
) {
    let focused = panel.focus == field;
    let accent = if field == Field::Find { theme::blue() } else { theme::green() };
    // The title is centered on the top border, matching the file browser's
    // boxes — the shared chrome is what makes the two read as one editor.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { accent } else { theme::border() }))
        .style(Style::default().bg(theme::bg()))
        .title(
            Line::from(Span::styled(
                title.to_string(),
                Style::default()
                    .fg(if focused { accent } else { theme::fg_dim() })
                    .add_modifier(Modifier::BOLD),
            ))
            .centered(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    zones.push(area, Action::FocusReplaceField(field));
    if inner.height == 0 || inner.width < 4 {
        return;
    }

    // The prompt glyph, then the text, then the cursor — the same shape the
    // file browser's `🔍` prompt uses.
    let glyph = if field == Field::Find { " 󰍉 " } else { " 󰛔 " };
    let mut spans = vec![Span::styled(glyph, Style::default().fg(accent))];
    // An empty Replace field is a real choice (delete the match), so it says so
    // rather than looking unfinished.
    if value.is_empty() {
        let hint = match field {
            Field::Find => "pattern…",
            Field::Replace => "(empty — deletes the match)",
            Field::Results => "",
        };
        spans.push(Span::styled(hint, Style::default().fg(theme::fg_dim())));
    } else {
        spans.push(Span::styled(value.to_string(), Style::default().fg(theme::fg())));
    }
    // The cursor sits only in the field that has focus.
    if focused {
        spans.push(Span::styled("▏", Style::default().fg(accent)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
    // …and so does the real terminal cursor. The glyph prefix is three cells
    // wide (` 󰍉 ` / ` 󰛔 `), and an empty field shows a hint the cursor sits in
    // front of rather than after.
    if focused {
        let right = inner.x + inner.width.saturating_sub(1);
        f.set_cursor_position(((inner.x + 3 + display_width(value) as u16).min(right), inner.y));
    }

    // Right-aligned status on each prompt, the way the browser shows `11 / 11`:
    // the live 'ignorecase' state on Find, the match count on Replace.
    let (label, color) = match field {
        Field::Find if panel.ignorecase => (" Aa ".to_string(), theme::orange()),
        Field::Find => (" Aa ".to_string(), theme::fg_dim()),
        _ if panel.error.is_some() => (" error ".to_string(), theme::red()),
        _ => (format!(" {} ", panel.match_count()), theme::fg_dim()),
    };
    let lw = label.chars().count() as u16;
    if inner.width > lw + 4 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(label, Style::default().fg(color)))),
            Rect { x: inner.x + inner.width - lw, y: inner.y, width: lw, height: 1 },
        );
    }
}

/// One rendered row of the results list.
///
/// Hits arrive in path order, so the list can be grouped under file headings
/// rather than repeating an elided path on every row — the same shape the
/// quickfix pane would want, and the reason a match reads as "line 52 of
/// ex.rs" instead of "…lvim-editor/src/ex.rs:52".
enum Row<'a> {
    File { path: &'a Path, hits: usize },
    Hit { index: usize, hit: &'a ReplaceHit },
}

/// Group `hits` into display rows: a heading per file, then its matches.
fn rows(hits: &[ReplaceHit]) -> Vec<Row<'_>> {
    let mut out = Vec::new();
    let mut current: Option<&Path> = None;
    for (index, hit) in hits.iter().enumerate() {
        if current != Some(hit.path.as_path()) {
            current = Some(hit.path.as_path());
            let n = hits.iter().filter(|h| h.path == hit.path).map(|h| h.count).sum();
            out.push(Row::File { path: hit.path.as_path(), hits: n });
        }
        out.push(Row::Hit { index, hit });
    }
    out
}

/// The matching-lines list, plus the count/error line under it.
fn results(f: &mut Frame, panel: &ReplacePanel, area: Rect, zones: &mut Zones) {
    let focused = panel.focus == Field::Results;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { theme::purple() } else { theme::border() }))
        .style(Style::default().bg(theme::bg()))
        .title(
            Line::from(Span::styled(
                " Matches ",
                Style::default()
                    .fg(if focused { theme::purple() } else { theme::fg_dim() })
                    .add_modifier(Modifier::BOLD),
            ))
            .centered(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }

    // Last row is the summary; the rest is the list.
    let list_h = inner.height.saturating_sub(1) as usize;
    let summary_y = inner.y + inner.height - 1;
    let color = if panel.error.is_some() { theme::red() } else { theme::fg_dim() };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", truncate(&panel.summary(), inner.width.saturating_sub(1) as usize)),
            Style::default().fg(color),
        ))),
        Rect { x: inner.x, y: summary_y, width: inner.width, height: 1 },
    );

    if list_h == 0 || panel.hits.is_empty() {
        return;
    }
    let all = rows(&panel.hits);
    let sel = panel.selected.min(panel.hits.len() - 1);
    // Scroll so the selected *hit* stays visible; the window is measured in
    // display rows, which the file headings inflate.
    let sel_row = all
        .iter()
        .position(|r| matches!(r, Row::Hit { index, .. } if *index == sel))
        .unwrap_or(0);
    let start = if sel_row >= list_h { sel_row + 1 - list_h } else { 0 };
    // Right-aligned line numbers in a gutter sized to the widest one on screen.
    let gutter = all
        .iter()
        .skip(start)
        .take(list_h)
        .filter_map(|r| match r {
            Row::Hit { hit, .. } => Some((hit.line + 1).to_string().len()),
            Row::File { .. } => None,
        })
        .max()
        .unwrap_or(1);

    for (row, item) in all.iter().skip(start).take(list_h).enumerate() {
        let rect = Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 };
        let width = inner.width as usize;
        match item {
            Row::File { path, hits } => {
                f.render_widget(Block::default().style(row_style(false)), rect);
                // The count sits right-aligned against the heading.
                let count = format!(" {hits} ");
                let name = format!("  {}", path.display());
                let room = width.saturating_sub(count.chars().count());
                let name = elide_left(&name, room);
                let pad = room.saturating_sub(name.chars().count());
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            name,
                            Style::default().fg(theme::blue()).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(count, Style::default().fg(theme::fg_dim())),
                    ]))
                    .style(row_style(false)),
                    rect,
                );
            }
            Row::Hit { index, hit } => {
                let selected = *index == sel;
                f.render_widget(Block::default().style(row_style(selected)), rect);
                let mut spans = vec![
                    selection_bar(selected, theme::purple()),
                    Span::styled(
                        format!("{:>w$}  ", hit.line + 1, w = gutter),
                        Style::default().fg(theme::fg_dim()),
                    ),
                ];
                let used: usize = spans.iter().map(|s| s.width()).sum();
                spans.push(Span::styled(
                    truncate(hit.text.trim(), width.saturating_sub(used)),
                    Style::default().fg(if selected { theme::fg() } else { theme::fg_muted() }),
                ));
                f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), rect);
                zones.push(rect, Action::SelectReplaceHit(*index));
            }
        }
    }
}

/// The highlighted hit in context: surrounding lines as they are, and the match
/// line shown twice — `-` what it is, `+` what it becomes.
fn preview(f: &mut Frame, app: &App, panel: &ReplacePanel, area: Rect) {
    let hit = panel.current();
    let title = match hit {
        Some(h) => format!(" {}:{} ", h.path.display(), h.line + 1),
        None => " Preview ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg()))
        .title(
            Line::from(Span::styled(
                truncate(&title, area.width.saturating_sub(4) as usize),
                Style::default().fg(theme::fg_dim()),
            ))
            .centered(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 8 {
        return;
    }

    let Some(hit) = hit else {
        let msg = if panel.error.is_some() { "" } else { "  no match selected" };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(theme::fg_dim())))),
            inner,
        );
        return;
    };

    // The same source the search read, so the context can't disagree with it.
    let Some(lines) = app.replace_preview_lines(&hit.path) else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  (cannot read file)",
                Style::default().fg(theme::red()),
            ))),
            inner,
        );
        return;
    };

    let first = hit.line.saturating_sub(CONTEXT);
    let last = (hit.line + CONTEXT).min(lines.len().saturating_sub(1));
    // Widest gutter in the window, so the code column doesn't jitter.
    let gutter = (last + 1).to_string().len();
    let text_w = (inner.width as usize).saturating_sub(gutter + 4);
    // Scroll far enough right that the match itself is on screen — a diff of
    // two lines both cut off before the change shows nothing. Every line in the
    // pane shares the offset so the before/after and their context stay aligned.
    let offset = h_offset(hit.col, text_w);

    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    for n in first..=last {
        if y >= bottom {
            break;
        }
        let num = Span::styled(
            format!("{:>w$} ", n + 1, w = gutter),
            Style::default().fg(theme::fg_dim()),
        );
        if n == hit.line && panel.search_only {
            // A plain match: highlight the line itself, no before/after diff
            // — there is nothing for it to become.
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    num.clone(),
                    Span::styled("▸ ", Style::default().fg(theme::orange())),
                    Span::styled(
                        h_window(&hit.text, offset, text_w),
                        Style::default().fg(theme::orange()).add_modifier(Modifier::BOLD),
                    ),
                ])),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
        } else if n == hit.line {
            // The match line, as a two-row diff.
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    num.clone(),
                    Span::styled("- ", Style::default().fg(theme::red())),
                    Span::styled(h_window(&hit.text, offset, text_w), Style::default().fg(theme::red())),
                ])),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
            y += 1;
            if y >= bottom {
                break;
            }
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ".repeat(gutter + 1), Style::default()),
                    Span::styled("+ ", Style::default().fg(theme::green())),
                    Span::styled(
                        h_window(&hit.preview, offset, text_w),
                        Style::default().fg(theme::green()).add_modifier(Modifier::BOLD),
                    ),
                ])),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
        } else {
            let line = lines.get(n).map(String::as_str).unwrap_or("");
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    num,
                    Span::raw("  "),
                    Span::styled(h_window(line, offset, text_w), Style::default().fg(theme::fg_muted())),
                ])),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
        }
        y += 1;
    }
}

/// The one-row key hint bar, which names what the *focused* field's keys do.
fn hints(f: &mut Frame, panel: &ReplacePanel, area: Rect) {
    let keys: &[(&str, &str)] = if panel.search_only {
        if panel.focus == Field::Results {
            &[("j/k", "move"), ("⏎", "open"), ("⇥", "field"), ("^i", "case"), ("esc", "close")]
        } else {
            &[("⇥", "next field"), ("⏎", "open"), ("↑↓", "matches"), ("^i", "case"), ("esc", "close")]
        }
    } else {
        match panel.focus {
            Field::Results => &[
                ("j/k", "move"),
                ("y", "replace one"),
                ("Y", "replace all"),
                ("⏎", "open"),
                ("⇥", "field"),
                ("^i", "case"),
                ("esc", "close"),
            ],
            _ => &[
                ("⇥", "next field"),
                ("⏎", "replace all"),
                ("^a", "replace all"),
                ("^i", "case"),
                ("↑↓", "matches"),
                ("esc", "close"),
            ],
        }
    };
    let mut spans = Vec::new();
    for (key, what) in keys {
        spans.push(Span::styled(
            format!("  {key} "),
            Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(what.to_string(), Style::default().fg(theme::fg_dim())));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::bg())),
        area,
    );
}

/// How far right the preview must scroll for char column `col` to be visible
/// in `width` columns. Zero whenever the match already fits, so short lines
/// aren't shifted for no reason; otherwise the match is parked a third of the
/// way in, leaving context on both sides of it.
fn h_offset(col: usize, width: usize) -> usize {
    if width == 0 || col + 4 < width {
        0
    } else {
        col.saturating_sub(width / 3)
    }
}

/// The slice of `line` starting `offset` chars in and fitting `width` columns,
/// with `…` marking each end that was cut.
fn h_window(line: &str, offset: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = line.chars().collect();
    if offset >= chars.len() {
        return String::new();
    }
    let (mut out, avail) = if offset > 0 {
        (String::from("…"), width - 1)
    } else {
        (String::new(), width)
    };
    let rest = &chars[offset..];
    if rest.len() > avail {
        out.extend(rest.iter().take(avail.saturating_sub(1)));
        out.push('…');
    } else {
        out.extend(rest.iter());
    }
    out
}

/// Cut `s` to `max` display columns, marking the cut with `…`.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Like [`truncate`] but keeps the *end* — for paths, where the file name
/// identifies the row better than the leading directories do.
fn elide_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max == 0 {
        return s.to_string();
    }
    let mut out = String::from("…");
    out.extend(s.chars().skip(n - max.saturating_sub(1)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, line: usize, count: usize) -> ReplaceHit {
        ReplaceHit {
            path: std::path::PathBuf::from(path),
            line,
            col: 0,
            text: "foo".into(),
            preview: "bar".into(),
            count,
        }
    }

    #[test]
    fn rows_put_one_heading_above_each_files_matches() {
        let hits = vec![
            hit("a.rs", 0, 2),
            hit("a.rs", 7, 1),
            hit("b.rs", 3, 1),
        ];
        let rows = rows(&hits);
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                Row::File { path, hits } => format!("# {} ({hits})", path.display()),
                Row::Hit { index, hit } => format!("  {index}:{}", hit.line),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["# a.rs (3)", "  0:0", "  1:7", "# b.rs (1)", "  2:3"],
            "the heading counts matches, not lines"
        );
    }

    #[test]
    fn hit_rows_keep_their_index_into_the_flat_list() {
        // The heading rows shift display positions, but selection is by hit
        // index — a mismatch would select the wrong match.
        let hits = vec![hit("a.rs", 0, 1), hit("b.rs", 1, 1), hit("c.rs", 2, 1)];
        let indices: Vec<usize> = rows(&hits)
            .iter()
            .filter_map(|r| match r {
                Row::Hit { index, .. } => Some(*index),
                Row::File { .. } => None,
            })
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(rows(&hits).len(), 6, "three headings interleaved with three hits");
    }

    #[test]
    fn an_empty_result_set_has_no_rows() {
        assert!(rows(&[]).is_empty());
    }

    #[test]
    fn a_match_that_fits_is_not_scrolled() {
        assert_eq!(h_offset(0, 40), 0);
        assert_eq!(h_offset(20, 40), 0);
        assert_eq!(h_window("let x = 1;", 0, 40), "let x = 1;");
    }

    #[test]
    fn a_match_off_the_right_edge_scrolls_into_view_with_context() {
        let line = "let a = 1; let b = 2; let c = 3; let target = 4;";
        let col = line.find("target").unwrap(); // 37
        let width = 20;
        let offset = h_offset(col, width);
        assert!(offset > 0, "a match at {col} cannot fit in {width} columns unscrolled");
        let out = h_window(line, offset, width);
        assert!(out.contains("target"), "the match must be visible: {out:?}");
        assert!(out.starts_with('…'), "the cut is marked: {out:?}");
        assert!(out.chars().count() <= width, "overflowed the pane: {out:?}");
    }

    #[test]
    fn before_and_after_lines_share_one_offset_so_the_diff_lines_up() {
        // The same offset applied to both halves keeps the changed text in the
        // same screen column, which is what makes the diff readable.
        let before = "call(a, b, c, some_very_long_thing, old_name);";
        let after = "call(a, b, c, some_very_long_thing, new_name);";
        let col = before.find("old_name").unwrap();
        let (offset, width) = (h_offset(col, 24), 24);
        let (b, a) = (h_window(before, offset, width), h_window(after, offset, width));
        assert!(b.contains("old_name"), "{b:?}");
        assert!(a.contains("new_name"), "{a:?}");
        assert_eq!(
            b.find("old_name"),
            a.find("new_name"),
            "the change should sit in the same column:\n{b}\n{a}"
        );
    }

    #[test]
    fn windowing_past_the_end_of_a_short_line_is_empty_not_a_panic() {
        assert_eq!(h_window("ab", 9, 20), "");
        assert_eq!(h_window("", 0, 20), "");
        assert_eq!(h_window("abc", 0, 0), "");
        // A context line shorter than the offset (common beside a long match).
        assert_eq!(h_window("}", 30, 20), "");
    }

    #[test]
    fn truncate_marks_the_cut_and_leaves_short_text_alone() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn eliding_a_path_keeps_the_file_name() {
        assert_eq!(elide_left("a/b.rs", 20), "a/b.rs");
        let out = elide_left("crates/ctrlvim-tui/src/ui/replace.rs", 14);
        assert!(out.ends_with("replace.rs"), "kept the tail: {out}");
        assert!(out.starts_with('…'));
        assert_eq!(out.chars().count(), 14);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("🌍🌍🌍", 3), "🌍🌍🌍");
        assert_eq!(truncate("🌍🌍🌍🌍", 3), "🌍🌍…");
    }
}
