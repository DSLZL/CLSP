# Architecture

CLSP is built around one principle: **keep Codex integration thin, keep language processes reusable, and avoid turning the VS Code extension into a second LSP stack.**

## Components

```text
┌───────────────────────────────────────────────────────────┐
│ Codex CLI                                                 │
│                                                           │
│  MCP client ───────────────┐                              │
│  lifecycle hooks ──────────┼──────────────┐               │
└────────────────────────────┼──────────────┼───────────────┘
                             │              │
                             ▼              ▼
                      ┌─────────────────────────┐
                      │ CLSP per-workspace      │
                      │ Broker                  │
                      │                         │
                      │ discovery / install     │
                      │ server lifecycle        │
                      │ diagnostics / routing   │
                      │ IDE session registry    │
                      └───────┬─────────┬───────┘
                              │         │
                    LSP stdio │         │ local IDE actions
                              ▼         ▼
                  ┌──────────────┐   ┌────────────────┐
                  │ Language     │   │ clsp ide-host  │
                  │ Servers      │   └───────┬────────┘
                  └──────────────┘           │ stdio JSON
                                             ▼
                                    ┌──────────────────┐
                                    │ VS Code          │
                                    │ CLSP IDE Bridge  │
                                    │                  │
                                    │ editor context   │
                                    │ Problems         │
                                    │ save confirmation│
                                    │ native diffs     │
                                    └──────────────────┘
```

## CLI Process

`clsp.exe` provides several command surfaces:

- `setup` — installs/configures the project integration
- `mcp` — stdio MCP adapter used by Codex
- `status` — prints a Broker snapshot
- `tui` — attaches the terminal overview
- `hook ...` — Codex lifecycle hook handlers
- `broker` — per-workspace background Broker
- `ide-host` — stdio relay between the VS Code extension and Broker

`broker` and `ide-host` are internal runtime commands rather than normal user entry points.

## Workspace Identity

CLSP resolves a workspace root and derives per-workspace runtime state.

State lives under:

```text
%LOCALAPPDATA%\clsp\state\workspaces\<workspace-hash>\
```

Shared managed artifacts live under:

```text
%LOCALAPPDATA%\clsp\artifacts\
```

This keeps generated runtime/install state out of the source workspace.

## Broker

The Broker is the long-lived coordinator for one workspace.

Its responsibilities include:

- workspace discovery
- built-in Language Server registry handling
- executable resolution and managed installation
- Language Server startup/reuse/shutdown
- LSP request routing
- diagnostics collection
- Codex session leases
- IDE session registration/routing
- bounded event/status state
- post-edit diagnostic baselines and review metadata

Clients do not each start an independent server stack by default. They route requests through the Broker.

## MCP Path

Codex starts CLSP's stdio MCP adapter.

The adapter exposes:

- `lsp_query`
- `lsp_diagnostics`
- `lsp_status`
- `ide_diagnostics`

The MCP adapter validates workspace paths and request bounds, then forwards structured requests to the Broker.

The public MCP tool surface is read-only.

## Language Server Path

The Broker uses the built-in registry to decide which Language Server applies to a file/workspace.

Resolution prefers compatible existing executables before installation.

Once resolved, the Broker starts the Language Server over stdio and keeps the process associated with the relevant server/root identity.

The Broker can idle servers out instead of creating a fresh process for every MCP call.

See [Language Servers](language-servers.md) for the concrete resolver order.

## Codex Hooks

`clsp setup` installs five lifecycle handlers:

```text
SessionStart
UserPromptSubmit
PreToolUse
PostToolUse
SessionEnd
```

They serve different purposes.

### SessionStart

Acquires a session lease and gives Codex a small status/context message.

### UserPromptSubmit

When a safe IDE route is available, requests the current editor context from the bound VS Code window.

The editor buffer is not continuously mirrored.

### PreToolUse

For `apply_patch`, CLSP can:

- identify target files
- validate bounded hook input
- route to the matching IDE window
- ask VS Code to save dirty target buffers
- establish an IDE diagnostic baseline

### PostToolUse

After an edit, CLSP can:

- renew the session lease
- synchronize changed files through the IDE route or CLSP LSP path
- compare IDE diagnostics against the baseline
- surface newly introduced errors
- ask VS Code to open native review diffs

### SessionEnd

Releases the Codex session lease.

## VS Code Bridge

The extension does not run an LSP client for CLSP.

Instead, it spawns:

```text
clsp ide-host --workspace <root> --session-id <id>
```

`ide-host` registers the VS Code session with the Broker and relays a bounded local JSON protocol over stdio.

The Broker can request four IDE actions:

- editor context
- diagnostics
- edit preparation
- diff opening

The extension performs those actions through public VS Code APIs.

## IDE Session Binding

Each VS Code window generates a random session ID.

When all CLSP hosts in that window are connected, the extension injects:

```text
CLSP_IDE_SESSION_ID
```

into newly created integrated terminals.

Codex started from that terminal carries a reliable hint back to the originating VS Code window.

If no hint exists and several matching IDE sessions are live, CLSP rejects the ambiguous route rather than choosing whichever window happens to be focused.

## IPC and Local Trust Boundary

The Broker uses local Windows named-pipe IPC.

CLSP stores Broker metadata/token material under the local CLSP state directory and applies Windows access-control checks to sensitive runtime files.

The design assumes a local same-user integration boundary, not a network service boundary.

Protocol messages are versioned and bounded. A stale component can fail with `protocol_mismatch` instead of attempting to interpret incompatible traffic.

## Bounded Data

Several data paths are intentionally limited:

- file size
- MCP response size
- hook input size
- Language Server stderr capture
- IDE selection text
- IDE diagnostics
- IDE action queue/messages
- diff pair count

The goal is predictable behavior even when a workspace, extension, or Language Server produces unexpectedly large output.

## Lifecycle

There are three distinct notions of liveness:

### Codex session lease

A Codex session acquires and renews a lease through hooks. The default lease is 120 seconds.

### Language Server idle timeout

Managed servers can be shut down after they have been unused for the configured idle period. Default: 1200 seconds.

### Broker idle timeout

The workspace Broker can exit after the configured idle period. Default: 900 seconds.

IDE session registration has its own short freshness window so a dead VS Code bridge disappears quickly from routing.

## Failure Model

CLSP tries to degrade rather than block ordinary Codex work.

Examples:

- missing IDE bridge: standalone LSP/MCP remains available
- ambiguous IDE routing: do not guess the VS Code window
- missing IDE baseline: skip the IDE delta path rather than inventing a result
- missing compatible Language Server with auto-install disabled: return an explicit runtime-unavailable result
- protocol mismatch: fail clearly and require process restart

The edit-protection path is intentionally stricter when an unsaved target would otherwise be overwritten.

## Setup Ownership

`clsp setup` owns only the CLSP-managed pieces of project Codex configuration.

It merges into:

```text
.codex/config.toml
.codex/hooks.json
```

and uses a managed marker so the bundled VS Code bridge can recognize configured workspaces.

Unrelated MCP and hook configuration should remain untouched.

For operational problems, see [Troubleshooting](troubleshooting.md).
