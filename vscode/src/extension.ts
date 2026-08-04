import { Buffer } from "node:buffer";
import { ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";

import * as vscode from "vscode";

import { encodeMessage, HostAction, HostOutput, MAX_MESSAGE_BYTES, Severity, parseHostOutputLine, stripWindowsVerbatimPrefix, truncateUtf8 } from "./protocol";

const MARKER = "# clsp-ide-bridge: managed-v1";
const SESSION_ENV = "CLSP_IDE_SESSION_ID";
const SELECTION_STATE = "clsp.selectionSharing";
const MAX_SELECTION_BYTES = 8 * 1024;
const MAX_DIAGNOSTICS_BYTES = 128 * 1024;

type ActionResult = Record<string, unknown> & { type: string };

export function activate(context: vscode.ExtensionContext): void {
  context.environmentVariableCollection.persistent = false;
  const toggle = vscode.commands.registerCommand("clsp.toggleSelectionSharing", async () => {
    const enabled = context.workspaceState.get<boolean>(SELECTION_STATE, true);
    await context.workspaceState.update(SELECTION_STATE, !enabled);
    void vscode.window.showInformationMessage(`CLSP selection sharing ${enabled ? "disabled" : "enabled"}.`);
  });
  context.subscriptions.push(toggle);

  if (!vscode.workspace.isTrusted || vscode.env.remoteName || vscode.env.uiKind !== vscode.UIKind.Desktop) {
    context.environmentVariableCollection.delete(SESSION_ENV);
    return;
  }

  const bridge = new Bridge(context);
  context.subscriptions.push(bridge);
  bridge.scheduleRefresh();
}

class Bridge implements vscode.Disposable {
  private readonly hosts = new Map<string, Host>();
  private readonly sessionId = randomBytes(32).toString("hex");
  private readonly output = vscode.window.createOutputChannel("CLSP IDE Bridge");
  private refreshChain = Promise.resolve();
  private disposed = false;

  constructor(private readonly context: vscode.ExtensionContext) {
    const watcher = vscode.workspace.createFileSystemWatcher("**/.codex/config.toml");
    watcher.onDidCreate(() => this.scheduleRefresh());
    watcher.onDidChange(() => this.scheduleRefresh());
    watcher.onDidDelete(() => this.scheduleRefresh());
    context.subscriptions.push(watcher, vscode.workspace.onDidChangeWorkspaceFolders(() => this.scheduleRefresh()), this.output);
  }

  scheduleRefresh(): void {
    this.refreshChain = this.refreshChain.then(() => this.refresh()).catch(() => {
      this.output.appendLine("CLSP workspace reconciliation failed.");
    });
  }

  dispose(): void {
    this.disposed = true;
    for (const host of this.hosts.values()) {
      host.dispose();
    }
    this.hosts.clear();
    this.updateEnvironment();
  }

  private async refresh(): Promise<void> {
    if (this.disposed) {
      return;
    }
    const marked = new Map<string, string>();
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      if (folder.uri.scheme !== "file" || !(await hasMarker(folder.uri))) {
        continue;
      }
      const root = canonicalExisting(folder.uri.fsPath);
      marked.set(normalizePath(root), root);
    }

    for (const [key, host] of this.hosts) {
      if (!marked.has(key)) {
        host.dispose();
        this.hosts.delete(key);
      }
    }

    const executable = resolveExecutable(this.context.extension.packageJSON.version as string);
    for (const [key, root] of marked) {
      if (this.hosts.has(key)) {
        continue;
      }
      if (!executable) {
        this.output.appendLine("CLSP executable locator is invalid.");
        break;
      }
      const host = new Host(
        root,
        executable,
        this.sessionId,
        () => this.context.workspaceState.get<boolean>(SELECTION_STATE, true),
        () => this.updateEnvironment(),
        this.output,
      );
      this.hosts.set(key, host);
      host.start();
    }
    this.updateEnvironment();
  }

  private updateEnvironment(): void {
    const allConnected = this.hosts.size > 0 && [...this.hosts.values()].every((host) => host.connected);
    if (allConnected) {
      this.context.environmentVariableCollection.replace(SESSION_ENV, this.sessionId);
    } else {
      this.context.environmentVariableCollection.delete(SESSION_ENV);
    }
  }
}

