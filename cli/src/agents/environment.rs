//! Environment profiles in server sessions (`cloud::environments`).
//!
//! * Before a sandboxed server session's agent starts, [`prepare`] resolves
//!   the worktree's tools (the repository's version files win, the profile
//!   fills gaps), writes the worktree's mise config, installs the tools into
//!   the shared cache, enables corepack and runs the profile's setup
//!   commands, all in one bounded setup container. Setup is skipped when
//!   nothing it depends on changed (the tools, the commands and variables,
//!   the worktree's lockfiles). Progress and failures are system entries in
//!   the chat and part of the agent's instructions; a failure never stops the
//!   session. A new session starts this as soon as its worktree exists, so the
//!   tools install while the user writes the first message.
//! * Versions the repository's files declare that the profile does not know
//!   yet are recorded in it automatically, with where they came from.
//! * Agents read the profile and record what they learn with
//!   `difu_environment`; every other running session of the repository gets a
//!   short system note, and every worktree of the repository gets the new mise
//!   config at once, so its shells use it.
use super::{
    Job, Session,
    sandbox::{Role, Sandbox},
    server::Store,
};
use crate::{
    cloud::{
        environments::{self, Detected, Edit, Plan, Profile, View},
        repos,
    },
    process::{self, Cancel},
    storage::Storage,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub(super) const TOOL: &str = "difu_environment";
/// How long one preparation may take before the agent starts without it.
const LIMIT: Duration = Duration::from_secs(20 * 60);
pub(super) const INSTRUCTIONS: &str = "Environment: this sandbox has mise (https://mise.jdx.dev) and corepack. Toolchains mise installs go to a cache shared by every session on this server, and difu remembers what each repository needs in its environment profile: tools and versions, setup commands, non-secret environment variables and notes. Before you started, difu installed the versions the repository's files declare (.nvmrc, .tool-versions, mise.toml, package.json packageManager, ...) and the profile's tools, and ran its setup commands; the outcome is below. Before running builds or tests, check the profile with difu_environment. If a runtime or tool is missing or has the wrong version, install it with mise (for example `mise install node@24`; it persists in the shared cache) and record it with difu_environment remember, which also activates it in this session's shells, so future sessions start ready. Record the setup commands, variables and notes a fresh worktree needs the same way, and forget entries that turn out wrong. Don't apt-install runtimes or add mise files to the repository unless the user asks, and never record secrets.";

/// Whether session `session` gets `difu_environment` and the environment
/// instructions: coding sessions on a difu server.
pub(super) fn offered(session: &Session) -> bool {
    session.server_mode && !session.orchestrator && matches!(session.job, Job::Coding(_))
}

/// The environment part of a server session's instructions.
pub(super) fn instructions(session: &Session) -> String {
    match &session.environment {
        Some(status) => format!(
            "{INSTRUCTIONS}\n\nEnvironment status: {}",
            environments::short(status, 4000)
        ),
        None => INSTRUCTIONS.to_owned(),
    }
}

pub(super) fn tool() -> Value {
    let map = |what: &str| json!({"type":"object","additionalProperties":{"type":"string"},"description":what});
    let list = |what: &str| json!({"type":"array","items":{"type":"string"},"description":what});
    json!({"type":"function","name":TOOL,"description":"Read or update this repository's environment profile on the difu server: the tools and versions its sandboxes need, setup commands run in each new worktree, non-secret environment variables and notes for agents. Without arguments it returns the profile, the versions this worktree's files declare, what this sandbox resolved (repository files win over the profile) and the outcome of the last preparation. remember adds or updates entries and forget removes them; both apply to every future session of this repository and the tools activate in this session's shells right away (run `mise install` if one is not installed yet). Record only what a fresh worktree really needs, never secrets.","inputSchema":{"type":"object","properties":{
        "remember":{"type":"object","properties":{
            "tools":map("mise tool name to version, e.g. {\"node\":\"24\",\"pnpm\":\"9\",\"python\":\"3.12\"}"),
            "setup":list("Commands to run in each new worktree before the agent starts, e.g. pnpm install --frozen-lockfile"),
            "env":map("Non-secret environment variables, e.g. {\"NODE_OPTIONS\":\"--max-old-space-size=4096\"}"),
            "notes":list("Short notes for future agents, e.g. backend tests need Postgres via docker compose")
        },"additionalProperties":false},
        "forget":{"type":"object","properties":{
            "tools":list("Tool names to remove"),
            "setup":list("Setup commands to remove, exactly as recorded"),
            "env":list("Variable names to remove"),
            "notes":list("Notes to remove, exactly as recorded")
        },"additionalProperties":false}
    },"additionalProperties":false}})
}

/// The `owner/name` of the server repository `session` works in.
pub(super) fn repository(storage: &Storage, session: &Session) -> Option<String> {
    repos::name_of(storage, session.job.root())
}

/// A string-to-string map argument; numbers are accepted as versions.
fn pairs(value: Option<&Value>, what: &str) -> Result<Vec<(String, String)>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => Ok((key.clone(), text.clone())),
                Value::Number(number) => Ok((key.clone(), number.to_string())),
                _ => bail!("{what}.{key} must be a string"),
            })
            .collect(),
        Some(_) => bail!("{what} must be an object of strings"),
    }
}

