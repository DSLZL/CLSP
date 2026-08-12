use super::*;

use crate::test_support as support;

#[test]
fn mcp_input_rejects_zero_based_positions() {
    let directory = support::tempdir().unwrap();
    support::create_dir(directory.path().join("src")).unwrap();
    support::write(directory.path().join("src/lib.rs"), "").unwrap();
    let workspace = Workspace::open(directory.path()).unwrap();
    let input: QueryInput = serde_json::from_value(serde_json::json!({
        "operation": "definition",
        "file": "src/lib.rs",
        "line": 0,
        "character": 1
    }))
    .unwrap();
    assert_eq!(
        query_request(input, &workspace, 1024).unwrap_err().code,
        crate::protocol::ErrorCode::InvalidRequest
    );
}
