use crate::{
    diff::Snapshot,
    model::{ModelChoice, ModelInfo, PrDetail},
    process::{self, Cancel},
    repo::Worktree,
    storage::{self, Storage},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub const INSTRUCTIONS: &str = include_str!("../prompts/guide.md");

// Progress presentation does not change guide content or invalidate cached guides.
const PROGRESS_INSTRUCTIONS: &str = "While working, occasionally send a brief commentary preamble starting with 'Progress: ' describing the concrete inspection or chapter-grouping task underway. Use one short sentence, without private reasoning, command output, or code excerpts. The JSON-only requirement applies to your final response; keep that final response strictly within the supplied schema.";

fn progress_message(event: &Value) -> Option<String> {
    let kind = event.get("type")?.as_str()?;
    let item = event.get("item");
    let item_kind = item.and_then(|i| i.get("type")).and_then(Value::as_str);
    if matches!(kind, "item.started" | "item.updated" | "item.completed")
        && item_kind == Some("agent_message")
    {
        // An explicit prefix works with CLI versions that omit message phase.
        // Never put the final structured guide or full reasoning text in the footer.
        let message = item?
            .get("text")?
            .as_str()?
            .trim()
            .strip_prefix("Progress: ")?;
        let cleaned = crate::model::clean(message);
        let single_line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if single_line.is_empty() {
            return None;
        }
        return Some(format!(
            "Codex: {}",
            single_line.chars().take(240).collect::<String>()
        ));
    }
    if matches!(kind, "item.started" | "item.updated" | "item.completed")
        && item_kind == Some("reasoning")
    {
        // Codex summary events may contain a bold heading followed by prose.
        // Accept only that complete, short heading; never display the body or
        // treat an unstructured reasoning paragraph as a status message.
        let first_line = item?.get("text")?.as_str()?.trim_start().lines().next()?;
        let (heading, _) = first_line.strip_prefix("**")?.split_once("**")?;
        let heading = heading.trim();
        if heading.is_empty()
            || heading.chars().count() > 120
            || heading.chars().any(char::is_control)
        {
            return None;
        }
        return Some(format!("Codex summary: {}", crate::model::clean(heading)));
    }
    let message = match kind {
        "thread.started" => "Codex connected",
        "turn.started" => "Reading the PR and planning chapters",
        "item.started" if item_kind == Some("command_execution") => "Reading repository context",
        "item.completed" if item_kind == Some("command_execution") => "Repository context read",
        "turn.completed" => "Validating chapter coverage",
        _ => return None,
    };
    Some(message.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guide {
    pub chapters: Vec<Chapter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chapter {
    pub title: String,
    pub explanation: String,
    pub hunks: Vec<String>,
}

impl Guide {
    pub fn validate(&self, snapshot: &Snapshot) -> Result<()> {
        let expected: HashSet<_> = snapshot.units().map(|(_, h)| h.id.as_str()).collect();
        ensure!(
            !self.chapters.is_empty() || expected.is_empty(),
            "Guide has no chapters"
        );
        let mut seen = HashSet::new();
        for chapter in &self.chapters {
            ensure!(
                !chapter.title.trim().is_empty() && !chapter.explanation.trim().is_empty(),
                "Guide has an empty title or explanation"
            );
            ensure!(
                !chapter.hunks.is_empty(),
                "Chapter '{}' references no changes",
                chapter.title
            );
            for id in &chapter.hunks {
                ensure!(
                    expected.contains(id.as_str()),
                    "Guide references an unknown hunk: {id}"
                );
                ensure!(
                    seen.insert(id.as_str()),
                    "Guide assigns hunk {id} more than once"
                );
            }
        }
        let missing: Vec<_> = expected.difference(&seen).copied().collect();
        ensure!(
            missing.is_empty(),
            "Guide omitted {} change(s): {}. Retry generation.",
            missing.len(),
            missing.join(", ")
        );
        Ok(())
    }
}

pub fn cache_key(pr: &PrDetail, snapshot: &Snapshot, model: &ModelChoice) -> Result<String> {
    // Tree identities cover all repository context Codex can read. Commit-only
    // rewrites can reuse a guide; changed code, hunks, or instructions cannot.
    Ok(storage::hash(serde_json::to_vec(&(
        pr.key.id(),
        &pr.title,
        &pr.body,
        (&snapshot.head_tree, &snapshot.base_tree, &snapshot.files),
        model,
        INSTRUCTIONS,
        schema(),
    ))?))
}

/// Preserve access to guides written before content-based cache identities.
pub fn legacy_cache_key(pr: &PrDetail, snapshot: &Snapshot, model: &ModelChoice) -> Result<String> {
    #[derive(Serialize)]
    struct LegacySnapshot<'a> {
        base: &'a str,
        head: &'a str,
        merge_base: &'a str,
        files: &'a [crate::diff::DiffFile],
    }
    let legacy = LegacySnapshot {
        base: &snapshot.base,
        head: &snapshot.head,
        merge_base: &snapshot.merge_base,
        files: &snapshot.files,
    };
    Ok(storage::hash(serde_json::to_vec(&(
        pr.key.id(),
        &pr.title,
        &pr.body,
        legacy,
        model,
        INSTRUCTIONS,
        schema(),
    ))?))
}

fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["chapters"],"properties":{"chapters":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["title","explanation","hunks"],"properties":{"title":{"type":"string"},"explanation":{"type":"string"},"hunks":{"type":"array","items":{"type":"string"}}}}}}})
}

