//! Codex asynchronous questions arrive on agent messages, not JSON-RPC requests.
use super::{Pending, Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

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
    store.update(id, |session| {
        if updated.is_async_question() && updated.unanswered_questions().is_empty() {
            if let Some(id) = updated.id.as_str() {
                session.answered_questions.insert(id.into());
            }
            session.pending.retain(|p| p.id != updated.id);
        } else if let Some(pending) = session.pending.iter_mut().find(|p| p.id == updated.id) {
            *pending = updated;
        }
    })?;
    store.save(id)
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
