![NPM Version](https://img.shields.io/npm/v/%40dslzl%2Fclsp)
![NPM Downloads](https://img.shields.io/npm/dm/%40dslzl%2Fclsp)
[![Socket Badge](https://badge.socket.dev/npm/package/@dslzl/clsp)](https://badge.socket.dev/npm/package/@dslzl/clsp)

[**English**](README.md) | [简体中文](README_ZH.md)

# CLSP

**LSP and live VS Code diagnostics for Codex CLI on Windows.**

CLSP gives Codex CLI the language-aware context you normally expect from an IDE: definitions, references, hover information, diagnostics, and live VS Code Problems.

It runs as a lightweight per-workspace broker, reuses compatible Language Servers already available on your machine, and can install supported servers when they are missing.

> Current release target: **Windows x64 + VS Code Desktop**.

## Quick Start

### Requirements

- Windows x64
- Node.js / npm
- VS Code Desktop
- `code` available in `PATH`
- Codex CLI

### Install

```powershell
npm install -g @dslzl/clsp
```

Then run this inside the project you want to use with Codex:

```powershell
clsp setup --workspace .
```

`setup` installs the bundled VS Code adapter and merges the CLSP MCP + hook configuration into the project's `.codex` directory.

After setup:

1. Open Codex in the project.
2. Run `/hooks` and review/trust the project hooks.
3. Reload VS Code.
4. Use Codex normally.

## What CLSP Adds

- **Code navigation** — hover, definition, and references through LSP.
- **Language diagnostics** — bounded diagnostics from CLSP-managed Language Servers.
- **Live IDE diagnostics** — reuse the current VS Code Problems state, including diagnostics for dirty documents when VS Code has published them.
- **Post-edit feedback** — compare diagnostics around Codex edits and surface newly introduced errors.
- **Edit review** — open native `Before Codex ↔ After Codex` diffs in VS Code.
- **Environment reuse** — prefer project-local and already-installed Language Servers before installing anything.
- **Status tools** — inspect the Broker, servers, IDE bridge, and degraded states from the CLI or TUI.

## Supported Languages

| Language | Language Server | Missing-server strategy |
| --- | --- | --- |
| Astro | Astro Language Server | npm-compatible manager |
| Bash / shell | Bash Language Server | npm-compatible manager |
| C# | Roslyn Language Server | `dotnet tool` |
| Clojure | clojure-lsp | Manual |
| C / C++ | clangd | Reuse local/VS Code copy, otherwise verified CLSP download |
| Dart | Dart Language Server | Manual Dart/Flutter SDK |
| Deno | Deno Language Server | Manual Deno CLI |
| Go | gopls | `go install` |
| Python | Pyright | npm-compatible manager |
| Rust | rust-analyzer | `rustup component add` |
| TypeScript / JavaScript | TypeScript Language Server | npm-compatible manager |
| YAML | YAML Language Server | npm-compatible manager |

CLSP follows a simple rule:

> **Reuse first. Install only when necessary.**

Dart support reuses `dart` from `PATH` or `[lsp.dart].executable`; CLSP does not install the Dart or Flutter SDK.

Deno support is enabled only below `deno.json` or `deno.jsonc`. It reuses `deno` from `PATH` or `[lsp.deno].executable`; CLSP does not install Deno or scan the official VS Code extension for a bundled server.

For exact versions, discovery order, and installation behavior, see [Language Servers](docs/language-servers.md).

## VS Code Integration

The bundled **CLSP IDE Bridge** is deliberately small. It does not implement another language server.

Instead, it uses VS Code's public APIs to provide CLSP with:

- the active file
- document version and dirty state
- the primary selection, when selection sharing is enabled
- current VS Code Problems
- dirty-file confirmation before an edit
- native diff views after an edit

Selection sharing can be toggled with:

```text
CLSP: Toggle Selection Sharing
```

See [IDE Integration](docs/ide-integration.md) for routing, limits, privacy behavior, and edit lifecycle details.

## MCP Tools

CLSP exposes four read-only MCP tools:

| Tool | Purpose |
| --- | --- |
| `lsp_query` | Hover, definition, and references |
| `lsp_diagnostics` | Diagnostics from CLSP-managed Language Servers |
| `lsp_status` | Broker, server, hook, and integration status |
| `ide_diagnostics` | Current VS Code Problems |

`lsp_diagnostics` and `ide_diagnostics` are intentionally separate:

- `lsp_diagnostics` talks to Language Servers managed by CLSP.
- `ide_diagnostics` reuses diagnostics already published inside VS Code.

## CLI

```text
clsp setup --workspace <path>
clsp status [--workspace <path>]
clsp tui [--workspace <path>]
```

`mcp`, `hook`, `broker`, and `ide-host` are integration/runtime commands. In normal use, `clsp setup` configures the pieces that Codex and VS Code need.

## Configuration

Most users do not need a `.clsp.toml`.

A common reason to create one is to disable automatic Language Server installation:

```toml
auto_install = false
```

CLSP also supports user-level configuration at:

```text
%APPDATA%\clsp\config.toml
```

Project configuration in `.clsp.toml` overrides user configuration.

See [Configuration](docs/configuration.md) for all supported keys and defaults.

## Platform Support

Supported release target:

- Windows x64
- `x86_64-pc-windows-msvc`
- VS Code Desktop

Not currently supported by the bundled VS Code bridge:

- VS Code Remote windows
- untrusted workspaces
- virtual workspaces
- non-desktop VS Code UI
- Windows GNU as a release target

When the IDE bridge is unavailable, CLSP keeps its standalone LSP/MCP path available where possible.

## Documentation

- [Language Servers](docs/language-servers.md) — discovery, versions, installation, and overrides
- [IDE Integration](docs/ide-integration.md) — VS Code Problems, selection sharing, edit checks, and privacy
- [Configuration](docs/configuration.md) — `.clsp.toml`, user config, defaults, and limits
- [Troubleshooting](docs/troubleshooting.md) — common setup, runtime, IDE, and server issues
- [Architecture](docs/architecture.md) — Broker, MCP, hooks, LSP processes, IPC, and lifecycle
- [Contributing](CONTRIBUTING.md) — local development, tests, registry changes, and releases

## Status

Print a Broker snapshot:

```powershell
clsp status --workspace .
```

Open the terminal overview:

```powershell
clsp tui --workspace .
```

If something looks wrong, start with [Troubleshooting](docs/troubleshooting.md).

## Uninstall

Remove the VS Code adapter:

```powershell
code --uninstall-extension clsp.clsp-ide
```

To fully remove CLSP from a project, also remove the CLSP-managed MCP and hook entries created by `clsp setup`, then restart Codex and VS Code.

## Contributing

Development instructions live in [CONTRIBUTING.md](CONTRIBUTING.md).

## License

GNU Affero General Public License v3.0 only (`AGPL-3.0-only`)
