use super::*;
use crate::settings::Motion;

#[test]
fn motion_off_stops_animation_wakeups_but_keeps_notice_and_elapsed_updates() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    let mut ui = Ui::new(storage, &config);
    ui.selected = Some("selected".into());
    ui.positions.entry("selected".into()).or_default().sent =
        reveal::Typing::start("sending", 1000, Instant::now());
    assert_eq!(
        ui.redraw_interval(),
        None,
        "decorative animation must not schedule frames"
    );
    ui.notice = Some(("Saved".into(), false));
    assert_eq!(
        ui.redraw_interval(),
        Some(Duration::from_secs(1)),
        "notice expiry still needs a timer"
    );
    ui.notice = None;
    ui.busy = true;
    assert_eq!(ui.redraw_interval(), Some(Duration::from_secs(1)));
    Ok(())
}

#[test]
fn compact_and_separator_previews_keep_the_original_text() {
    let now = Instant::now();
    let mut config = Config::default();
    let mut preview = settings_preview::SettingsPreview::new(now);
    let comfortable = preview.render(&config, 80, now, false);
    config.preferences.appearance.density = crate::settings::Density::Compact;
    let compact = preview.render(&config, 80, now, false);
    assert!(compact.len() < comfortable.len());
    let words = |rows: &[Line]| {
        rows.iter()
            .flat_map(|r| r.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<String>()
    };
    assert_eq!(words(&compact), words(&comfortable));
    config.preferences.appearance.message_separators = true;
    let separated = preview.render(&config, 80, now, false);
    assert!(
        separated
            .iter()
            .any(|row| row.to_string() == "─".repeat(80))
    );
    assert!(words(&separated).contains("The greeting now says"));
}

#[test]
fn preview_replays_the_real_sent_message_effect_independently_of_reply_effects() {
    let now = Instant::now();
    let mut config = Config::default();
    config.preferences.motion.reply_tool = false;
    let mut preview = settings_preview::SettingsPreview::new(now);
    let settled = preview.render(&config, 80, now, false);
    let playing = preview.render(&config, 80, now, true);
    assert_ne!(playing.first(), settled.first());
    assert!(preview.animating());
    config.preferences.motion.sent_message = false;
    let off = preview.render(&config, 80, now, true);
    assert_eq!(off, settled);
    assert_eq!(config.tweaks.sent_message_ms, 1000);
}

#[test]
fn zero_durations_settle_existing_effects_without_erasing_other_timings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.tweaks.max_animation_ms = 0;
    config.tweaks.sent_message_ms = 1600;
    config.tweaks.input_clear_ms = 1200;
    let mut ui = Ui::new(storage, &config);
    let now = Instant::now();
    let position = ui.positions.entry("session".into()).or_default();
    position.sent = reveal::Typing::start("sent", 1600, now);
    position.clearing = reveal::Typing::start("composer", 1200, now);
    config.tweaks.sent_message_ms = 0;
    ui.apply_settings(&config);
    let position = ui
        .positions
        .get_mut("session")
        .ok_or_else(|| anyhow::anyhow!("position"))?;
    assert!(position.sent.is_none());
    assert!(
        reveal::clearing(position, now + Duration::from_millis(1000)).is_some(),
        "uncapped clearing remains independent"
    );
    position.draft = Editor::from("new draft");
    config.tweaks.input_clear_ms = 0;
    ui.apply_settings(&config);
    assert!(
        ui.positions
            .get("session")
            .is_some_and(|p| p.clearing.is_none() && p.draft.text() == "new draft")
    );
    assert_eq!(ui.tweaks.max_animation_ms, 0);
    Ok(())
}

