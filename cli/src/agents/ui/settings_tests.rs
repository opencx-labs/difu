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
    config.preferences.conversation.timestamps = true;
    ui.apply_settings(&config);
    terminal.draw(|frame| ui.draw(frame))?;
    assert!(ui.copy_chat_selection());
    assert_eq!(
        ui.clipboard.as_ref(),
        Some(&selected),
        "conversation formatting reflow preserves the selected source"
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

#[test]
fn conversation_tool_modes_and_line_budget_drive_shared_preview() {
    use crate::settings::ToolDisplay;
    let now = Instant::now();
    let mut preview = settings_preview::SettingsPreview::new(now);
    let mut config = Config::default();
    let text = |rows: Vec<Line>| {
        rows.iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    config.preferences.conversation.tool_display = ToolDisplay::Collapsed;
    let collapsed = text(preview.render(&config, 80, now, false));
    assert!(collapsed.contains("cargo test greeting"));
    assert!(!collapsed.contains("All checks passed."));
    config.preferences.conversation.tool_display = ToolDisplay::Compact;
    config.preferences.conversation.tool_preview_lines = 2;
    let compact = text(preview.render(&config, 80, now, false));
    assert!(compact.contains("Hello, Difu!"));
    assert!(!compact.contains("All checks passed."));
    config.preferences.conversation.tool_preview_lines = 6;
    let longer = text(preview.render(&config, 80, now, false));
    assert!(longer.contains("Extra tool output."));
    config.preferences.conversation.tool_display = ToolDisplay::Expanded;
    config.preferences.conversation.tool_preview_lines = 1;
    let expanded = text(preview.render(&config, 80, now, false));
    assert!(expanded.contains("Extra tool output."));
    assert!(!expanded.contains("Enter/click to expand"));
}

fn conversation_fixture(storage: Storage, config: &Config) -> Ui {
    let mut ui = Ui::new(storage, config);
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
    ui.selected = Some(session.id.clone());
    ui.sessions.insert(session.id.clone(), session);
    ui.drilled = true;
    ui.focus = Focus::Composer;
    ui
}
fn native_frame(ui: &mut Ui, width: u16, height: u16) -> anyhow::Result<String> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| ui.draw(frame))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n"))
}
#[test]
fn reasoning_detail_changes_native_transcript_and_preserves_draft() -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config {
        agent_list_visible: false,
        ..Default::default()
    };
    config.preferences.motion.level = Motion::Off;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    ui.sessions
        .get_mut("session")
        .context("session")?
        .note("reasoning", "**Inspect paths**\nUnique reasoning detail");
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Retain this draft");
    let folded = native_frame(&mut ui, 100, 30)?;
    assert!(folded.contains("Inspect paths"));
    assert!(!folded.contains("Unique reasoning detail"));
    config.preferences.conversation.reasoning_expanded = true;
    ui.preview_visual_settings(&config);
    assert!(native_frame(&mut ui, 100, 30)?.contains("Unique reasoning detail"));
    assert_eq!(
        ui.positions
            .get("session")
            .context("position")?
            .draft
            .text(),
        "Retain this draft"
    );
    ui.apply_settings(&Config::default());
    assert!(!native_frame(&mut ui, 100, 30)?.contains("Unique reasoning detail"));
    Ok(())
}
#[test]
fn new_entry_timestamps_are_recorded_and_legacy_times_stay_absent() -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config {
        agent_list_visible: false,
        ..Default::default()
    };
    config.preferences.motion.level = Motion::Off;
    config.preferences.conversation.timestamps = true;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    let session = ui.sessions.get_mut("session").context("session")?;
    let before = chrono::Utc::now().timestamp_millis();
    session.note("agentMessage", "New reply");
    let saved = serde_json::to_value(session.entries.first().context("entry")?)?;
    let time = saved
        .get("timestamp")
        .and_then(Value::as_i64)
        .context("new entry records its receipt timestamp")?;
    assert!((before..=chrono::Utc::now().timestamp_millis()).contains(&time));
    let legacy: Entry = serde_json::from_value(
        serde_json::json!({"id":"legacy","kind":"agentMessage","text":"Old reply","started_at":1,"finished_at":2}),
    )?;
    assert!(
        serde_json::to_value(&legacy)?
            .get("timestamp")
            .is_none_or(Value::is_null)
    );
    session.entries.first_mut().context("entry")?.timestamp = Some(1_609_459_261_000);
    session.entries.push(legacy);
    let drawn = native_frame(&mut ui, 100, 30)?;
    assert_eq!(drawn.matches("UTC").count(), 1);
    assert!(
        drawn.contains("00:01:01 UTC"),
        "render the stored receipt time, not the current clock"
    );
    config.preferences.conversation.timestamps = false;
    ui.preview_visual_settings(&config);
    assert!(!native_frame(&mut ui, 100, 30)?.contains("UTC"));
    Ok(())
}
#[test]
fn scroll_policies_follow_streaming_or_preserve_the_reading_anchor() {
    use crate::settings::AutoScroll;
    for (policy, follows) in [
        (AutoScroll::WhenAtBottom, false),
        (AutoScroll::Always, true),
        (AutoScroll::Manual, false),
    ] {
        let mut session = Session::new(
            "s".into(),
            Job::Coding(Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "x".into(),
                model: None,
                effort: None,
            }),
        );
        for n in 0..15 {
            session.note("agentMessage", format!("message {n}\nmore content"));
        }
        let mut window = transcript_window::Window::default();
        window.conversation.auto_scroll = policy;
        let mut position = Position::default();
        let initial = window.render(&session, &mut position, 60, 5, false);
        assert_eq!(position.conversation > 0, follows, "{policy:?}");
        if !follows {
            assert_eq!(position.conversation, 0);
        }
        let before = position.conversation;
        session.note("agentMessage", "Newest streaming reply\nextra rows");
        let next = window.render(&session, &mut position, 60, 5, false);
        if follows {
            assert!(position.conversation > before);
        } else {
            assert_eq!(position.conversation, before);
        }
        assert!(next.total > initial.total);
        position.follow = true;
        window.render(&session, &mut position, 60, 5, false);
        assert_eq!(position.conversation > 0, policy != AutoScroll::Manual);
    }
}

