//! New-session defaults for paired clients (`agents.defaults`,
//! `agents.defaults.set`): the defaults the terminal's "Default repository,
//! model, reasoning and worktree" screen edits, read from and saved to this
//! machine's settings, so the terminal, phones and agents share them. Both
//! answer with the saved defaults and the effective ones the service starts
//! sessions with (the newest Claude Opus when no model is saved, Auto on a
//! server), each marked automatic or saved.
use crate::{
    agents::{
        Reply, Request, client,
        effective::{Defaults, Start},
        permissions::{self, CodexPreset},
        provider::Provider,
    },
    settings::{SettingId, SettingValue},
    storage::Storage,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

/// `agents.defaults`: the saved and effective defaults, and the choices that
/// depend on the provider (models come from `agents.models`, repositories
/// from `agents.repositories`).
pub fn get(storage: &Storage) -> Result<Value> {
    Ok(view(&effective(storage)?))
}

/// This machine's defaults as its service starts sessions with them.
fn effective(storage: &Storage) -> Result<Defaults> {
    match client::request(storage, Request::AgentDefaults { server: None })? {
        Reply::AgentDefaults(defaults) => Ok(*defaults),
        Reply::Error(error) => anyhow::bail!("{error}"),
        _ => anyhow::bail!("Unexpected new-session defaults reply"),
    }
}

/// `agents.defaults.set`: changes the fields present in `params` (`null` or
/// `""` sets one back to automatic) with the terminal's settings transaction.
/// A model of another provider switches the provider, which drops the saved
/// effort and starting mode unless they are given too.
pub fn set(storage: &Storage, params: &Value) -> Result<Value> {
    let provider = match text(params, "provider")? {
        None => None,
        Some(None) => Some(None),
        Some(Some(key)) => Some(Some(match key.as_str() {
            "codex" => Provider::Codex,
            "claude" => Provider::Claude,
            _ => anyhow::bail!("Unknown provider `{key}`; choose codex or claude"),
        })),
    };
    let model = text(params, "model")?;
    let effort = text(params, "effort")?;
    let mode = text(params, "startingMode")?;
    let mode_given = mode.is_some();
    let repository = text(params, "repository")?;
    let preset = match text(params, "codexPreset")? {
        None => None,
        Some(None) => Some(None),
        Some(Some(preset)) => Some(Some(CodexPreset::parse(&preset).with_context(|| {
            format!("Unknown Codex preset `{preset}`; choose Ask, Auto, Full or ReadOnly")
        })?)),
    };
    let isolated = match params.get("isolated") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(isolated)) => Some(*isolated),
        Some(_) => anyhow::bail!("Invalid parameter `isolated`"),
    };
    if let Some(Some(path)) = &repository {
        let place = if super::api::config(storage).server.enabled {
            "server"
        } else {
            "computer"
        };
        ensure!(
            Path::new(path).is_dir(),
            "{path} isn't a folder on this {place}"
        );
    }
    let mut draft = storage.settings_draft()?;
    let current = draft.config().agent_defaults.resolved_provider();
    let target = provider.flatten().or_else(|| {
        model
            .clone()
            .flatten()
            .map(|m| Provider::for_model(Some(&m)))
    });
    if let Some(target) = target {
        if target != current {
            for (id, given) in [
                (SettingId::DefaultModel, model.is_some()),
                (SettingId::DefaultEffort, effort.is_some()),
                (SettingId::StartingMode, mode.is_some()),
            ] {
                if !given {
                    draft.set(id, SettingValue::Text(None))?;
                }
            }
        }
        draft.set(
            SettingId::Provider,
            SettingValue::Choice(target.key().into()),
        )?;
    }
    for (id, value) in [
        (SettingId::DefaultModel, model),
        (SettingId::DefaultEffort, effort),
        (SettingId::StartingMode, mode),
        (SettingId::DefaultRepository, repository),
    ] {
        if let Some(value) = value {
            draft.set(id, SettingValue::Text(value))?;
        }
    }
    // Back to an automatic provider: a starting mode it wouldn't offer goes
    // too, unless one is given.
    let automatic = |defaults: &mut crate::storage::AgentDefaults| {
        defaults.provider = None;
        let provider = defaults.resolved_provider();
        if !mode_given
            && defaults.starting_mode.as_ref().is_some_and(|mode| {
                !permissions::mode_choices(provider)
                    .iter()
                    .any(|choice| &choice.id == mode)
            })
        {
            defaults.starting_mode = None;
        }
    };
    // Checked first for a plain message; saving checks again under the lock.
    let mut checked = draft.config().agent_defaults.clone();
    if provider == Some(None) {
        automatic(&mut checked);
    }
    checked.validate()?;
    let saved = storage.save_settings_draft(&mut draft)?.agent_defaults;
    // The settings registry has no automatic provider and no preset.
    let cleared = provider == Some(None) && saved.provider.is_some();
    let isolated = isolated.filter(|i| *i != saved.isolated);
    let preset = preset.filter(|p| *p != saved.codex_preset);
    if cleared || isolated.is_some() || preset.is_some() {
        storage.try_patch_config(|config| {
            let defaults = &mut config.agent_defaults;
            if cleared {
                automatic(defaults);
            }
            if let Some(isolated) = isolated {
                defaults.isolated = isolated;
            }
            if let Some(preset) = preset {
                defaults.codex_preset = preset;
            }
            defaults.validate()
        })?;
    }
    get(storage)
}

