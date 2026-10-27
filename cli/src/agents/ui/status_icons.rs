//! Small, cached status sprites using the terminal's existing graphics protocol.
//! Text terminals keep the same labels and receive colored Unicode symbols.
use super::Status;
use crate::ui::{GREEN, PURPLE, RED, YELLOW};
use image::{DynamicImage, Rgba, RgbaImage, imageops};
use ratatui::{
    Frame,
    layout::{Rect, Size},
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};
use ratatui_image::{
    Image, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub(super) const WIDTH: u16 = 2;
const STATUSES: [Status; 7] = [
    Status::Starting,
    Status::Running,
    Status::Waiting,
    Status::Idle,
    Status::Completed,
    Status::Interrupted,
    Status::Failed,
];

/// Asynchronous questions may still need attention after the agent is ready.
/// Request attention takes priority for the icon without changing run state.
pub(super) fn display_status(status: Status, has_requests: bool) -> Status {
    if has_requests {
        Status::Waiting
    } else {
        status
    }
}

pub(super) fn color(status: Status) -> Color {
    match status {
        Status::Starting => PURPLE,
        Status::Running => Color::Rgb(165, 210, 255),
        Status::Waiting => YELLOW,
        Status::Idle | Status::Completed => GREEN,
        Status::Interrupted | Status::Failed => RED,
    }
}

fn symbol(status: Status) -> &'static str {
    match status {
        Status::Starting => "⌛\u{fe0e}",
        Status::Running => "🏃\u{fe0e}",
        Status::Waiting => "✋\u{fe0e}",
        Status::Idle => "●",
        Status::Completed => "✓",
        Status::Interrupted => "⚠\u{fe0e}",
        Status::Failed => "⛔\u{fe0e}",
    }
}

/// Frame count and cadence. Completed is intentionally a settled check.
fn timing(status: Status) -> (usize, u64) {
    match status {
        Status::Running => (4, 120),
        Status::Starting => (4, 700),
        Status::Idle | Status::Interrupted => (8, 300),
        Status::Waiting => (2, 900),
        Status::Failed => (8, 425),
        Status::Completed => (1, 0),
    }
}

fn phase(status: Status, elapsed: Duration, animated: bool) -> (usize, Option<Duration>) {
    let (frames, ms) = timing(status);
    if !animated || frames == 1 {
        return (if frames == 8 { 3 } else { 0 }, None);
    }
    let elapsed = elapsed.as_millis();
    let step = u128::from(ms);
    let frame = ((elapsed / step) % frames as u128) as usize;
    (
        frame,
        Some(Duration::from_millis((step - elapsed % step) as u64)),
    )
}

pub(super) struct Icons {
    picker: Option<Picker>,
    obscured: bool,
    frames: HashMap<(u8, usize), Option<Protocol>>,
    palette: [Color; 7],
    started: Instant,
    now: Instant,
    next_frame: Option<Duration>,
}

impl Default for Icons {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            picker: None,
            obscured: false,
            frames: HashMap::new(),
            palette: STATUSES.map(|status| crate::theme::resolve(color(status))),
            started: now,
            now,
            next_frame: None,
        }
    }
}

impl Icons {
    pub(super) fn configure(&mut self, picker: Option<&Picker>, obscured: bool) {
        self.obscured = obscured;
        let picker = picker.filter(|p| {
            let font = p.font_size();
            p.protocol_type() != ProtocolType::Halfblocks
                && (1..=256).contains(&font.width)
                && (1..=512).contains(&font.height)
        });
        let key = |p: &Picker| (p.protocol_type(), p.font_size().width, p.font_size().height);
        if self.picker.as_ref().map(key) != picker.map(key) {
            self.frames.clear();
            self.picker = picker.cloned();
        }
    }

    pub(super) fn begin_frame(&mut self, now: Instant) {
        self.now = now;
        self.next_frame = None;
        let palette = STATUSES.map(|status| crate::theme::resolve(color(status)));
        if self.palette != palette {
            self.frames.clear();
            self.palette = palette;
        }
    }

    pub(super) fn redraw_interval(&self) -> Option<Duration> {
        self.next_frame
    }