#[test]
fn layout_width_visibility_and_bottom_helper_change_real_panes() -> anyhow::Result<()> {
    use crate::settings::HelperPlacement;
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    ui.panels.view = Some(panels::View::Shell {
        id: "tool".into(),
        command: "echo preview".into(),
    });
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Keep composer");
    for (width, requested, expected) in [(120, 0, 29), (120, 20, 20), (120, 40, 40), (40, 40, 18)] {
        config.preferences.layout.sidebar_width = requested;
        ui.apply_settings(&config);
        native_frame(&mut ui, width, 40)?;
        let pane = ui
            .hits
            .iter()
            .find_map(|(r, a)| matches!(a, Action::Focus(Focus::List)).then_some(r))
            .context("sidebar")?;
        assert_eq!(pane.width, expected, "width {width}, requested {requested}");
    }
    config.agent_list_visible = false;
    config.preferences.layout.helper_placement = Some(HelperPlacement::Bottom);
    ui.preview_visual_settings(&config);
    native_frame(&mut ui, 100, 40)?;
    assert!(
        !ui.hits
            .iter()
            .any(|(_, a)| matches!(a, Action::Focus(Focus::List)))
    );
    let chat = ui
        .hits
        .iter()
        .find_map(|(r, a)| matches!(a, Action::Focus(Focus::Conversation)).then_some(*r))
        .context("conversation")?;
    assert!(ui.panels.rect.y > chat.y + chat.height / 2);
    assert!(ui.panels.rect.width > 80);
    assert!(native_frame(&mut ui, 40, 12)?.contains("Shell output"));
    config.preferences.layout.helper_placement = Some(HelperPlacement::Main);
    config.agent_panel_right = false;
    ui.preview_visual_settings(&config);
    let main = native_frame(&mut ui, 100, 40)?;
    assert!(main.contains("Shell output"));
    assert!(
        !ui.hits
            .iter()
            .any(|(_, a)| matches!(a, Action::Focus(Focus::Conversation)))
    );
    assert_eq!(
        ui.positions
            .get("session")
            .context("position")?
            .draft
            .text(),
        "Keep composer"
    );
    config.agent_changes_visible = true;
    ui.panels.view = None;
    ui.preview_visual_settings(&config);
    assert!(native_frame(&mut ui, 100, 40)?.contains("Changes"));
    Ok(())
}
#[test]
fn composer_height_and_paste_thresholds_use_real_editor_and_retain_source() -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    config.agent_list_visible = false;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    let source = (1..=25)
        .map(|n| format!("composer row {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    for (limit, expected) in [(3, 5), (10, 12), (20, 22)] {
        config.preferences.input.composer_max_height = limit;
        ui.preview_visual_settings(&config);
        ui.positions.entry("session".into()).or_default().draft = Editor::from(source.as_str());
        native_frame(&mut ui, 100, 45)?;
        let composer = ui
            .hits
            .iter()
            .find_map(|(r, a)| matches!(a, Action::Focus(Focus::Composer)).then_some(r))
            .context("composer")?;
        assert_eq!(composer.height, expected);
        assert_eq!(
            ui.positions
                .get("session")
                .context("position")?
                .draft
                .text(),
            source
        );
    }
    for threshold in [Some(250), Some(500), Some(1000), None] {
        config.preferences.input.paste_threshold = threshold;
        ui.apply_settings(&config);
        for size in [249, 250, 499, 500, 999, 1000] {
            let text = "λ".repeat(size);
            ui.positions.entry("session".into()).or_default().draft = Editor::default();
            ui.paste(&text);
            let draft = &mut ui.positions.get_mut("session").context("position")?.draft;
            assert_eq!(draft.text(), text);
            draft.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
            assert_eq!(
                draft.selected_text(),
                Some(text.clone()),
                "copy exposes the full pasted source"
            );
            let shown = draft.chars.iter().collect::<String>();
            assert_eq!(
                shown.contains("Pasted content"),
                threshold.is_some_and(|t| size >= t as usize),
                "{threshold:?} size {size}"
            );
        }
    }
    Ok(())
}
#[test]
fn running_preference_changes_primary_send_but_keeps_explicit_queue() -> anyhow::Result<()> {
    use crate::settings::RunningInput;
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    for (running, active, explicit_queue, want) in [
        (RunningInput::Steer, true, false, false),
        (RunningInput::Queue, true, false, true),
        (RunningInput::Steer, true, true, true),
        (RunningInput::Queue, false, false, false),
    ] {
        ui.sessions.get_mut("session").context("session")?.turn_id =
            active.then(|| "active-turn".into());
        config.preferences.input.running = running;
        ui.apply_settings(&config);
        // An existing in-flight delivery keeps the next control local to the real send queue.
        ui.send_queues.insert(
            "session".into(),
            VecDeque::from([(
                999,
                Control::Message {
                    text: "already sending".into(),
                    queue: false,
                    skills: Vec::new(),
                    attachments: Vec::new(),
                },
            )]),
        );
        ui.positions.entry("session".into()).or_default().draft = Editor::from("Next message");
        ui.key(KeyEvent::new(
            KeyCode::Enter,
            if explicit_queue {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        ));
        let (_, control) = ui
            .send_queues
            .get("session")
            .and_then(|q| q.back())
            .context("queued control")?;
        assert!(
            matches!(control,Control::Message { text,queue,.. } if text=="Next message" && *queue==want),
            "{running:?} explicit {explicit_queue}"
        );
    }
    Ok(())
}

#[test]
fn selected_layout_input_and_scroll_samples_use_native_consumers() {
    use crate::settings::{AutoScroll, SettingId};
    let now = Instant::now();
    let mut preview = settings_preview::SettingsPreview::new(now);
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    let text = |rows: Vec<Line>| {
        rows.iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let layout = text(preview.render_control(&config, 100, now, &SettingId::HelperPlacement));
    assert!(
        layout.contains("Shell output"),
        "layout sample must draw the actual helper pane"
    );
    config.preferences.input.composer_max_height = 3;
    let short = text(preview.render_control(&config, 100, now, &SettingId::ComposerHeight));
    config.preferences.input.composer_max_height = 20;
    let tall = text(preview.render_control(&config, 100, now, &SettingId::ComposerHeight));
    assert!(tall.matches("composer row").count() > short.matches("composer row").count());
    let folded = text(preview.render_control(&config, 100, now, &SettingId::PasteThreshold));
    assert!(folded.contains("Pasted content"));
    config.preferences.input.paste_threshold = None;
    assert!(
        !text(preview.render_control(&config, 100, now, &SettingId::PasteThreshold))
            .contains("Pasted content")
    );
    config.preferences.conversation.auto_scroll = AutoScroll::Manual;
    let manual = text(preview.render_control(&config, 80, now, &SettingId::AutoScroll));
    assert!(manual.contains("Reading message 0"));
    config.preferences.conversation.auto_scroll = AutoScroll::Always;
    assert!(
        text(preview.render_control(&config, 80, now, &SettingId::AutoScroll))
            .contains("Newest streaming reply")
    );
    let folded = text(preview.render(&config, 80, now, false));
    assert!(!folded.contains("Unique reasoning detail"));
    config.preferences.conversation.reasoning_expanded = true;
    assert!(text(preview.render(&config, 80, now, false)).contains("Unique reasoning detail"));
    config.preferences.conversation.timestamps = true;
    assert!(text(preview.render(&config, 80, now, false)).contains("UTC"));
    config.preferences.layout.session_sort = crate::settings::SessionSort::ActiveFirst;
    let active = text(preview.render_control(&config, 160, now, &SettingId::SessionSort));
    assert!(active.find("Active session") < active.find("Recent session"));
    config.preferences.layout.session_sort = crate::settings::SessionSort::Recent;
    let recent = text(preview.render_control(&config, 160, now, &SettingId::SessionSort));
    assert!(recent.find("Recent session") < recent.find("Active session"));
    config.preferences.layout.startup_tab = crate::settings::StartupTab::Reviews;
    assert!(
        text(preview.render_control(&config, 80, now, &SettingId::StartupTab))
            .contains("Launch opens Reviews")
    );
}

#[test]
fn full_entry_modal_stays_full_under_global_detail_changes() -> anyhow::Result<()> {
    use crate::settings::ToolDisplay;
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    config.preferences.motion.level = Motion::Off;
    config.agent_list_visible = false;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    let session = ui.sessions.get_mut("session").context("session")?;
    session.entries.push(Entry {
        id: "tool".into(),
        kind: "commandExecution".into(),
        text: "output one\noutput two\noutput three\noutput four\nlast source line".into(),
        data: serde_json::json!({"command":"echo sample"}).into(),
        ..Default::default()
    });
    session.entries.push(Entry {
        id: "reason".into(),
        kind: "reasoning".into(),
        text: "Reason heading\nlast reasoning detail".into(),
        ..Default::default()
    });
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Unsent across modals");
    for display in [
        ToolDisplay::Collapsed,
        ToolDisplay::Compact,
        ToolDisplay::Expanded,
    ] {
        for reasoning in [false, true] {
            config.preferences.conversation.tool_display = display;
            config.preferences.conversation.reasoning_expanded = reasoning;
            ui.apply_settings(&config);
            for (id, last) in [
                ("tool", "last source line"),
                ("reason", "last reasoning detail"),
            ] {
                ui.action(Action::ToggleEntry(id.into()));
                assert!(native_frame(&mut ui, 100, 40)?.contains(last));
                ui.apply_settings(&config);
                assert!(native_frame(&mut ui, 100, 40)?.contains(last));
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(ui.modal.is_none());
                assert_eq!(
                    ui.positions
                        .get("session")
                        .context("position")?
                        .draft
                        .text(),
                    "Unsent across modals"
                );
            }
            let ordinary = native_frame(&mut ui, 100, 40)?;
            assert_eq!(
                ordinary.contains("last source line"),
                display == ToolDisplay::Expanded
            );
            assert_eq!(ordinary.contains("last reasoning detail"), reasoning);
        }
    }
    Ok(())
}
#[test]
fn manual_scroll_keeps_selected_source_draft_and_anchor_when_content_arrives() -> anyhow::Result<()>
{
    use crate::settings::AutoScroll;
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config {
        agent_list_visible: false,
        ..Default::default()
    };
    config.preferences.motion.level = Motion::Off;
    config.preferences.conversation.auto_scroll = AutoScroll::Manual;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    let session = ui.sessions.get_mut("session").context("session")?;
    for n in 0..30 {
        session.note("agentMessage", format!("reading row {n}"));
    }
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Keep this draft");
    ui.focus = Focus::Conversation;
    let frame = native_frame(&mut ui, 100, 30)?;
    let (y, line) = frame
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("reading row 0"))
        .context("visible row")?;
    let x = line.find("reading row 0").context("column")? as u16;
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), x),
        (MouseEventKind::Drag(MouseButton::Left), x + 13),
        (MouseEventKind::Up(MouseButton::Left), x + 13),
    ] {
        ui.mouse(MouseEvent {
            kind,
            column,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(ui.copy_chat_selection());
    let selected = ui.clipboard.take().context("selected source")?;
    let anchor = ui
        .positions
        .get("session")
        .context("position")?
        .scroll_anchor
        .clone();
    ui.sessions
        .get_mut("session")
        .context("session")?
        .note("agentMessage", "new streaming text\nmore content");
    native_frame(&mut ui, 100, 30)?;
    let position = ui.positions.get("session").context("position")?;
    assert_eq!(position.scroll_anchor, anchor);
    assert_eq!(position.draft.text(), "Keep this draft");
    assert_eq!(position.conversation, 0);
    assert!(ui.copy_chat_selection());
    assert_eq!(ui.clipboard.as_ref(), Some(&selected));
    Ok(())
}
#[test]
fn session_sort_uses_activity_or_recency_inside_existing_pin_groups() -> anyhow::Result<()> {
    use crate::settings::SessionSort;
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &Config::default(),
    );
    let seed = ui.sessions.get("session").context("session")?.summary();
    for (id, status, updated) in [
        ("pinned-old", Status::Idle, 1),
        ("pinned-active", Status::Running, 2),
        ("manual-first", Status::Idle, 3),
        ("recent", Status::Idle, 100),
        ("active", Status::Running, 5),
        (super::super::super::orchestrator::ID, Status::Idle, 0),
    ] {
        let mut summary = seed.clone();
        summary.id = id.into();
        summary.status = status;
        summary.updated = updated;
        ui.summaries.push(summary);
    }
    ui.pinned_sessions = ["pinned-old".into(), "pinned-active".into()]
        .into_iter()
        .collect();
    ui.session_order = vec![
        "manual-first".into(),
        "recent".into(),
        "active".into(),
        "pinned-old".into(),
        "pinned-active".into(),
    ];
    for (sort, want) in [
        (
            SessionSort::Manual,
            vec![
                super::super::super::orchestrator::ID,
                "pinned-old",
                "pinned-active",
                "manual-first",
                "recent",
                "active",
            ],
        ),
        (
            SessionSort::Recent,
            vec![
                super::super::super::orchestrator::ID,
                "pinned-active",
                "pinned-old",
                "recent",
                "active",
                "manual-first",
            ],
        ),
        (
            SessionSort::ActiveFirst,
            vec![
                super::super::super::orchestrator::ID,
                "pinned-active",
                "pinned-old",
                "active",
                "recent",
                "manual-first",
            ],
        ),
    ] {
        ui.preferences.layout.session_sort = sort;
        assert_eq!(
            ui.filtered()
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            want,
            "{sort:?}"
        );
    }
    assert_eq!(
        ui.session_order.first().map(String::as_str),
        Some("manual-first")
    );
    Ok(())
}

#[test]
fn native_event_receipt_time_survives_completion_and_serialization() -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &Config::default(),
    );
    let session = ui.sessions.get_mut("session").context("session")?;
    let before = chrono::Utc::now().timestamp_millis();
    super::super::super::engine::apply_event(
        session,
        &serde_json::json!({"method":"item/started","params":{"item":{"id":"native","type":"agentMessage","text":"First words"}}}),
    );
    let receipt = session
        .entries
        .last()
        .and_then(|e| e.timestamp)
        .context("receipt time")?;
    assert!((before..=chrono::Utc::now().timestamp_millis()).contains(&receipt));
    super::super::super::engine::apply_event(
        session,
        &serde_json::json!({"method":"item/completed","params":{"item":{"id":"native","type":"agentMessage","text":"Completed words"}}}),
    );
    let persisted: Entry = serde_json::from_value(serde_json::to_value(
        session.entries.last().context("entry")?,
    )?)?;
    assert_eq!(persisted.timestamp, Some(receipt));
    assert!(persisted.finished_at.is_some());
    Ok(())
}