class Host implements vscode.Disposable {
  connected = false;
  private child: ChildProcessWithoutNullStreams | undefined;
  private stdoutBuffer = Buffer.alloc(0);
  private stopped = false;
  private stderrBytes = 0;

  constructor(
    private readonly root: string,
    private readonly executable: string,
    private readonly sessionId: string,
    private readonly selectionSharing: () => boolean,
    private readonly stateChanged: () => void,
    private readonly output: vscode.OutputChannel,
  ) {}

  start(): void {
    if (this.stopped || this.child) {
      return;
    }
    try {
      this.child = spawn(this.executable, ["ide-host", "--workspace", this.root, "--session-id", this.sessionId], {
        cwd: this.root,
        shell: false,
        windowsHide: true,
        stdio: "pipe",
      });
    } catch {
      this.stop("CLSP IDE host could not be started.");
      return;
    }
    this.child.stdout.on("data", (chunk: Buffer) => this.acceptStdout(chunk));
    this.child.stderr.on("data", (chunk: Buffer) => this.acceptStderr(chunk));
    this.child.on("error", () => this.stop("CLSP IDE host failed to start."));
    this.child.on("exit", () => this.stop("CLSP IDE host exited."));
  }

  dispose(): void {
    if (this.stopped) {
      return;
    }
    this.stopped = true;
    try {
      this.child?.stdin.write(encodeMessage({ type: "shutdown" }));
      this.child?.stdin.end();
    } catch {
      // Best effort; Broker TTL is authoritative.
    }
    this.child?.kill();
    this.connected = false;
    this.stateChanged();
  }

  private acceptStdout(chunk: Buffer): void {
    this.stdoutBuffer = Buffer.concat([this.stdoutBuffer, chunk]);
    for (;;) {
      const newline = this.stdoutBuffer.indexOf(0x0a);
      if (newline < 0) {
        if (this.stdoutBuffer.length > MAX_MESSAGE_BYTES) {
          this.stop("CLSP IDE host sent an oversized message.");
        }
        return;
      }
      const line = this.stdoutBuffer.subarray(0, newline);
      this.stdoutBuffer = this.stdoutBuffer.subarray(newline + 1);
      if (line.length > MAX_MESSAGE_BYTES) {
        this.stop("CLSP IDE host sent an oversized message.");
        return;
      }
      try {
        const text = line.toString("utf8");
        if (!Buffer.from(text, "utf8").equals(line)) {
          throw new Error("invalid UTF-8");
        }
        this.handleMessage(parseHostOutputLine(text));
      } catch {
        this.stop("CLSP IDE host sent an invalid protocol message.");
        return;
      }
    }
  }

  private acceptStderr(chunk: Buffer): void {
    const remaining = 64 * 1024 - this.stderrBytes;
    if (remaining <= 0) {
      return;
    }
    const text = truncateUtf8(chunk.toString("utf8"), remaining);
    this.stderrBytes += Buffer.byteLength(text, "utf8");
    this.output.append(text);
  }

  private handleMessage(message: HostOutput): void {
    if (message.type === "status") {
      this.connected = message.state === "connected";
      this.stateChanged();
      return;
    }
    void this.dispatch(message.action)
      .then((result) => this.send({ type: "action_result", action_id: message.action_id, result }))
      .catch(() => this.send({ type: "action_result", action_id: message.action_id, result: { type: "error", message: "IDE action failed" } }));
  }

  private async dispatch(action: HostAction): Promise<ActionResult> {
    switch (action.type) {
      case "get_editor_context":
        return { type: "editor_context", context: editorContext(this.root, this.selectionSharing()) };
      case "get_diagnostics":
        return diagnostics(this.root, action.file, action.minimum_severity);
      case "prepare_edit":
        return prepareEdit(this.root, action.targets);
      case "open_diff":
        return openDiff(action.pairs);
    }
  }

  private send(message: unknown): void {
    if (this.stopped || !this.child || this.child.stdin.destroyed) {
      return;
    }
    try {
      this.child.stdin.write(encodeMessage(message), (error) => {
        if (error) {
          this.stop("CLSP IDE host input closed.");
        }
      });
    } catch {
      this.stop("CLSP IDE response exceeded its limit.");
    }
  }

