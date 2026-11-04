//! New-session defaults for paired clients (`agents.defaults`,
//! `agents.defaults.set`): the defaults the terminal's "Default repository,
//! model, reasoning and worktree" screen edits, read from and saved to this
//! machine's settings, so the terminal, phones and agents share them.
use crate::{
    agents::{permissions, provider::Provider},
    settings::{SettingId, SettingValue},
    storage::{AgentDefaults, Storage},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

/// `agents.defaults`: the saved defaults and the choices that depend on the
/// provider (models come from `agents.models`, repositories from
/// `agents.repositories`).
pub fn get(storage: &Storage) -> Value {
    view(&super::api::config(storage).agent_defaults)
}

/// `agents.defaults.set`: changes the fields present in `params` (`null` or
/// `""` sets one back to automatic) with the terminal's settings transaction.
/// A model of another provider switches the provider, which drops the saved
/// effort and starting mode unless they are given too.
pub fn set(storage: &Storage, params: &Value) -> Result<Value> {
    let provider = match text(params, "provider")?.flatten() {
        None => None,
        Some(key) => Some(match key.as_str() {
            "codex" => Provider::Codex,
            "claude" => Provider::Claude,
            _ => anyhow::bail!("Unknown provider `{key}`; choose codex or claude"),
        }),
    };
    let model = text(params, "model")?;
    let effort = text(params, "effort")?;
    let mode = text(params, "startingMode")?;
    let repository = text(params, "repository")?;
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
    let target = provider.or_else(|| {
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
    // Checked first for a plain message; saving checks again under the lock.
    draft.config().agent_defaults.validate()?;
    let mut config = storage.save_settings_draft(&mut draft)?;
    if let Some(isolated) = isolated.filter(|i| *i != config.agent_defaults.isolated) {
        config = storage.try_patch_config(|config| {
            config.agent_defaults.isolated = isolated;
            Ok(())
        })?;
    }
    Ok(view(&config.agent_defaults))
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

fn view(defaults: &AgentDefaults) -> Value {
    let providers: Vec<Value> = [Provider::Codex, Provider::Claude]
        .into_iter()
        .map(|provider| {
            let modes: Vec<Value> = permissions::mode_choices(provider)
                .into_iter()
                .map(|mode| {
                    json!({"id": mode.id, "label": mode.label, "description": mode.description})
                })
                .collect();
            json!({"id": provider.key(), "label": provider.label(), "startingModes": modes})
        })
        .collect();
    json!({
        "provider": defaults.resolved_provider().key(),
        "model": defaults.model,
        "effort": defaults.effort,
        "repository": defaults.repository.as_ref().map(|path| path.display().to_string()),
        "startingMode": defaults.starting_mode,
        "isolated": defaults.isolated,
        "providers": providers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn storage(dir: &tempfile::TempDir) -> Storage {
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        }
    }

    fn error(result: Result<Value>) -> Result<String> {
        Ok(result.err().context("expected an error")?.to_string())
    }

    #[test]
    fn defaults_list_the_saved_values_and_each_providers_starting_modes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let fresh = get(&storage);
        assert_eq!(fresh.get("provider"), Some(&json!("codex")));
        assert_eq!(fresh.get("model"), Some(&Value::Null));
        assert_eq!(fresh.get("isolated"), Some(&json!(true)));
        storage.update_config(|config| {
            config.agent_defaults = AgentDefaults {
                provider: None,
                starting_mode: Some("acceptEdits".into()),
                repository: Some("/code/difu".into()),
                model: Some("claude/opus".into()),
                effort: Some("xhigh".into()),
                isolated: true,
            }
        })?;
        let saved = get(&storage);
        assert_eq!(saved.get("provider"), Some(&json!("claude")), "inferred");
        assert_eq!(saved.get("model"), Some(&json!("claude/opus")));
        assert_eq!(saved.get("effort"), Some(&json!("xhigh")));
        assert_eq!(saved.get("repository"), Some(&json!("/code/difu")));
        assert_eq!(saved.get("startingMode"), Some(&json!("acceptEdits")));
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
        Ok(())
    }

    #[test]
    fn saved_defaults_persist_where_the_terminal_reads_them() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
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
                    "repository": path, "startingMode": "plan", "isolated": true}),
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
            }
        );
        assert_eq!(config.theme.as_deref(), Some("matrix"));
        let raw: Value = serde_json::from_slice(&std::fs::read(&storage.config)?)?;
        assert_eq!(raw.pointer("/future_field/kept"), Some(&json!(true)));
        assert_eq!(get(&storage), saved, "read back as saved");

        // Absent fields stay; null and "" set them back to automatic.
        set(&storage, &json!({"effort": "high"}))?;
        let config = storage.load_config()?;
        assert_eq!(config.agent_defaults.effort.as_deref(), Some("high"));
        assert_eq!(config.agent_defaults.model.as_deref(), Some("claude/opus"));
        assert_eq!(config.agent_defaults.repository, Some(repository));
        set(
            &storage,
            &json!({"model": null, "repository": "", "startingMode": null}),
        )?;
        let config = storage.load_config()?;
        assert_eq!(config.agent_defaults.model, None);
        assert_eq!(config.agent_defaults.repository, None);
        assert_eq!(config.agent_defaults.starting_mode, None);
        assert_eq!(config.agent_defaults.provider, Some(Provider::Claude));

        set(&storage, &json!({"isolated": false}))?;
        assert!(!storage.load_config()?.agent_defaults.isolated);
        Ok(())
    }

    #[test]
    fn another_providers_model_switches_and_drops_what_belonged_to_the_old_one() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
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
        let storage = storage(&dir);
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
