# Configuration

CLSP is designed to work without a project configuration file. Create one only when you need to change discovery, diagnostics, lifecycle, IDE privacy rules, or a specific Language Server.

## Configuration Files

CLSP merges configuration in this order:

1. built-in defaults
2. user configuration
3. project configuration
4. command/runtime overrides, when applicable

User configuration:

```text
%APPDATA%\clsp\config.toml
```

Project configuration:

```text
<workspace>\.clsp.toml
```

Project values override user values recursively.

Unknown keys are rejected instead of ignored.

## Minimal Examples

Disable automatic Language Server installation:

```toml
auto_install = false
```

Disable prewarming:

```toml
prewarm = false
```

Use a custom clangd:

```toml
[lsp.clangd]
executable = "C:/LLVM/bin/clangd.exe"
```

Disable a built-in server:

```toml
[lsp.yaml-ls]
enabled = false
```

Raise the default diagnostic threshold to warnings:

```toml
[diagnostics]
minimum_severity = "warning"
```

## Full Default Shape

```toml
enabled = true
auto_install = true
prewarm = true

[runtime]
probe_timeout_ms = 1500

[install]
command_timeout_seconds = 180

[discovery]
max_initial_ms = 300
max_entries = 100000
max_depth = 8
respect_gitignore = true

[diagnostics]
minimum_severity = "error"
wait_ms = 5000
max_files = 5
max_per_file = 20
include_related_files = 2

[lifecycle]
session_lease_seconds = 120
server_idle_seconds = 1200
broker_idle_seconds = 900

[limits]
max_response_bytes = 4194304
max_file_bytes = 4194304
max_hook_input_bytes = 1048576
max_stderr_bytes = 262144

[tui]
refresh_hz_active = 10
recent_events = 100

[ide]
denied_paths = [
  ".git/**",
  "**/.env",
  "**/.env.*",
  "**/*.pem",
  "**/*.key",
  "**/*.p12",
  "**/*.pfx",
]
```

`[lsp.<id>]` tables are empty by default.

## Top-level Settings

### `enabled`

Default: `true`

Disables CLSP behavior when set to `false`.

### `auto_install`

Default: `true`

Allows CLSP to run the built-in installation recipe after compatible local/global candidates have been exhausted.

When `false`, compatible existing servers can still be reused.

### `prewarm`

Default: `true`

Allows the Broker to prepare detected language services ahead of the first explicit query.

## `[runtime]`

### `probe_timeout_ms`

Default: `1500`

Timeout for executable/version probes.

Accepted range: `1..=30000`.

## `[install]`

### `command_timeout_seconds`

Default: `180`

Timeout for managed install commands.

Accepted range: `1..=3600`.

## `[discovery]`

### `max_initial_ms`

Default: `300`

Time budget for the initial workspace discovery pass.

Accepted range: `1..=30000`.

### `max_entries`

Default: `100000`

Maximum number of filesystem entries considered by bounded discovery.

Accepted range: `1..=1000000`.

### `max_depth`

Default: `8`

Maximum discovery depth.

Accepted range: `1..=64`.

### `respect_gitignore`

Default: `true`

Controls whether discovery respects Git ignore rules.

## `[diagnostics]`

### `minimum_severity`

Default: `"error"`

Supported severities follow CLSP's diagnostic severity enum:

```text
error
warning
information
hint
```

### `wait_ms`

Default: `5000`

Maximum diagnostic wait window.

Accepted range: `0..=60000`.

### `max_files`

Default: `5`

Maximum number of files accepted by a bounded diagnostic request.

Accepted range: `1..=100`.

### `max_per_file`

Default: `20`

Maximum diagnostics retained per file.

Accepted range: `1..=1000`.

### `include_related_files`

Default: `2`

Bounds additional related files included around a diagnostic operation.

Accepted range: `0..=100`.

## `[lifecycle]`

### `session_lease_seconds`

Default: `120`

How long a Codex session lease remains valid without renewal.

Accepted range: `5..=86400`.

### `server_idle_seconds`

Default: `1200`

How long an unused managed Language Server may remain idle.

Accepted range: `1..=604800`.

### `broker_idle_seconds`

Default: `900`

How long an idle per-workspace Broker may remain alive.

Accepted range: `1..=604800`.

## `[limits]`

### `max_response_bytes`

Default: `4194304` (4 MiB)

Bounds CLSP protocol responses.

Accepted range: 64 KiB through 64 MiB.

### `max_file_bytes`

Default: `4194304` (4 MiB)

Maximum file size CLSP accepts for file-oriented operations.

Accepted range: 1 byte through 64 MiB.

### `max_hook_input_bytes`

Default: `1048576` (1 MiB)

Maximum configured Codex hook input size.

Accepted range: 1 byte through 16 MiB.

CLSP also has a hard absolute input ceiling above the configurable value.

### `max_stderr_bytes`

Default: `262144` (256 KiB)

Bounds captured Language Server stderr.

Accepted range: 1 byte through 16 MiB.

## `[tui]`

### `refresh_hz_active`

Default: `10`

Active TUI refresh rate.

Accepted range: `1..=60`.

### `recent_events`

Default: `100`

Number of recent events retained for the TUI view.

Accepted range: `1..=1000`.

## `[ide]`

### `denied_paths`

Default:

```toml
[
  ".git/**",
  "**/.env",
  "**/.env.*",
  "**/*.pem",
  "**/*.key",
  "**/*.p12",
  "**/*.pfx",
]
```

The patterns use gitignore-style matching.

Important: configuring `denied_paths` replaces the default list. It does not append to it.

## `[lsp.<id>]`

Supported server IDs:

```text
astro
bash
csharp
clangd
clojure-lsp
dart
deno
elixir-ls
gopls
pyright
rust
typescript
yaml-ls
```

Each server override supports:

```toml
[lsp.rust]
enabled = true
executable = "C:/path/to/rust-analyzer.exe"
```

Both fields are optional.

Relative executable paths are resolved from the workspace root.

## Removed / Unsupported Settings

Configuration is strict. Old or invented options do not silently do nothing.

For example, removed policy/download settings such as these are invalid:

```toml
[runtime]
policy = "managed-only"
```

```toml
[install]
download_timeout_seconds = 120
```

```toml
[lsp.rust]
policy = "local-only"
```

If CLSP reports `invalid_config`, compare your file against this document and remove stale fields.
