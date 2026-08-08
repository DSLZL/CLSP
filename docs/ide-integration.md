# IDE Integration

CLSP's VS Code component is a thin local bridge. Its job is to expose a small amount of live editor state to the CLSP Broker without reimplementing VS Code's language features.

The extension is bundled with the npm package and installed by:

```powershell
clsp setup --workspace .
```

It is not required from the VS Code Marketplace.

## When the Bridge Runs

The extension activates after VS Code startup, but it only creates a CLSP host for workspace folders whose `.codex/config.toml` contains the CLSP managed marker.

The bridge stays disabled when:

- the workspace is not trusted
- VS Code is running remotely
- the UI is not VS Code Desktop
- the workspace is virtual

Each eligible VS Code window creates its own random session ID.

That session ID is injected only into integrated terminals created by that window after the bridge is connected. Codex hooks and MCP can then route IDE requests back to the correct window.

## What the Bridge Can Read

The bridge exposes a small action surface to the Broker:

- current editor context
- current VS Code diagnostics
- edit preparation for dirty target files
- native diff opening

It does not continuously stream editor buffers to CLSP.

### Editor context

On a `UserPromptSubmit` hook, CLSP may request:

- active file path
- document version
- dirty state
- primary selection range
- selected text, when selection sharing is enabled and the selection is small enough

Selected text is capped at **8 KiB**. Larger selections are reported as omitted rather than truncated into misleading content.

Toggle selection sharing from the Command Palette:

```text
CLSP: Toggle Selection Sharing
```

Disabling selection sharing keeps the allowed active-file metadata but removes selected text.

## VS Code Problems

`ide_diagnostics` reads diagnostics through VS Code's public `vscode.languages.getDiagnostics()` API.

This means it sees what VS Code currently considers Problems for the workspace, which can include unsaved-buffer diagnostics.

It does **not** read:

- Output Channel text
- terminal output
- build logs
- arbitrary extension stderr

An empty Problems result therefore means only that VS Code has no matching structured diagnostics at that moment. It does not prove that the project builds successfully.

### Bounds

A workspace-wide IDE diagnostic read is intentionally bounded:

- at most 5 files
- at most 20 diagnostics per file
- diagnostic messages are truncated
- the serialized diagnostic payload is bounded

When the result cannot be complete, CLSP marks it as truncated instead of pretending it is exhaustive.

## `lsp_diagnostics` vs `ide_diagnostics`

These two tools are not interchangeable.

### `lsp_diagnostics`

Uses a Language Server managed by CLSP.

Use it when Codex needs an explicit bounded check through CLSP's own LSP path.

### `ide_diagnostics`

Uses diagnostics already published inside the active VS Code window.

Use it when live editor state matters, especially for unsaved documents or diagnostics produced by an existing VS Code language extension.

## Edit Lifecycle

For `apply_patch`, CLSP can coordinate with the bound VS Code window.

### Before the edit

The PreTool hook:

1. determines the patch targets
2. routes to the correct VS Code session when possible
3. checks whether any target document is dirty
4. asks VS Code to save dirty target documents
5. records the current Problems error baseline for those targets

If dirty targets exist, VS Code shows a modal confirmation. CLSP only continues with the save path when the user chooses:

```text
Save and continue
```

Cancelling the prompt cancels the protected edit path rather than silently overwriting an unsaved target.

### After the edit

The PostTool hook:

1. routes back to the same IDE session when available
2. reads the relevant Problems again
3. compares them with the pre-edit baseline
4. reports newly introduced errors to Codex
5. asks VS Code to open review diffs for changed files

The bridge accepts at most five diff pairs in one request.

Closing a diff view does not revert the edit.

## Session Routing

The extension uses the `CLSP_IDE_SESSION_ID` environment variable to bind a Codex process started from a VS Code integrated terminal to that VS Code window.

This avoids relying on the globally focused window.

If no explicit session binding exists and multiple live windows match the same workspace, CLSP treats the route as ambiguous instead of guessing.

Opening a fresh integrated terminal from the intended VS Code window is the simplest way to restore a clean binding.

## Denied Paths

CLSP blocks a default set of sensitive paths from IDE context and Problems results:

```text
.git/**
**/.env
**/.env.*
**/*.pem
**/*.key
**/*.p12
**/*.pfx
```

You can replace the list in project configuration:

```toml
[ide]
denied_paths = [
  ".git/**",
  "**/.env",
  "private/**",
]
```

This setting replaces the default list; it does not append to it. Keep any default protections you still want.

## Local State and Privacy

CLSP keeps operational state under:

```text
%LOCALAPPDATA%\clsp
```

The Broker and IDE bridge use local IPC. The extension does not need a remote CLSP service.

Selection text and current Problems are fetched on demand. They are not intended to become a permanent editor mirror.

Diff review baselines may be retained temporarily so VS Code can open a `Before Codex ↔ After Codex` comparison after the edit.

For the process/IPC layout, see [Architecture](architecture.md).
