//! Effective new-session defaults: what a session started without choices gets
//! on this machine. Saved `AgentDefaults` fields win. Without a saved model
//! (and unless Codex is the saved provider) sessions use the newest Claude
//! Opus in this machine's model catalog at `xhigh`, or that model's highest
//! effort. On a difu server, sessions without a saved starting mode or preset
//! start in Claude Code's native `auto` mode, and Codex sessions get the
//! "Approve for me" preset; both keep their providers' own checks. Bypass
//! permissions and Full Access are never chosen automatically.
//!
//! The terminal's new-session form, phones and agents all ask the service, so
//! every client shows and gets the same defaults.
use super::{
    permissions::{self, CodexPreset},
    provider::Provider,
};
use crate::{model::ModelInfo, process::Cancel, storage::AgentDefaults};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// The effort automatic Claude Opus sessions use when the model offers it.
pub const PREFERRED_EFFORT: &str = "xhigh";
/// Efforts from lowest to highest, for a model's highest effort.
const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
/// The mode Claude Code sessions start in on a server.
pub const SERVER_CLAUDE_MODE: &str = "auto";
/// The approval preset Codex sessions get on a server.
pub const SERVER_CODEX_PRESET: CodexPreset = CodexPreset::Auto;

/// How one provider's new sessions start at a location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Start {
    /// A `permissions::mode_choices` id; `None` is the provider's own default.
    pub mode: Option<String>,
    /// Codex only: the approval preset; `None` is Codex's configured policy.
    pub preset: Option<CodexPreset>,
    /// `mode` comes from the server rule, not from saved defaults.
    pub automatic_mode: bool,
    /// `preset` comes from the server rule, not from saved defaults.
    pub automatic_preset: bool,
}

/// A location's new-session defaults as sessions get them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// What the location saved, as its settings store it.
    pub saved: AgentDefaults,
    /// The location is a difu server (`config.server.enabled`).
    pub server: bool,
    /// The provider of sessions started without a model.
    pub provider: Provider,
    /// `None` is the provider's configured model.
    pub model: Option<String>,
    /// `None` is the model's default effort.
    pub effort: Option<String>,
    /// `model` is the newest Claude Opus, chosen because none is saved.
    pub automatic_model: bool,
    /// `effort` was chosen for the automatic model.
    pub automatic_effort: bool,
    pub codex: Start,
    pub claude: Start,
}

/// What a new session starts with: a request's choices, blanks filled from
/// the location's effective defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub provider: Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub starting_mode: Option<String>,
    pub preset: Option<CodexPreset>,
    /// The starting mode or preset came from the server rule: a provider
    /// refusing it starts in its own default with a note instead of failing.
    pub automatic_start: bool,
}

