import { Buffer } from "node:buffer";

export const MAX_MESSAGE_BYTES = 256 * 1024;

export type Severity = "error" | "warning" | "information" | "hint";

export type HostAction =
  | { type: "get_editor_context" }
  | { type: "get_diagnostics"; file: string | null; minimum_severity: Severity }
  | { type: "prepare_edit"; targets: string[] }
  | { type: "open_diff"; pairs: { left: string; right: string; title: string }[] };

export type HostOutput =
  | { type: "status"; state: "connected" | "disconnected" }
  | { type: "action"; action_id: number; action: HostAction };

export function parseHostOutputLine(line: string): HostOutput {
  if (Buffer.byteLength(line, "utf8") > MAX_MESSAGE_BYTES) {
    throw new Error("host message exceeds limit");
  }
  const value: unknown = JSON.parse(line);
  const message = object(value, "host message");
  const type = stringField(message, "type", 64);
  if (type === "status") {
    exactKeys(message, ["type", "state"]);
    if (message.state !== "connected" && message.state !== "disconnected") {
      throw new Error("invalid host state");
    }
    return { type, state: message.state };
  }
  if (type !== "action") {
    throw new Error("unknown host message type");
  }
  exactKeys(message, ["type", "action_id", "action"]);
  const actionId = positiveInteger(message.action_id, "action_id");
  return { type, action_id: actionId, action: parseAction(message.action) };
}

export function encodeMessage(message: unknown): string {
  const encoded = `${JSON.stringify(message)}\n`;
  if (Buffer.byteLength(encoded, "utf8") > MAX_MESSAGE_BYTES) {
    throw new Error("adapter message exceeds limit");
  }
  return encoded;
}

export function truncateUtf8(value: string, maxBytes: number): string {
  const bytes = Buffer.from(value, "utf8");
  if (bytes.length <= maxBytes) {
    return value;
  }
  return bytes.subarray(0, maxBytes).toString("utf8").replace(/\uFFFD$/, "");
}

export function stripWindowsVerbatimPrefix(value: string): string {
  if (value.startsWith("\\\\?\\UNC\\")) {
    return `\\\\${value.slice(8)}`;
  }
  return value.startsWith("\\\\?\\") ? value.slice(4) : value;
}

function parseAction(value: unknown): HostAction {
  const action = object(value, "action");
  const type = stringField(action, "type", 64);
  switch (type) {
    case "get_editor_context":
      exactKeys(action, ["type"]);
      return { type };
    case "get_diagnostics": {
      exactKeys(action, ["type", "file", "minimum_severity"]);
      const file = action.file === null ? null : pathField(action.file, "file");
      const severity = action.minimum_severity;
      if (!isSeverity(severity)) {
        throw new Error("invalid minimum_severity");
      }
      return { type, file, minimum_severity: severity };
    }
    case "prepare_edit": {
      exactKeys(action, ["type", "targets"]);
      if (!Array.isArray(action.targets) || action.targets.length > 64) {
        throw new Error("invalid edit targets");
      }
      return { type, targets: action.targets.map((item) => pathField(item, "target")) };
    }
    case "open_diff": {
      exactKeys(action, ["type", "pairs"]);
      if (!Array.isArray(action.pairs) || action.pairs.length > 5) {
        throw new Error("invalid diff pairs");
      }
      const pairs = action.pairs.map((item) => {
        const pair = object(item, "diff pair");
        exactKeys(pair, ["left", "right", "title"]);
        return {
          left: pathField(pair.left, "left"),
          right: pathField(pair.right, "right"),
          title: valueString(pair.title, "title", 256),
        };
      });
      return { type, pairs };
    }
    default:
      throw new Error("unknown IDE action");
  }
}

function object(value: unknown, name: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const allowedSet = new Set(allowed);
  if (Object.keys(value).some((key) => !allowedSet.has(key)) || allowed.some((key) => !(key in value))) {
    throw new Error("message fields do not match protocol");
  }
}

function stringField(value: Record<string, unknown>, key: string, maxBytes: number): string {
  return valueString(value[key], key, maxBytes);
}

function valueString(value: unknown, name: string, maxBytes: number): string {
  if (typeof value !== "string" || value.length === 0 || Buffer.byteLength(value, "utf8") > maxBytes || value.includes("\0")) {
    throw new Error(`invalid ${name}`);
  }
  return value;
}

function pathField(value: unknown, name: string): string {
  return valueString(value, name, 32 * 1024);
}

function positiveInteger(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) {
    throw new Error(`invalid ${name}`);
  }
  return value;
}

function isSeverity(value: unknown): value is Severity {
  return value === "error" || value === "warning" || value === "information" || value === "hint";
}
