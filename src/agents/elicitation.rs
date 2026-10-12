//! Present MCP forms as native questions and build protocol responses internally.
use super::Pending;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

const DECISION: &str = "difu-decision";

pub(super) fn is_form(pending: &Pending) -> bool {
    pending
        .params
        .get("difuElicitation")
        .and_then(Value::as_bool)
        == Some(true)
}

fn supported(schema: &Value) -> bool {
    matches!(
        schema.get("type").and_then(Value::as_str),
        Some("string" | "boolean" | "integer" | "number")
    ) && schema.get("oneOf").is_none()
        && schema.get("anyOf").is_none()
}

pub(super) fn normalize(pending: &mut Pending) {
    if pending.method != "mcpServer/elicitation/request" {
        return;
    }
    let Some(params) = pending.params.as_object() else {
        return;
    };
    let properties = params
        .get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object);
    let supported = params.get("mode").and_then(Value::as_str) == Some("url")
        || properties.is_some_and(|p| p.values().all(supported));
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("This tool needs your permission.");
    let prompt = if supported {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        format!(
            "{message}\n{url}\nAllow this request? Any requested information will be collected as individual questions."
        )
    } else {
        format!(
            "{message}\nThis tool requested a form difu cannot display. Decline or cancel it, then ask the agent to collect the information through normal questions."
        )
    };
    let mut choices = vec![
        json!({"label":"Deny","description":"Decline this request"}),
        json!({"label":"Cancel","description":"Cancel without granting permission"}),
    ];
    if supported {
        choices.insert(
            0,
            json!({"label":"Allow once","description":"Allow only this request"}),
        );
    }
    let mut questions =
        vec![json!({"id":DECISION,"header":"Permission","question":prompt,"options":choices})];
    if supported && let Some(properties) = properties {
        for (name, schema) in properties {
            let title = schema.get("title").and_then(Value::as_str).unwrap_or(name);
            let description = schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let required = required(&pending.params, name);
            let hint = match schema.get("type").and_then(Value::as_str) {
                Some("integer") => "Enter a whole number.",
                Some("number") => "Enter a number.",
                Some("boolean") => "Choose Yes or No.",
                _ => "",
            };
            let mut options: Vec<_> = schema
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|label| json!({"label":label,"description":""}))
                .collect();
            if schema.get("type").and_then(Value::as_str) == Some("boolean") {
                options = vec![
                    json!({"label":"Yes","description":""}),
                    json!({"label":"No","description":""}),
                ];
            }
            if !required {
                options.push(json!({"label":"Skip this field","description":"Leave this optional field unset"}));
            }
            questions.push(json!({"id":format!("field:{name}"),"header":title,"question":format!("{title}\n{description}\n{hint}"),"options":options}));
        }
    }
    if let Some(params) = pending.params.as_object_mut() {
        params.insert("difuElicitation".into(), json!(true));
        params.insert("difuFormSupported".into(), json!(supported));
        params.insert("questions".into(), json!(questions));
    }
    pending.method = "item/tool/requestUserInput".into();
}

fn required(params: &Value, name: &str) -> bool {
    params
        .pointer("/requestedSchema/required")
        .and_then(Value::as_array)
        .is_some_and(|fields| fields.iter().any(|field| field.as_str() == Some(name)))
}

fn value(schema: &Value, text: &str) -> Result<Value> {
    let value = match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            let length = text.chars().count() as u64;
            ensure!(
                schema
                    .get("minLength")
                    .and_then(Value::as_u64)
                    .is_none_or(|min| length >= min),
                "The answer is too short"
            );
            ensure!(
                schema
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .is_none_or(|max| length <= max),
                "The answer is too long"
            );
            Value::String(text.into())
        }
        Some("boolean") => match text.trim().to_lowercase().as_str() {
            "yes" | "true" => json!(true),
            "no" | "false" => json!(false),
            _ => anyhow::bail!("Choose Yes or No"),
        },
        Some("integer") => json!(text.trim().parse::<i64>().context("Enter a whole number")?),
        Some("number") => {
            let number = text.trim().parse::<f64>().context("Enter a number")?;
            Value::Number(serde_json::Number::from_f64(number).context("Enter a finite number")?)
        }
        _ => anyhow::bail!("This field cannot be collected as a question"),
    };
    if let Some(number) = value.as_f64() {
        ensure!(
            schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_none_or(|min| number >= min),
            "The number is below the allowed minimum"
        );
        ensure!(
            schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_none_or(|max| number <= max),
            "The number exceeds the allowed maximum"
        );
    }
    ensure!(
        schema
            .get("enum")
            .and_then(Value::as_array)
            .is_none_or(|values| values.contains(&value)),
        "Choose one of the listed values"
    );
    Ok(value)
}

pub(super) fn check_answer(pending: &Pending, question: &str, answer: Option<&str>) -> Result<()> {
    if !is_form(pending) {
        return Ok(());
    }
    if question == DECISION {
        ensure!(
            matches!(answer, Some("Allow once" | "Deny" | "Cancel")),
            "Choose Allow once, Deny, or Cancel explicitly"
        );
        ensure!(
            answer != Some("Allow once")
                || pending
                    .params
                    .get("difuFormSupported")
                    .and_then(Value::as_bool)
                    == Some(true),
            "This form cannot be accepted; choose Deny or Cancel"
        );
    } else {
        let name = question
            .strip_prefix("field:")
            .context("Unknown form question")?;
        let schema = pending
            .params
            .pointer("/requestedSchema/properties")
            .and_then(|p| p.get(name))
            .context("Unknown form field")?;
        if let Some(text) = answer.filter(|text| *text != "Skip this field") {
            value(schema, text)?;
        } else {
            ensure!(!required(&pending.params, name), "This field is required");
        }
    }
    Ok(())
}