#[test]
fn explicit_main_overrides_legacy_side_and_remains_keyboard_reachable() -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    config.preferences.layout.helper_placement = Some(crate::settings::HelperPlacement::Main);
    let mut ui = conversation_fixture(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        },
        &config,
    );
    ui.panels.view = Some(panels::View::Shell {
        id: "tool".into(),
        command: "echo example".into(),
    });
    ui.positions.entry("session".into()).or_default().draft = Editor::from("Keep draft");
    let main = native_frame(&mut ui, 100, 30)?;
    assert!(
        !ui.hits
            .iter()
            .any(|(_, a)| matches!(a, Action::Focus(Focus::Conversation))),
        "explicit Main must replace the conversation even with legacy right=true"
    );
    assert!(main.contains("Shell output"));
    ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(ui.panels.focused, "Main helper must be reachable by Tab");
    ui.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert!(ui.panels.scroll > 0);
    assert_eq!(
        ui.positions
            .get("session")
            .context("position")?
            .draft
            .text(),
        "Keep draft"
    );
    Ok(())
}
#[test]
fn legacy_move_toggle_persists_a_consistent_right_or_main_after_any_setting() -> anyhow::Result<()>
{
    use crate::settings::{HelperPlacement, SettingId, SettingValue};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    for (placement, want) in [
        (HelperPlacement::Right, "main"),
        (HelperPlacement::Bottom, "main"),
        (HelperPlacement::Main, "right"),
    ] {
        let mut config = Config::default();
        config.preferences.layout.helper_placement = Some(placement);
        config.agent_panel_right = placement != HelperPlacement::Main;
        storage.save_config(&config)?;
        let mut ui = conversation_fixture(storage.clone(), &config);
        ui.panels.view = Some(panels::View::Shell {
            id: "tool".into(),
            command: "echo example".into(),
        });
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        let saved = storage.load_config()?;
        assert_eq!(
            crate::settings::get(&saved, &SettingId::HelperPlacement),
            SettingValue::Choice(want.into())
        );
        assert!(saved.preferences.layout.helper_placement.is_none());
        ui.apply_settings(&saved);
        assert_eq!(ui.panels.right, want == "right");
        assert!(ui.preferences.layout.helper_placement.is_none());
    }
    Ok(())
}