#[test]
fn sidebar_and_screen_controls_drive_live_flaps_with_legacy_zero_compatibility()
-> anyhow::Result<()> {
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
    for (level, sidebar, screen, speed, cap, sidebar_moves, screen_moves) in [
        (Motion::Full, true, true, 200, 0, true, true),
        (Motion::Full, false, true, 200, 0, false, true),
        (Motion::Full, true, false, 200, 0, true, false),
        (Motion::Full, true, true, 0, 0, false, false),
        (Motion::Full, true, true, 200, 10, true, false),
        (Motion::Reduced, true, true, 200, 0, false, false),
        (Motion::Off, true, true, 200, 0, false, false),
    ] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut config = Config::default();
        config.preferences.motion.level = level;
        config.preferences.motion.sidebar = sidebar;
        config.preferences.motion.screen_transition = screen;
        config.tweaks.agent_reply_cps = speed;
        config.tweaks.max_animation_ms = cap;
        let mut ui = Ui::new(storage, &config);
        let mut session = Session::new(
            "session".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "Old title".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        ui.selected = Some(session.id.clone());
        ui.summaries.push(session.summary());
        ui.sessions.insert(session.id.clone(), session);
        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        terminal.draw(|frame| ui.draw(frame))?;
        if let Some(summary) = ui.summaries.first_mut() {
            summary.title = "Changed title".into();
        }
        terminal.draw(|frame| ui.draw(frame))?;
        assert_eq!(
            ui.sidebar_boards
                .values()
                .any(|board| board.animating(Instant::now())),
            sidebar_moves,
            "{level:?} sidebar={sidebar} speed={speed}"
        );
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 2));
        for cell in &mut buffer.content {
            cell.set_symbol("a");
        }
        assert_eq!(
            ui.flip_screen(&mut buffer, Instant::now() - Duration::from_millis(400)),
            screen_moves,
            "{level:?} screen={screen} speed={speed} cap={cap}"
        );
        assert_eq!(ui.tweaks, config.tweaks);
    }
    Ok(())
}

#[test]
fn production_density_and_separators_keep_chat_copy_selection_and_draft() -> anyhow::Result<()> {
    use anyhow::Context;
    use ratatui::{Terminal, backend::TestBackend};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    config.agent_list_visible = false;
    let mut ui = Ui::new(storage, &config);
    let mut session = Session::new(
        "session".into(),
        Job::Coding(Launch {
            repository: "/tmp/project".into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Task".into(),
            model: None,
            effort: None,
        }),
    );
    session.status = Status::Idle;
    session.note("agentMessage", "alpha");
    session.note("agentMessage", "beta");
    ui.selected = Some(session.id.clone());
    ui.sessions.insert(session.id.clone(), session);
    ui.drilled = true;
    ui.focus = Focus::Conversation;
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Unsent draft");
    let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let comfortable = ui.conversation_lines;
    let location = |word: &str| -> anyhow::Result<(u16, u16)> {
        for (row, cells) in terminal.backend().buffer().content.chunks(100).enumerate() {
            let line = cells.iter().map(|cell| cell.symbol()).collect::<String>();
            if let Some(index) = line.find(word) {
                return Ok((
                    line.get(..index).context("prefix")?.chars().count() as u16,
                    row as u16,
                ));
            }
        }
        anyhow::bail!("missing word {word}")
    };
    let (x, y) = location("alpha")?;
    let (end_x, end_y) = location("beta")?;
    for (kind, column, row) in [
        (MouseEventKind::Down(MouseButton::Left), x, y),
        (MouseEventKind::Drag(MouseButton::Left), end_x + 4, end_y),
        (MouseEventKind::Up(MouseButton::Left), end_x + 4, end_y),
    ] {
        ui.mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(ui.copy_chat_selection());
    let selected = ui.clipboard.take().context("selection")?;
    assert!(selected.contains("alpha") && selected.contains("beta"));
    config.preferences.appearance.density = crate::settings::Density::Compact;
    ui.apply_settings(&config);
    terminal.draw(|frame| ui.draw(frame))?;
    assert!(ui.conversation_lines < comfortable);
    assert!(ui.copy_chat_selection());
    assert_eq!(ui.clipboard.as_ref(), Some(&selected));
    config.preferences.appearance.message_separators = true;
    ui.apply_settings(&config);
    terminal.draw(|frame| ui.draw(frame))?;
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "─")
            .count()
            > 100
    );
    assert!(ui.copy_chat_selection());
    assert_eq!(ui.clipboard.as_ref(), Some(&selected));
    assert_eq!(
        ui.positions
            .get("session")
            .context("position")?
            .draft
            .text(),
        "Unsent draft"
    );
    Ok(())
}

#[test]
fn reduced_preview_shows_complete_text_immediately() {
    let now = Instant::now();
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Reduced;
    let mut preview = settings_preview::SettingsPreview::new(now);
    let expected = preview.render(&config, 80, now, false);
    let actual = preview.render(&config, 80, now, true);
    assert_eq!(actual, expected);
    assert!(!preview.animating());
}
