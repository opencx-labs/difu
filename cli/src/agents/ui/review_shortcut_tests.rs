use super::*;
use crate::settings::BindingKey;
use crate::shortcuts::{ids::*, key};
use anyhow::Result;

pub(crate) fn install_review(ui: &mut Ui, editing: bool) {
    let mut app = crate::app::App::new(ui.storage.clone(), ui.shortcut_config.clone());
    app.home = false;
    app.view = crate::app::View::Diff;
    app.focus = crate::app::Focus::Navigation;
    if editing {
        app.filters.focused = Some(crate::filter::Kind::Files);
    }
    ui.panels.view = Some(panels::View::PullRequest { app: Box::new(app) });
    ui.panels.focused = true;
}
pub(crate) fn review(ui: &Ui) -> Result<&crate::app::App> {
    match &ui.panels.view {
        Some(panels::View::PullRequest { app }) => Ok(app),
        _ => anyhow::bail!("review missing"),
    }
}

#[test]
fn embedded_review_keeps_host_focus_order_with_custom_review_navigation() -> Result<()> {
    use crate::settings::HelperPlacement;
    for placement in [
        HelperPlacement::Right,
        HelperPlacement::Bottom,
        HelperPlacement::Main,
    ] {
        for (action, code, initial, next) in [
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                crate::app::Focus::Navigation,
                crate::app::Focus::Content,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                crate::app::Focus::Content,
                crate::app::Focus::Navigation,
            ),
        ] {
            for custom in [false, true] {
                let (mut ui, _dir, mut config) = super::shortcut_tests::fixture(None)?;
                config.preferences.layout.helper_placement = Some(placement);
                if custom {
                    config
                        .preferences
                        .keyboard
                        .bindings
                        .insert(action.0.into(), key(BindingKey::Function(12), 0));
                }
                ui.apply_settings(&config);
                install_review(&mut ui, false);
                if let Some(panels::View::PullRequest { app }) = &mut ui.panels.view {
                    app.focus = initial;
                }
                if custom {
                    ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                    assert_eq!(review(&ui)?.focus, initial);
                }
                let active = KeyEvent::new(
                    if custom { KeyCode::F(12) } else { code },
                    KeyModifiers::NONE,
                );
                ui.key(active);
                assert_eq!(review(&ui)?.focus, next);
                assert!(ui.panels.focused);
                ui.key(active);
                assert_eq!(
                    ui.panels.focused,
                    placement == HelperPlacement::Main,
                    "side helpers leave after App panes; Main stays in App"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn embedded_review_rendered_custom_tabs_and_blank_mouse_targets() -> Result<()> {
    use crate::settings::HelperPlacement;
    for placement in [
        HelperPlacement::Right,
        HelperPlacement::Bottom,
        HelperPlacement::Main,
    ] {
        for width in [40, 80, 160] {
            if width == 40 && placement != HelperPlacement::Main {
                continue;
            }
            let (mut ui, dir, mut config) = super::shortcut_tests::fixture(None)?;
            config
                .preferences
                .keyboard
                .bindings
                .insert(REVIEW_TAB_1.0.into(), key(BindingKey::Function(12), 0));
            config.preferences.layout.helper_placement = Some(placement);
            ui.apply_settings(&config);
            let mut app = crate::ui::tests::guide_app(dir.path());
            app.config = config.clone();
            app.view = crate::app::View::Diff;
            ui.panels.view = Some(panels::View::PullRequest { app: Box::new(app) });
            ui.panels.focused = true;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40))?;
            terminal.draw(|frame| ui.draw(frame))?;
            if width == 40 {
                assert!(!review(&ui)?.hits.iter().any(|(_, action)| matches!(
                    action,
                    crate::app::Action::SetView(crate::app::View::Overview)
                )));
                let area = review(&ui)?
                    .render_area
                    .ok_or_else(|| anyhow::anyhow!("narrow embedded area"))?;
                assert!(area.width > 2 && area.height > 3);
                let column = area.x + 2;
                let row = area.y + 3;
                assert_eq!(terminal.backend().buffer()[(column, row)].symbol(), " ");
                ui.mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                });
                assert_eq!(review(&ui)?.view, crate::app::View::Diff);
                config.agent_list_visible = false;
                ui.apply_settings(&config);
                terminal.draw(|frame| ui.draw(frame))?;
            }
            let app = review(&ui)?;
            let target = app
                .hits
                .iter()
                .find_map(|(rect, action)| {
                    matches!(
                        action,
                        crate::app::Action::SetView(crate::app::View::Overview)
                    )
                    .then_some(*rect)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("missing rendered Overview target {placement:?} {width}")
                })?;
            let text = (target.x..target.right())
                .map(|x| terminal.backend().buffer()[(x, target.y)].symbol())
                .collect::<String>();
            assert!(
                text.contains("F12"),
                "visible target lacks effective hint: {text}"
            );
            let area = app
                .render_area
                .ok_or_else(|| anyhow::anyhow!("embedded area"))?;
            let blank = (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .find(|&(x, y)| {
                    terminal.backend().buffer()[(x, y)].symbol() == " "
                        && !app.hits.iter().any(|(r, _)| r.contains((x, y).into()))
                });
            if let Some((column, row)) = blank {
                ui.mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                });
                assert_eq!(review(&ui)?.view, crate::app::View::Diff);
            }
            ui.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: target.x,
                row: target.y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(
                review(&ui)?.view,
                crate::app::View::Overview,
                "{placement:?} {width}"
            );
        }
    }
    Ok(())
}