/// A text field: absent (`None`), cleared (`Some(None)`), or set.
fn text(params: &Value, key: &str) -> Result<Option<Option<String>>> {
    match params.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(text)) => Ok(Some(
            Some(text.trim().to_owned()).filter(|text| !text.is_empty()),
        )),
        Some(_) => anyhow::bail!("Invalid parameter `{key}`"),
    }
}

fn view(defaults: &Defaults) -> Value {
    let saved = &defaults.saved;
    let providers: Vec<Value> = [Provider::Codex, Provider::Claude]
        .into_iter()
        .map(|provider| {
            let modes: Vec<Value> = permissions::mode_choices(provider)
                .into_iter()
                .map(|mode| {
                    json!({"id": mode.id, "label": mode.label, "description": mode.description})
                })
                .collect();
            let Start {
                mode,
                preset,
                automatic_mode,
                automatic_preset,
            } = defaults.start(provider);
            let mut entry = json!({
                "id": provider.key(),
                "label": provider.label(),
                "startingModes": modes,
                "startingMode": mode,
                "startingModeAutomatic": automatic_mode,
            });
            if provider == Provider::Codex
                && let Some(entry) = entry.as_object_mut()
            {
                let presets: Vec<Value> = CodexPreset::ALL
                    .into_iter()
                    .map(|preset| {
                        json!({"id": preset.id(), "label": preset.label(),
                               "description": preset_description(preset)})
                    })
                    .collect();
                entry.insert("presets".into(), json!(presets));
                entry.insert("codexPreset".into(), json!(preset.map(|p| p.id())));
                entry.insert("codexPresetAutomatic".into(), json!(automatic_preset));
            }
            entry
        })
        .collect();
    json!({
        "provider": saved.resolved_provider().key(),
        "savedProvider": saved.provider.map(Provider::key),
        "model": saved.model,
        "effort": saved.effort,
        "repository": saved.repository.as_ref().map(|path| path.display().to_string()),
        "startingMode": saved.starting_mode,
        "codexPreset": saved.codex_preset.map(|p| p.id()),
        "isolated": saved.isolated,
        "server": defaults.server,
        "providers": providers,
        "effective": {
            "provider": defaults.provider.key(),
            "model": defaults.model,
            "effort": defaults.effort,
            "modelAutomatic": defaults.automatic_model,
            "effortAutomatic": defaults.automatic_effort,
        },
    })
}