/// A list-of-strings argument; a single string is one item.
fn strings(value: Option<&Value>, what: &str) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![text.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .with_context(|| format!("{what} must hold strings"))
            })
            .collect(),
        Some(_) => bail!("{what} must be a list of strings"),
    }
}

fn section<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    allowed: &[&str],
) -> Result<Option<&'a Map<String, Value>>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(section)) => {
            if let Some(unknown) = section.keys().find(|k| !allowed.contains(&k.as_str())) {
                bail!("{key} does not take {unknown}; it takes tools, setup, env and notes");
            }
            Ok(Some(section))
        }
        Some(_) => bail!("{key} must be an object"),
    }
}

/// `difu_environment`'s arguments as an edit.
fn edit(args: &Value) -> Result<Edit> {
    let args = match args {
        Value::Null => Map::new(),
        Value::Object(args) => args.clone(),
        _ => bail!("{TOOL} takes an object of arguments"),
    };
    if let Some(unknown) = args
        .keys()
        .find(|key| !["remember", "forget"].contains(&key.as_str()))
    {
        bail!("{TOOL} does not take {unknown}; it takes remember and forget");
    }
    const FIELDS: [&str; 4] = ["tools", "setup", "env", "notes"];
    let mut edit = Edit::default();
    if let Some(remember) = section(&args, "remember", &FIELDS)? {
        edit.tools = pairs(remember.get("tools"), "remember.tools")?;
        edit.setup = strings(remember.get("setup"), "remember.setup")?;
        edit.env = pairs(remember.get("env"), "remember.env")?;
        edit.notes = strings(remember.get("notes"), "remember.notes")?;
    }
    if let Some(forget) = section(&args, "forget", &FIELDS)? {
        edit.forget_tools = strings(forget.get("tools"), "forget.tools")?;
        edit.forget_setup = strings(forget.get("setup"), "forget.setup")?;
        edit.forget_env = strings(forget.get("env"), "forget.env")?;
        edit.forget_notes = strings(forget.get("notes"), "forget.notes")?;
    }
    Ok(edit)
}

/// Who an agent's change is attributed to in the profile's history.
fn agent(session: &Session) -> String {
    format!(
        "{} in session {} ({})",
        session.provider.label(),
        session.id,
        environments::short(&session.title, 60)
    )
}

/// Handles `difu_environment` for session `id`.
pub(super) fn handle(store: &Store, id: &str, args: &Value) -> Result<Value> {
    let session = store.get(id)?;
    ensure!(
        offered(&session),
        "{TOOL} works in coding sessions on a difu cloud server"
    );
    let repository = repository(&store.storage, &session).context(
        "This session's repository is not one of the server's repositories, so it has no environment profile; sessions started on owner/name have one",
    )?;
    let edit = edit(args)?;
    let mut recorded = Vec::new();
    if !edit.is_empty() {
        let (_, changes) =
            environments::update(&store.storage, &repository, &agent(&session), |profile| {
                environments::apply(profile, &edit)
            })?;
        if !changes.is_empty() {
            let summary = environments::summary(&changes);
            store.update(id, |s| {
                s.note(
                    "system",
                    format!("Recorded in the {repository} environment: {summary}."),
                )
            })?;
            store.save(id)?;
            changed(
                store,
                &repository,
                Some(id),
                &format!(
                    "Another session recorded in the {repository} environment: {summary}. New commands and shells use it; run `mise install` if a new tool is missing."
                ),
            );
        }
        recorded = changes;
    }
    let profile = environments::load(&store.storage, &repository)?;
    let workspace = session
        .workspace
        .clone()
        .filter(|_| session.workspace_ready);
    let detected = workspace
        .as_deref()
        .map(environments::detect)
        .unwrap_or_default();
    let resolved = environments::resolve(&profile, &detected);
    let history = profile
        .history
        .iter()
        .rev()
        .take(5)
        .map(|change| json!({"at": change.at, "by": change.by, "summary": change.summary}))
        .collect::<Vec<_>>();
    Ok(json!({
        "repository": repository,
        "profile": {"tools": profile.tools, "setup": profile.setup, "env": profile.env, "notes": profile.notes},
        "detected": detected,
        "resolved": resolved,
        "recorded": recorded,
        "lastPreparation": session.environment,
        "recentChanges": history,
        "sharedCache": environments::CACHE_MOUNT,
        "note": "Repository files win over the profile; the profile fills gaps. Recorded tools are active in new commands at once; run `mise install` to install one that is missing. Setup commands run in new worktrees.",
    }))
}

