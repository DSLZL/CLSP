use super::*;

#[tokio::test]
async fn bounded_line_preserves_embedded_json_escapes() {
    let input = b"{\"type\":\"shutdown\",\"value\":\"a\\\\nb\"}\n";
    let mut reader = &input[..];
    let line = read_bounded_line(&mut reader).await.unwrap().unwrap();
    assert_eq!(line, &input[..input.len() - 1]);
}

#[tokio::test]
async fn bounded_line_rejects_oversize_input() {
    let bytes = vec![b'a'; IDE_STDIO_MAX_BYTES + 1];
    let mut reader = &bytes[..];
    assert!(read_bounded_line(&mut reader).await.is_err());
}
