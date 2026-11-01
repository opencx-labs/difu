//! CPU diagnostics is a global view. Its data and warning lifetime invalidate
//! once when changed; neither adds a cosmetic animation timer.
use super::*;
use crate::cpu::{self, Area, Client, Overview, Process};
use ratatui::{
    layout::Margin,
    text::{Line, Span},
    widgets::{Block, Borders, Clear},
};
use std::time::Instant;

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    scroll: u16,
    data: Overview,
    client: Option<Client>,
    pending_warning: Option<f64>,
    toast: Option<(String, Instant)>,
    toast_rect: Rect,
}
impl Panel {
    pub fn start(&mut self, storage: Storage) {
        match Client::new(storage) {
            Ok(client) => self.client = Some(client),
            Err(error) => {
                self.data.ui.error = Some(format!("CPU monitoring could not start: {error}"))
            }
        }
    }
    pub fn tick(&mut self, focused: bool) {
        if let Some(data) = self.client.as_ref().and_then(Client::take) {
            self.pending_warning = data.warning.or(self.pending_warning);
            self.data = data;
            if self.open {
                crate::repaint::request();
            }
        }
        let now = Instant::now();
        if focused && let Some(percent) = self.pending_warning.take() {
            self.toast = Some((
                format!(
                    "High difu CPU: {percent:.1}% · Above 25% for 30s. Open /cpu in Cmd+K for details."
                ),
                now,
            ));
            crate::repaint::request();
        }
        if self.toast.as_ref().is_some_and(|(_, at)| {
            now.saturating_duration_since(*at) >= crate::agents::ui::toast::LIFETIME
        }) {
            self.toast = None;
            self.toast_rect = Rect::default();
            crate::repaint::request();
        }
    }
    pub fn draw_toast(&mut self, frame: &mut Frame) {
        self.toast_rect = self
            .toast
            .as_ref()
            .map(|(text, _)| crate::agents::ui::toast::draw(frame, frame.area(), text, true))
            .unwrap_or_default();
    }
    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from("CPU usage · 100% = one core; multiple cores can exceed 100%"),
            Line::from(
                "UI + background service only. Agent, build and Git subprocess CPU is excluded.",
            )
            .style(Style::default().fg(DIM)),
            Line::from(""),
            Line::from(format!("Combined now: {}", percent(self.data.combined())))
                .style(Style::default().fg(ACCENT)),
            Line::from(format!(
                "{:<22} {:>10} {:>12} {:>10}",
                "Process", "Now", "5-min avg", "Peak"
            )),
        ];
        for (label, process) in [
            ("Terminal UI", &self.data.ui),
            ("Background service", &self.data.service),
        ] {
            let current = process.current(self.data.at_ms).and_then(|s| s.percent);
            let (average, peak) = summary(process);
            lines.push(Line::from(format!(
                "{label:<22} {:>10} {:>12} {:>10}",
                percent(current),
                percent(average),
                percent(peak)
            )));
            if let Some(error) = &process.error {
                lines.push(
                    Line::from(crate::model::clean(error))
                        .style(Style::default().fg(crate::ui::RED)),
                );
            } else if current.is_none() {
                lines.push(
                    Line::from("  Waiting for a fresh CPU sample…").style(Style::default().fg(DIM)),
                );
            }
        }
        let fps = self
            .data
            .ui
            .current(self.data.at_ms)
            .map(|s| s.frames as f64 / s.seconds);
        let peak_fps = self
            .data
            .ui
            .samples
            .iter()
            .filter(|s| s.seconds > 0.0)
            .map(|s| s.frames as f64 / s.seconds)
            .reduce(f64::max);
        lines.extend([
            Line::from(""),
            Line::from(format!("Redraws: {} /s now · {} /s peak", number(fps), number(peak_fps))),
            Line::from("This view replaces chat rendering. History retains the workload before opening it.").style(Style::default().fg(DIM)),
            Line::from(""),
            Line::from("CPU history · last five minutes, newest at right").style(Style::default().fg(ACCENT)),
        ]);
        let columns = usize::from(width.saturating_sub(18)).clamp(1, 100);
        let scale = self
            .data
            .ui
            .samples
            .iter()
            .chain(&self.data.service.samples)
            .filter_map(|s| s.percent)
            .fold(25.0_f64, f64::max)
            .ceil();
        for (label, process) in [("UI", &self.data.ui), ("Service", &self.data.service)] {
            lines.push(Line::from(format!(
                "{label:<10} {}",
                chart(process, self.data.at_ms, columns, scale)
            )));
        }
        lines.push(Line::from(format!("Bar ceiling: {scale:.0}% · Each bar is the maximum in its time window; · = no reading")).style(Style::default().fg(DIM)));
        lines.extend([
            Line::from(""),
            Line::from("Instrumented CPU · five-minute totals").style(Style::default().fg(ACCENT)),
            Line::from(format!(
                "{:<22} {:>12} {:>12} {:>10}",
                "Area", "UI CPU ms", "Svc CPU ms", "Calls"
            )),
        ]);
        for (index, area) in Area::ALL.into_iter().enumerate() {
            let sum = |process: &Process| {
                process
                    .samples
                    .iter()
                    .filter_map(|s| s.areas.get(index))
                    .fold(cpu::Cost::default(), |mut sum, value| {
                        sum.cpu_ns = sum.cpu_ns.saturating_add(value.cpu_ns);
                        sum.calls = sum.calls.saturating_add(value.calls);
                        sum
                    })
            };
            let ui = sum(&self.data.ui);
            let service = sum(&self.data.service);
            lines.push(Line::from(format!(
                "{:<22} {:>12.1} {:>12.1} {:>10}",
                area.label(),
                ui.cpu_ns as f64 / 1_000_000.0,
                service.cpu_ns as f64 / 1_000_000.0,
                ui.calls.saturating_add(service.calls)
            )));
        }
        lines.extend([
            Line::from("Timings exclude blocked I/O. Nested work belongs to its outer area; other CPU is uninstrumented.").style(Style::default().fg(DIM)),
            Line::from("Scopes are recorded on completion, so short-window attribution can differ from process totals.").style(Style::default().fg(DIM)),
            Line::from(""),
            Line::from("Warnings: combined CPU >25% for 30s · 10-minute cooldown · No automatic throttling"),
            Line::from("Sampled once per second. Five-minute history stays in memory; no telemetry or disk log.").style(Style::default().fg(DIM)),
            Line::from("CPU usage is not a measurement of battery watts or terminal/GPU consumption.").style(Style::default().fg(DIM)),
        ]);
        lines
    }
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" difu CPU ")
                .style(Style::default().bg(BG).fg(crate::ui::TEXT)),
            area,
        );
        let body = area.inner(Margin::new(2, 1));
        let rows = self.lines(body.width);
        let maximum = rows
            .len()
            .saturating_sub(usize::from(body.height.saturating_sub(1)))
            .min(usize::from(u16::MAX)) as u16;
        self.scroll = self.scroll.min(maximum);
        frame.render_widget(
            Paragraph::new(rows).scroll((self.scroll, 0)),
            Rect {
                height: body.height.saturating_sub(1),
                ..body
            },
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Esc", Style::default().fg(ACCENT)),
                Span::raw(" Close  ·  ↑/↓ Scroll  ·  Cmd+K Commands"),
            ]))
            .style(Style::default().fg(DIM)),
            Rect::new(body.x, body.bottom().saturating_sub(1), body.width, 1),
        );
    }
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.open = false,
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = u16::MAX,
            _ => {}
        }
    }
}
fn percent(value: Option<f64>) -> String {
    value
        .map(|p| format!("{p:.1}%"))
        .unwrap_or_else(|| "—".into())
}
fn number(value: Option<f64>) -> String {
    value
        .map(|p| format!("{p:.1}"))
        .unwrap_or_else(|| "—".into())
}
fn summary(process: &Process) -> (Option<f64>, Option<f64>) {
    let (weighted, seconds, peak) = process
        .samples
        .iter()
        .filter_map(|s| s.percent.map(|p| (p, s.seconds)))
        .fold((0.0, 0.0, None), |(total, seconds, peak), (p, elapsed)| {
            (
                total + p * elapsed,
                seconds + elapsed,
                Some(peak.map_or(p, |old: f64| old.max(p))),
            )
        });
    ((seconds > 0.0).then(|| weighted / seconds), peak)
}
fn chart(process: &Process, now_ms: i64, columns: usize, scale: f64) -> String {
    let mut bins = vec![None; columns];
    for sample in &process.samples {
        let age = now_ms.saturating_sub(sample.at_ms);
        if !(0..300_000).contains(&age) {
            continue;
        }
        let index = ((299_999 - age) as usize).saturating_mul(columns) / 300_000;
        if let Some(bin) = bins.get_mut(index)
            && let Some(percent) = sample.percent
        {
            *bin = Some(bin.map_or(percent, |old: f64| old.max(percent)));
        }
    }
    let levels = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    bins.into_iter()
        .map(|p| {
            p.map(|p| {
                levels
                    .get(((p / scale).clamp(0.0, 1.0) * 7.0).round() as usize)
                    .copied()
                    .unwrap_or('█')
            })
            .unwrap_or('·')
        })
        .collect()
}
impl Shell {
    pub fn start_cpu_monitor(&mut self) {
        self.cpu.start(self.reviews.storage.clone());
    }
    pub(super) fn open_cpu(&mut self) {
        self.agents.release_terminal_focus();
        self.agents.cancel_voice();
        self.palette = None;
        self.cpu.open = true;
        self.cpu.scroll = 0;
        self.switched = None;
        crate::repaint::request();
    }
    pub(super) fn cpu_key(&mut self, key: KeyEvent) -> bool {
        if !self.cpu.open {
            return false;
        }
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if self.palette.is_some() {
            self.palette_key(key);
            return true;
        }
        use crate::shortcuts::{Bindings, contexts, ids};
        match Bindings::new(&self.reviews.config).resolve(contexts::PALETTE, key) {
            Some(ids::PALETTE_OPEN) => {
                self.palette = Some(palette::Palette::new(self.reviews.storage.clone()))
            }
            Some(ids::APP_QUIT) => self.reviews.quit = true,
            _ => self.cpu.key(key),
        }
        true
    }
    pub(super) fn cpu_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.palette.is_some() {
            if self.cpu.open {
                self.palette_mouse(mouse);
                return true;
            }
            return false;
        }
        if self
            .cpu
            .toast_rect
            .contains((mouse.column, mouse.row).into())
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        {
            self.cpu.toast = None;
            self.cpu.toast_rect = Rect::default();
            self.open_cpu();
            return true;
        }
        if !self.cpu.open {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.cpu.scroll = self.cpu.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => self.cpu.scroll = self.cpu.scroll.saturating_add(3),
            _ => {}
        }
        true
    }
    pub(super) fn draw_cpu(&mut self, frame: &mut Frame) -> bool {
        if !self.cpu.open {
            return false;
        }
        self.cpu.draw(frame);
        self.draw_palette(frame);
        self.cpu.draw_toast(frame);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Duration;

    fn shell(storage: Storage) -> Shell {
        let mut config = Config::default();
        config.preferences.motion.level = crate::settings::Motion::Off;
        config.preferences.layout.startup_tab = crate::settings::StartupTab::Agents;
        Shell::new(storage, config, None)
    }
    #[test]
    fn cpu_warning_is_static_expires_and_can_open_diagnostics() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut shell = shell(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        shell.cpu.pending_warning = Some(42.0);
        shell.cpu.tick(true);
        assert!(shell.cpu.toast.is_some());
        assert!(shell.agents.notice.is_none());
        assert_eq!(shell.redraw_interval(), None);
        let mut terminal = Terminal::new(TestBackend::new(100, 35))?;
        terminal.draw(|frame| shell.draw(frame))?;
        let hit = shell.cpu.toast_rect;
        assert!(hit.width > 0);
        shell.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(shell.cpu.open);
        shell.cpu.pending_warning = Some(45.0);
        shell.cpu.tick(true);
        shell.cpu.toast.as_mut().context("toast")?.1 -= Duration::from_secs(6);
        shell.cpu.tick(false);
        assert!(shell.cpu.toast.is_none());
        assert_eq!(shell.redraw_interval(), None);
        Ok(())
    }
    #[test]
    fn diagnostics_handles_small_terminals_and_unavailable_samples_without_zero() -> Result<()> {
        let mut panel = Panel::default();
        panel.open = true;
        panel.data.service.error = Some("Service disconnected".into());
        let mut terminal = Terminal::new(TestBackend::new(70, 20))?;
        terminal.draw(|frame| panel.draw(frame))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Combined now: —"));
        assert!(text.contains("Service disconnected"));
        for (width, height) in [(1, 1), (20, 8), (120, 45)] {
            terminal.backend_mut().resize(width, height);
            terminal.resize(Rect::new(0, 0, width, height))?;
            terminal.draw(|frame| panel.draw(frame))?;
        }
        Ok(())
    }
    #[test]
    fn cpu_view_has_no_animation_timer_and_escape_preserves_active_tab() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut shell = shell(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        shell.agents_active = false;
        shell.open_cpu();
        assert!(!shell.agents_active);
        assert_eq!(shell.redraw_interval(), None);
        shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!shell.cpu.open);
        assert!(!shell.agents_active);
        Ok(())
    }
}
