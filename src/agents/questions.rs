//! Codex asynchronous questions arrive on agent messages, not JSON-RPC requests.
use super::{Pending, Prompt, Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

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
        !pending.responded,
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