#[test]
fn opening_changes_moves_a_main_helper_to_right_and_survives_refresh() -> anyhow::Result<()> {
    use crate::settings::{HelperPlacement, SettingId, SettingValue};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    for (placement, want) in [
        (HelperPlacement::Main, "right"),
        (HelperPlacement::Bottom, "bottom"),
        (HelperPlacement::Right, "right"),
    ] {
        let mut config = Config::default();
        config.preferences.layout.helper_placement = Some(placement);
        config.agent_panel_right = placement != HelperPlacement::Main;
        storage.save_config(&config)?;
        let mut ui = conversation_fixture(storage.clone(), &config);
        ui.panels.view = Some(panels::View::Shell {
            id: "tool".into(),
            command: "echo example".into(),
        });
        ui.toggle_changes();
        assert!(ui.changes_visible && ui.panels.right);
        let saved = storage.load_config()?;
        assert_eq!(
            crate::settings::get(&saved, &SettingId::HelperPlacement),
            SettingValue::Choice(want.into()),
            "opening Changes must persist its effective helper move for {placement:?}"
        );
        ui.apply_settings(&saved);
        assert!(ui.changes_visible && ui.panels.right);
        native_frame(&mut ui, 120, 40)?;
        assert!(
            ui.hits.iter().any(|(rect, action)| !rect.is_empty()
                && matches!(action, Action::Focus(Focus::Changes))),
            "Changes remains visible after refresh"
        );
    }
    Ok(())
}