pub(crate) fn install_compose(ui: &mut Ui) {
    install_review(ui, false);
    if let Some(panels::View::PullRequest { app }) = &mut ui.panels.view {
        app.modal = Some(crate::app::Modal::Workflow(Box::new(
            crate::workflow::Wizard::Compose(crate::workflow::Compose {
                key: crate::model::PrKey {
                    owner: "example".into(),
                    repo: "repo".into(),
                    number: 1,
                },
                head: "head".into(),
                kind: crate::workflow::Kind::PrComment,
                editor: "draft".into(),
                choice: 0,
                focus: 0,
                mention: 0,
            }),
        )));
    }
}

#[test]
fn embedded_review_custom_host_and_review_actions_keep_default_escape_precedence() -> Result<()> {
    use crate::settings::HelperPlacement;
    for placement in [
        HelperPlacement::Right,
        HelperPlacement::Bottom,
        HelperPlacement::Main,
    ] {
        let (mut ui, _dir, mut config) = super::shortcut_tests::fixture(None)?;
        config.preferences.layout.helper_placement = Some(placement);
        config
            .preferences
            .keyboard
            .bindings
            .insert(HELPER_PLACEMENT.0.into(), key(BindingKey::Function(12), 0));
        config
            .preferences
            .keyboard
            .bindings
            .insert(REVIEW_HELP.0.into(), key(BindingKey::Function(11), 0));
        ui.storage.update_config(|saved| *saved = config.clone())?;
        ui.apply_settings(&config);
        install_review(&mut ui, false);
        ui.key(KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE));
        assert!(matches!(
            review(&ui)?.modal,
            Some(crate::app::Modal::Help(_))
        ));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(review(&ui)?.modal.is_none());
        let before = ui.panels.right;
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        assert_eq!(ui.panels.right, before);
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert_eq!(ui.panels.right, !before);
        assert_eq!(
            ui.storage
                .load_config()?
                .preferences
                .layout
                .helper(ui.panels.right),
            if before {
                HelperPlacement::Main
            } else {
                HelperPlacement::Right
            }
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            ui.panels.view.is_none(),
            "host Escape closes content helper before App Back"
        );
    }
    Ok(())
}

#[test]
fn unfocused_embedded_filter_keeps_host_helper_controls_after_outside_click() -> Result<()> {
    use crate::settings::HelperPlacement;
    for (phase, custom) in super::shortcut_tests::BINDING_PHASES {
        for placement in [
            HelperPlacement::Right,
            HelperPlacement::Bottom,
            HelperPlacement::Main,
        ] {
            for (action, old) in [
                (HELPER_PLACEMENT, KeyCode::Char('p')),
                (HELPER_VISIBILITY, KeyCode::Char(']')),
            ] {
                let (mut ui, _dir, mut config) =
                    super::shortcut_tests::binding_fixture(action, phase)?;
                config.preferences.layout.helper_placement = Some(placement);
                ui.apply_settings(&config);
                install_review(&mut ui, true);
                if let Some(panels::View::PullRequest { app }) = &mut ui.panels.view {
                    app.filters
                        .editor_mut(crate::filter::Kind::Files)
                        .insert("retained filter");
                }
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40))?;
                terminal.draw(|frame| ui.draw(frame))?;
                let target = ui
                    .hits
                    .iter()
                    .find_map(|(rect, action)| {
                        let wanted = if placement == HelperPlacement::Main {
                            Focus::List
                        } else {
                            Focus::Composer
                        };
                        matches!(action, Action::Focus(focus) if *focus == wanted).then_some(*rect)
                    })
                    .ok_or_else(|| anyhow::anyhow!("rendered host target"))?;
                ui.mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: target.x,
                    row: target.y,
                    modifiers: KeyModifiers::NONE,
                });
                assert!(!ui.panels.focused, "outside click must return to host");
                let before = ui.panels.right;
                if custom {
                    ui.key(KeyEvent::new(old, KeyModifiers::ALT));
                    assert_eq!(ui.panels.right, before);
                    assert!(!ui.panels.hidden);
                }
                let event = if custom {
                    KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE)
                } else {
                    KeyEvent::new(old, KeyModifiers::ALT)
                };
                ui.key(event);
                if action == HELPER_PLACEMENT {
                    assert_eq!(ui.panels.right, !before, "{phase} {placement:?}");
                } else {
                    assert!(ui.panels.hidden, "{phase} {placement:?}");
                    ui.key(event);
                    assert!(!ui.panels.hidden, "hidden App must not block host show");
                }
                assert_eq!(
                    review(&ui)?.filters.focused,
                    Some(crate::filter::Kind::Files)
                );
                assert_eq!(
                    review(&ui)?
                        .filters
                        .editor(crate::filter::Kind::Files)
                        .text(),
                    "retained filter"
                );
                assert_eq!(
                    ui.positions
                        .get("one")
                        .ok_or_else(|| anyhow::anyhow!("host draft"))?
                        .draft
                        .text(),
                    "unsent draft"
                );
            }
        }
    }
    Ok(())
}
