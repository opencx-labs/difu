//! Bounded, explicit retrieval for the manager. Never return Session or Entry JSON.
use super::*;

pub(super) fn short(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = text.get(..end).unwrap_or_default().to_owned();
    if end < text.len() {
        result.push('…');
    }
    result
}
fn limit(args: &Value) -> Result<usize> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10);
    ensure!((1..=20).contains(&limit), "limit must be between 1 and 20");
    Ok(limit as usize)
}
pub(super) fn status(session: &Session) -> Value {
    json!({"id":session.id,"title":short(&session.title,512),"repository":session.job.root(),
        "status":session.status,"version":session.version,"archived":session.archived,
        "pending":session.pending_question_count(),"queued":session.queue.len(),
        "error":session.error.as_deref().map(|s|short(s,1024))})
}
pub(super) fn handle(
    store: &Store,
    caller: &str,
    args: &Value,
    action: &str,
) -> Result<Option<Value>> {
    match action {
        "status" | "read" => {
            let id = required(args, "id")?;
            // Preserve delivery of questions to the requesting Slack thread without
            // exporting permission payloads or chat history to the model.
            let route = store.inspect(caller, |s| s.slack_route.clone())?;
            if let Some(route) = route {
                store.update(&id, |s| {
                    for pending in &mut s.pending {
                        if let Some(params) = pending.params.as_object_mut() {
                            params
                                .entry("difuSlackRoute")
                                .or_insert_with(|| json!(route));
                        }
                    }
                })?;
                store.save(&id)?;
            }
            Ok(Some(store.inspect(&id, status)?))
        }
        "list" => {
            let mut sessions = store.list()?;
            sessions.sort_by(|a, b| a.id.cmp(&b.id));
            let after = args
                .get("after")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            let limit = limit(args)?;
            let mut matches = sessions.into_iter().filter(|s| {
                s.kind != "Orchestrator"
                    && s.id.as_str() > after
                    && (query.is_empty()
                        || s.title.to_lowercase().contains(&query)
                        || s.workspace
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&query))
            });
            let items:Vec<_>=matches.by_ref().take(limit).map(|s|json!({"id":s.id,"title":short(&s.title,512),"repository":s.repository,"status":s.status,"archived":s.archived,"version":s.version,"pending":s.pending,"queued":s.queued})).collect();
            let next = matches
                .next()
                .and_then(|_| items.last()?.get("id").cloned());
            Ok(Some(json!({"sessions":items,"next_after":next})))
        }
        "search" => {
            let query = required(args, "query")?;
            ensure!(query.len() <= 256, "Search query must be at most 256 bytes");
            let query = query.to_lowercase();
            let limit = limit(args)?;
            let target = args.get("id").and_then(Value::as_str);
            let after = args
                .get("after")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut sessions = store.list()?;
            sessions.sort_by(|a, b| a.id.cmp(&b.id));
            let mut hits = Vec::new();
            for s in sessions
                .into_iter()
                .filter(|s| s.kind != "Orchestrator" && target.is_none_or(|id| id == s.id))
            {
                store.inspect(&s.id, |session| {
                    for (index,entry) in session.entries.iter().enumerate() {
                        // Tool outputs may contain recursive copies of other chats.
                        // Search actual conversation messages, never those copies.
                        if !matches!(entry.kind.as_str(),"userMessage"|"agentMessage") { continue; }
                        let cursor=format!("{}:{index:020}",session.id);
                        if cursor.as_str()<=after {continue;}
                        let text=entry.text.to_lowercase();
                        let Some(offset)=text.find(&query) else {continue;};
                        // Unicode case conversion can change byte offsets. Use a
                        // conservative character-based preview around the match.
                        let start=text.get(..offset).unwrap_or_default().chars().count().saturating_sub(120);
                        let excerpt:String=entry.text.chars().skip(start).take(600).collect();
                        hits.push(json!({"session":session.id,"entry":entry.id,"kind":entry.kind,"cursor":cursor,"excerpt":excerpt}));
                        if hits.len()>limit {break;}
                    }
                })?;
                if hits.len() > limit {
                    break;
                }
            }
            let more = hits.len() > limit;
            hits.truncate(limit);
            let next = if more {
                hits.last().and_then(|v| v.get("cursor")).cloned()
            } else {
                None
            };
            Ok(Some(json!({"matches":hits,"next_after":next})))
        }
        "state_get" => {
            let key = required(args, "key")?;
            let path = record_path(store, &key)?;
            let record = read_record(&path)?;
            Ok(Some(json!({"key":key,"record":record})))
        }
        "state_list" => {
            let after = args
                .get("after")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let limit = limit(args)?;
            let directory = store.home.join("orchestrator-state");
            let mut keys = Vec::new();
            if directory.exists() {
                for entry in std::fs::read_dir(&directory)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file()
                        && let Some(key) = entry
                            .file_name()
                            .to_str()
                            .and_then(|s| s.strip_suffix(".json"))
                        && key > after
                    {
                        keys.push(key.to_owned());
                    }
                }
            }
            keys.sort();
            let more = keys.len() > limit;
            keys.truncate(limit);
            let next = if more { keys.last().cloned() } else { None };
            Ok(Some(json!({"keys":keys,"next_after":next})))
        }
        "state_put" => {
            let key = required(args, "key")?;
            let path = record_path(store, &key)?;
            let value = args.get("value").context("Missing value")?.clone();
            ensure!(
                serde_json::to_vec(&value)?.len() <= 8192,
                "Keep each task record within 8 KiB; split independent tasks into separate records"
            );
            let expected = args
                .get("revision")
                .and_then(Value::as_u64)
                .context("Supply the current revision, or 0 for a new record")?;
            let result = store.durable(|| {
                let revision = read_record(&path)?.map_or(0, |r| r.revision);
                ensure!(
                    revision == expected,
                    "State changed; retrieve the record before updating it"
                );
                let revision = revision
                    .checked_add(1)
                    .context("State revision exhausted")?;
                std::fs::create_dir_all(path.parent().context("Missing task directory")?)?;
                crate::storage::atomic_json(&path, &Record { revision, value })?;
                Ok(json!({"key":key,"revision":revision}))
            })?;
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

fn record_path(store: &Store, key: &str) -> Result<std::path::PathBuf> {
    ensure!(
        !key.is_empty()
            && key.len() <= 128
            && key != "."
            && key != ".."
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "State key must use 1–128 ASCII letters, digits, hyphens, underscores, or dots"
    );
    Ok(store
        .home
        .join("orchestrator-state")
        .join(format!("{key}.json")))
}
fn read_record(path: &std::path::Path) -> Result<Option<Record>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Task record must be a regular file"
    );
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(home: &std::path::Path) -> Store {
        let mut manager = Session::new(
            ID.into(),
            Job::Coding(Launch {
                repository: home.into(),
                isolated: false,
                base: String::new(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        manager.orchestrator = true;
        let mut worker = manager.clone();
        worker.id = "worker".into();
        worker.orchestrator = false;
        worker.note("userMessage", "Fix invoice rounding");
        worker.note("agentMessage", "The invoice total now uses cents");
        worker.note("dynamicToolCall", "invoice copied history".repeat(100_000));
        Store::fixture(home.into(), vec![manager, worker])
    }
    #[test]
    fn retrieval_never_exports_transcripts_or_nested_tool_results() -> Result<()> {
        let home = tempfile::tempdir()?;
        let store = fixture(home.path());
        let status = super::super::handle(&store, ID, &json!({"action":"read","id":"worker"}))?;
        assert!(status.get("entries").is_none());
        assert!(serde_json::to_vec(&status)?.len() < 2048);
        let first = super::super::handle(
            &store,
            ID,
            &json!({"action":"search","query":"invoice","limit":1}),
        )?;
        assert_eq!(first["matches"].as_array().map(Vec::len), Some(1));
        let next = super::super::handle(
            &store,
            ID,
            &json!({"action":"search","query":"invoice","limit":1,"after":first["next_after"]}),
        )?;
        assert_eq!(next["matches"].as_array().map(Vec::len), Some(1));
        assert!(next["next_after"].is_null());
        let legacy = super::super::handle(
            &store,
            ID,
            &json!({"action":"control","control":{"lookup":{"action":"search","query":"invoice"}}}),
        )?;
        assert_eq!(legacy["matches"].as_array().map(Vec::len), Some(2));
        assert!(
            super::super::handle(
                &store,
                ID,
                &json!({"action":"control","control":{"lookup":{"action":"delete","id":"worker"}}})
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn task_state_survives_restart_without_entering_chat_and_rejects_lost_updates() -> Result<()> {
        let home = tempfile::tempdir()?;
        let store = fixture(home.path());
        let args = json!({"action":"state_put","key":"billing-task","revision":0,"value":{"session":"worker","status":"assigned"}});
        let before = store.get(ID)?.version;
        super::super::handle(&store, ID, &args)?;
        assert_eq!(store.get(ID)?.version, before);
        assert!(super::super::handle(&store, ID, &args).is_err());
        drop(store);
        let store = fixture(home.path());
        let value = super::super::handle(
            &store,
            ID,
            &json!({"action":"state_get","key":"billing-task"}),
        )?;
        assert_eq!(value["record"]["revision"], 1);
        assert_eq!(value["record"]["value"]["session"], "worker");
        assert!(
            super::super::handle(&store, ID, &json!({"action":"state_get","key":"../config"}))
                .is_err()
        );
        Ok(())
    }
}
