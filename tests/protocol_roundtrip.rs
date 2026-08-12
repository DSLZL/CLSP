use std::path::PathBuf;

use clsp::protocol::{
    ClspError, ErrorCode, IDE_SESSION_ID_HEX_LEN, IdeAction, IdeHostInput, RequestEnvelope,
    RpcRequest, WireMessage,
};

#[test]
fn protocol_round_trip_preserves_tagged_request() {
    let message = WireMessage::Request(RequestEnvelope {
        id: 7,
        body: RpcRequest::AcquireLease {
            session_id: "session-1".into(),
        },
    });
    let encoded = serde_json::to_vec(&message).unwrap();
    let decoded: WireMessage = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, message);
}

#[test]
fn protocol_round_trip_preserves_ide_diagnostic_sync() {
    let request = RpcRequest::SyncIdeDiagnostics {
        session_id: "a".repeat(IDE_SESSION_ID_HEX_LEN),
        codex_session_id: "codex-session".into(),
        tool_use_id: "tool-call".into(),
        paths: vec![PathBuf::from("C:/workspace/src/lib.rs")],
    };
    let encoded = serde_json::to_vec(&request).unwrap();
    assert_eq!(
        serde_json::from_slice::<RpcRequest>(&encoded).unwrap(),
        request
    );
}

#[test]
fn ide_stdio_rejects_unknown_fields() {
    let input = br#"{"type":"shutdown","extra":true}"#;
    assert!(serde_json::from_slice::<IdeHostInput>(input).is_err());
}

#[test]
fn ide_action_rejects_unknown_fields() {
    let input = br#"{"type":"get_editor_context","extra":true}"#;
    assert!(serde_json::from_slice::<IdeAction>(input).is_err());
}

#[test]
fn expected_error_has_a_stable_code() {
    let error = ClspError::new(ErrorCode::BrokerUnavailable, "not running").retryable();
    let value = serde_json::to_value(error).unwrap();
    assert_eq!(value["code"], "broker_unavailable");
    assert_eq!(value["retryable"], true);
}