// An empty table is merged with inherited MCP configuration, so it does not
// disable servers. Enumerate names without logging transports or credentials.
fn isolation_overrides(root: &Path, worktree: &Path, cancel: &Cancel) -> Result<Vec<String>> {
    let mut args = vec![
        "-c".into(),
        "project_doc_max_bytes=0".into(),
        "-c".into(),
        format!(
            "developer_instructions={}",
            serde_json::to_string(&format!("{INSTRUCTIONS}\n\n{PROGRESS_INSTRUCTIONS}"))?
        ),
    ];
    let mut projects = Vec::new();
    for path in [root, worktree] {
        let canonical = path.canonicalize()?;
        let path = canonical
            .to_str()
            .context("Codex requires a UTF-8 repository path")?;
        projects.push(format!(
            "{} = {{ trust_level = \"untrusted\" }}",
            serde_json::to_string(path)?
        ));
    }
    // Put quoted path keys inside TOML; the CLI's dotted-key parser does not
    // interpret quoted segments in -c keys.
    args.extend([
        "-c".into(),
        format!("projects = {{ {} }}", projects.join(", ")),
    ]);
    let output = process::run(
        Command::new("codex")
            .current_dir(worktree)
            .args(["mcp", "list", "--json"])
            .args(&args),
        None,
        cancel,
    )?;
    ensure!(
        output.code == 0,
        "Cannot inspect Codex connector configuration; guide generation stopped: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    args.extend(disable_mcp_overrides(&output.stdout)?);
    Ok(args)
}
fn disable_mcp_overrides(output: &[u8]) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Server {
        name: String,
        transport: Transport,
    }
    #[derive(Deserialize)]
    struct Transport {
        #[serde(rename = "type")]
        kind: String,
    }
    let servers: Vec<Server> =
        serde_json::from_slice(output).context("Cannot verify Codex MCP server configuration")?;
    let mut entries = Vec::new();
    for server in servers {
        ensure!(
            !server.name.is_empty(),
            "Codex returned an unnamed MCP server"
        );
        // With --ignore-user-config the inherited entry may no longer exist.
        // Disabled entries still require a valid transport. Supply inert values
        // of the same transport type, without copying credentials into argv.
        let transport = match server.transport.kind.as_str() {
            "stdio" => "command = \"false\"",
            "streamable_http" => "url = \"http://127.0.0.1:9\"",
            _ => anyhow::bail!("Cannot safely disable an unknown Codex MCP transport"),
        };
        entries.push(format!(
            "{} = {{ enabled = false, {transport} }}",
            serde_json::to_string(&server.name)?
        ));
    }
    Ok(vec![
        "-c".into(),
        format!("mcp_servers = {{ {} }}", entries.join(", ")),
    ])
}

