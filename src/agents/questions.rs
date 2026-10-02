//! Codex asynchronous questions arrive on agent messages, not JSON-RPC requests.
use super::{Pending, Prompt, Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(super) const TOOL: &str = "difu_remove_questions";
pub(super) const INSTRUCTIONS: &str = "Use difu_remove_questions to remove stale unanswered questions from this session's question panel. Pass questions: [] to list pending question IDs, then pass the request_id and question_id of each question to remove. Removal does not answer a question or grant approval.";

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Remove specific stale, unanswered asynchronous questions from this session's question panel. Pass an empty questions array to inspect pending questions and their IDs. Returns the remaining questions. Removal persists across restarts and does not submit answers or grant approval.","inputSchema":{"type":"object","properties":{"questions":{"type":"array","items":{"type":"object","properties":{"request_id":{"type":"string"},"question_id":{"type":"string"}},"required":["request_id","question_id"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}})
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Removal {
    questions: Vec<QuestionId>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionId {
    request_id: String,
    question_id: String,
}

fn remove(session: &mut Session, args: &Value) -> Result<Value> {
    let removal: Removal = serde_json::from_value(args.clone())?;
    // Validate the entire batch before changing anything. Reuse skip tracking so
    // event replay cannot restore removed questions or overwrite saved answers.
    let mut updates = Vec::new();
    for question in &removal.questions {
        let request = json!(question.request_id);
        ensure!(
            session
                .pending
                .iter()
                .any(|pending| pending.id == request && pending.is_async_question()),
            "No asynchronous question request {} is pending",
            question.request_id
        );
        let (updated, _) = prepare_answer(session, &request, &question.question_id, None)?;
        updates.push(updated);
    }
    for updated in updates {
        record_answer(session, updated);
    }
    let remaining: Vec<_> = session
        .pending
        .iter()
        .filter(|pending| pending.is_async_question())
        .flat_map(|pending| {
            pending.unanswered_questions().into_iter().map(move |(_, q)| {
                json!({"request_id":pending.id,"question_id":q.get("id"),"question":q.get("question")})
            })
        })
        .collect();
    Ok(json!({"remaining_questions":remaining}))
}

pub(super) fn handle(store: &super::server::Store, id: &str, args: &Value) -> Result<Value> {
    let mut result = Ok(Value::Null);
    // Keep validation and removal under the same lock as user answers.
    store.update(id, |session| result = remove(session, args))?;
    let result = result?;
    store.save(id)?;
    Ok(result)
}

// Build at delivery time, including for answers queued during a workspace move.
// The display text stays the user's answer; this is a separate model input item.
pub(super) fn pending_context(session: &Session, prompt: &Prompt) -> Option<String> {
    let Prompt::QuestionAnswer {
        question_request,
        question_id,
        ..
    } = prompt
    else {
        return None;
    };
    let remaining: Vec<_> = session.pending.iter()
        .filter(|p| p.is_async_question())
        .flat_map(|pending| pending.unanswered_questions().into_iter().filter_map(move |(_, q)| {
            let id = q.get("id").and_then(Value::as_str)?;
            if pending.id == *question_request && question_id.as_deref().is_none_or(|answered| answered == id) {
                return None;
            }
            Some(json!({"request_id":pending.id,"question_id":id,"question":q.get("question")}))
        }))
        .collect();
    Some(format!(
        "Difu question state (application context): The user submits answers individually and each answer is delivered immediately. Apply the answer above and retain previous answers. The following questions are still pending in Difu's question panel; the user can answer them there. Do not ask them again, including reworded versions, or issue a replacement batch. Do not infer answers or approval for them. Continue independent work, or wait if a pending decision is required. Ask only genuinely new questions that are not already pending or answered. This list is a snapshot at delivery time; later answers supersede it.\nRemaining pending questions ({}): {}",
        remaining.len(),
        serde_json::to_string(&remaining).ok()?
    ))
}

pub(super) fn prepare_answer(
    session: &Session,
    request: &Value,
    question: &str,
    answer: Option<&str>,
) -> Result<(Pending, String)> {
    let pending = session
        .pending
        .iter()
        .find(|p| &p.id == request)
        .context("This question is no longer pending")?;
    ensure!(
        pending.method == "item/tool/requestUserInput",
        "Expected a question"
    );
    ensure!(
        pending.is_async_question() || !pending.responded,
        "A response was already sent; inspect the session before retrying"
    );
    let question_text = pending
        .unanswered_questions()
        .into_iter()
        .find(|(_, q)| q.get("id").and_then(Value::as_str) == Some(question))
        .and_then(|(_, q)| q.get("question"))
        .and_then(Value::as_str)
        .context("This question was already answered or skipped")?;
    let text = if let Some(answer) = answer {
        ensure!(
            !answer.trim().is_empty(),
            "Enter an answer or choose a suggestion"
        );
        format!("> {question_text}\n\n{answer}")
    } else {
        String::new()
    };
    let mut updated = pending.clone();
    // Async answers are independent messages. Older versions could leave this
    // batch-wide flag set after a failed send; it must not prevent a user retry.
    if updated.is_async_question() {
        updated.responded = false;
    }
    let params = updated
        .params
        .as_object_mut()
        .context("Invalid question parameters")?;
    let answers = params
        .entry("difuAnswers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Invalid saved answers")?;
    answers.insert(
        question.into(),
        json!({"answers":answer.into_iter().collect::<Vec<_>>()}),
    );
    Ok((updated, text))
}

pub(super) fn save_answer(store: &super::server::Store, id: &str, updated: Pending) -> Result<()> {
    store.update(id, |session| record_answer(session, updated))?;
    store.save(id)
}

pub(super) fn record_answer(session: &mut Session, mut updated: Pending) {
    // Answer delivery can overlap a skipped question from the same batch.
    // Keep answers saved since this response was prepared so they cannot reappear.
    if let Some(saved) = session.pending.iter().find(|p| p.id == updated.id)
        && let Some(answers) = saved.params.get("difuAnswers").and_then(Value::as_object)
        && let Some(params) = updated.params.as_object_mut()
        && let Some(merged) = params
            .entry("difuAnswers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
    {
        merged.extend(answers.clone());
    }
    if updated.is_async_question() && updated.unanswered_questions().is_empty() {
        if let Some(id) = updated.id.as_str() {
            session.answered_questions.insert(id.into());
        }
        session.pending.retain(|p| p.id != updated.id);
    } else if let Some(pending) = session.pending.iter_mut().find(|p| p.id == updated.id) {
        *pending = updated;
    }
}

impl Session {
    pub(crate) fn restore_async_questions(&mut self) {
        for entry in &self.entries {
            if entry.kind != "agentMessage"
                || entry.data.get("delivery").and_then(Value::as_str) != Some("async")
            {
                continue;
            }
            let Some(questions) = entry.data.get("questions").and_then(Value::as_array) else {
                continue;
            };
            if questions.is_empty() || entry.id.is_empty() {
                continue;
            }
            let id = format!("difu-async:{}", entry.id);
            if self.answered_questions.contains(&id)
                || self.pending.iter().any(|pending| pending.id == id)
            {
                continue;
            }
            let questions: Vec<_> = questions.iter().enumerate().map(|(index, question)| {
                let options: Vec<_> = question.get("options").and_then(Value::as_array)
                    .into_iter().flatten().filter_map(Value::as_str)
                    .map(|label| json!({"label":label,"description":""})).collect();
                json!({"id":index.to_string(),"header":format!("Question {}", index + 1),"question":question.get("title"),"options":options})
            }).collect();
            self.pending.push(Pending {
                id: json!(id),
                method: "item/tool/requestUserInput".into(),
                params: json!({"difuAsync":true,"itemId":entry.id,"questions":questions}),
                responded: false,
            });
        }
    }

    pub fn pending_question_count(&self) -> usize {
        self.pending
            .iter()
            .filter(|p| p.method == "item/tool/requestUserInput")
            .map(|p| p.unanswered_questions().len())
            .sum()
    }
}

pub(super) fn answer_text(pending: &Pending, response: &Value) -> Result<String> {
    ensure!(
        pending.is_async_question(),
        "Expected asynchronous questions"
    );
    let questions = pending
        .params
        .get("questions")
        .and_then(Value::as_array)
        .context("Missing questions")?;
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .context("Missing answers")?;
    let mut text = String::from("Answers to your questions:");
    for question in questions {
        let id = question
            .get("id")
            .and_then(Value::as_str)
            .context("Missing question ID")?;
        let title = question
            .get("question")
            .and_then(Value::as_str)
            .context("Missing question text")?;
        let answer = answers
            .get(id)
            .and_then(|v| v.get("answers"))
            .and_then(Value::as_array)
            .context("Missing question answer")?;
        let answer = answer
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        ensure!(
            !answer.trim().is_empty(),
            "Answer each question before submitting"
        );
        text.push_str(&format!("\n\n> {title}\n{answer}"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Job, Launch, engine::apply_event};

    fn session() -> Session {
        Session::new(
            "test".into(),
            Job::Coding(Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "questions".into(),
                model: None,
                effort: None,
            }),
        )
    }

    fn question_event(id: &str, method: &str) -> Value {
        json!({"method":method,"params":{"item":{
            "id":id,"type":"agentMessage","delivery":"async","text":"Choose scope",
            "questions":[{"title":"Scope?","options":["Small","Large"]},{"title":"Any notes?"}]
        }}})
    }

    #[test]
    fn removed_questions_stay_removed_without_losing_answers_or_other_batches() -> Result<()> {
        let mut session = session();
        for id in ["q", "other"] {
            apply_event(&mut session, &question_event(id, "item/completed"));
        }
        let listing = remove(&mut session, &json!({"questions":[]}))?;
        let remaining = listing
            .get("remaining_questions")
            .and_then(Value::as_array)
            .context("questions")?;
        assert_eq!(remaining.len(), 4);
        assert_eq!(
            remaining.first().and_then(|q| q.get("request_id")),
            Some(&json!("difu-async:q"))
        );
        let request = json!("difu-async:q");
        let (answer, _) = prepare_answer(&session, &request, "0", Some("Small"))?;
        let before_entries = session.entries.len();
        remove(
            &mut session,
            &json!({"questions":[{"request_id":"difu-async:q","question_id":"1"}]}),
        )?;
        // An answer prepared before removal can still arrive without restoring it.
        record_answer(&mut session, answer);
        assert_eq!(session.pending_question_count(), 2);
        assert_eq!(session.entries.len(), before_entries);
        assert!(session.queue.is_empty());
        assert!(session.answered_questions.contains("difu-async:q"));
        let mut restored: Session = serde_json::from_value(serde_json::to_value(session)?)?;
        restored.restore_async_questions();
        apply_event(&mut restored, &question_event("q", "item/completed"));
        assert_eq!(restored.pending_question_count(), 2);
        assert_eq!(
            restored.pending.first().context("pending")?.id,
            "difu-async:other"
        );
        remove(
            &mut restored,
            &json!({"questions":[
                {"request_id":"difu-async:other","question_id":"0"},
                {"request_id":"difu-async:other","question_id":"1"}
            ]}),
        )?;
        restored.restore_async_questions();
        assert_eq!(restored.pending_question_count(), 0);
        Ok(())
    }

    #[test]
    fn removal_validates_the_whole_batch_and_recovers_stale_async_flags() -> Result<()> {
        let mut session = session();
        apply_event(&mut session, &question_event("q", "item/completed"));
        let before = serde_json::to_value(&session)?;
        for args in [
            json!({"questions":[
                {"request_id":"difu-async:q","question_id":"0"},
                {"request_id":"difu-async:q","question_id":"missing"}
            ]}),
            json!({"questions":[{"request_id":"another-session","question_id":"0"}]}),
            json!({"questions":[],"session_id":"another-session"}),
        ] {
            assert!(remove(&mut session, &args).is_err());
            assert_eq!(serde_json::to_value(&session)?, before);
        }
        session.pending.first_mut().context("pending")?.responded = true;
        let result = remove(
            &mut session,
            &json!({"questions":[{"request_id":"difu-async:q","question_id":"0"}]}),
        );
        result?;
        assert_eq!(session.pending_question_count(), 1);
        let pending = session.pending.first_mut().context("pending")?;
        pending.responded = false;
        pending
            .params
            .as_object_mut()
            .context("params")?
            .insert("difuAsync".into(), json!(false));
        let result = remove(
            &mut session,
            &json!({"questions":[{"request_id":"difu-async:q","question_id":"0"}]}),
        );
        assert!(result.is_err());
        assert_eq!(session.pending_question_count(), 1);
        Ok(())
    }

    #[test]
    fn unanswered_async_questions_allow_retry_after_a_persisted_send_failure() -> Result<()> {
        let mut session = session();
        apply_event(&mut session, &question_event("q", "item/completed"));
        session.pending.first_mut().context("pending")?.responded = true;
        let mut restored: Session = serde_json::from_value(serde_json::to_value(session)?)?;
        restored.restore_async_questions();
        let request = json!("difu-async:q");
        // Preparing an answer does not consume it if delivery subsequently fails.
        let _ = prepare_answer(&restored, &request, "0", Some("Small"))?;
        assert_eq!(restored.pending_question_count(), 2);
        let (updated, text) = prepare_answer(&restored, &request, "0", Some("Small"))?;
        assert!(!updated.responded);
        assert_eq!(text, "> Scope?\n\nSmall");
        record_answer(&mut restored, updated);
        assert!(prepare_answer(&restored, &request, "0", Some("Small")).is_err());
        let (updated, _) = prepare_answer(&restored, &request, "1", Some("Keep the draft"))?;
        record_answer(&mut restored, updated);
        restored.restore_async_questions();
        assert_eq!(restored.pending_question_count(), 0);
        assert!(restored.answered_questions.contains("difu-async:q"));
        Ok(())
    }

    #[test]
    fn native_question_responses_keep_the_duplicate_guard() -> Result<()> {
        let mut session = session();
        apply_event(&mut session, &question_event("q", "item/completed"));
        let pending = session.pending.first_mut().context("pending")?;
        pending.responded = true;
        pending.params["difuAsync"] = json!(false);
        assert!(prepare_answer(&session, &json!("difu-async:q"), "0", Some("Small")).is_err());
        Ok(())
    }

    #[test]
    fn answers_include_remaining_questions_without_changing_display_text() -> Result<()> {
        let mut session = session();
        let event = json!({"method":"item/completed","params":{"item":{
            "id":"four","type":"agentMessage","delivery":"async","text":"Four questions",
            "questions":[{"title":"Charging?"},{"title":"Eligibility?"},{"title":"Identity?"},{"title":"History?"}]
        }}});
        apply_event(&mut session, &event);
        let request = json!("difu-async:four");
        for (index, title) in ["Charging?", "Eligibility?", "Identity?", "History?"]
            .iter()
            .enumerate()
        {
            let (updated, text) = prepare_answer(
                &session,
                &request,
                &index.to_string(),
                Some("My answer\nNote: keep it simple"),
            )?;
            let prompt = Prompt::QuestionAnswer {
                text: text.clone(),
                question_request: request.clone(),
                question_id: Some(index.to_string()),
            };
            let context = pending_context(&session, &prompt).context("answer context")?;
            assert!(context.contains(&format!("Remaining pending questions ({})", 3 - index)));
            assert!(!context.contains(title));
            assert!(context.contains("Do not ask them again, including reworded versions"));
            assert_eq!(
                prompt.text(),
                format!("> {title}\n\nMy answer\nNote: keep it simple")
            );
            // Persisted queued answers keep their identity; context reflects delivery-time state.
            let queued: Prompt = serde_json::from_value(serde_json::to_value(&prompt)?)?;
            assert_eq!(prompt, queued);
            record_answer(&mut session, updated);
            assert_eq!(pending_context(&session, &queued), Some(context));
        }
        assert!(pending_context(&session, &Prompt::from("ordinary message")).is_none());
        apply_event(&mut session, &question_event("other", "item/completed"));
        let prompt = Prompt::QuestionAnswer {
            text: "Earlier answer".into(),
            question_request: request,
            question_id: None,
        };
        let context = pending_context(&session, &prompt).context("other batch")?;
        assert!(context.contains("Remaining pending questions (2)"));
        assert!(context.contains("Scope?") && context.contains("Any notes?"));
        let prompt = Prompt::QuestionAnswer {
            text: "Whole batch".into(),
            question_request: json!("difu-async:other"),
            question_id: None,
        };
        assert!(
            pending_context(&session, &prompt)
                .context("batch context")?
                .contains("Remaining pending questions (0): []")
        );
        Ok(())
    }

    #[test]
    fn overlapping_answers_do_not_restore_answered_or_skipped_questions() -> Result<()> {
        for reverse in [false, true] {
            let mut session = session();
            apply_event(&mut session, &question_event("q", "item/completed"));
            let request = json!("difu-async:q");
            let (answer, _) = prepare_answer(&session, &request, "0", Some("Small"))?;
            let (skip, _) = prepare_answer(&session, &request, "1", None)?;
            let updates = if reverse {
                [answer, skip]
            } else {
                [skip, answer]
            };
            for (index, update) in updates.into_iter().enumerate() {
                record_answer(&mut session, update);
                assert_eq!(session.pending_question_count(), 1 - index);
            }
            assert!(session.pending.is_empty());
            assert!(session.answered_questions.contains("difu-async:q"));
            let mut restored: Session = serde_json::from_value(serde_json::to_value(session)?)?;
            restored.restore_async_questions();
            apply_event(&mut restored, &question_event("q", "item/completed"));
            assert_eq!(restored.pending_question_count(), 0);
        }
        Ok(())
    }

    #[test]
    fn replayed_questions_keep_identity_and_partial_answers_after_restart() -> Result<()> {
        let mut session = session();
        for method in ["item/started", "item/completed", "item/completed"] {
            apply_event(&mut session, &question_event("q", method));
        }
        assert_eq!(session.pending.len(), 1);
        assert_eq!(session.entries.len(), 1);
        let request = json!("difu-async:q");
        let (answer, _) = prepare_answer(&session, &request, "0", Some("Small"))?;
        record_answer(&mut session, answer);
        let mut restored: Session = serde_json::from_value(serde_json::to_value(session)?)?;
        for _ in 0..3 {
            restored.restore_async_questions();
            apply_event(&mut restored, &question_event("q", "item/completed"));
            assert_eq!(restored.pending.len(), 1);
            assert_eq!(restored.pending_question_count(), 1);
            assert!(prepare_answer(&restored, &request, "0", Some("Small")).is_err());
        }
        let (answer, _) = prepare_answer(&restored, &request, "1", Some("No notes"))?;
        record_answer(&mut restored, answer);
        apply_event(&mut restored, &question_event("q", "item/completed"));
        assert!(restored.pending.is_empty());
        // A new question may intentionally reuse the same wording.
        apply_event(&mut restored, &question_event("new-q", "item/completed"));
        assert_eq!(restored.pending.len(), 1);
        assert_eq!(restored.pending_question_count(), 2);
        Ok(())
    }
}