    pub(super) fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Status,
        animated: bool,
        unobscured: bool,
    ) {
        // Kitty transmits a cached image only once. Never consume that first
        // transmission in a buffer which a modal, palette or toast will cover.
        let graphics = unobscured && !self.obscured && !crate::images::drawing_deferred();
        let animated = animated && graphics;
        if area.width < WIDTH || area.height == 0 {
            return;
        }
        let area = Rect::new(area.x, area.y, WIDTH, 1).intersection(frame.area());
        if area.width < WIDTH || area.height == 0 {
            return;
        }
        let (phase, next) = phase(
            status,
            self.now.saturating_duration_since(self.started),
            animated,
        );
        self.next_frame = self.next_frame.into_iter().chain(next).min();
        if graphics && let Some(picker) = &self.picker {
            let protocol = self.frames.entry((status as u8, phase)).or_insert_with(|| {
                let font = picker.font_size();
                let sprite = sprite(status, phase, rgb(crate::theme::resolve(color(status))));
                let width = u32::from(font.width) * u32::from(WIDTH);
                let height = u32::from(font.height);
                let side = width.min(height);
                let scaled = imageops::resize(&sprite, side, side, imageops::FilterType::Nearest);
                let mut canvas = RgbaImage::new(width, height);
                imageops::overlay(
                    &mut canvas,
                    &scaled,
                    i64::from((width - side) / 2),
                    i64::from((height - side) / 2),
                );
                picker
                    .new_protocol(
                        DynamicImage::ImageRgba8(canvas),
                        Size::new(WIDTH, 1),
                        Resize::Fit(Some(imageops::FilterType::Nearest)),
                    )
                    .ok()
            });
            if let Some(protocol) = protocol {
                frame.render_widget(Image::new(protocol), area);
                return;
            }
        }
        let mut style = Style::default().fg(color(status));
        if animated && phase % 4 == 0 && status != Status::Completed {
            style = style.add_modifier(Modifier::DIM);
        }
        frame.render_widget(Paragraph::new(symbol(status)).style(style), area);
    }
}

fn rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(index) => match index {
            0..=15 => {
                const ANSI: [[u8; 3]; 16] = [
                    [0, 0, 0],
                    [128, 0, 0],
                    [0, 128, 0],
                    [128, 128, 0],
                    [0, 0, 128],
                    [128, 0, 128],
                    [0, 128, 128],
                    [192, 192, 192],
                    [128, 128, 128],
                    [255, 0, 0],
                    [0, 255, 0],
                    [255, 255, 0],
                    [0, 0, 255],
                    [255, 0, 255],
                    [0, 255, 255],
                    [255, 255, 255],
                ];
                ANSI.get(usize::from(index))
                    .copied()
                    .unwrap_or([220, 230, 220])
            }
            16..=231 => {
                let index = index - 16;
                let channel = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
                [
                    channel(index / 36),
                    channel((index % 36) / 6),
                    channel(index % 6),
                ]
            }
            _ => [8 + (index - 232) * 10; 3],
        },
        Color::Black => [0, 0, 0],
        Color::DarkGray => [128, 128, 128],
        Color::Red => [128, 0, 0],
        Color::LightRed => [255, 0, 0],
        Color::Green => [0, 128, 0],
        Color::LightGreen => [0, 255, 0],
        Color::Yellow => [128, 128, 0],
        Color::LightYellow => [255, 255, 0],
        Color::Blue => [0, 0, 128],
        Color::LightBlue => [0, 0, 255],
        Color::Magenta => [128, 0, 128],
        Color::LightMagenta => [255, 0, 255],
        Color::Cyan => [0, 128, 128],
        Color::LightCyan => [0, 255, 255],
        Color::Gray => [192, 192, 192],
        Color::White => [255, 255, 255],
        Color::Reset => [220, 230, 220],
    }
}

const RUNNER: [[u16; 12]; 4] = [
    [
        0b000000110000,
        0b000001111000,
        0b000000110000,
        0b000001100000,
        0b000011110100,
        0b000110011100,
        0b000000110000,
        0b000001100000,
        0b000011110000,
        0b000110011000,
        0b001100001100,
        0b001000000100,
    ],
    [
        0b000000000000,
        0b000000110000,
        0b000001111000,
        0b000000110000,
        0b000001100000,
        0b000011111000,
        0b000110110000,
        0b000001100000,
        0b000011100000,
        0b000001110000,
        0b000000110000,
        0b000001100000,
    ],
    [
        0b000000110000,
        0b000001111000,
        0b000000110000,
        0b000001100000,
        0b001001110000,
        0b001111001000,
        0b000001100000,
        0b000001100000,
        0b000011110000,
        0b000110011000,
        0b001100001100,
        0b001000000100,
    ],
    [
        0b000000000000,
        0b000000110000,
        0b000001111000,
        0b000000110000,
        0b000001100000,
        0b000111100000,
        0b000001111000,
        0b000001100000,
        0b000001110000,
        0b000011100000,
        0b000011000000,
        0b000001100000,
    ],
];
const HAND: [u16; 12] = [
    0b000001000000,
    0b000101010000,
    0b000101010100,
    0b000101010100,
    0b000111110100,
    0b000111111100,
    0b010111111100,
    0b011111111100,
    0b001111111000,
    0b000111110000,
    0b000111100000,
    0b000111100000,
];
const HOURGLASS: [u16; 12] = [
    0b000000000000,
    0b001111111100,
    0b000100001000,
    0b000110011000,
    0b000011110000,
    0b000001100000,
    0b000001100000,
    0b000011110000,
    0b000110011000,
    0b000111111000,
    0b001111111100,
    0b000000000000,
];
const CHECK: [u16; 12] = [
    0b000000000000,
    0b000000000011,
    0b000000000111,
    0b000000001110,
    0b000000011100,
    0b110000111000,
    0b111001110000,
    0b011111100000,
    0b001111000000,
    0b000110000000,
    0b000000000000,
    0b000000000000,
];