/// After `repository`'s profile changed: every ready worktree of its sessions
/// gets the new mise config (so their shells and commands use it at once),
/// and every other active session gets `message` as a system note.
fn changed(store: &Store, repository: &str, author: Option<&str>, message: &str) {
    let Ok(ids) = store.ids() else {
        return;
    };
    for id in ids {
        let Ok((root, workspace, archived, active, coding)) = store.inspect(&id, |s| {
            (
                s.job.root().clone(),
                s.workspace.clone().filter(|_| s.workspace_ready),
                s.archived,
                s.status.active(),
                offered(s),
            )
        }) else {
            continue;
        };
        if !coding
            || archived
            || repos::name_of(&store.storage, &root)
                .is_none_or(|name| !name.eq_ignore_ascii_case(repository))
        {
            continue;
        }
        if let Some(workspace) = &workspace
            && let Err(error) = regenerate(&store.storage, repository, workspace)
        {
            eprintln!("Environment config for session {id}: {error:#}");
        }
        if active && author != Some(id.as_str()) {
            let _ = store.update(&id, |s| s.note("system", message));
            let _ = store.save(&id);
        }
    }
}

/// Rewrites a worktree's mise config from the current profile.
fn regenerate(storage: &Storage, repository: &str, workspace: &Path) -> Result<()> {
    let Some(state) = environments::state_dir(workspace) else {
        return Ok(());
    };
    let profile = environments::load(storage, repository)?;
    let resolved = environments::resolve(&profile, &environments::detect(workspace));
    environments::write_config(&state, &environments::mise_config(repository, &resolved))?;
    Ok(())
}

/// Records versions the repository's files declare that the profile lacks,
/// and tells session `id`.
fn learn(store: &Store, id: &str, repository: &str, detected: &[Detected]) -> Result<()> {
    let profile = environments::load(&store.storage, repository)?;
    if environments::learnable(&profile, detected).is_empty() {
        return Ok(());
    }
    let by = format!("difu, from the repository's files in session {id}");
    let (_, changes) = environments::update(&store.storage, repository, &by, |profile| {
        let mut changes = Vec::new();
        for tool in environments::learnable(profile, detected) {
            if environments::validate_tool(&tool.tool, &tool.version).is_ok()
                && profile.tools.len() < 40
            {
                profile
                    .tools
                    .insert(tool.tool.clone(), tool.version.clone());
                changes.push(format!(
                    "{} {} (from {})",
                    tool.tool, tool.version, tool.source
                ));
            }
        }
        Ok(changes)
    })?;
    if !changes.is_empty() {
        store.update(id, |s| {
            s.note(
                "system",
                format!(
                    "Remembered in the {repository} environment: {}.",
                    environments::summary(&changes)
                ),
            )
        })?;
        store.save(id)?;
    }
    Ok(())
}

/// Starts [`prepare`] on its own thread for a new session whose worktree is
/// ready, so its tools install while the user writes the first message.
pub(super) fn prepare_in_background(store: &Arc<Store>, id: &str, cancel: &Cancel) {
    let wanted = store
        .storage
        .load_config()
        .is_ok_and(|config| config.server.enabled && config.server.sandbox.enabled)
        && store.inspect(id, offered).unwrap_or(false);
    if !wanted {
        return;
    }
    let (store, id, cancel) = (store.clone(), id.to_owned(), cancel.clone());
    let spawned = thread::Builder::new()
        .name(format!("difu-environment-{id}"))
        .spawn(move || prepare(&store, &id, &cancel));
    if let Err(error) = spawned {
        eprintln!("Cannot prepare the environment in the background: {error}");
    }
}

/// Applies `repository`'s environment to session `id`'s sandbox before its
/// agent starts. Never fails: problems become chat notes and part of the
/// agent's instructions. Concurrent calls for one worktree wait for each
/// other, and the later one finds nothing left to do.
pub(super) fn prepare(store: &Store, id: &str, cancel: &Cancel) {
    // Ordinary installations never get here: only server sessions qualify.
    if !store.inspect(id, offered).unwrap_or(false) {
        return;
    }
    if let Err(error) = apply(store, id, cancel)
        && !cancel.cancelled()
    {
        let message = format!("Could not prepare the environment: {error:#}");
        let _ = store.update(id, |s| {
            s.note("system", message.clone());
            s.environment = Some(message.clone());
        });
        let _ = store.save(id);
    }
}