impl Defaults {
    /// The effective defaults for `saved`. `catalog` (this machine's models)
    /// is asked only when the model is automatic; `None` means discovery
    /// failed, which falls back to the provider default.
    pub fn new(
        saved: AgentDefaults,
        catalog: impl FnOnce() -> Option<Vec<ModelInfo>>,
        server: bool,
    ) -> Self {
        let mut defaults = Self {
            server,
            provider: saved.resolved_provider(),
            model: saved.model.clone(),
            effort: saved.effort.clone(),
            ..Self::default()
        };
        if saved.model.is_none() && saved.provider != Some(Provider::Codex) {
            let models = catalog().unwrap_or_default();
            if let Some(opus) = newest_opus(&models) {
                defaults.provider = Provider::Claude;
                defaults.model = Some(opus.id.clone());
                defaults.automatic_model = true;
                // A saved effort the model offers still wins.
                let offered = |effort: &String| {
                    opus.efforts.is_empty() || opus.efforts.iter().any(|e| e == effort)
                };
                if !saved.effort.as_ref().is_some_and(offered) {
                    defaults.effort = preferred_effort(opus);
                    defaults.automatic_effort = defaults.effort.is_some();
                }
            }
        }
        defaults.codex = start(&saved, Provider::Codex, server);
        defaults.claude = start(&saved, Provider::Claude, server);
        defaults.saved = saved;
        defaults
    }
    /// How `provider`'s new sessions start here.
    pub fn start(&self, provider: Provider) -> &Start {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
        }
    }
    /// `request` with its blank model, effort, starting mode and preset
    /// filled in. A model picks its own provider; a provider without a model
    /// uses that provider's configured model unless it is the effective one.
    /// The effective effort belongs to the effective model.
    pub fn resolve(&self, request: &AgentDefaults) -> Result<Resolved> {
        if let (Some(provider), Some(model)) = (request.provider, request.model.as_deref()) {
            ensure!(
                provider == Provider::for_model(Some(model)),
                "The model belongs to a different provider; choose a compatible model or Default"
            );
        }
        let (provider, model) = match (&request.model, request.provider) {
            (Some(model), _) => (Provider::for_model(Some(model)), Some(model.clone())),
            (None, Some(provider)) if provider != self.provider => (provider, None),
            (None, _) => (self.provider, self.model.clone()),
        };
        let effort = request.effort.clone().or_else(|| {
            (provider == self.provider && model == self.model)
                .then(|| self.effort.clone())
                .flatten()
        });
        let start = self.start(provider);
        let (starting_mode, automatic_mode) = match &request.starting_mode {
            Some(mode) => {
                let modes = permissions::mode_choices(provider);
                ensure!(
                    modes.iter().any(|choice| &choice.id == mode),
                    "{mode} isn't a {} mode; choose {}",
                    provider.label(),
                    modes
                        .iter()
                        .map(|choice| choice.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                (Some(mode.clone()), false)
            }
            None => (start.mode.clone(), start.automatic_mode),
        };
        let (preset, automatic_preset) = match (provider, request.codex_preset) {
            (Provider::Claude, _) => (None, false),
            (Provider::Codex, Some(preset)) => (Some(preset), false),
            (Provider::Codex, None) => (start.preset, start.automatic_preset),
        };
        Ok(Resolved {
            provider,
            model,
            effort,
            starting_mode,
            preset,
            automatic_start: automatic_mode || automatic_preset,
        })
    }
}

/// How `provider`'s sessions start: a saved starting mode the provider offers
/// (when it was saved for that provider, or for none), else `auto` for
/// Claude Code on a server; Codex takes the saved preset, else "Approve for
/// me" on a server.
fn start(saved: &AgentDefaults, provider: Provider, server: bool) -> Start {
    let pinned = saved
        .provider
        .or_else(|| saved.model.as_deref().map(|m| Provider::for_model(Some(m))));
    let mode = saved.starting_mode.clone().filter(|mode| {
        pinned.is_none_or(|pinned| pinned == provider)
            && permissions::mode_choices(provider)
                .iter()
                .any(|choice| &choice.id == mode)
    });
    let mut start = Start::default();
    match mode {
        Some(mode) => start.mode = Some(mode),
        None if server && provider == Provider::Claude => {
            start.mode = Some(SERVER_CLAUDE_MODE.into());
            start.automatic_mode = true;
        }
        None => {}
    }
    if provider == Provider::Codex {
        match saved.codex_preset {
            Some(preset) => start.preset = Some(preset),
            None if server => {
                start.preset = Some(SERVER_CODEX_PRESET);
                start.automatic_preset = true;
            }
            None => {}
        }
    }
    start
}

/// The newest Claude Opus: the highest version (from the id, else the name),
/// then the `opus` alias. Plan-mode and other models merely named after Opus
/// don't count, and a 1M-context variant (`opus[1m]`) loses to the plain
/// model of the same version. Dates (`20250514`) are not versions.
pub fn newest_opus(models: &[ModelInfo]) -> Option<&ModelInfo> {
    struct Candidate<'a> {
        model: &'a ModelInfo,
        version: Vec<u64>,
        from_id: bool,
        variant: bool,
        alias: bool,
        index: usize,
    }
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let version = |tokens: &[String]| -> Vec<u64> {
        tokens
            .iter()
            .filter(|token| token.chars().count() < 6)
            .filter_map(|token| token.parse().ok())
            .collect()
    };
    models
        .iter()
        .enumerate()
        .filter(|(_, model)| Provider::for_model(Some(&model.id)) == Provider::Claude)
        .filter_map(|(index, model)| {
            let lower = model.id.to_lowercase();
            let bare = lower.strip_prefix("claude/").unwrap_or(&lower);
            let base = bare.split('[').next().unwrap_or(bare);
            let tokens = words(base);
            if !tokens.iter().any(|token| token == "opus") {
                return None;
            }
            let from_id = version(&tokens);
            let name = words(&model.name.to_lowercase());
            let from_name = if name.iter().any(|token| token == "opus") {
                version(&name)
            } else {
                Vec::new()
            };
            Some(Candidate {
                model,
                from_id: !from_id.is_empty(),
                version: if from_id.is_empty() {
                    from_name
                } else {
                    from_id
                },
                variant: base != bare,
                alias: base == "opus",
                index,
            })
        })
        .min_by(|a, b| {
            b.version
                .cmp(&a.version)
                .then_with(|| b.from_id.cmp(&a.from_id))
                .then_with(|| a.variant.cmp(&b.variant))
                .then_with(|| b.alias.cmp(&a.alias))
                .then_with(|| a.index.cmp(&b.index))
        })
        .map(|candidate| candidate.model)
}

