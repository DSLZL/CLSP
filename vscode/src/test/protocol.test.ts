import assert from "node:assert/strict";
import test from "node:test";

import { encodeMessage, MAX_MESSAGE_BYTES, parseHostOutputLine, stripWindowsVerbatimPrefix, truncateUtf8 } from "../protocol";

test("protocol preserves embedded newlines and validates the closed action set", () => {
  const encoded = encodeMessage({ type: "action_result", action_id: 7, result: { message: "a\nb" } });
  assert.equal(encoded.split("\n").length, 2);
  assert.deepEqual(
    parseHostOutputLine('{"type":"action","action_id":7,"action":{"type":"get_editor_context"}}'),
    { type: "action", action_id: 7, action: { type: "get_editor_context" } },
  );
  assert.deepEqual(parseHostOutputLine('{"type":"status","state":"disconnected"}'), {
    type: "status",
    state: "disconnected",
  });
  assert.throws(() =>
    parseHostOutputLine('{"type":"action","action_id":7,"extra":true,"action":{"type":"get_editor_context"}}'),
  );
  assert.throws(() =>
    parseHostOutputLine('{"type":"action","action_id":7,"action":{"type":"get_editor_context","extra":true}}'),
  );
});

test("protocol rejects oversize lines and truncates UTF-8 on a character boundary", () => {
  assert.throws(() => parseHostOutputLine(`{"type":"status","state":"${"a".repeat(MAX_MESSAGE_BYTES)}"}`));
  const value = truncateUtf8("ab\u{1F642}cd", 5);
  assert.equal(value, "ab");
  assert.ok(Buffer.byteLength(value, "utf8") <= 5);
});

test("protocol converts Windows verbatim paths before passing them to VS Code", () => {
  assert.equal(stripWindowsVerbatimPrefix(String.raw`\\?\C:\workspace\main.rs`), String.raw`C:\workspace\main.rs`);
  assert.equal(stripWindowsVerbatimPrefix(String.raw`\\?\UNC\server\share\main.rs`), String.raw`\\server\share\main.rs`);
  assert.equal(stripWindowsVerbatimPrefix("/workspace/main.rs"), "/workspace/main.rs");
});