fn rect(image: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgba<u8>) {
    for row in y..(y + h).min(image.height()) {
        for column in x..(x + w).min(image.width()) {
            image.put_pixel(column, row, color);
        }
    }
}

fn bitmap(image: &mut RgbaImage, rows: &[u16; 12], color: Rgba<u8>) {
    for (y, row) in rows.iter().enumerate() {
        for x in 0..12 {
            if row & (1 << (11 - x)) != 0 {
                rect(image, x * 2, y as u32 * 2, 2, 2, color);
            }
        }
    }
}

fn sprite(status: Status, phase: usize, [r, g, b]: [u8; 3]) -> RgbaImage {
    let mut image = RgbaImage::new(24, 24);
    let ink = Rgba([r, g, b, 255]);
    let clear = Rgba([0, 0, 0, 0]);
    let alpha = [150, 180, 215, 255, 255, 215, 180, 150]
        .get(phase)
        .copied()
        .unwrap_or(255);
    match status {
        Status::Running => {
            if let Some(rows) = RUNNER.get(phase).or_else(|| RUNNER.first()) {
                bitmap(&mut image, rows, ink);
            }
        }
        Status::Waiting => {
            bitmap(&mut image, &HAND, ink);
            rect(
                &mut image,
                20,
                0,
                3,
                3,
                Rgba([r, g, b, if phase == 0 { 100 } else { 255 }]),
            );
        }
        Status::Starting => {
            bitmap(&mut image, &HOURGLASS, ink);
            image = match phase {
                1 => imageops::rotate90(&image),
                2 => imageops::rotate180(&image),
                3 => imageops::rotate270(&image),
                _ => image,
            };
        }
        Status::Idle => {
            for (row, width) in [6, 10, 12, 14, 14, 16, 16, 16, 16, 16, 16, 14, 14, 12, 10, 6]
                .into_iter()
                .enumerate()
            {
                rect(
                    &mut image,
                    12 - width / 2,
                    4 + row as u32,
                    width,
                    1,
                    Rgba([r, g, b, alpha]),
                );
            }
        }
        Status::Completed => bitmap(&mut image, &CHECK, ink),
        Status::Interrupted => {
            for row in 1..11 {
                let inset = (11 - row) / 2;
                rect(
                    &mut image,
                    inset * 2,
                    row * 2,
                    24 - inset * 4,
                    2,
                    Rgba([r, g, b, alpha]),
                );
            }
            rect(&mut image, 11, 7, 2, 7, clear);
            rect(&mut image, 11, 17, 2, 2, clear);
        }
        Status::Failed => {
            rect(&mut image, 7, 1, 10, 22, Rgba([r, g, b, alpha]));
            rect(&mut image, 3, 4, 18, 17, Rgba([r, g, b, alpha]));
            rect(&mut image, 1, 7, 22, 10, Rgba([r, g, b, alpha]));
            for i in 0..5 {
                rect(&mut image, 7 + i * 2, 6 + i * 2, 3, 3, clear);
                rect(&mut image, 15 - i * 2, 6 + i * 2, 3, 3, clear);
            }
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn reduced_motion_stops_frames_and_working_has_a_real_run_cycle() {
        for status in STATUSES {
            let (first, repaint) = phase(status, Duration::ZERO, false);
            assert!(repaint.is_none());
            assert_eq!(phase(status, Duration::from_secs(5), false), (first, None));
            let image = sprite(status, first, [0, 255, 65]);
            assert!(image.pixels().any(|p| p.0.last().is_some_and(|a| *a > 0)));
        }
        let images: Vec<_> = (0..4)
            .map(|frame| sprite(Status::Running, frame, [0, 255, 65]))
            .collect();
        for pair in images.windows(2) {
            if let [a, b] = pair {
                assert_ne!(a, b);
            }
        }
        assert_eq!(
            phase(Status::Running, Duration::from_millis(120), true).0,
            1
        );
        assert_eq!(
            phase(Status::Running, Duration::from_millis(480), true).0,
            0
        );
        assert!(phase(Status::Completed, Duration::ZERO, true).1.is_none());
    }

    #[test]
    fn text_fallback_stays_in_its_slot_and_only_visible_icons_repaint() -> Result<()> {
        let mut icons = Icons::default();
        let mut terminal = Terminal::new(TestBackend::new(12, 3))?;
        for status in STATUSES {
            icons.begin_frame(Instant::now());
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new("session"), Rect::new(4, 1, 7, 1));
                icons.draw(frame, Rect::new(1, 1, WIDTH, 1), status, false, true);
            })?;
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .cell((1, 1))
                    .context("icon")?
                    .symbol(),
                symbol(status)
            );
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .cell((4, 1))
                    .context("title")?
                    .symbol(),
                "s"
            );
            assert!(icons.redraw_interval().is_none());
        }
        icons.begin_frame(Instant::now());
        terminal.draw(|frame| {
            icons.draw(
                frame,
                Rect::new(1, 1, WIDTH, 1),
                Status::Running,
                true,
                true,
            )
        })?;
        assert!(icons.redraw_interval().is_some());
        icons.begin_frame(Instant::now());
        assert!(icons.redraw_interval().is_none());
        Ok(())
    }

    #[test]
    fn graphics_frames_are_cached_and_unsupported_terminals_use_unicode() -> Result<()> {
        let mut icons = Icons::default();
        let mut picker = Picker::halfblocks();
        icons.configure(Some(&picker), false);
        assert!(icons.picker.is_none());
        picker.set_protocol_type(ProtocolType::Kitty);
        icons.configure(Some(&picker), false);
        let mut terminal = Terminal::new(TestBackend::new(12, 3))?;
        for status in STATUSES {
            icons.begin_frame(Instant::now());
            terminal
                .draw(|frame| icons.draw(frame, Rect::new(1, 1, WIDTH, 1), status, false, true))?;
        }
        assert_eq!(icons.frames.len(), STATUSES.len());
        assert!(icons.frames.values().all(Option::is_some));
        terminal.draw(|frame| {
            icons.draw(frame, Rect::new(5, 1, WIDTH, 1), Status::Idle, false, true)
        })?;
        assert_eq!(
            icons.frames.len(),
            STATUSES.len(),
            "placements share encoded frames"
        );
        icons.configure(None, false);
        assert!(icons.frames.is_empty());
        terminal.draw(|frame| {
            icons.draw(frame, Rect::new(1, 1, WIDTH, 1), Status::Idle, false, true)
        })?;
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((1, 1))
                .context("fallback")?
                .symbol(),
            "●"
        );
        Ok(())
    }

    #[test]
    fn overlays_do_not_consume_the_first_kitty_transmission() -> Result<()> {
        let mut icons = Icons::default();
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut terminal = Terminal::new(TestBackend::new(12, 3))?;
        for (shell_overlay, local_unobscured) in [(true, true), (false, false)] {
            icons.configure(Some(&picker), shell_overlay);
            icons.begin_frame(Instant::now());
            terminal.draw(|frame| {
                icons.draw(
                    frame,
                    Rect::new(1, 1, WIDTH, 1),
                    Status::Idle,
                    true,
                    local_unobscured,
                );
            })?;
            assert!(icons.frames.is_empty());
            assert!(icons.redraw_interval().is_none());
        }
        icons.configure(Some(&picker), false);
        terminal.draw(|frame| {
            icons.draw(frame, Rect::new(1, 1, WIDTH, 1), Status::Idle, false, true)
        })?;
        let cell = terminal
            .backend()
            .buffer()
            .cell((1, 1))
            .context("visible icon")?;
        assert!(
            cell.symbol().contains("\x1b_G"),
            "first visible frame transmits its pixels"
        );
        terminal.draw(|frame| {
            icons.draw(frame, Rect::new(1, 1, WIDTH, 1), Status::Idle, false, true)
        })?;
        let cell = terminal
            .backend()
            .buffer()
            .cell((1, 1))
            .context("cached icon")?;
        assert!(
            !cell.symbol().contains("\x1b_G"),
            "later frames reuse the transmitted image"
        );
        Ok(())
    }

    #[test]
    fn sprite_frames_fit_all_supported_graphics_protocols() -> Result<()> {
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            let mut icons = Icons::default();
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            icons.configure(Some(&picker), false);
            let mut terminal = Terminal::new(TestBackend::new(12, 3))?;
            for status in STATUSES {
                let (frames, _) = timing(status);
                for index in 0..frames {
                    let elapsed = Duration::from_millis(timing(status).1 * index as u64);
                    icons.begin_frame(icons.started + elapsed);
                    terminal.draw(|frame| {
                        icons.draw(frame, Rect::new(1, 1, WIDTH, 1), status, true, true)
                    })?;
                }
            }
            for frame in icons.frames.values() {
                assert_eq!(
                    frame.as_ref().context("encoded sprite frame")?.size(),
                    Size::new(WIDTH, 1)
                );
            }
        }
        Ok(())
    }
}
