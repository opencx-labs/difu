use super::*;
use crate::agents::{Job, Launch, Pending};
use serde_json::json;
fn session() -> Session {
    Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: "/tmp".into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "private task".into(),
            model: None,
            effort: None,
        }),
    )
}
#[test]
fn reducer_success_notifies_but_failure_and_interruption_are_not_completion() {
    let mut session = session();
    let mut actual = Vec::new();
    for (turn, status) in [
        ("one", "completed"),
        ("two", "failed"),
        ("three", "interrupted"),
        ("four", "completed"),
    ] {
        crate::agents::engine::apply_event(
            &mut session,
            &json!({"method":"turn/started","params":{"turn":{"id":turn}}}),
        );
        let before = Transition::capture(&session);
        crate::agents::engine::apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"id":turn,"status":status}}}),
        );
        actual.extend(before.events(&mut session).into_iter().map(|e| e.kind));
    }
    assert_eq!(
        actual,
        vec![Kind::Completion, Kind::Error, Kind::Completion]
    );
}
#[test]
fn a_new_pending_identity_not_a_waiting_edge_triggers_question() {
    let mut session = session();
    session.status = Status::Waiting;
    session.pending.push(Pending {
        id: json!("old"),
        method: "item/tool/requestUserInput".into(),
        params: json!({"private":"do not expose"}),
        responded: false,
    });
    let before = Transition::capture(&session);
    session.pending.push(Pending {
        id: json!("new"),
        method: "item/tool/requestUserInput".into(),
        params: json!({"questions":[]}),
        responded: false,
    });
    let events = before.events(&mut session);
    assert_eq!(events.len(), 1);
    assert!(events.iter().all(|e| e.kind == Kind::Question));
    assert!(
        Transition::capture(&session)
            .events(&mut session)
            .is_empty()
    );
}
#[test]
fn missing_or_unknown_provider_outcome_and_old_json_are_not_success() -> anyhow::Result<()> {
    let mut session = session();
    for status in [serde_json::Value::Null, json!("unknown")] {
        session.completed_turn = None;
        let before = Transition::capture(&session);
        crate::agents::engine::apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"id":"turn","status":status}}}),
        );
        assert!(before.events(&mut session).is_empty());
        assert!(!session.completed_turn_successful);
    }
    let mut legacy = serde_json::to_value(&session)?;
    if let Some(object) = legacy.as_object_mut() {
        object.remove("completed_turn_successful");
    }
    assert!(!serde_json::from_value::<Session>(legacy)?.completed_turn_successful);
    Ok(())
}