pub fn generate(
    root: &Path,
    pr: &PrDetail,
    snapshot: &Snapshot,
    model: &ModelChoice,
    storage: &Storage,
    cancel: &Cancel,
    progress: impl Fn(String) + Send + Sync + 'static,
) -> Result<Guide> {
    let key = cache_key(pr, snapshot, model)?;
    progress("Preparing the PR worktree".into());
    let mut worktree = Worktree::create(root, &snapshot.head, cancel)?;
    let result = (|| -> Result<Guide> {
        let inputs = tempfile::Builder::new().prefix("difu-guide-").tempdir()?;
        let manifest = inputs.path().join("review.json");
        let output = inputs.path().join("guide.json");
        let schema_path = inputs.path().join("schema.json");
        fs::write(
            &manifest,
            serde_json::to_vec(
                &json!({"pull_request":{"title":pr.title,"description":pr.body},"snapshot":snapshot}),
            )?,
        )?;
        fs::write(&schema_path, serde_json::to_vec(&schema())?)?;
        let overrides = isolation_overrides(root, &worktree.path, cancel)?;
        let prompt = format!(
            "Review input JSON file: {}\nRepository snapshot: {}\nBase for comparison: {}\nHead: {}\nRead the input file and explore the repository as needed, then return the guide JSON.\n",
            manifest.display(),
            worktree.path.display(),
            snapshot.merge_base,
            snapshot.head
        );
        let mut command = Command::new("codex");
        command
            .current_dir(&worktree.path)
            .args([
                "exec",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--sandbox",
                "read-only",
                "--json",
                "--color",
                "never",
            ])
            .args([
                "--model",
                &model.model,
                "-c",
                &format!(
                    "model_reasoning_effort={}",
                    serde_json::to_string(&model.effort)?
                ),
                "-c",
                "model_reasoning_summary=\"auto\"",
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "web_search=\"disabled\"",
            ])
            .args([
                "--disable",
                "apps",
                "--disable",
                "plugins",
                "--disable",
                "hooks",
                "--disable",
                "multi_agent",
                "--disable",
                "memories",
                "--disable",
                "browser_use",
                "--disable",
                "computer_use",
            ])
            .args(overrides)
            .arg("--output-schema")
            .arg(&schema_path)
            .arg("--output-last-message")
            .arg(&output)
            .arg("-");
        progress("Starting Codex".into());
        let response = process::streaming(
            &mut command,
            Some(prompt.into_bytes()),
            cancel,
            move |line| {
                if let Ok(event) = serde_json::from_str::<Value>(line)
                    && let Some(message) = progress_message(&event)
                {
                    progress(message);
                }
            },
        )?;
        ensure!(
            response.code == 0,
            "Codex could not generate the guide: {}",
            String::from_utf8_lossy(&response.stderr).trim()
        );
        cancel.check()?;
        let guide: Guide =
            serde_json::from_slice(&fs::read(output).context("Codex did not produce a guide")?)
                .context("Codex returned a guide with an invalid structure")?;
        guide.validate(snapshot)?;
        storage.save_guide(&key, &guide)?;
        Ok(guide)
    })();
    let cleanup = worktree.cleanup();
    match (result, cleanup) {
        (Ok(guide), Ok(())) => Ok(guide),
        (Err(error), Ok(())) => Err(error),
        (result, Err(error)) => Err(error.context(if result.is_ok() {
            "Guide cached, but cleanup needs attention"
        } else {
            "Generation and cleanup failed"
        })),
    }
}

