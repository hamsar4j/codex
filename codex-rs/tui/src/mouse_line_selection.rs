use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

use crate::insert_history::HistoryRow;
use crate::style::text_selection_style;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MouseLineSelectionOutcome {
    None,
    Redraw,
    Copy(String),
}

#[derive(Debug, Default)]
pub(crate) struct MouseLineSelection {
    active: Option<ActiveSelection>,
}

pub(crate) struct MouseLineSelectionSource<'a> {
    area: Rect,
    history_rows: &'a [HistoryRow],
    preferred_area: Option<Rect>,
    viewport_buffer: &'a Buffer,
}

#[derive(Debug, Clone, Copy)]
struct ActiveSelection {
    area: Rect,
    anchor: SelectionPoint,
    current: SelectionPoint,
    dragged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionPoint {
    pub(crate) row: u16,
    pub(crate) column: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionRange {
    start: SelectionPoint,
    end: SelectionPoint,
}

impl<'a> MouseLineSelectionSource<'a> {
    pub(crate) fn new_with_preferred_area(
        viewport_buffer: &'a Buffer,
        history_rows: &'a [HistoryRow],
        preferred_area: Option<Rect>,
    ) -> Self {
        let history_row_count = history_rows
            .len()
            .min(viewport_buffer.area.top() as usize)
            .min(u16::MAX as usize) as u16;
        let history_rows = &history_rows[history_rows
            .len()
            .saturating_sub(history_row_count as usize)..];
        let top = viewport_buffer.area.top().saturating_sub(history_row_count);
        let height = viewport_buffer
            .area
            .height
            .saturating_add(history_row_count);

        Self {
            area: Rect::new(
                viewport_buffer.area.x,
                top,
                viewport_buffer.area.width,
                height,
            ),
            history_rows,
            preferred_area: preferred_area.filter(|area| !area.is_empty()),
            viewport_buffer,
        }
    }

    fn selection_area_for_event(&self, event: MouseEvent) -> Option<Rect> {
        self.preferred_area
            .filter(|area| point_in_area(event, *area).is_some())
    }

    fn line_text(&self, row: u16, area: Rect) -> Option<String> {
        row_in_area(row, area)?;

        if row < self.viewport_buffer.area.top() {
            let index = (row - self.area.top()) as usize;
            return self
                .history_rows
                .get(index)
                .map(|row| row.plain_text.to_string());
        }

        buffer_line_text(self.viewport_buffer, row, area)
    }
}

impl SelectionRange {
    fn new(anchor: SelectionPoint, current: SelectionPoint) -> Self {
        if (anchor.row, anchor.column) <= (current.row, current.column) {
            Self {
                start: anchor,
                end: current,
            }
        } else {
            Self {
                start: current,
                end: anchor,
            }
        }
    }

    pub(crate) fn columns_for_row(self, row: u16, area: Rect) -> Option<(u16, u16)> {
        if row < self.start.row || row > self.end.row || area.width == 0 {
            return None;
        }

        let left = area.left();
        let right = area.right();
        let (start, end) = if self.start.row == self.end.row {
            (self.start.column, self.end.column.saturating_add(1))
        } else if row == self.start.row {
            (self.start.column, right)
        } else if row == self.end.row {
            (left, self.end.column.saturating_add(1))
        } else {
            (left, right)
        };

        let start = start.clamp(left, right);
        let end = end.clamp(left, right);
        (start < end).then_some((start, end))
    }
}

impl MouseLineSelection {
    pub(crate) fn active_range(&self) -> Option<SelectionRange> {
        let active = self.active?;
        Some(SelectionRange::new(active.anchor, active.current))
    }

    pub(crate) fn handle_mouse_event(
        &mut self,
        event: MouseEvent,
        source: MouseLineSelectionSource<'_>,
    ) -> MouseLineSelectionOutcome {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let area = source
                    .selection_area_for_event(event)
                    .unwrap_or(source.area);
                let Some(point) = point_in_area(event, area) else {
                    return if self.active.take().is_some() {
                        MouseLineSelectionOutcome::Redraw
                    } else {
                        MouseLineSelectionOutcome::None
                    };
                };
                self.active = Some(ActiveSelection {
                    area,
                    anchor: point,
                    current: point,
                    dragged: false,
                });
                MouseLineSelectionOutcome::Redraw
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(active) = self.active.as_mut() else {
                    return MouseLineSelectionOutcome::None;
                };
                let Some(point) = point_clamped_to_area(event, active.area) else {
                    return MouseLineSelectionOutcome::None;
                };
                active.dragged = true;
                if active.current == point {
                    MouseLineSelectionOutcome::None
                } else {
                    active.current = point;
                    MouseLineSelectionOutcome::Redraw
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(mut active) = self.active.take() else {
                    return MouseLineSelectionOutcome::None;
                };
                if let Some(point) = point_clamped_to_area(event, active.area) {
                    active.dragged |= point != active.anchor;
                    active.current = point;
                }
                if !active.dragged {
                    return MouseLineSelectionOutcome::Redraw;
                }
                selected_text(&source, active)
                    .map(MouseLineSelectionOutcome::Copy)
                    .unwrap_or(MouseLineSelectionOutcome::Redraw)
            }
            _ => {
                if self.active.take().is_some() {
                    MouseLineSelectionOutcome::Redraw
                } else {
                    MouseLineSelectionOutcome::None
                }
            }
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        let Some(active) = self.active else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let area = intersect_rect(area, active.area);
        if area.is_empty() {
            return;
        }
        let range = SelectionRange::new(active.anchor, active.current);
        let start_y = range.start.row.max(area.top());
        let end_y = range.end.row.min(area.bottom().saturating_sub(1));
        if start_y > end_y {
            return;
        }
        let style = text_selection_style();
        for y in start_y..=end_y {
            if let Some((start, end)) = range.columns_for_row(y, area) {
                buf.set_style(Rect::new(start, y, end - start, 1), style);
            }
        }
    }
}

fn buffer_line_text(buffer: &Buffer, row: u16, area: Rect) -> Option<String> {
    row_in_area(row, buffer.area)?;

    let mut line = String::new();
    for x in area.left()..area.right() {
        line.push_str(buffer[(x, row)].symbol());
    }
    Some(line)
}

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    if left >= right || top >= bottom {
        Rect::ZERO
    } else {
        Rect::new(left, top, right - left, bottom - top)
    }
}

fn point_in_area(event: MouseEvent, area: Rect) -> Option<SelectionPoint> {
    if area.width == 0
        || area.height == 0
        || event.row < area.top()
        || event.row >= area.bottom()
        || event.column < area.left()
        || event.column >= area.right()
    {
        None
    } else {
        Some(SelectionPoint {
            row: event.row,
            column: event.column,
        })
    }
}

fn point_clamped_to_area(event: MouseEvent, area: Rect) -> Option<SelectionPoint> {
    if area.width == 0 || area.height == 0 {
        None
    } else {
        Some(SelectionPoint {
            row: event.row.clamp(area.top(), area.bottom().saturating_sub(1)),
            column: event
                .column
                .clamp(area.left(), area.right().saturating_sub(1)),
        })
    }
}

fn row_in_area(row: u16, area: Rect) -> Option<u16> {
    if area.width == 0 || area.height == 0 || row < area.top() || row >= area.bottom() {
        None
    } else {
        Some(row)
    }
}

fn selected_text(source: &MouseLineSelectionSource<'_>, active: ActiveSelection) -> Option<String> {
    let range = SelectionRange::new(active.anchor, active.current);
    let start_y = range.start.row;
    let end_y = range.end.row;
    let mut lines = Vec::with_capacity((end_y - start_y + 1) as usize);
    for y in start_y..=end_y {
        let (start, end) = range.columns_for_row(y, active.area)?;
        let mut line = slice_text_columns(
            &source.line_text(y, active.area)?,
            start.saturating_sub(active.area.left()) as usize,
            end.saturating_sub(active.area.left()) as usize,
        );
        if end >= active.area.right() {
            line = line.trim_end().to_string();
        }
        lines.push(line);
    }
    let text = lines.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn slice_text_columns(text: &str, start: usize, end: usize) -> String {
    let mut selected = String::new();
    let mut column = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let next_column = column + width;
        if next_column > start && column < end {
            selected.push(ch);
        }
        column = next_column;
        if column >= end {
            break;
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Cell;
    use ratatui::style::Modifier;
    use ratatui::text::Line;

    fn mouse(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn buffer_with_lines(lines: &[&str]) -> Buffer {
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
            .max(1) as u16;
        let height = lines.len() as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        for (y, line) in lines.iter().enumerate() {
            for (x, ch) in line.chars().enumerate() {
                buffer[(x as u16, y as u16)].set_symbol(&ch.to_string());
            }
        }
        buffer
    }

    fn history_rows(lines: &[&str]) -> Vec<HistoryRow> {
        lines
            .iter()
            .map(|line| HistoryRow {
                line: Line::from((*line).to_string()),
                plain_text: (*line).to_string(),
            })
            .collect()
    }

    fn source<'a>(
        buffer: &'a Buffer,
        history_rows: &'a [HistoryRow],
    ) -> MouseLineSelectionSource<'a> {
        MouseLineSelectionSource::new_with_preferred_area(buffer, history_rows, None)
    }

    #[test]
    fn drag_release_copies_selected_lines() {
        let buffer = buffer_with_lines(&["alpha", "beta", "gamma"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 0
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Redraw
        );
        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    /*row*/ 2,
                    /*column*/ 4
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Redraw
        );
        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 2,
                    /*column*/ 4
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("alpha\nbeta\ngamma".to_string())
        );
    }

    #[test]
    fn drag_release_copies_partial_single_line_selection() {
        let buffer = buffer_with_lines(&["abcdef"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 1,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 3,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 3
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("bcd".to_string())
        );
    }

    #[test]
    fn release_at_different_column_counts_as_selection() {
        let buffer = buffer_with_lines(&["abcdef"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 1,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 3
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("bcd".to_string())
        );
    }

    #[test]
    fn preferred_area_selects_buffer_text_relative_to_area() {
        let buffer = buffer_with_lines(&["xxabcdefxx"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();
        let preferred_area = Rect::new(2, 0, 6, 1);
        let source = |buffer, rows| {
            MouseLineSelectionSource::new_with_preferred_area(buffer, rows, Some(preferred_area))
        };

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 3,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 5
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("bcd".to_string())
        );
    }

    #[test]
    fn drag_release_copies_partial_multi_line_selection() {
        let buffer = buffer_with_lines(&["alpha", "beta", "gamma"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 2,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 2,
                /*column*/ 1,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 2,
                    /*column*/ 1
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("pha\nbeta\nga".to_string())
        );
    }

    #[test]
    fn release_on_blank_selection_only_redraws() {
        let buffer = buffer_with_lines(&["", ""]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 0,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 0,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 0
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Redraw
        );
    }

    #[test]
    fn click_without_drag_does_not_copy() {
        let buffer = buffer_with_lines(&["alpha"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 0,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 0,
                    /*column*/ 0
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Redraw
        );
    }

    #[test]
    fn drag_release_copies_history_lines_above_viewport() {
        let mut buffer = buffer_with_lines(&["live         "]);
        buffer.area.y = 2;
        let history_rows = history_rows(&["first output", "second output"]);
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 0,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 1,
                /*column*/ 12,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 1,
                    /*column*/ 12
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("first output\nsecond output".to_string())
        );
    }

    #[test]
    fn drag_release_copies_across_history_and_viewport() {
        let mut buffer = buffer_with_lines(&["live  "]);
        buffer.area.y = 1;
        let history_rows = history_rows(&["output"]);
        let mut selection = MouseLineSelection::default();

        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 0,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 1,
                /*column*/ 3,
            ),
            source(&buffer, &history_rows),
        );

        assert_eq!(
            selection.handle_mouse_event(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    /*row*/ 1,
                    /*column*/ 3
                ),
                source(&buffer, &history_rows)
            ),
            MouseLineSelectionOutcome::Copy("output\nlive".to_string())
        );
    }

    #[test]
    fn render_highlights_active_columns() {
        let mut buffer = buffer_with_lines(&["one", "two", "three"]);
        let history_rows = Vec::new();
        let mut selection = MouseLineSelection::default();
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 1,
                /*column*/ 1,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 2,
                /*column*/ 3,
            ),
            source(&buffer, &history_rows),
        );

        let area = buffer.area;
        selection.render(area, &mut buffer);
        insta::assert_snapshot!(row_selection_cells(&buffer), @r"
.....
.####
####.
");
    }

    #[test]
    fn render_highlights_preferred_area_columns() {
        let mut buffer = buffer_with_lines(&["xxabcdefxx"]);
        let history_rows = Vec::new();
        let preferred_area = Rect::new(2, 0, 6, 1);
        let source = |buffer, rows| {
            MouseLineSelectionSource::new_with_preferred_area(buffer, rows, Some(preferred_area))
        };
        let mut selection = MouseLineSelection::default();
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 3,
            ),
            source(&buffer, &history_rows),
        );
        selection.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                /*row*/ 0,
                /*column*/ 5,
            ),
            source(&buffer, &history_rows),
        );

        let area = buffer.area;
        selection.render(area, &mut buffer);
        insta::assert_snapshot!(row_selection_cells(&buffer), @r"
...###....
");
    }

    fn row_selection_cells(buffer: &Buffer) -> String {
        let mut output = String::new();
        for y in buffer.area.top()..buffer.area.bottom() {
            for x in buffer.area.left()..buffer.area.right() {
                let Cell { modifier, .. } = &buffer[(x, y)];
                output.push(if modifier.contains(Modifier::REVERSED) {
                    '#'
                } else {
                    '.'
                });
            }
            output.push('\n');
        }
        output
    }
}
