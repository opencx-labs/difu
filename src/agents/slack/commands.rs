//! Explicit commands never fall through to the model on malformed/stale input.
use super::super::{Control, Pending};
use anyhow::{Result, ensure};
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Message,
    Answer { reference: String, text: String },
    Decision { reference: String, approve: bool },
    Help,
}
pub(super) fn parse(text: &str) -> Result<Command> {
    let mut words = text.trim().splitn(2, char::is_whitespace);
    let verb = words.next().unwrap_or_default();
    let tail = words.next().unwrap_or_default().trim();
    match verb {
        "approve" | "deny" => {
            let (reference, extra) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
            ensure!(!reference.is_empty(), "Use: {verb} <request-id>");
            ensure!(
                extra.trim().is_empty(),
                "Use: {verb} <request-id>. Requested information is collected through questions."
            );
            Ok(Command::Decision {
                reference: reference.into(),
                approve: verb == "approve",
            })
        }
        "answer" => {
            let (reference, text) = tail
                .split_once(char::is_whitespace)
                .ok_or_else(|| anyhow::anyhow!("Use: answer <question-id> <answer>"))?;
            ensure!(
                !reference.is_empty() && !text.trim().is_empty(),
                "An explicit answer is required"
            );
            Ok(Command::Answer {
                reference: reference.into(),
                text: text.trim().into(),
            })
        }
        "help" if tail.is_empty() => Ok(Command::Help),
        _ => Ok(Command::Message),
    }
}

pub(super) fn decision(pending: &Pending, approve: bool) -> Result<Control> {
    ensure!(!pending.responded, "This request was already answered");
    let response = match pending.method.as_str() {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"decision":if approve {"accept"} else {"decline"}})
        }
        "item/permissions/requestApproval" => {
            json!({"permissions":if approve {pending.params.get("permissions").cloned().unwrap_or_else(|| json!({}))} else {json!({})},"scope":"turn"})
        }
        _ => anyhow::bail!("This request is not a supported permission; answer it in difu"),
    };
    Ok(Control::Respond {
        request: pending.id.clone(),
        response,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn casual_messages_cannot_approve_and_malformed_commands_do_not_become_prompts() -> Result<()> {
        for text in [
            "yes",
            "sure, approve it",
            "please continue",
            "Approved",
            "approve-looking prose",
        ] {
            assert_eq!(parse(text)?, Command::Message);
        }
        for text in ["approve", "approve a b", "deny", "answer a"] {
            assert!(parse(text).is_err());
        }
        assert_eq!(
            parse("approve abc")?,
            Command::Decision {
                reference: "abc".into(),
                approve: true
            }
        );
        assert_eq!(
            parse("answer abc yes please")?,
            Command::Answer {
                reference: "abc".into(),
                text: "yes please".into()
            }
        );
        Ok(())
    }
    #[test]
    fn slack_never_grants_session_wide_permissions() -> Result<()> {
        let pending = Pending {
            id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({}),
            responded: false,
        };
        let Control::Respond { response, .. } = decision(&pending, true)? else {
            anyhow::bail!("response");
        };
        assert_eq!(response, json!({"decision":"accept"}));
        assert!(
            decision(
                &Pending {
                    method: "item/tool/requestUserInput".into(),
                    ..pending
                },
                true
            )
            .is_err()
        );
        Ok(())
    }
}