  private stop(message: string): void {
    if (this.stopped) {
      return;
    }
    this.stopped = true;
    this.connected = false;
    this.output.appendLine(message);
    this.child?.kill();
    this.stateChanged();
  }
}

function editorContext(root: string, shareSelection: boolean): Record<string, unknown> | null {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.uri.scheme !== "file" || !isInside(root, editor.document.uri.fsPath)) {
    return null;
  }
  const selection = editor.selection;
  let selected: Record<string, unknown> | undefined;
  if (shareSelection && !selection.isEmpty) {
    const text = editor.document.getText(selection);
    const range = {
      start: { line: selection.start.line, character: selection.start.character },
      end: { line: selection.end.line, character: selection.end.character },
    };
    selected = Buffer.byteLength(text, "utf8") <= MAX_SELECTION_BYTES
      ? { ...range, text }
      : { ...range, selection_omitted: "too_large" };
  }
  return {
    active_file: editor.document.uri.fsPath,
    document_version: editor.document.version,
    dirty: editor.document.isDirty,
    ...(selected ? { selection: selected } : {}),
  };
}

async function diagnostics(root: string, file: string | null, minimum: Severity): Promise<ActionResult> {
  if (file && (!path.isAbsolute(file) || !isInside(root, file))) {
    return { type: "error", message: "Diagnostic path is outside the workspace" };
  }
  const uri = file ? vscode.Uri.file(stripWindowsVerbatimPrefix(file)) : null;
  const groups: [vscode.Uri, readonly vscode.Diagnostic[]][] = uri
    ? [[uri, vscode.languages.getDiagnostics(uri)]]
    : vscode.languages.getDiagnostics();
  const threshold = severityRank(minimum);
  const eligible = groups
    .filter(([uri]) => uri.scheme === "file" && isInside(root, uri.fsPath))
    .map(([uri, items]) => [uri, items.filter((item) => item.severity <= threshold)] as const)
    .filter(([, items]) => items.length > 0)
    .sort(([left], [right]) => normalizePath(left.fsPath).localeCompare(normalizePath(right.fsPath)));
  let truncated = !file && eligible.length > 5;
  const result: Record<string, unknown>[] = [];
  for (const [uri, rawItems] of eligible.slice(0, file ? 1 : 5)) {
    const sorted = [...rawItems].sort(compareDiagnostics);
    if (sorted.length > 20) {
      truncated = true;
    }
    for (const item of sorted.slice(0, 20)) {
      const mapped: Record<string, unknown> = {
        path: uri.fsPath,
        range: {
          start: { line: item.range.start.line, character: item.range.start.character },
          end: { line: item.range.end.line, character: item.range.end.character },
        },
        severity: severityName(item.severity),
        message: truncateUtf8(item.message, 4 * 1024),
      };
      if (item.source) {
        mapped.source = truncateUtf8(item.source, 256);
      }
      const code = diagnosticCode(item.code);
      if (code) {
        mapped.code = truncateUtf8(code, 256);
      }
      const candidate = [...result, mapped];
      if (Buffer.byteLength(JSON.stringify(candidate), "utf8") > MAX_DIAGNOSTICS_BYTES) {
        truncated = true;
        return { type: "diagnostics", items: result, truncated };
      }
      result.push(mapped);
    }
  }
  return { type: "diagnostics", items: result, truncated };
}

async function prepareEdit(root: string, targets: string[]): Promise<ActionResult> {
  if (targets.some((target) => !path.isAbsolute(target) || !isInside(root, target))) {
    return { type: "error", message: "Edit target is outside the workspace" };
  }
  const targetSet = new Set(targets.map((target) => normalizePath(canonicalExisting(target))));
  const dirty = vscode.workspace.textDocuments.filter(
    (document) =>
      document.uri.scheme === "file"
      && document.isDirty
      && targetSet.has(normalizePath(canonicalExisting(document.uri.fsPath))),
  );
  if (dirty.length === 0) {
    return { type: "prepared", outcome: "ready", message: null };
  }
  const listed = dirty.slice(0, 5).map((document) => path.relative(root, document.uri.fsPath));
  const remainder = dirty.length > 5 ? `\n...and ${dirty.length - 5} more.` : "";
  const prompt = truncateUtf8(`Save ${dirty.length} dirty edit target(s) before continuing?\n${listed.join("\n")}${remainder}`, 1024);
  const choice = await vscode.window.showWarningMessage(prompt, { modal: true }, "Save and continue");
  if (choice !== "Save and continue") {
    return { type: "prepared", outcome: "cancelled", message: "Edit cancelled because dirty files were not saved" };
  }
  for (const document of dirty) {
    if (!(await document.save())) {
      return { type: "prepared", outcome: "save_failed", message: "A dirty edit target could not be saved" };
    }
  }
  return { type: "prepared", outcome: "ready", message: null };
}

