//! Text selection uses terminal columns, including wide Unicode characters.
use super::*;
use unicode_width::UnicodeWidthChar;

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    region: usize,
    row: usize,
    column: usize,
}
#[derive(Default)]
pub(super) struct Selection {
    documents: HashMap<usize, std::collections::BTreeMap<usize, Line<'static>>>,
    visible: Vec<(Rect, usize, usize)>,
    anchor: Option<Point>,
    end: Option<Point>,
    dragging: bool,
    click: Option<Action>,
    last_click: Option<(Point, Instant)>,
    pending_click: Option<(Action, Instant)>,
}
impl Selection {
    pub fn frame(&mut self) {
        self.visible.clear();
    }
    pub fn clear(&mut self) {
        self.documents.clear();
        self.anchor = None;
        self.end = None;
        self.dragging = false;
        self.click = None;
        self.last_click = None;
        self.pending_click = None;
    }
    fn press(&mut self, point: Point, action: Option<Action>, now: Instant) {
        let double = self.last_click.is_some_and(|(last, at)| {
            last.region == point.region
                && last.row == point.row
                && last.column.abs_diff(point.column) <= 1
                && now.saturating_duration_since(at) < DOUBLE_CLICK_WINDOW
        });
        self.pending_click = None;
        self.anchor = Some(point);
        self.end = Some(point);
        self.dragging = double;
        self.click = if double { None } else { action };
        self.last_click = None;
        if double {
            let width = self
                .documents
                .get(&point.region)
                .and_then(|rows| rows.get(&point.row))
                .map_or(0, Line::width);
            self.anchor = Some(Point { column: 0, ..point });
            self.end = Some(Point {
                column: width,
                ..point
            });
        }
    }
    fn release(&mut self, now: Instant) {
        if !self.dragging {
            self.last_click = self.anchor.map(|point| (point, now));
            self.pending_click = self.click.take().map(|action| (action, now));
            self.anchor = None;
            self.end = None;
        }
    }
    fn ready_click(&mut self, now: Instant) -> Option<Action> {
        if self
            .pending_click
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= DOUBLE_CLICK_WINDOW)
        {
            self.last_click = None;
            self.pending_click.take().map(|(action, _)| action)
        } else {
            None
        }
    }
    #[cfg(test)]
    pub fn register(&mut self, region: usize, area: Rect, offset: usize, lines: &[Line<'static>]) {
        self.visible.push((area, region, offset));
        self.documents
            .insert(region, lines.iter().cloned().enumerate().collect());
    }
    pub fn rebase(&mut self, region: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        for point in [&mut self.anchor, &mut self.end].into_iter().flatten() {
            if point.region == region {
                point.row = point.row.saturating_add_signed(delta);
            }
        }
        if let Some((point, _)) = &mut self.last_click
            && point.region == region
        {
            point.row = point.row.saturating_add_signed(delta);
        }
        if let Some(rows) = self.documents.get_mut(&region) {
            *rows = std::mem::take(rows)
                .into_iter()
                .map(|(row, line)| (row.saturating_add_signed(delta), line))
                .collect();
        }
    }
    pub fn register_window(
        &mut self,
        region: usize,
        area: Rect,
        offset: usize,
        start: usize,
        lines: &[Line<'static>],
    ) {
        self.visible.push((area, region, offset));
        let rows = self.documents.entry(region).or_default();
        if self.anchor.is_none() {
            rows.clear();
        }
        rows.extend(
            lines
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, line)| (start + i, line)),
        );
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
            for (&row, line) in lines {
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
    pub(super) fn tick_selection_click(&mut self) {
        if let Some(action) = self.text_selection.ready_click(Instant::now()) {
            self.action(action);
        }
    }
    pub fn copy_chat_selection(&mut self) -> bool {
        if let Some(text) = self.focused_editor().and_then(Editor::selected_text) {
            self.clipboard = Some(text);
            return true;
        }
        if self.modal.is_some() && !matches!(self.modal, Some(Modal::Transcript { .. })) {
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
        if self.modal.is_some() && !matches!(self.modal, Some(Modal::Transcript { .. })) {
            return false;
        }
        let point = self.text_selection.point(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if point.is_some() => {
                let action = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains((event.column, event.row).into()))
                    .map(|(_, action)| action.clone());
                if let Some(point) = point {
                    self.text_selection.press(point, action, Instant::now());
                }
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
                self.text_selection.release(Instant::now());
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
    fn link_clicks_accept_plain_and_command_modified_mouse_events() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().into(),
            },
            &Config::default(),
        );
        let area = Rect::new(10, 5, 20, 1);
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SUPER] {
            ui.text_selection
                .register(1, area, 0, &[Line::from("invoice")]);
            ui.hits = vec![(area, Action::Link("https://example.com/invoice".into()))];
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                assert!(ui.selection_mouse(MouseEvent {
                    kind,
                    column: 12,
                    row: 5,
                    modifiers
                }));
            }
            assert!(
                matches!(ui.text_selection.ready_click(Instant::now() + Duration::from_secs(1)), Some(Action::Link(url)) if url == "https://example.com/invoice")
            );
        }
        Ok(())
    }
    #[test]
    fn double_click_selects_the_displayed_row_and_cancels_link_activation() {
        let mut selection = Selection::default();
        selection.register_window(
            1,
            Rect::new(10, 5, 30, 2),
            8,
            8,
            &[Line::from("hello 界!"), Line::from("next row")],
        );
        let point = Point {
            region: 1,
            row: 8,
            column: 6,
        };
        let now = Instant::now();
        selection.press(point, Some(Action::Link("https://example.com".into())), now);
        selection.release(now);
        assert!(
            selection
                .ready_click(now + Duration::from_millis(100))
                .is_none()
        );
        selection.press(
            point,
            Some(Action::Link("https://example.com".into())),
            now + Duration::from_millis(150),
        );
        selection.release(now + Duration::from_millis(200));
        assert_eq!(selection.text().as_deref(), Some("hello 界!"));
        assert!(
            selection
                .ready_click(now + Duration::from_secs(1))
                .is_none()
        );
        assert_eq!(
            selection.range().map(|(a, b)| (a.column, b.column)),
            Some((0, 9))
        );
    }
    #[test]
    fn single_click_opens_once_and_dragging_never_activates_a_link() {
        let mut selection = Selection::default();
        let point = Point {
            region: 1,
            row: 0,
            column: 0,
        };
        let now = Instant::now();
        selection.press(point, Some(Action::Link("https://example.com".into())), now);
        selection.release(now);
        assert!(
            matches!(selection.ready_click(now + Duration::from_millis(350)), Some(Action::Link(url)) if url == "https://example.com")
        );
        assert!(
            selection
                .ready_click(now + Duration::from_secs(1))
                .is_none()
        );
        selection.press(point, Some(Action::Link("https://example.com".into())), now);
        selection.dragging = true;
        selection.end = Some(Point { column: 4, ..point });
        selection.release(now);
        assert!(
            selection
                .ready_click(now + Duration::from_secs(1))
                .is_none()
        );
    }
    #[test]
    fn selection_survives_loading_and_rebasing_virtual_rows() {
        let mut s = Selection::default();
        let area = Rect::new(0, 0, 20, 2);
        s.register_window(
            1,
            area,
            100,
            100,
            &[Line::from("alpha"), Line::from("beta")],
        );
        s.anchor = s.point(0, 0);
        s.end = s.point(4, 1);
        assert_eq!(s.text().as_deref(), Some("alpha\nbeta"));
        s.rebase(1, 5);
        s.frame();
        s.register_window(
            1,
            area,
            106,
            106,
            &[Line::from("beta"), Line::from("gamma")],
        );
        s.end = s.point(5, 1);
        assert_eq!(s.text().as_deref(), Some("alpha\nbeta\ngamma"));
        s.clear();
        assert!(s.documents.is_empty());
    }
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
