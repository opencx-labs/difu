//! Text selection uses terminal columns, including wide Unicode characters.
use super::*;
use unicode_width::UnicodeWidthChar;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    region: usize,
    row: usize,
    column: usize,
}
#[derive(Default)]
pub(super) struct Selection {
    documents: HashMap<usize, Vec<Line<'static>>>,
    visible: Vec<(Rect, usize, usize)>,
    anchor: Option<Point>,
    end: Option<Point>,
    dragging: bool,
    click: Option<Action>,
}
impl Selection {
    pub fn frame(&mut self) {
        self.visible.clear();
    }
    pub fn clear(&mut self) {
        self.anchor = None;
        self.end = None;
        self.dragging = false;
        self.click = None;
    }
    pub fn register(&mut self, region: usize, area: Rect, offset: usize, lines: &[Line<'static>]) {
        self.visible.push((area, region, offset));
        self.documents.insert(region, lines.to_vec());
    }
    fn point(&self, x: u16, y: u16) -> Option<Point> {
        self.visible
            .iter()
            .rev()
            .find(|(r, _, _)| r.contains((x, y).into()))
            .map(|(r, region, offset)| Point {
                region: *region,
                row: offset + usize::from(y - r.y),
                column: usize::from(x - r.x),
            })
    }
    fn range(&self) -> Option<(Point, Point)> {
        let a = self.anchor?;
        let b = self.end?;
        (a != b).then_some((a.min(b), a.max(b)))
    }
    pub fn text(&self) -> Option<String> {
        let (start, end) = self.range()?;
        let mut output = Vec::new();
        for region in start.region..=end.region {
            let Some(lines) = self.documents.get(&region) else {
                continue;
            };
            for (row, line) in lines.iter().enumerate() {
                if (region, row) < (start.region, start.row)
                    || (region, row) > (end.region, end.row)
                {
                    continue;
                }
                let left = if (region, row) == (start.region, start.row) {
                    start.column
                } else {
                    0
                };
                let right = if (region, row) == (end.region, end.row) {
                    end.column
                } else {
                    usize::MAX
                };
                let mut x = 0usize;
                let text = line
                    .to_string()
                    .chars()
                    .filter(|c| {
                        let next = x.saturating_add(c.width().unwrap_or(0));
                        let selected = x < right && next > left;
                        x = next;
                        selected
                    })
                    .collect::<String>();
                output.push(text.trim_end().to_owned());
            }
        }
        Some(output.join("\n"))
    }
    pub fn highlight(&self, frame: &mut Frame) {
        let Some((start, end)) = self.range() else {
            return;
        };
        for (area, region, offset) in &self.visible {
            for y in area.y..area.bottom() {
                let row = offset + usize::from(y - area.y);
                for x in area.x..area.right() {
                    let p = Point {
                        region: *region,
                        row,
                        column: usize::from(x - area.x),
                    };
                    if start <= p
                        && p < end
                        && let Some(cell) = frame.buffer_mut().cell_mut((x, y))
                    {
                        cell.set_style(Style::default().bg(ACCENT).fg(crate::ui::INK));
                    }
                }
            }
        }
    }
}
impl Ui {
    pub fn copy_chat_selection(&mut self) -> bool {
        if let Some(text) = self.focused_editor().and_then(Editor::selected_text) {
            self.clipboard = Some(text);
            return true;
        }
        if self.modal.is_some() {
            return false;
        }
        if let Some(text) = self.text_selection.text() {
            self.clipboard = Some(text);
            true
        } else {
            false
        }
    }
    pub(super) fn selection_mouse(&mut self, event: MouseEvent) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let point = self.text_selection.point(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if point.is_some() => {
                self.text_selection.anchor = point;
                self.text_selection.end = point;
                self.text_selection.dragging = false;
                self.text_selection.click = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains((event.column, event.row).into()))
                    .map(|(_, action)| action.clone());
                if let Some(p) = self
                    .selected
                    .as_ref()
                    .and_then(|id| self.positions.get_mut(id))
                {
                    p.follow = false;
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.text_selection.anchor.is_some() => {
                self.text_selection.dragging = true;
                if let Some(point) = point {
                    self.text_selection.end = Some(point);
                } else if let Some((area, region, offset)) = self
                    .text_selection
                    .visible
                    .iter()
                    .find(|(_, region, _)| *region == 1)
                    .copied()
                    && self.modal.is_none()
                    && area.width > 0
                    && area.height > 0
                {
                    let down = event.row >= area.bottom();
                    let up = event.row < area.y;
                    if up || down {
                        self.focus = Focus::Conversation;
                        self.scroll(if down { 1 } else { -1 }, false);
                        self.text_selection.end = Some(Point {
                            region,
                            row: offset
                                + if down {
                                    usize::from(area.height) - 1
                                } else {
                                    0
                                },
                            column: usize::from(
                                event.column.saturating_sub(area.x).min(area.width),
                            ),
                        });
                    }
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.text_selection.anchor.is_some() => {
                if !self.text_selection.dragging {
                    let click = self.text_selection.click.take();
                    self.text_selection.clear();
                    if let Some(action) = click {
                        self.action(action);
                    }
                }
                true
            }
            MouseEventKind::Down(_) => {
                self.text_selection.clear();
                false
            }
            _ => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_portions_across_messages_with_wide_characters() {
        let mut s = Selection::default();
        s.register(
            1,
            Rect::new(10, 5, 30, 4),
            0,
            &[
                Line::from("hello 界!"),
                Line::from(""),
                Line::from("next message"),
            ],
        );
        s.anchor = s.point(16, 5);
        s.end = s.point(14, 7);
        assert_eq!(s.text().as_deref(), Some("界!\n\nnext"));
        std::mem::swap(&mut s.anchor, &mut s.end);
        assert_eq!(s.text().as_deref(), Some("界!\n\nnext"));
    }
}
