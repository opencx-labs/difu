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
            Ok(Some(store.inspect(
                ID,
                |s| json!({"key":key,"record":s.orchestrator_state.get(&key)}),
            )?))
        }
        "state_list" => {
            let after = args
                .get("after")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let limit = limit(args)?;
            Ok(Some(store.inspect(ID, |s| {
                let mut keys = s
                    .orchestrator_state
                    .iter()
                    .filter(|(k, _)| k.as_str() > after);
                let records: Vec<_> = keys
                    .by_ref()
                    .take(limit)
                    .map(|(k, v)| json!({"key":k,"revision":v.revision}))
                    .collect();
                let next = keys
                    .next()
                    .and_then(|_| records.last()?.get("key").cloned());
                json!({"records":records,"next_after":next})
            })?))
        }
        "state_put" => {
            let key = required(args, "key")?;
            ensure!(key.len() <= 128, "State key must be at most 128 bytes");
            let value = args.get("value").context("Missing value")?.clone();
            ensure!(
                serde_json::to_vec(&value)?.len() <= 8192,
                "Keep each task record within 8 KiB; split independent tasks into separate records"
            );
            let expected = args
                .get("revision")
                .and_then(Value::as_u64)
                .context("Supply the current revision, or 0 for a new record")?;
            let mut result = Err(anyhow::anyhow!("State was not saved"));
            store.update(ID, |s| {
                let revision = s.orchestrator_state.get(&key).map_or(0, |v| v.revision);
                if revision != expected {
                    result = Err(anyhow::anyhow!(
                        "State changed; retrieve the record before updating it"
                    ));
                    return;
                }
                let revision = revision.saturating_add(1);
                s.orchestrator_state
                    .insert(key.clone(), Record { revision, value });
                result = Ok(json!({"key":key,"revision":revision}));
            })?;
            let result = result?;
            store.save(ID)?;
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}
