//! Slack event filtering is independent of credentials, networking, and agent execution.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) struct Scope<'a> {
    pub team: &'a str,
    pub user: &'a str,
    pub channel: &'a str,
    pub bot: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Mention {
    pub event_id: String,
    pub channel: String,
    pub thread: String,
    pub text: String,
}

/// Accept direct messages from the configured user, or explicit mentions in the
/// configured channel. Edits, bots, and other users/workspaces cannot dispatch.
pub(super) fn mention(payload: &Value, scope: &Scope<'_>) -> Option<Mention> {
    if payload.get("type")?.as_str()? != "event_callback"
        || payload.get("team_id")?.as_str()? != scope.team
    {
        return None;
    }
    let event = payload.get("event")?;
    if event.get("user")?.as_str()? != scope.user
        || event.get("bot_id").is_some()
        || event.get("subtype").is_some()
    {
        return None;
    }
    let channel = event.get("channel")?.as_str()?;
    let direct = event.get("type")?.as_str()? == "message"
        && event.get("channel_type").and_then(Value::as_str) == Some("im")
        && super::valid_id(channel, &['D']);
    let channel_mention = event.get("type")?.as_str()? == "app_mention"
        && !scope.channel.is_empty()
        && channel == scope.channel;
    if !direct && !channel_mention {
        return None;
    }
    let text = event.get("text")?.as_str()?;
    let tag = format!("<@{}>", scope.bot);
    if !direct && !text.contains(&tag) {
        return None;
    }
    let timestamp = event.get("ts")?.as_str()?;
    let thread = match event.get("thread_ts") {
        Some(value) => value.as_str()?,
        None => timestamp,
    };
    let event_id = payload.get("event_id")?.as_str()?;
    if event_id.is_empty() || timestamp.is_empty() || thread.is_empty() {
        return None;
    }
    Some(Mention {
        event_id: event_id.into(),
        channel: channel.into(),
        thread: thread.into(),
        text: text
            .replacen(&tag, "", 1)
            .trim()
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use serde_json::json;

    fn scope() -> Scope<'static> {
        Scope {
            team: "TTEAM",
            user: "UALI",
            channel: "CPRIVATE",
            bot: "UDIFU",
        }
    }

    fn payload() -> Value {
        json!({
            "type": "event_callback", "team_id": "TTEAM", "event_id": "Ev1",
            "event": {"type": "app_mention", "user": "UALI", "channel": "CPRIVATE",
                "text": "<@UDIFU> explain a &lt; b &amp;&amp; c &gt; d", "ts": "100.000001"}
        })
    }

    #[test]
    fn mentions_keep_the_root_thread_and_decode_slack_escaping() -> Result<()> {
        let mut event = payload();
        let parsed = mention(&event, &scope()).context("mention")?;
        assert_eq!(parsed.thread, "100.000001");
        assert_eq!(parsed.text, "explain a < b && c > d");
        event
            .get_mut("event")
            .and_then(Value::as_object_mut)
            .context("event")?
            .insert("thread_ts".into(), json!("99.000001"));
        assert_eq!(
            mention(&event, &scope()).context("thread mention")?.thread,
            "99.000001"
        );
        Ok(())
    }

    #[test]
    fn direct_messages_start_threads_and_followups_keep_them_without_mentions() -> Result<()> {
        let mut event = json!({"type":"event_callback","team_id":"TTEAM","event_id":"Ev2",
            "event":{"type":"message","channel_type":"im","user":"UALI","channel":"DDIRECT",
                "text":"list my sessions","ts":"101.000001"}});
        let mut scope = scope();
        scope.channel = "";
        let parsed = mention(&event, &scope).context("direct message")?;
        assert_eq!(parsed.channel, "DDIRECT");
        assert_eq!(parsed.thread, "101.000001");
        assert_eq!(parsed.text, "list my sessions");
        event
            .get_mut("event")
            .and_then(Value::as_object_mut)
            .context("event")?
            .insert("thread_ts".into(), json!("100.000001"));
        assert_eq!(
            mention(&event, &scope).context("followup")?.thread,
            "100.000001"
        );
        for (path, value) in [
            ("/event/user", json!("UOTHER")),
            ("/team_id", json!("TOTHER")),
            ("/event/channel_type", json!("mpim")),
            ("/event/channel", json!("CPUBLIC")),
            ("/event/thread_ts", json!("")),
        ] {
            let mut invalid = event.clone();
            *invalid.pointer_mut(path).context("field")? = value;
            assert!(mention(&invalid, &scope).is_none(), "{path}");
        }
        for field in ["bot_id", "subtype"] {
            let mut invalid = event.clone();
            invalid
                .get_mut("event")
                .and_then(Value::as_object_mut)
                .context("event")?
                .insert(field.into(), json!("ignored"));
            assert!(mention(&invalid, &scope).is_none());
        }
        Ok(())
    }

    #[test]
    fn other_users_channels_workspaces_bots_and_edits_cannot_dispatch() -> Result<()> {
        for (path, value) in [
            ("/team_id", json!("TOTHER")),
            ("/type", json!("url_verification")),
            ("/event/type", json!("message")),
            ("/event/user", json!("UOTHER")),
            ("/event/channel", json!("CPUBLIC")),
            ("/event/text", json!("@difu plain text is not a mention")),
            ("/event/text", json!("<@UDIFUOTHER> wrong identity")),
            ("/event/ts", Value::Null),
            ("/event_id", json!("")),
        ] {
            let mut event = payload();
            *event.pointer_mut(path).context("fixture field")? = value;
            assert!(mention(&event, &scope()).is_none(), "{path}");
        }
        for field in ["subtype", "bot_id"] {
            let mut event = payload();
            event
                .get_mut("event")
                .and_then(Value::as_object_mut)
                .context("event")?
                .insert(field.into(), json!("ignored"));
            assert!(mention(&event, &scope()).is_none(), "{field}");
        }
        Ok(())
    }
}