/// `1m 12s`, `42s`.
fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// What `prepare` decided to run for one worktree.
struct Decision {
    tools: bool,
    setup: bool,
}

fn decide(
    toolchains: &Path,
    state: &Path,
    recorded: &environments::State,
    resolved: &[Detected],
    profile: &Profile,
    tools_key: &str,
    setup_key: &str,
) -> Decision {
    Decision {
        tools: !resolved.is_empty()
            && (recorded.tools.as_deref() != Some(tools_key)
                || !environments::installed(toolchains, state, resolved)),
        setup: !profile.setup.is_empty() && recorded.setup.as_deref() != Some(setup_key),
    }
}

fn apply(store: &Store, id: &str, cancel: &Cancel) -> Result<()> {
    let session = store.get(id)?;
    if !offered(&session) {
        return Ok(());
    }
    let Some(workspace) = session
        .workspace
        .clone()
        .filter(|_| session.workspace_ready)
    else {
        return Ok(());
    };
    let Some(sandbox) = Sandbox::for_session(&store.storage, &session)? else {
        store.update(id, |s| {
            s.environment = Some(
                "Sandboxing is off on this server, so difu installs no tools before sessions start; the environment profile is still shared by every session of this repository.".into(),
            )
        })?;
        return store.save(id);
    };
    let Some(repository) = repository(&store.storage, &session) else {
        return Ok(());
    };
    let Some(state) = environments::state_dir(&workspace) else {
        return Ok(());
    };
    std::fs::create_dir_all(&state)?;
    let _lock = repos::lock(&state.join("lock"), cancel)?;
    let detected = environments::detect(&workspace);
    learn(store, id, &repository, &detected)?;
    let profile = environments::load(&store.storage, &repository)?;
    let resolved = environments::resolve(&profile, &detected);
    let config = environments::mise_config(&repository, &resolved);
    environments::write_config(&state, &config)?;
    let tools_key = environments::tools_key(&config, &resolved);
    let setup_key = environments::setup_key(&workspace, &tools_key, &profile);
    let mut recorded = environments::State::load(&state);
    let cache = match sandbox.toolchains() {
        Some(toolchains) => toolchains.cache.clone(),
        None => super::sandbox::Toolchains::cache(&store.storage)?,
    };
    let decision = decide(
        &cache, &state, &recorded, &resolved, &profile, &tools_key, &setup_key,
    );
    let tools = environments::describe(&resolved);
    let first = store.inspect(id, |s| s.environment.is_none())?;
    if !decision.tools && !decision.setup {
        if resolved.is_empty() && profile.setup.is_empty() {
            store.update(id, |s| {
                s.environment = Some(format!(
                    "{repository} declares no tool versions and its profile is empty; the image's tools apply until you record some."
                ))
            })?;
            return store.save(id);
        }
        let status = ready(&repository, &tools, &profile, false, "already installed");
        store.update(id, |s| {
            if first {
                s.note("system", status.clone());
            }
            s.environment = Some(status);
        })?;
        return store.save(id);
    }
    let plan = Plan {
        workspace: workspace.clone(),
        state: state.clone(),
        tools: decision.tools,
        corepack: if decision.tools {
            environments::corepack(&resolved)
        } else {
            Vec::new()
        },
        setup: if decision.setup {
            profile.setup.clone()
        } else {
            Vec::new()
        },
    };
    store.update(id, |s| {
        s.note("system", starting(&repository, &tools, &plan))
    })?;
    store.save(id)?;
    let started = Instant::now();
    let outcome = run(&sandbox, &plan, cancel);
    let took = elapsed(started.elapsed());
    let status = match outcome {
        Ok(outcome) => match &outcome.failed {
            None => {
                if decision.tools || recorded.tools.as_deref() == Some(&tools_key) {
                    recorded.tools = Some(tools_key.clone());
                }
                if decision.setup {
                    recorded.setup = Some(setup_key.clone());
                }
                recorded.save(&state)?;
                if let Some(report) = &outcome.report {
                    learn(
                        store,
                        id,
                        &repository,
                        &environments::reported(report, &workspace),
                    )?;
                }
                let note = ready(
                    &repository,
                    &tools,
                    &profile,
                    decision.setup,
                    &format!("in {took}"),
                );
                store.update(id, |s| s.note("system", note.clone()))?;
                note
            }
            Some((step, code, tail)) => {
                let setup_step = step.starts_with("setup-");
                recorded.tools = setup_step.then(|| tools_key.clone());
                recorded.setup = None;
                recorded.save(&state)?;
                let note = format!(
                    "Environment setup for {repository} failed at {} (exit {code}) after {took}{}. The agent starts anyway and knows; it can fix this and record the fix with {TOOL}.",
                    plan.label(step),
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(":\n{tail}")
                    }
                );
                store.update(id, |s| s.note("error", note.clone()))?;
                note
            }
        },
        Err(_) if cancel.cancelled() => return Ok(()),
        Err(error) => {
            recorded.tools = None;
            recorded.setup = None;
            recorded.save(&state)?;
            let note = format!(
                "Environment setup for {repository} stopped after {took}: {error:#}. The agent starts anyway; {tools_note}",
                tools_note = if tools.is_empty() {
                    "nothing was installed.".to_owned()
                } else {
                    format!("check that {tools} work before relying on them.")
                }
            );
            store.update(id, |s| s.note("error", note.clone()))?;
            note
        }
    };
    store.update(id, |s| s.environment = Some(status))?;
    store.save(id)
}