/// `xhigh` when `model` offers it, otherwise its highest effort; `None` when
/// it lists none.
pub fn preferred_effort(model: &ModelInfo) -> Option<String> {
    if model
        .efforts
        .iter()
        .any(|effort| effort == PREFERRED_EFFORT)
    {
        return Some(PREFERRED_EFFORT.into());
    }
    let rank = |effort: &str| {
        EFFORTS
            .iter()
            .position(|known| known.eq_ignore_ascii_case(effort))
    };
    model
        .efforts
        .iter()
        .enumerate()
        .max_by(|(a_index, a), (b_index, b)| {
            rank(a).cmp(&rank(b)).then_with(|| a_index.cmp(b_index))
        })
        .map(|(_, effort)| effort.clone())
}

type Loader = dyn Fn(&Cancel) -> Result<Vec<ModelInfo>> + Send + Sync;

/// This machine's model catalog for automatic models, discovered on first use
/// (a few seconds: it starts Claude Code) and kept for a while, so new
/// sessions don't wait for it again.
pub struct Catalog {
    load: Box<Loader>,
    /// When it was discovered, and the models (`None`: discovery failed).
    cached: Mutex<Option<(Instant, Option<Vec<ModelInfo>>)>>,
    /// One discovery at a time.
    loading: Mutex<()>,
}

impl Catalog {
    /// Discovered models are kept this long.
    const FRESH: Duration = Duration::from_secs(10 * 60);
    /// A failed discovery is retried after this long.
    const RETRY: Duration = Duration::from_secs(60);

    pub fn new(load: Box<Loader>) -> Self {
        Self {
            load,
            cached: Mutex::new(None),
            loading: Mutex::new(()),
        }
    }
    /// The Claude Code models this machine's login offers: only Claude Opus
    /// is ever chosen automatically.
    pub fn discover() -> Self {
        Self::new(Box::new(super::claude::models))
    }
    fn fresh(&self) -> Option<Option<Vec<ModelInfo>>> {
        let cached = self.cached.lock().ok()?;
        let (at, models) = cached.as_ref()?;
        let limit = if models.is_some() {
            Self::FRESH
        } else {
            Self::RETRY
        };
        (at.elapsed() < limit).then(|| models.clone())
    }
    /// The catalog, discovered again when stale; `None` when discovery failed.
    pub fn get(&self) -> Option<Vec<ModelInfo>> {
        if let Some(models) = self.fresh() {
            return models;
        }
        let _loading = self.loading.lock().ok()?;
        // Another request may have discovered it meanwhile.
        if let Some(models) = self.fresh() {
            return models;
        }
        let cancel = Cancel::default();
        let models = {
            let _deadline = super::connections::Deadline::new(cancel.clone());
            (self.load)(&cancel)
                .ok()
                .filter(|models| !models.is_empty())
        };
        if let Ok(mut cached) = self.cached.lock() {
            *cached = Some((Instant::now(), models.clone()));
        }
        models
    }
    /// The catalog for creating a session, which never starts a provider:
    /// fresh discovered models, or else Claude Code's `opus` alias (which it
    /// resolves to its newest Opus) when the Claude CLI is installed.
    /// Discovery can take seconds and must not delay or precede a session's
    /// worktree; screens that show defaults run it instead.
    pub fn for_launch(&self) -> Option<Vec<ModelInfo>> {
        if let Some(Some(models)) = self.fresh() {
            return Some(models);
        }
        installed("claude").then(|| {
            vec![ModelInfo {
                id: "claude/opus".into(),
                name: "Claude Code · Opus".into(),
                efforts: ["low", "medium", "high", "xhigh", "max"]
                    .map(String::from)
                    .to_vec(),
            }]
        })
    }
    /// Keeps models discovered elsewhere (the model picker's list).
    pub fn remember(&self, models: &[ModelInfo]) {
        if let Ok(mut cached) = self.cached.lock() {
            *cached = Some((Instant::now(), Some(models.to_vec())));
        }
    }
}