pub(super) fn finish_decline(pending: &mut Pending, question: &str, answer: Option<&str>) {
    if !is_form(pending) || question != DECISION || !matches!(answer, Some("Deny" | "Cancel")) {
        return;
    }
    let remaining: Vec<_> = pending
        .unanswered_questions()
        .into_iter()
        .filter_map(|(_, q)| q.get("id").and_then(Value::as_str).map(String::from))
        .collect();
    if let Some(answers) = pending
        .params
        .get_mut("difuAnswers")
        .and_then(Value::as_object_mut)
    {
        for id in remaining {
            answers.insert(id, json!({"answers":[]}));
        }
    }
}

pub(super) fn response(pending: &Pending, response: Value) -> Result<Value> {
    if !is_form(pending) {
        return Ok(response);
    }
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .context("Answer the request's questions")?;
    let answer = |id: &str| {
        answers
            .get(id)
            .and_then(|v| v.get("answers"))
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .and_then(Value::as_str)
    };
    let decision = answer(DECISION);
    check_answer(pending, DECISION, decision)?;
    match decision {
        Some("Deny") => return Ok(json!({"action":"decline"})),
        Some("Cancel") => return Ok(json!({"action":"cancel"})),
        _ => {}
    }
    let mut content = serde_json::Map::new();
    if let Some(properties) = pending
        .params
        .pointer("/requestedSchema/properties")
        .and_then(Value::as_object)
    {
        for (name, schema) in properties {
            let id = format!("field:{name}");
            let text = answer(&id);
            check_answer(pending, &id, text)?;
            if let Some(text) = text.filter(|text| *text != "Skip this field") {
                content.insert(name.clone(), value(schema, text)?);
            }
        }
    }
    Ok(json!({"action":"accept","content":content}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(schema: Value) -> Pending {
        let mut pending = Pending {
            id: json!(9),
            method: "mcpServer/elicitation/request".into(),
            params: json!({"message":"Export the report?","requestedSchema":schema}),
            responded: false,
        };
        normalize(&mut pending);
        pending
    }

    #[test]
    fn simple_permissions_are_questions_and_only_explicit_choices_grant_access() -> Result<()> {
        let pending = form(json!({"type":"object","properties":{}}));
        assert_eq!(pending.method, "item/tool/requestUserInput");
        assert_eq!(pending.unanswered_questions().len(), 1);
        for (answer, action) in [
            ("Allow once", "accept"),
            ("Deny", "decline"),
            ("Cancel", "cancel"),
        ] {
            let result = response(&pending, json!({"answers":{DECISION:{"answers":[answer]}}}))?;
            assert_eq!(result.get("action").and_then(Value::as_str), Some(action));
        }
        for answer in ["yes", "", "Allow for session", "Allow once\nonly if safe"] {
            assert!(check_answer(&pending, DECISION, Some(answer)).is_err());
        }
        Ok(())
    }

    #[test]
    fn form_answers_become_typed_content_without_json_input() -> Result<()> {
        let pending = form(json!({"type":"object","properties":{
            "format":{"type":"string","enum":["CSV","PDF"]},
            "count":{"type":"integer","minimum":1,"maximum":10},
            "include_notes":{"type":"boolean"},
            "comment":{"type":"string"}
        },"required":["format","count","include_notes"]}));
        let result = response(
            &pending,
            json!({"answers":{
                DECISION:{"answers":["Allow once"]},
                "field:format":{"answers":["CSV"]}, "field:count":{"answers":["3"]},
                "field:include_notes":{"answers":["No"]}, "field:comment":{"answers":["Skip this field"]}
            }}),
        )?;
        assert_eq!(
            result,
            json!({"action":"accept","content":{"format":"CSV","count":3,"include_notes":false}})
        );
        assert!(check_answer(&pending, "field:count", Some("3.5")).is_err());
        assert!(check_answer(&pending, "field:count", Some("100")).is_err());
        assert!(check_answer(&pending, "field:format", Some("XML")).is_err());
        assert!(check_answer(&pending, "field:count", None).is_err());
        Ok(())
    }

    #[test]
    fn declining_does_not_require_answering_fields() -> Result<()> {
        let mut pending = form(
            json!({"type":"object","properties":{"reason":{"type":"string"}},"required":["reason"]}),
        );
        pending
            .params
            .as_object_mut()
            .context("params")?
            .insert("difuAnswers".into(), json!({DECISION:{"answers":["Deny"]}}));
        finish_decline(&mut pending, DECISION, Some("Deny"));
        assert!(pending.unanswered_questions().is_empty());
        assert_eq!(
            response(
                &pending,
                json!({"answers":pending.params.get("difuAnswers")})
            )?,
            json!({"action":"decline"})
        );
        Ok(())
    }

    #[test]
    fn unsupported_forms_cannot_be_accepted_and_native_questions_are_unchanged() -> Result<()> {
        let pending = form(json!({"type":"object","properties":{"nested":{"type":"object"}}}));
        assert_eq!(pending.unanswered_questions().len(), 1);
        assert!(
            response(
                &pending,
                json!({"answers":{DECISION:{"answers":["Allow once"]}}})
            )
            .is_err()
        );
        assert_eq!(
            response(
                &pending,
                json!({"answers":{DECISION:{"answers":["Cancel"]}}})
            )?,
            json!({"action":"cancel"})
        );
        let mut native = Pending {
            id: json!(1),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions":[]}),
            responded: false,
        };
        normalize(&mut native);
        let answer = json!({"answers":{"a":{"answers":["yes"]}}});
        assert_eq!(response(&native, answer.clone())?, answer);
        Ok(())
    }
}