fn preset_description(preset: CodexPreset) -> &'static str {
    match preset {
        CodexPreset::Ask => "Workspace writes; asks before anything else",
        CodexPreset::Auto => "Workspace writes; Codex reviews other requests itself",
        CodexPreset::Full => "No sandbox and no approvals",
        CodexPreset::ReadOnly => "Reads only; asks before changes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::{effective::tests::catalog, server::Served},
        storage::AgentDefaults,
    };
    use anyhow::Context;

    /// This machine's service with the usual catalog (Claude Opus 4.7).
    fn served(dir: &tempfile::TempDir) -> Result<(Served, Storage)> {
        let served = Served::start_with(dir.path().into(), Vec::new(), catalog())?;
        let storage = served.store.storage.clone();
        Ok((served, storage))
    }

    fn error(result: Result<Value>) -> Result<String> {
        Ok(result.err().context("expected an error")?.to_string())
    }

    fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
        value.pointer(pointer).unwrap_or(&Value::Null)
    }

    #[test]
    fn defaults_list_the_saved_values_and_each_providers_starting_modes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        let fresh = get(&storage)?;
        assert_eq!(fresh.get("provider"), Some(&json!("codex")));
        assert_eq!(fresh.get("savedProvider"), Some(&Value::Null));
        assert_eq!(fresh.get("model"), Some(&Value::Null));
        assert_eq!(fresh.get("isolated"), Some(&json!(true)));
        assert_eq!(fresh.get("server"), Some(&json!(false)));
        // Nothing saved: the newest Opus at xhigh, marked automatic.
        assert_eq!(
            fresh.get("effective"),
            Some(
                &json!({"provider": "claude", "model": "claude/claude-opus-4-7",
                         "effort": "xhigh", "modelAutomatic": true, "effortAutomatic": true})
            )
        );
        storage.update_config(|config| {
            config.agent_defaults = AgentDefaults {
                provider: None,
                starting_mode: Some("acceptEdits".into()),
                repository: Some("/code/difu".into()),
                model: Some("claude/opus".into()),
                effort: Some("xhigh".into()),
                isolated: true,
                codex_preset: Some(CodexPreset::ReadOnly),
            }
        })?;
        let saved = get(&storage)?;
        assert_eq!(saved.get("provider"), Some(&json!("claude")), "inferred");
        assert_eq!(saved.get("savedProvider"), Some(&Value::Null));
        assert_eq!(saved.get("model"), Some(&json!("claude/opus")));
        assert_eq!(saved.get("effort"), Some(&json!("xhigh")));
        assert_eq!(saved.get("repository"), Some(&json!("/code/difu")));
        assert_eq!(saved.get("startingMode"), Some(&json!("acceptEdits")));
        assert_eq!(saved.get("codexPreset"), Some(&json!("ReadOnly")));
        assert_eq!(
            saved.get("effective"),
            Some(
                &json!({"provider": "claude", "model": "claude/opus", "effort": "xhigh",
                         "modelAutomatic": false, "effortAutomatic": false})
            )
        );
        let providers = saved
            .get("providers")
            .and_then(Value::as_array)
            .context("providers")?;
        let ids: Vec<_> = providers.iter().filter_map(|p| p.get("id")).collect();
        assert_eq!(ids, [&json!("codex"), &json!("claude")]);
        let claude_modes: Vec<_> = providers
            .get(1)
            .and_then(|p| p.get("startingModes"))
            .and_then(Value::as_array)
            .context("modes")?
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .collect();
        let expected: Vec<_> = permissions::mode_choices(Provider::Claude)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(claude_modes, expected);
        assert_eq!(
            providers.get(1).and_then(|p| p.get("label")),
            Some(&json!("Claude Code"))
        );
        // Each provider's start: the saved Claude mode, Codex's saved preset.
        assert_eq!(at(&saved, "/providers/1/startingMode"), "acceptEdits");
        assert_eq!(at(&saved, "/providers/1/startingModeAutomatic"), false);
        assert_eq!(at(&saved, "/providers/0/startingMode"), &Value::Null);
        assert_eq!(at(&saved, "/providers/0/codexPreset"), "ReadOnly");
        assert_eq!(at(&saved, "/providers/0/codexPresetAutomatic"), false);
        let presets: Vec<_> = at(&saved, "/providers/0/presets")
            .as_array()
            .context("presets")?
            .iter()
            .filter_map(|p| p.get("id").and_then(Value::as_str))
            .collect();
        assert_eq!(presets, ["Ask", "Auto", "Full", "ReadOnly"]);
        assert_eq!(at(&saved, "/providers/1/presets"), &Value::Null);
        Ok(())
    }

    #[test]
    fn a_server_reports_auto_starts_as_automatic() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        storage.update_config(|config| config.server.enabled = true)?;
        let server = get(&storage)?;
        assert_eq!(server.get("server"), Some(&json!(true)));
        assert_eq!(at(&server, "/providers/1/startingMode"), "auto");
        assert_eq!(at(&server, "/providers/1/startingModeAutomatic"), true);
        assert_eq!(at(&server, "/providers/0/startingMode"), &Value::Null);
        assert_eq!(at(&server, "/providers/0/codexPreset"), "Auto");
        assert_eq!(at(&server, "/providers/0/codexPresetAutomatic"), true);
        // A saved preset wins and is not automatic.
        set(&storage, &json!({"codexPreset": "Ask"}))?;
        let saved = get(&storage)?;
        assert_eq!(at(&saved, "/providers/0/codexPreset"), "Ask");
        assert_eq!(at(&saved, "/providers/0/codexPresetAutomatic"), false);
        assert_eq!(saved.get("codexPreset"), Some(&json!("Ask")));
        Ok(())
    }

    #[test]
    fn saved_defaults_persist_where_the_terminal_reads_them() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        let repository = dir.path().join("repo");
        std::fs::create_dir(&repository)?;
        // Unrelated settings and unknown fields survive the save.
        std::fs::write(
            &storage.config,
            serde_json::to_vec(&json!({"theme": "matrix", "future_field": {"kept": true}}))?,
        )?;
        let path = repository.display().to_string();
        let saved = set(
            &storage,
            &json!({"provider": "claude", "model": "claude/opus", "effort": "xhigh",
                    "repository": path, "startingMode": "plan", "isolated": true,
                    "codexPreset": "approve for me"}),
        )?;
        assert_eq!(saved.get("model"), Some(&json!("claude/opus")));
        let config = storage.load_config()?;
        assert_eq!(
            config.agent_defaults,
            AgentDefaults {
                provider: Some(Provider::Claude),
                starting_mode: Some("plan".into()),
                repository: Some(repository.clone()),
                model: Some("claude/opus".into()),
                effort: Some("xhigh".into()),
                isolated: true,
                codex_preset: Some(CodexPreset::Auto),
            }
        );
        assert_eq!(config.theme.as_deref(), Some("matrix"));
        let raw: Value = serde_json::from_slice(&std::fs::read(&storage.config)?)?;
        assert_eq!(raw.pointer("/future_field/kept"), Some(&json!(true)));
        assert_eq!(get(&storage)?, saved, "read back as saved");

        // Absent fields stay; null and "" set them back to automatic.
        set(&storage, &json!({"effort": "high"}))?;
        let config = storage.load_config()?;
        assert_eq!(config.agent_defaults.effort.as_deref(), Some("high"));
        assert_eq!(config.agent_defaults.model.as_deref(), Some("claude/opus"));
        assert_eq!(config.agent_defaults.repository, Some(repository));
        set(
            &storage,
            &json!({"model": null, "repository": "", "startingMode": null, "codexPreset": null}),
        )?;
        let config = storage.load_config()?;
        assert_eq!(config.agent_defaults.model, None);
        assert_eq!(config.agent_defaults.repository, None);
        assert_eq!(config.agent_defaults.starting_mode, None);
        assert_eq!(config.agent_defaults.codex_preset, None);
        assert_eq!(config.agent_defaults.provider, Some(Provider::Claude));

        set(&storage, &json!({"isolated": false}))?;
        assert!(!storage.load_config()?.agent_defaults.isolated);
        Ok(())
    }

    #[test]
    fn a_null_provider_returns_the_model_to_automatic() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        storage.update_config(|config| {
            config.agent_defaults.provider = Some(Provider::Codex);
            config.agent_defaults.starting_mode = Some("plan".into());
        })?;
        let codex = get(&storage)?;
        assert_eq!(
            at(&codex, "/effective/provider"),
            "codex",
            "Codex was saved"
        );
        assert_eq!(at(&codex, "/effective/model"), &Value::Null);
        let automatic = set(
            &storage,
            &json!({"provider": null, "model": null, "effort": null}),
        )?;
        assert_eq!(automatic.get("savedProvider"), Some(&Value::Null));
        assert_eq!(at(&automatic, "/effective/model"), "claude/claude-opus-4-7");
        assert_eq!(at(&automatic, "/effective/modelAutomatic"), true);
        let defaults = storage.load_config()?.agent_defaults;
        assert_eq!(defaults.provider, None);
        assert_eq!(
            defaults.starting_mode.as_deref(),
            Some("plan"),
            "both providers offer plan"
        );
        // A Claude mode with the automatic model saves Claude, which keeps
        // the model automatic.
        let claude = set(
            &storage,
            &json!({"provider": "claude", "startingMode": "acceptEdits"}),
        )?;
        assert_eq!(at(&claude, "/effective/model"), "claude/claude-opus-4-7");
        assert_eq!(at(&claude, "/providers/1/startingMode"), "acceptEdits");
        // Back to automatic drops the mode only Claude Code offers.
        set(&storage, &json!({"provider": null}))?;
        let defaults = storage.load_config()?.agent_defaults;
        assert_eq!((defaults.provider, defaults.starting_mode), (None, None));
        Ok(())
    }

    #[test]
    fn another_providers_model_switches_and_drops_what_belonged_to_the_old_one() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        storage.update_config(|config| {
            config.agent_defaults.provider = Some(Provider::Codex);
            config.agent_defaults.model = Some("gpt-5.6".into());
            config.agent_defaults.effort = Some("low".into());
            config.agent_defaults.starting_mode = Some("plan".into());
        })?;
        let saved = set(&storage, &json!({"model": "claude/opus"}))?;
        assert_eq!(saved.get("provider"), Some(&json!("claude")));
        let defaults = storage.load_config()?.agent_defaults;
        assert_eq!(defaults.provider, Some(Provider::Claude));
        assert_eq!(defaults.model.as_deref(), Some("claude/opus"));
        assert_eq!(defaults.effort, None, "Codex's effort stays with Codex");
        assert_eq!(defaults.starting_mode, None);

        // Choosing only a provider clears the other provider's model.
        set(&storage, &json!({"provider": "codex", "effort": "high"}))?;
        let defaults = storage.load_config()?.agent_defaults;
        assert_eq!(defaults.provider, Some(Provider::Codex));
        assert_eq!(defaults.model, None);
        assert_eq!(defaults.effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn invalid_defaults_are_refused_without_saving() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_served, storage) = served(&dir)?;
        storage.update_config(|config| config.agent_defaults.model = Some("gpt-5.6".into()))?;
        let before = std::fs::read(&storage.config)?;
        let mismatch = error(set(
            &storage,
            &json!({"provider": "codex", "model": "claude/opus"}),
        ))?;
        assert!(mismatch.contains("different provider"), "{mismatch}");
        let mode = error(set(
            &storage,
            &json!({"model": "claude/opus", "startingMode": "bogus"}),
        ))?;
        assert!(mode.contains("Starting mode"), "{mode}");
        // Codex has no "acceptEdits" mode.
        let codex_mode = error(set(&storage, &json!({"startingMode": "acceptEdits"})))?;
        assert!(codex_mode.contains("Starting mode"), "{codex_mode}");
        let provider = error(set(&storage, &json!({"provider": "gemini"})))?;
        assert!(provider.contains("Unknown provider"), "{provider}");
        let preset = error(set(&storage, &json!({"codexPreset": "yolo"})))?;
        assert!(preset.contains("Unknown Codex preset"), "{preset}");
        let missing = dir.path().join("missing");
        let folder = error(set(
            &storage,
            &json!({"repository": missing.display().to_string()}),
        ))?;
        assert!(
            folder.contains("isn't a folder on this computer"),
            "{folder}"
        );
        let kind = error(set(&storage, &json!({"isolated": "yes"})))?;
        assert!(kind.contains("`isolated`"), "{kind}");
        let effort = error(set(&storage, &json!({"effort": 3})))?;
        assert!(effort.contains("`effort`"), "{effort}");
        assert_eq!(std::fs::read(&storage.config)?, before, "nothing saved");
        Ok(())
    }
}