async function openDiff(pairs: { left: string; right: string; title: string }[]): Promise<ActionResult> {
  let opened = 0;
  let failed = 0;
  for (const pair of pairs) {
    if (!path.isAbsolute(pair.left) || !path.isAbsolute(pair.right)) {
      failed += 1;
      continue;
    }
    try {
      await vscode.commands.executeCommand("vscode.diff", vscode.Uri.file(stripWindowsVerbatimPrefix(pair.left)), vscode.Uri.file(stripWindowsVerbatimPrefix(pair.right)), pair.title, {
        preview: false,
        preserveFocus: true,
      });
      opened += 1;
    } catch {
      failed += 1;
    }
  }
  return { type: "diff_opened", opened, failed };
}

async function hasMarker(root: vscode.Uri): Promise<boolean> {
  try {
    const bytes = await vscode.workspace.fs.readFile(vscode.Uri.joinPath(root, ".codex", "config.toml"));
    if (bytes.byteLength > 1024 * 1024) {
      return false;
    }
    return Buffer.from(bytes).toString("utf8").split(/\r?\n/u).includes(MARKER);
  } catch {
    return false;
  }
}

function resolveExecutable(extensionVersion: string): string | undefined {
  const localAppData = process.env.LOCALAPPDATA;
  if (!localAppData) {
    return "clsp";
  }
  const locator = path.join(localAppData, "clsp", "install.json");
  let bytes: Buffer;
  try {
    bytes = fs.readFileSync(locator);
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ENOENT" ? "clsp" : undefined;
  }
  if (bytes.length > 64 * 1024) {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(bytes.toString("utf8"));
    if (!isPlainObject(value) || Object.keys(value).some((key) => key !== "executable" && key !== "version")) {
      return undefined;
    }
    if (value.version !== extensionVersion || typeof value.executable !== "string" || !path.isAbsolute(value.executable)) {
      return undefined;
    }
    const stat = fs.statSync(value.executable);
    return stat.isFile() ? value.executable : undefined;
  } catch {
    return undefined;
  }
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function canonicalExisting(value: string): string {
  try {
    return fs.realpathSync.native(value);
  } catch {
    return path.resolve(value);
  }
}

function isInside(root: string, candidate: string): boolean {
  const relative = path.relative(root, canonicalExisting(candidate));
  return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

function normalizePath(value: string): string {
  const normalized = path.normalize(value);
  return process.platform === "win32" ? normalized.toLocaleLowerCase("en-US") : normalized;
}

function severityRank(value: Severity): vscode.DiagnosticSeverity {
  return {
    error: vscode.DiagnosticSeverity.Error,
    warning: vscode.DiagnosticSeverity.Warning,
    information: vscode.DiagnosticSeverity.Information,
    hint: vscode.DiagnosticSeverity.Hint,
  }[value];
}

function severityName(value: vscode.DiagnosticSeverity): Severity {
  switch (value) {
    case vscode.DiagnosticSeverity.Error:
      return "error";
    case vscode.DiagnosticSeverity.Warning:
      return "warning";
    case vscode.DiagnosticSeverity.Information:
      return "information";
    default:
      return "hint";
  }
}

function diagnosticCode(code: vscode.Diagnostic["code"]): string | undefined {
  if (code === undefined) {
    return undefined;
  }
  if (typeof code === "object") {
    return String(code.value);
  }
  return String(code);
}

function compareDiagnostics(left: vscode.Diagnostic, right: vscode.Diagnostic): number {
  return left.range.start.line - right.range.start.line
    || left.range.start.character - right.range.start.character
    || left.range.end.line - right.range.end.line
    || left.range.end.character - right.range.end.character
    || left.severity - right.severity
    || left.message.localeCompare(right.message);
}