/// `Preparing the environment for acme/widgets: node 24 (.nvmrc); then
/// `pnpm install --frozen-lockfile`…`
fn starting(repository: &str, tools: &str, plan: &Plan) -> String {
    let mut text = format!("Preparing the environment for {repository}");
    if plan.tools && !tools.is_empty() {
        text.push_str(&format!(": {tools}"));
    }
    if !plan.setup.is_empty() {
        text.push_str(&format!(
            "{}{}",
            if plan.tools && !tools.is_empty() {
                "; then "
            } else {
                ": "
            },
            plan.setup
                .iter()
                .map(|command| format!("`{}`", environments::short(command, 80)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    text.push('…');
    text
}

/// The status of a ready environment, for the chat and the instructions.
fn ready(repository: &str, tools: &str, profile: &Profile, ran: bool, when: &str) -> String {
    let mut text = format!("Environment for {repository} ready ({when})");
    if !tools.is_empty() {
        text.push_str(&format!(": {tools}"));
    }
    if !profile.setup.is_empty() {
        text.push_str(&format!(
            ". Setup {} {}",
            profile
                .setup
                .iter()
                .map(|command| format!("`{}`", environments::short(command, 80)))
                .collect::<Vec<_>>()
                .join(", "),
            if ran {
                "succeeded"
            } else {
                "skipped: its lockfiles, commands and tools are unchanged since it last succeeded in this worktree"
            }
        ));
    }
    if !profile.env.is_empty() {
        text.push_str(&format!(
            ". Variables: {}",
            profile.env.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !profile.notes.is_empty() {
        text.push_str(&format!(". Notes: {}", profile.notes.join(" · ")));
    }
    text.push('.');
    text
}

/// Runs `plan`'s script in a setup container of the session's pod, bounded by
/// [`LIMIT`].
fn run(sandbox: &Sandbox, plan: &Plan, cancel: &Cancel) -> Result<environments::Outcome> {
    let mut command = Command::new("bash");
    command.arg("-s").current_dir(&plan.workspace);
    let (mut wrapped, mut container) = sandbox.command(Role::Setup, &command)?;
    let script = plan.script().into_bytes();
    let output = repos::bounded(LIMIT, cancel, |cancel| {
        process::run(&mut wrapped, Some(script), cancel)
    })?;
    container.exited();
    Ok(environments::parse(
        &String::from_utf8_lossy(&output.stdout),
        output.code,
    ))
}

/// `Request::Environment` on this machine: reads, or replaces with `profile`,
/// a server repository's profile. A change reaches the repository's running
/// sessions like an agent's.
pub(super) fn request(
    store: &Store,
    repository: &str,
    profile: Option<Profile>,
    by: Option<String>,
) -> Result<View> {
    ensure!(
        store.storage.load_config()?.server.enabled,
        "This computer is not a difu server; environment profiles are only available on servers"
    );
    let name = name(&store.storage, repository)?;
    if let Some(profile) = profile {
        let by = by
            .map(|by| by.trim().to_owned())
            .filter(|by| !by.is_empty())
            .unwrap_or_else(|| "a client".into());
        let (_, changes) = environments::replace(&store.storage, &name, &by, &profile)?;
        if !changes.is_empty() {
            changed(
                store,
                &name,
                None,
                &format!(
                    "{by} updated the {name} environment: {}. New commands and shells use it.",
                    environments::summary(&changes)
                ),
            );
        }
    }
    view(&store.storage, &name)
}

/// `owner/name` for `repository`: a server repository's name (in its listed
/// spelling) or a path inside its base clone.
pub(crate) fn name(storage: &Storage, repository: &str) -> Result<String> {
    let repository = repository.trim();
    ensure!(!repository.is_empty(), "Choose a repository");
    let name = if repository.starts_with(['/', '~', '.']) {
        repos::name_of(storage, Path::new(repository))
            .with_context(|| format!("{repository} is not one of this server's repositories"))?
    } else {
        repos::validate(repository)?;
        repository.to_owned()
    };
    Ok(storage
        .load_config()?
        .server
        .repositories
        .into_iter()
        .find(|listed| listed.eq_ignore_ascii_case(&name))
        .unwrap_or(name))
}

/// `repository`'s profile and what its base clone declares.
pub(crate) fn view(storage: &Storage, repository: &str) -> Result<View> {
    let base: PathBuf = repos::path(storage, repository)?;
    Ok(View {
        repository: repository.to_owned(),
        profile: environments::load(storage, repository)?,
        detected: if base.join(".git").is_dir() {
            environments::detect(&base)
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::session_tools::tests::{repo, session};
    use super::*;

    /// A server-mode store with a server repository `acme/widgets` (a Git
    /// repository with `.nvmrc`) and a session `one` working in it.
    pub(in crate::agents) fn fixture(temp: &Path) -> Result<(Store, PathBuf)> {
        let home = temp.join("agents");
        std::fs::create_dir_all(&home)?;
        let store = Store::fixture(home, Vec::new());
        store.storage.update_config(|config| {
            config.server.enabled = true;
            config.server.repositories = vec!["acme/widgets".into()];
        })?;
        let base = repos::path(&store.storage, "acme/widgets")?;
        let base = repo(&base)?;
        std::fs::write(base.join(".nvmrc"), "24\n")?;
        let mut one = session("one", &base);
        one.server_mode = true;
        one.title = "Billing allowlist".into();
        store.insert(one);
        Ok((store, base))
    }

    fn call(store: &Store, id: &str, args: Value) -> Result<Value> {
        handle(store, id, &args)
    }

    #[test]
    fn the_tool_is_offered_only_to_server_coding_sessions() -> Result<()> {
        let mut one = session("one", Path::new("/tmp/repo"));
        let names = |session: &Session| {
            super::super::orchestrator::tools(session)
                .iter()
                .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(String::from))
                .collect::<Vec<_>>()
        };
        assert!(!names(&one).iter().any(|name| name == TOOL));
        assert!(!instructions(&one).is_empty());
        one.server_mode = true;
        assert!(names(&one).iter().any(|name| name == TOOL));
        let mut manager = one.clone();
        manager.orchestrator = true;
        assert!(!offered(&manager));
        let tool = tool();
        assert_eq!(
            tool.pointer("/inputSchema/additionalProperties"),
            Some(&json!(false))
        );
        assert_eq!(
            tool.pointer("/inputSchema/properties/remember/additionalProperties"),
            Some(&json!(false))
        );
        one.environment = Some("Environment for acme/widgets ready.".into());
        assert!(
            instructions(&one).ends_with("Environment status: Environment for acme/widgets ready.")
        );
        assert!(INSTRUCTIONS.contains("Don't apt-install runtimes"));
        Ok(())
    }

    #[test]
    fn reading_reports_the_profile_detected_versions_and_resolution() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (store, _base) = fixture(temp.path())?;
        let read = call(&store, "one", json!({}))?;
        assert_eq!(read.get("repository"), Some(&json!("acme/widgets")));
        assert_eq!(
            read.pointer("/detected/0"),
            Some(&json!({"tool": "node", "version": "24", "source": ".nvmrc"}))
        );
        assert_eq!(read.pointer("/resolved/0/source"), Some(&json!(".nvmrc")));
        assert_eq!(
            read.get("sharedCache"),
            Some(&json!("/opt/difu/toolchains"))
        );
        assert_eq!(read.get("recorded"), Some(&json!([])));
        // Reading changes nothing.
        assert!(environments::load(&store.storage, "acme/widgets")?.is_empty());
        for args in [
            json!({"profile": {}}),
            json!({"remember": {"tools": {"node": ["24"]}}}),
            json!({"remember": {"secrets": {}}}),
            json!({"remember": "node 24"}),
            json!([1]),
        ] {
            assert!(call(&store, "one", args.clone()).is_err(), "{args}");
        }
        Ok(())
    }

    #[test]
    fn remembering_records_validates_and_tells_other_sessions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (store, base) = fixture(temp.path())?;
        let mut other = session("two", &base);
        other.server_mode = true;
        store.insert(other);
        let mut idle = session("three", &base);
        idle.server_mode = true;
        idle.status = super::super::Status::Idle;
        store.insert(idle);
        let reply = call(
            &store,
            "one",
            json!({"remember": {
                "tools": {"node": "24", "pnpm": 9},
                "setup": ["pnpm install --frozen-lockfile"],
                "env": {"NODE_OPTIONS": "--max-old-space-size=4096"},
                "notes": "backend tests need Postgres via docker compose"
            }}),
        )?;
        assert_eq!(reply.pointer("/profile/tools/pnpm"), Some(&json!("9")));
        assert_eq!(
            reply.pointer("/profile/setup/0"),
            Some(&json!("pnpm install --frozen-lockfile"))
        );
        assert_eq!(
            reply
                .get("recorded")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(5)
        );
        let profile = environments::load(&store.storage, "acme/widgets")?;
        let change = profile.history.last().context("history")?;
        assert!(
            change.by.starts_with("Codex in session one"),
            "{}",
            change.by
        );
        // The caller and the other active session hear about it; idle ones do not.
        let last = |id: &str| -> Result<String> {
            Ok(store
                .get(id)?
                .entries
                .last()
                .map(|entry| entry.text.to_string())
                .unwrap_or_default())
        };
        assert!(last("one")?.starts_with("Recorded in the acme/widgets environment"));
        assert!(
            last("two")?.starts_with("Another session recorded"),
            "{}",
            last("two")?
        );
        assert!(store.get("three")?.entries.is_empty());
        // The worktree's mise config follows at once.
        let state = environments::state_dir(&base).context("state")?;
        let config = std::fs::read_to_string(state.join("mise.toml"))?;
        assert!(
            config.contains("\"node\" = \"24\"") && config.contains("\"pnpm\" = \"9\""),
            "{config}"
        );
        // Secrets and bad versions are refused and change nothing.
        for args in [
            json!({"remember": {"env": {"NPM_TOKEN": "abc"}}}),
            json!({"remember": {"tools": {"node": "24; curl evil"}}}),
            json!({"remember": {"env": {"PATH": "/tmp"}}}),
        ] {
            assert!(call(&store, "one", args.clone()).is_err(), "{args}");
        }
        assert_eq!(environments::load(&store.storage, "acme/widgets")?, profile);
        let forgot = call(
            &store,
            "one",
            json!({"forget": {"tools": ["pnpm"], "setup": ["pnpm install --frozen-lockfile"]}}),
        )?;
        assert_eq!(forgot.pointer("/profile/tools/pnpm"), None);
        assert_eq!(forgot.pointer("/profile/setup"), Some(&json!([])));
        Ok(())
    }

    #[test]
    fn sessions_outside_server_repositories_or_servers_have_no_profile() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (store, _base) = fixture(temp.path())?;
        let elsewhere = repo(&temp.path().join("elsewhere"))?;
        let mut outside = session("outside", &elsewhere);
        outside.server_mode = true;
        store.insert(outside);
        let error = call(&store, "outside", json!({}))
            .err()
            .context("no profile")?;
        assert!(format!("{error:#}").contains("not one of the server's repositories"));
        let mut local = session("local", &elsewhere);
        local.server_mode = false;
        store.insert(local);
        assert!(call(&store, "local", json!({})).is_err());
        Ok(())
    }

    #[test]
    fn preparation_learns_repository_versions_and_skips_without_a_sandbox() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (store, _base) = fixture(temp.path())?;
        // Sandboxing is off: nothing runs and nothing is learned.
        prepare(&store, "one", &Cancel::default());
        assert!(environments::load(&store.storage, "acme/widgets")?.is_empty());
        // Learning on its own records the repository's versions once.
        let detected = vec![Detected {
            tool: "node".into(),
            version: "24".into(),
            source: ".nvmrc".into(),
            corepack: false,
        }];
        learn(&store, "one", "acme/widgets", &detected)?;
        learn(&store, "one", "acme/widgets", &detected)?;
        let profile = environments::load(&store.storage, "acme/widgets")?;
        assert_eq!(profile.tools.get("node").map(String::as_str), Some("24"));
        assert_eq!(profile.history.len(), 1);
        let change = profile.history.first().context("history")?;
        assert!(
            change.summary.contains("node 24 (from .nvmrc)"),
            "{}",
            change.summary
        );
        assert!(change.by.contains("session one"));
        let notes: Vec<String> = store
            .get("one")?
            .entries
            .iter()
            .map(|entry| entry.text.to_string())
            .collect();
        assert_eq!(
            notes
                .iter()
                .filter(|note| note.starts_with("Remembered in the acme/widgets environment"))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn setup_is_skipped_by_fingerprint_and_rerun_when_inputs_change() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let cache = temp.path().join("cache");
        let state = temp.path().join("state");
        let resolved = vec![Detected {
            tool: "node".into(),
            version: "24".into(),
            source: ".nvmrc".into(),
            corepack: false,
        }];
        let profile = Profile {
            setup: vec!["pnpm install --frozen-lockfile".into()],
            ..Profile::default()
        };
        let mut recorded = environments::State::default();
        let first = decide(&cache, &state, &recorded, &resolved, &profile, "t1", "s1");
        assert!(first.tools && first.setup);
        recorded.tools = Some("t1".into());
        recorded.setup = Some("s1".into());
        // Installed and unchanged: nothing runs.
        std::fs::create_dir_all(cache.join("mise/installs/node/24"))?;
        let again = decide(&cache, &state, &recorded, &resolved, &profile, "t1", "s1");
        assert!(!again.tools && !again.setup);
        // A lockfile change reruns setup only.
        let lockfile = decide(&cache, &state, &recorded, &resolved, &profile, "t1", "s2");
        assert!(!lockfile.tools && lockfile.setup);
        // A cleaned cache installs again.
        std::fs::remove_dir_all(cache.join("mise"))?;
        let cleaned = decide(&cache, &state, &recorded, &resolved, &profile, "t1", "s1");
        assert!(cleaned.tools && !cleaned.setup);
        // Nothing declared and no setup: nothing to do.
        let empty = decide(
            &cache,
            &state,
            &recorded,
            &[],
            &Profile::default(),
            "t",
            "s",
        );
        assert!(!empty.tools && !empty.setup);
        Ok(())
    }

    #[test]
    fn notes_describe_progress_readiness_and_skips() {
        let plan = Plan {
            tools: true,
            setup: vec!["pnpm install --frozen-lockfile".into()],
            ..Plan::default()
        };
        assert_eq!(
            starting("acme/widgets", "node 24 (.nvmrc)", &plan),
            "Preparing the environment for acme/widgets: node 24 (.nvmrc); then `pnpm install --frozen-lockfile`…"
        );
        let profile = Profile {
            setup: plan.setup.clone(),
            notes: vec!["backend tests need Postgres".into()],
            ..Profile::default()
        };
        let skipped = ready(
            "acme/widgets",
            "node 24 (.nvmrc)",
            &profile,
            false,
            "already installed",
        );
        assert!(skipped.contains("skipped: its lockfiles"), "{skipped}");
        assert!(skipped.contains("Notes: backend tests need Postgres"));
        let ran = ready("acme/widgets", "", &profile, true, "in 42s");
        assert!(ran.starts_with("Environment for acme/widgets ready (in 42s). Setup `pnpm install --frozen-lockfile` succeeded"), "{ran}");
        assert_eq!(elapsed(Duration::from_secs(72)), "1m 12s");
        assert_eq!(elapsed(Duration::from_secs(9)), "9s");
    }

    #[test]
    fn client_requests_read_and_replace_profiles_by_name_or_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (store, base) = fixture(temp.path())?;
        let view = request(&store, "ACME/widgets", None, None)?;
        assert_eq!(view.repository, "acme/widgets");
        assert_eq!(
            view.detected.first().map(|tool| tool.source.as_str()),
            Some(".nvmrc")
        );
        let mut profile = Profile::default();
        profile.tools.insert("python".into(), "3.12".into());
        let saved = request(
            &store,
            base.to_str().context("path")?,
            Some(profile),
            Some("Ali's iPhone".into()),
        )?;
        assert_eq!(
            saved.profile.tools.get("python").map(String::as_str),
            Some("3.12")
        );
        assert_eq!(
            saved
                .profile
                .history
                .last()
                .map(|change| change.by.as_str()),
            Some("Ali's iPhone")
        );
        // The running session hears about it.
        assert!(store.get("one")?.entries.last().is_some_and(|entry| {
            entry
                .text
                .contains("Ali's iPhone updated the acme/widgets environment")
        }));
        assert!(request(&store, "/elsewhere", None, None).is_err());
        assert!(request(&store, "../x", None, None).is_err());
        store
            .storage
            .update_config(|config| config.server.enabled = false)?;
        let error = request(&store, "acme/widgets", None, None)
            .err()
            .context("not a server")?;
        assert!(format!("{error:#}").contains("not a difu server"));
        Ok(())
    }
}