/// Discover real model/effort combinations through Codex's supported protocol.
/// This starts no model turn and does not change Codex settings.
pub fn models(cancel: &Cancel) -> Result<Vec<ModelInfo>> {
    let mut command = Command::new("codex");
    command
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = process::ChildGroup::spawn(&mut command)
        .context("Could not start Codex model discovery")?;
    let mut input = child
        .child
        .stdin
        .take()
        .context("Missing Codex input pipe")?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("Missing Codex output pipe")?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("difu-models".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
    let result = (|| -> Result<Vec<ModelInfo>> {
        writeln!(
            input,
            "{}",
            json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"difu","version":env!("CARGO_PKG_VERSION")},"capabilities":{}}})
        )?;
        let receive = |id: u64| -> Result<Value> {
            loop {
                cancel.check()?;
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Ok(line)) => {
                        let v: Value = serde_json::from_str(&line)?;
                        if v.get("id").unwrap_or(&Value::Null).as_u64() == Some(id) {
                            ensure!(
                                v.get("error").is_none(),
                                "Codex model discovery failed: {}",
                                v.get("error").unwrap_or(&Value::Null)
                            );
                            return Ok(v.get("result").unwrap_or(&Value::Null).clone());
                        }
                    }
                    Ok(Err(error)) => return Err(error.into()),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("Codex closed model discovery")
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        };
        receive(1)?;
        writeln!(input, "{}", json!({"method":"initialized","params":{}}))?;
        let mut cursor = Value::Null;
        let mut all = Vec::new();
        let mut id = 2;
        loop {
            writeln!(
                input,
                "{}",
                json!({"id":id,"method":"model/list","params":{"limit":100,"includeHidden":true,"cursor":cursor}})
            )?;
            let result = receive(id)?;
            for model in result
                .get("data")
                .unwrap_or(&Value::Null)
                .as_array()
                .context("Codex returned no model catalog")?
            {
                let efforts = model
                    .get("supportedReasoningEfforts")
                    .unwrap_or(&Value::Null)
                    .as_array()
                    .context("Model has no supported reasoning levels")?
                    .iter()
                    .filter_map(|e| {
                        e.get("reasoningEffort")
                            .unwrap_or(&Value::Null)
                            .as_str()
                            .map(String::from)
                    })
                    .collect::<Vec<_>>();
                if !efforts.is_empty() {
                    all.push(ModelInfo {
                        id: model
                            .get("model")
                            .unwrap_or(&Value::Null)
                            .as_str()
                            .context("Model has no identifier")?
                            .into(),
                        name: model
                            .get("displayName")
                            .unwrap_or(&Value::Null)
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                        efforts,
                    });
                }
            }
            cursor = result.get("nextCursor").unwrap_or(&Value::Null).clone();
            if cursor.is_null() {
                break;
            }
            id += 1;
        }
        Ok(all)
    })();
    child.stop()?;
    drop(input);
    let _ = reader.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Result<Snapshot> {
        Ok(Snapshot {
            base: "b".into(),
            head: "h".into(),
            merge_base: "m".into(),
            head_tree: "head tree".into(),
            base_tree: "base tree".into(),
            files: crate::diff::parse("M\0a\0", "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\n")?,
        })
    }
    fn chapter(hunks: Vec<&str>) -> Chapter {
        Chapter {
            title: "Title".into(),
            explanation: "Explanation".into(),
            hunks: hunks.into_iter().map(String::from).collect(),
        }
    }
    #[test]
    fn progress_only_exposes_brief_commentary_not_final_json_or_reasoning() {
        assert_eq!(
            progress_message(
                &json!({"type":"item.completed","item":{"type":"agent_message","text":"Progress: Checking routing\n and tests."}})
            ),
            Some("Codex: Checking routing and tests.".into())
        );
        for item in [
            json!({"type":"agent_message","text":"{\"chapters\":[]}"}),
            json!({"type":"reasoning","text":"Progress: private reasoning"}),
            json!({"type":"command_execution","aggregated_output":"secret output"}),
        ] {
            let message = progress_message(&json!({"type":"item.completed","item":item}));
            assert!(message.is_none_or(|s| s == "Repository context read"));
        }
        let message = progress_message(
            &json!({"type":"item.updated","item":{"type":"agent_message","text":format!("Progress: {}", "é".repeat(500))}}),
        );
        assert_eq!(message.map(|s| s.chars().count()), Some(247));
    }

    #[test]
    fn summary_progress_exposes_only_a_complete_short_heading() {
        for kind in ["item.started", "item.updated", "item.completed"] {
            assert_eq!(
                progress_message(&json!({"type":kind,"item":{
                    "type":"reasoning", "text":"**Inspecting request routing**\n\nPrivate reasoning body must never appear."
                }})),
                Some("Codex summary: Inspecting request routing".into())
            );
        }
        assert_eq!(
            progress_message(&json!({"type":"item.completed","item":{
                "type":"reasoning", "text":"**Checking tests** Private body on the same line."
            }})),
            Some("Codex summary: Checking tests".into())
        );
        for text in [
            "Unstructured reasoning paragraph".into(),
            "**Incomplete heading".into(),
            "****".into(),
            "**Unsafe\u{1b} heading**".into(),
            format!("**{}**", "é".repeat(121)),
        ] {
            assert!(
                progress_message(&json!({"type":"item.completed","item":{
                    "type":"reasoning", "text":text
                }}))
                .is_none()
            );
        }
        assert!(
            progress_message(&json!({"type":"turn.failed","item":{
                "type":"reasoning", "text":"**Not a summary event**"
            }}))
            .is_none()
        );
    }

    #[test]
    fn validation_rejects_omissions_inventions_and_duplicate_assignments() -> Result<()> {
        let s = snapshot()?;
        assert!(Guide { chapters: vec![] }.validate(&s).is_err());
        assert!(
            Guide {
                chapters: vec![chapter(vec!["invented"])]
            }
            .validate(&s)
            .is_err()
        );
        assert!(
            Guide {
                chapters: vec![chapter(vec!["f0-h0", "f0-h0"])]
            }
            .validate(&s)
            .is_err()
        );
        assert!(
            Guide {
                chapters: vec![chapter(vec!["f0-h0"])]
            }
            .validate(&s)
            .is_ok()
        );
        Ok(())
    }
    #[test]
    fn cache_identity_changes_with_code_description_and_model_settings() -> Result<()> {
        let mut snapshot = snapshot()?;
        let mut pr = PrDetail {
            key: crate::model::PrKey {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 1,
            },
            title: "title".into(),
            body: "description".into(),
            author: "author".into(),
            head: "h".into(),
            base: "b".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
            state: "open".into(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
        };
        let mut model = ModelChoice::default();
        let initial = cache_key(&pr, &snapshot, &model)?;
        assert_eq!(initial, cache_key(&pr, &snapshot, &model)?);
        pr.body.push_str(" more context");
        let description = cache_key(&pr, &snapshot, &model)?;
        assert_ne!(initial, description);
        model.effort = "low".into();
        let effort = cache_key(&pr, &snapshot, &model)?;
        assert_ne!(description, effort);
        model.model = "gpt-5.6-sol".into();
        let other_model = cache_key(&pr, &snapshot, &model)?;
        assert_ne!(effort, other_model);
        snapshot.head = "new revision".into();
        snapshot.base = "new base revision".into();
        snapshot.merge_base = "new merge base revision".into();
        assert_eq!(other_model, cache_key(&pr, &snapshot, &model)?);
        snapshot.head_tree = "changed repository contents".into();
        assert_ne!(other_model, cache_key(&pr, &snapshot, &model)?);
        Ok(())
    }
    #[test]
    fn disables_each_mcp_server_without_exposing_transport_config() -> Result<()> {
        let args = disable_mcp_overrides(br#"[{"name":"first","transport":{"type":"stdio","env":{"TOKEN":"private"}}},{"name":"second.with.dots","transport":{"type":"streamable_http"}}]"#)?;
        assert!(
            args.join(" ")
                .contains("\"first\" = { enabled = false, command = \"false\" }")
        );
        assert!(
            args.join(" ").contains(
                "\"second.with.dots\" = { enabled = false, url = \"http://127.0.0.1:9\" }"
            )
        );
        assert!(!args.join(" ").contains("private"));
        assert!(disable_mcp_overrides(br#"[{"transport":{}}]"#).is_err());
        Ok(())
    }
    #[test]
    fn schema_rejects_unvalidated_reference_fields() {
        assert!(
            serde_json::from_str::<Guide>(
                r#"{"chapters":[{"title":"t","explanation":"e","hunks":["f0-h0"],"line":123}]}"#
            )
            .is_err()
        );
    }
}