/// Whether `program` is an executable on `PATH`, without running it.
fn installed(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            std::fs::metadata(directory.join(program))
                .is_ok_and(|file| file.is_file() && file.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use anyhow::Context;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// A catalog that never starts a provider: `models` as discovered.
    /// Already discovered, so session creation sees it too.
    pub(crate) fn fixed_catalog(models: Vec<ModelInfo>) -> Catalog {
        let catalog = Catalog::new(Box::new({
            let models = models.clone();
            move |_| Ok(models.clone())
        }));
        catalog.remember(&models);
        catalog
    }

    /// The catalog service tests use: no models, so no automatic model.
    pub(crate) fn fixture_catalog() -> Catalog {
        fixed_catalog(Vec::new())
    }

    pub(crate) fn model(id: &str, name: &str, efforts: &[&str]) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: name.into(),
            efforts: efforts.iter().map(|e| (*e).to_owned()).collect(),
        }
    }

    /// What Claude Code and Codex report on a typical machine.
    pub(crate) fn catalog() -> Vec<ModelInfo> {
        vec![
            model("gpt-5.6", "GPT-5.6", &["low", "medium", "high", "xhigh"]),
            model(
                "claude/default",
                "Claude Code · Default (recommended)",
                &["low", "high"],
            ),
            model(
                "claude/opus",
                "Claude Code · Opus",
                &["low", "medium", "high", "xhigh", "max"],
            ),
            model(
                "claude/claude-opus-4-7",
                "Claude Code · Opus 4.7",
                &["low", "medium", "high", "xhigh", "max"],
            ),
            model(
                "claude/claude-opus-4-7[1m]",
                "Claude Code · Opus 4.7 (1M context)",
                &["low", "high", "xhigh"],
            ),
            model(
                "claude/claude-opus-4-1-20250805",
                "Claude Code · Opus 4.1",
                &["low", "high"],
            ),
            model("claude/opusplan", "Claude Code · Opus Plan Mode", &["high"]),
            model("claude/sonnet", "Claude Code · Sonnet", &["low", "high"]),
        ]
    }

    fn defaults(saved: AgentDefaults, server: bool) -> Defaults {
        Defaults::new(saved, || Some(catalog()), server)
    }

    #[test]
    fn newest_opus_reads_versions_and_skips_dates_plans_and_long_context() -> Result<()> {
        let models = catalog();
        let opus = newest_opus(&models).context("opus")?;
        assert_eq!(opus.id, "claude/claude-opus-4-7");
        // 4.7 beats a dated 4.1; the date is not a version.
        let dated = [
            model("claude/claude-opus-4-1-20250805", "Opus 4.1", &[]),
            model("claude/claude-opus-4-0", "Opus 4", &[]),
        ];
        assert_eq!(
            newest_opus(&dated).map(|m| m.id.as_str()),
            Some("claude/claude-opus-4-1-20250805")
        );
        // Only the 1M variant and the alias: the versioned variant wins.
        let long = [
            model("claude/opus", "Claude Code · Opus", &[]),
            model("claude/claude-opus-4-7[1m]", "Opus 4.7 (1M)", &[]),
        ];
        assert_eq!(
            newest_opus(&long).map(|m| m.id.as_str()),
            Some("claude/claude-opus-4-7[1m]")
        );
        // The plain model beats its 1M variant.
        let both = [
            model("claude/claude-opus-4-7[1m]", "Opus 4.7 (1M)", &[]),
            model("claude/claude-opus-4-7", "Opus 4.7", &[]),
        ];
        assert_eq!(
            newest_opus(&both).map(|m| m.id.as_str()),
            Some("claude/claude-opus-4-7")
        );
        // A version from the name when the id has none; the alias otherwise.
        let named = [
            model("claude/opus", "Claude Code · Opus", &[]),
            model("claude/best", "Claude Code · Opus 5", &[]),
        ];
        assert_eq!(
            newest_opus(&named).map(|m| m.id.as_str()),
            Some("claude/opus"),
            "`best` isn't an Opus id"
        );
        let alias = [
            model("claude/opusplan", "Opus Plan Mode", &[]),
            model("claude/opus", "Claude Code · Opus", &[]),
            model("claude/sonnet", "Sonnet 4.5", &[]),
        ];
        assert_eq!(
            newest_opus(&alias).map(|m| m.id.as_str()),
            Some("claude/opus")
        );
        let versioned_name = [
            model("claude/opus", "Claude Code · Opus", &[]),
            model(
                "claude/opus[1m]",
                "Claude Code · Opus 4.7 (1M context)",
                &[],
            ),
        ];
        assert_eq!(
            newest_opus(&versioned_name).map(|m| m.id.as_str()),
            Some("claude/opus[1m]"),
            "a version from the name beats none"
        );
        // Codex models named like Opus and machines without Claude don't count.
        let codex = [model("gpt-opus-9", "GPT Opus 9", &["high"])];
        assert_eq!(newest_opus(&codex), None);
        assert_eq!(newest_opus(&[]), None);
        Ok(())
    }

    #[test]
    fn preferred_effort_is_xhigh_or_the_highest_offered() -> Result<()> {
        assert_eq!(
            preferred_effort(&model("claude/opus", "", &["low", "xhigh", "max"])).as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            preferred_effort(&model("claude/opus", "", &["medium", "high", "low"])).as_deref(),
            Some("high")
        );
        assert_eq!(
            preferred_effort(&model("claude/opus", "", &["max", "low"])).as_deref(),
            Some("max")
        );
        assert_eq!(preferred_effort(&model("claude/opus", "", &[])), None);
        Ok(())
    }

    #[test]
    fn nothing_saved_starts_the_newest_opus_at_xhigh() -> Result<()> {
        let computer = defaults(AgentDefaults::default(), false);
        assert_eq!(computer.provider, Provider::Claude);
        assert_eq!(computer.model.as_deref(), Some("claude/claude-opus-4-7"));
        assert_eq!(computer.effort.as_deref(), Some("xhigh"));
        assert!(computer.automatic_model && computer.automatic_effort);
        // Computers keep the providers' own starting modes.
        assert_eq!(computer.claude, Start::default());
        assert_eq!(computer.codex, Start::default());
        let resolved = computer.resolve(&AgentDefaults::default())?;
        assert_eq!(
            resolved,
            Resolved {
                provider: Provider::Claude,
                model: Some("claude/claude-opus-4-7".into()),
                effort: Some("xhigh".into()),
                starting_mode: None,
                preset: None,
                automatic_start: false,
            }
        );
        // An Opus without xhigh gets its highest effort.
        let max_only = Defaults::new(
            AgentDefaults::default(),
            || {
                Some(vec![model(
                    "claude/claude-opus-5",
                    "Opus 5",
                    &["low", "max"],
                )])
            },
            false,
        );
        assert_eq!(max_only.effort.as_deref(), Some("max"));
        Ok(())
    }

    #[test]
    fn saved_fields_win_over_automatic_ones() -> Result<()> {
        let asked = AtomicUsize::new(0);
        let saved = AgentDefaults {
            model: Some("gpt-5.6".into()),
            effort: Some("low".into()),
            starting_mode: Some("plan".into()),
            codex_preset: Some(CodexPreset::ReadOnly),
            ..AgentDefaults::default()
        };
        let server = Defaults::new(
            saved,
            || {
                asked.fetch_add(1, Ordering::Relaxed);
                Some(catalog())
            },
            true,
        );
        assert_eq!(
            asked.load(Ordering::Relaxed),
            0,
            "no catalog for a saved model"
        );
        assert_eq!(
            (
                server.provider,
                server.model.as_deref(),
                server.effort.as_deref()
            ),
            (Provider::Codex, Some("gpt-5.6"), Some("low"))
        );
        assert!(!server.automatic_model && !server.automatic_effort);
        assert_eq!(
            server.codex,
            Start {
                mode: Some("plan".into()),
                preset: Some(CodexPreset::ReadOnly),
                automatic_mode: false,
                automatic_preset: false,
            }
        );
        // The saved mode belongs to Codex; Claude sessions still start in Auto.
        assert_eq!(server.claude.mode.as_deref(), Some("auto"));
        assert!(server.claude.automatic_mode);

        // Codex saved without a model keeps Codex's configured model.
        let codex = defaults(
            AgentDefaults {
                provider: Some(Provider::Codex),
                ..AgentDefaults::default()
            },
            false,
        );
        assert_eq!(
            (codex.provider, codex.model.as_deref()),
            (Provider::Codex, None)
        );
        assert!(!codex.automatic_model);

        // A saved effort the automatic model offers wins; one it lacks doesn't.
        let high = defaults(
            AgentDefaults {
                effort: Some("high".into()),
                ..AgentDefaults::default()
            },
            false,
        );
        assert_eq!(high.effort.as_deref(), Some("high"));
        assert!(high.automatic_model && !high.automatic_effort);
        let minimal = defaults(
            AgentDefaults {
                effort: Some("minimal".into()),
                ..AgentDefaults::default()
            },
            false,
        );
        assert_eq!(minimal.effort.as_deref(), Some("xhigh"));

        // A saved Claude starting mode applies to Claude sessions only.
        let claude = defaults(
            AgentDefaults {
                provider: Some(Provider::Claude),
                starting_mode: Some("acceptEdits".into()),
                ..AgentDefaults::default()
            },
            true,
        );
        assert_eq!(claude.model.as_deref(), Some("claude/claude-opus-4-7"));
        assert_eq!(claude.claude.mode.as_deref(), Some("acceptEdits"));
        assert!(!claude.claude.automatic_mode);
        assert_eq!(claude.codex.mode, None);
        assert_eq!(claude.codex.preset, Some(CodexPreset::Auto));
        Ok(())
    }

    #[test]
    fn machines_without_claude_use_the_provider_default() -> Result<()> {
        let codex_only = Defaults::new(
            AgentDefaults::default(),
            || Some(vec![model("gpt-5.6", "GPT-5.6", &["high"])]),
            false,
        );
        assert_eq!(
            (
                codex_only.provider,
                codex_only.model.as_deref(),
                codex_only.effort.as_deref()
            ),
            (Provider::Codex, None, None)
        );
        assert!(!codex_only.automatic_model);
        let failed = Defaults::new(AgentDefaults::default(), || None, false);
        assert_eq!(
            (failed.provider, failed.model.as_deref()),
            (Provider::Codex, None)
        );
        // Claude saved but not installed: Claude Code's own default model.
        let claude = Defaults::new(
            AgentDefaults {
                provider: Some(Provider::Claude),
                effort: Some("high".into()),
                ..AgentDefaults::default()
            },
            || None,
            false,
        );
        assert_eq!(
            (
                claude.provider,
                claude.model.as_deref(),
                claude.effort.as_deref()
            ),
            (Provider::Claude, None, Some("high"))
        );
        Ok(())
    }

    #[test]
    fn servers_start_in_auto_and_computers_keep_provider_modes() -> Result<()> {
        let server = defaults(AgentDefaults::default(), true);
        assert_eq!(
            server.claude,
            Start {
                mode: Some("auto".into()),
                preset: None,
                automatic_mode: true,
                automatic_preset: false,
            }
        );
        assert_eq!(
            server.codex,
            Start {
                mode: None,
                preset: Some(CodexPreset::Auto),
                automatic_mode: false,
                automatic_preset: true,
            }
        );
        let claude = server.resolve(&AgentDefaults::default())?;
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(claude.starting_mode.as_deref(), Some("auto"));
        assert_eq!(claude.preset, None);
        assert!(claude.automatic_start);
        let codex = server.resolve(&AgentDefaults {
            model: Some("gpt-5.6".into()),
            ..AgentDefaults::default()
        })?;
        assert_eq!(codex.provider, Provider::Codex);
        assert_eq!(codex.effort, None, "xhigh belongs to the automatic Opus");
        assert_eq!(codex.starting_mode, None);
        assert_eq!(codex.preset, Some(CodexPreset::Auto));
        assert!(codex.automatic_start);
        // Never bypass or Full Access automatically.
        for start in [&server.claude, &server.codex] {
            assert_ne!(start.mode.as_deref(), Some("bypassPermissions"));
            assert_ne!(start.preset, Some(CodexPreset::Full));
        }
        let computer = defaults(AgentDefaults::default(), false);
        let resolved = computer.resolve(&AgentDefaults {
            model: Some("gpt-5.6".into()),
            ..AgentDefaults::default()
        })?;
        assert_eq!((resolved.starting_mode, resolved.preset), (None, None));
        assert!(!resolved.automatic_start);
        Ok(())
    }

    #[test]
    fn requests_choose_over_defaults_and_are_checked() -> Result<()> {
        let server = defaults(AgentDefaults::default(), true);
        let chosen = server.resolve(&AgentDefaults {
            model: Some("claude/sonnet".into()),
            effort: Some("high".into()),
            starting_mode: Some("plan".into()),
            ..AgentDefaults::default()
        })?;
        assert_eq!(
            chosen,
            Resolved {
                provider: Provider::Claude,
                model: Some("claude/sonnet".into()),
                effort: Some("high".into()),
                starting_mode: Some("plan".into()),
                preset: None,
                automatic_start: false,
            }
        );
        // The automatic model with its effort when only a mode is chosen.
        let mode_only = server.resolve(&AgentDefaults {
            starting_mode: Some("acceptEdits".into()),
            ..AgentDefaults::default()
        })?;
        assert_eq!(mode_only.model.as_deref(), Some("claude/claude-opus-4-7"));
        assert_eq!(mode_only.effort.as_deref(), Some("xhigh"));
        assert!(!mode_only.automatic_start);
        // A provider without a model: that provider's configured model.
        let codex = server.resolve(&AgentDefaults {
            provider: Some(Provider::Codex),
            codex_preset: Some(CodexPreset::Ask),
            ..AgentDefaults::default()
        })?;
        assert_eq!(
            (codex.provider, codex.model, codex.effort),
            (Provider::Codex, None, None)
        );
        assert_eq!(codex.preset, Some(CodexPreset::Ask));
        assert!(!codex.automatic_start);
        // A preset means nothing to Claude Code.
        let claude = server.resolve(&AgentDefaults {
            codex_preset: Some(CodexPreset::Full),
            ..AgentDefaults::default()
        })?;
        assert_eq!(claude.preset, None);
        let wrong_mode = server.resolve(&AgentDefaults {
            model: Some("gpt-5.6".into()),
            starting_mode: Some("acceptEdits".into()),
            ..AgentDefaults::default()
        });
        let message = wrong_mode
            .err()
            .context("Codex has no acceptEdits")?
            .to_string();
        assert!(message.contains("isn't a Codex mode"), "{message}");
        let mismatch = server.resolve(&AgentDefaults {
            provider: Some(Provider::Codex),
            model: Some("claude/opus".into()),
            ..AgentDefaults::default()
        });
        assert!(mismatch.is_err());
        Ok(())
    }

    #[test]
    fn the_catalog_is_discovered_once_and_kept() -> Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let catalog = Catalog::new(Box::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(super::tests::catalog())
        }));
        assert_eq!(
            catalog.get().map(|m| m.len()),
            Some(super::tests::catalog().len())
        );
        assert!(catalog.get().is_some());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        catalog.remember(&[model("claude/opus", "Opus", &[])]);
        assert_eq!(catalog.get().map(|m| m.len()), Some(1));
        let failing = Catalog::new(Box::new(|_| anyhow::bail!("no providers")));
        assert_eq!(failing.get(), None);
        Ok(())
    }
}
