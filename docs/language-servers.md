# Language Servers

CLSP ships with a closed built-in registry of supported Language Servers. The registry defines which file extensions and project markers map to each server, the compatible version range, and the installation recipe used when a server is missing.

The source of truth is [`registry/servers.toml`](../registry/servers.toml).

## Supported Servers

| ID | Language | Command | Compatible version | Managed install |
| --- | --- | --- | --- | --- |
| `astro` | Astro | `astro-ls` | `>=2.16.0, <3.0.0` | `@astrojs/language-server@2.16.13` + `typescript@5.9.2` |
| `bash` | Bash / shell | `bash-language-server` | `>=5.6.0, <6.0.0` | `bash-language-server@5.6.0` |
| `csharp` | C# | `roslyn-language-server` | `=5.9.0-1.26303.1` | global `dotnet tool` |
| `clojure-lsp` | Clojure | `clojure-lsp` | `>=2026.7.6, <2027.0.0` | manual |
| `dart` | Dart | `dart language-server --protocol=lsp` | `>=2.12.0` | manual Dart/Flutter SDK |
| `deno` | Deno | `deno lsp` | `>=1.40.0` | manual Deno CLI |
| `elixir-ls` | Elixir | `language_server.bat` | `>=0.31.1, <0.32.0` | official VS Code or manual ElixirLS release |
| `eslint` | JavaScript / TypeScript / Vue | `node eslintServer.js --stdio` | `>=3.0.34, <3.1.0` | official VS Code extension; manual |
| `rust` | Rust | `rust-analyzer` | `>=1.75.0` | `rustup component add rust-analyzer` |
| `typescript` | TypeScript / JavaScript | `typescript-language-server` | `>=4.0.0, <5.0.0` | `typescript-language-server@4.4.0` + `typescript@5.9.2` |
| `pyright` | Python | `pyright-langserver` | `>=1.1.300, <2.0.0` | `pyright@1.1.405` |
| `gopls` | Go | `gopls` | `>=0.15.0, <1.0.0` | `go install golang.org/x/tools/gopls@v0.19.1` |
| `clangd` | C / C++ | `clangd` | `>=16.0.0` | verified clangd `22.1.6` archive |
| `yaml-ls` | YAML | `yaml-language-server` | `>=1.14.0, <2.0.0` | `yaml-language-server@1.18.0` |

## Discovery Order

For every server, CLSP first checks local candidates in this order:

1. `<workspace>/node_modules/.bin`
2. `<workspace>/.venv/Scripts` on Windows
3. `<workspace>/bin`
4. `[lsp.<id>].executable`, when configured
5. the current `PATH`

A candidate is not accepted just because the executable exists. CLSP probes it and checks that the reported/package version satisfies the registry requirement.

Some server types then have an additional reuse path before installation:

- npm-based servers: the selected package manager's global installation
- command/toolchain servers: toolchain-specific locations such as Go's bin directory or the global .NET tool directory
- ElixirLS: the official release bundled in standard Stable/Insiders VS Code extension directories
- ESLint: `server/out/eslintServer.js` from the official `dbaeumer.vscode-eslint` Stable/Insiders extension
- clangd: the VS Code clangd extension's managed install, then CLSP's user-level artifact cache

Only after those reuse paths fail does automatic installation begin.

## Automatic Installation

Automatic installation is enabled by default:

```toml
auto_install = true
```

Disable it with:

```toml
auto_install = false
```

With `auto_install = false`, CLSP still reuses compatible existing servers, including supported toolchain/global locations and an already-complete CLSP clangd cache. It does not run an installer or download a new archive.

### npm-based servers

CLSP probes package managers in a fixed order:

1. `bun`
2. `pnpm`
3. `npm`

The first manager whose version probe succeeds is selected.

That selection is sticky for the current resolution attempt: if its global-root query or install command fails, CLSP reports the failure instead of silently switching to the next manager.

Managed npm installs use exact versions and disable package lifecycle scripts.

### Rust

CLSP requires `rustup` when it needs to install rust-analyzer:

```powershell
rustup component add rust-analyzer
```

If a compatible project-local or `PATH` rust-analyzer is already available, no install command is run.

### Go

CLSP uses the existing Go toolchain:

```powershell
go install golang.org/x/tools/gopls@v0.19.1
```

CLSP does not override `GOBIN`. It asks Go for `GOBIN` / `GOPATH` and reuses the resulting tool location when possible.

### C#

CLSP uses the existing `dotnet` CLI and the pinned global tool:

```powershell
dotnet tool install --global roslyn-language-server --version 5.9.0-1.26303.1
```

If the global tool already exists at another version, CLSP can update it to the pinned version and allows a downgrade when needed.

Resolution verifies both the global tool listing and the executable shim/version before accepting the server.

CLSP does not install the .NET SDK itself.

### Clojure

Clojure is intentionally manual.

Install a compatible `clojure-lsp` and make it discoverable through the project, explicit configuration, or `PATH`. The registry currently expects a 2026.x compatible version and also assumes your project has the Clojure build tooling it needs.

### Dart

Dart's Language Server ships with the Dart SDK, so its installation is intentionally manual. Install Dart SDK 2.12.0 or newer, or a Flutter SDK that includes it, then expose `dart` through `PATH` or configure it explicitly:

```toml
[lsp.dart]
executable = "C:/tools/dart-sdk/bin/dart.exe"
```

CLSP starts `dart language-server --protocol=lsp`. It does not install an SDK, read VS Code's `dart.sdkPath`, or scan Dart Code/Flutter private directories. The official Dart Code extension can independently publish VS Code Problems through the existing IDE bridge.

### Deno

Deno's Language Server ships in the Deno CLI. Install Deno 1.40.0 or newer, then expose `deno` through `PATH` or configure it explicitly:

```toml
[lsp.deno]
executable = "C:/tools/deno/deno.exe"
```

CLSP starts `deno lsp` only for `.ts`, `.tsx`, `.js`, `.jsx`, and `.mjs` files below the nearest `deno.json` or `deno.jsonc`, and sends `initializationOptions.enable = true`. Within that Deno root it does not also select the TypeScript Language Server.

The official `denoland.vscode-deno` extension is a separate client of the same external Deno CLI. It can publish VS Code Problems through the existing IDE bridge, but it does not supply a bundled server for CLSP to reuse. CLSP does not install Deno or read the extension's private settings/storage.

### Elixir

ElixirLS requires a working local Erlang/OTP and Elixir installation. CLSP verifies `elixir --version` but does not install either runtime.

CLSP first checks the normal project, explicit, and `PATH` candidates for an official `language_server.bat`. It then checks the public release carried by `JakeBecker.elixir-ls` under the standard Stable and Insiders extension directories. Each candidate must contain a bounded sibling `VERSION` file matching `>=0.31.1, <0.32.0`; CLSP does not start the launcher just to guess its version.

For a manually installed official release or a custom VS Code extensions directory, configure the launcher explicitly:

```toml
[lsp.elixir-ls]
executable = "C:/tools/elixir-ls/language_server.bat"
```

CLSP does not download or compile ElixirLS. On first launch, the official ElixirLS script may use Mix to prepare its normal user-level cache, so initialize can take several minutes. That state remains outside the workspace and is owned by ElixirLS/Mix.

ElixirLS loads and compiles Mix project and dependency code. Run it only in workspaces you trust. CLSP supports `.ex` and `.exs` below the nearest `mix.exs` or `mix.lock`; template/Phoenix extensions are not part of the current OpenCode-aligned contract.

### ESLint

ESLint requires a working local Node.js runtime, the official `dbaeumer.vscode-eslint` extension server `3.0.x`, and an `eslint` package in the selected root's `node_modules`. CLSP intentionally does not fall back to a global ESLint package.

After checking the normal project, explicit, and `PATH` candidates, CLSP scans only the standard Stable and Insiders extension directories for `server/out/eslintServer.js`. It verifies the extension manifest name, publisher, and version, then verifies the project ESLint manifest before starting the server with Node and `--stdio`.

For a custom VS Code extensions directory, configure the official server entry explicitly:

```toml
[lsp.eslint]
executable = "C:/tools/vscode-eslint/server/out/eslintServer.js"
```

CLSP does not download or build `vscode-eslint`, install Node.js, or modify the project's package files. It supports `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.mts`, `.cts`, and `.vue`; the nearest `package-lock.json`, `bun.lockb`, `bun.lock`, `pnpm-lock.yaml`, or `yarn.lock` selects the server root, with the normal workspace-root fallback when no marker exists.

ESLint configurations and plugins execute project code. Start the server only in workspaces you trust.

### clangd

clangd is the only built-in server with a direct archive download recipe.

Before downloading, CLSP checks:

1. normal local candidates
2. clangd installations managed by the VS Code `llvm-vs-code-extensions.vscode-clangd` extension
3. CLSP's user-level artifact cache

If none are usable and automatic installation is enabled, CLSP downloads the pinned Windows x64 archive over HTTPS, verifies the fixed SHA-256 digest, extracts it under:

```text
%LOCALAPPDATA%\clsp\artifacts
```

The archive is not unpacked into the workspace.

CLSP does not invoke winget, Chocolatey, Scoop, or another system package manager for clangd.

If you use a custom VS Code user-data directory that CLSP cannot discover, configure clangd explicitly instead:

```toml
[lsp.clangd]
executable = "C:/path/to/clangd.exe"
```

## Per-server Overrides

Every built-in server can be disabled or pointed at a specific executable:

```toml
[lsp.rust]
enabled = false

[lsp.pyright]
executable = "tools/pyright-langserver.cmd"
```

Relative executable paths are resolved from the workspace root.

An explicit executable still has to pass the server's version/probe checks.

## Detection

The built-in registry uses file extensions and project markers to decide which servers are relevant.

Examples:

- Rust: `.rs`, `Cargo.toml`, `rust-project.json`
- Go: `.go`, `go.mod`, `go.work`
- Python: `.py`, `.pyi`, `pyproject.toml`, `pyrightconfig.json`
- TypeScript/JavaScript: JS/TS extensions plus `package.json`, `tsconfig.json`, or `jsconfig.json`
- C/C++: C-family extensions plus `compile_commands.json`, `CMakeLists.txt`, or `.clangd`
- Dart: `.dart` plus `pubspec.yaml` or `analysis_options.yaml`
- Deno: `.ts`, `.tsx`, `.js`, `.jsx`, or `.mjs` below `deno.json` or `deno.jsonc`; this takes precedence over the TypeScript server within that root
- Elixir: `.ex` or `.exs` below the nearest `mix.exs` or `mix.lock`

The registry is deliberately bounded rather than accepting arbitrary server recipes from workspace configuration.

## Installation State

CLSP keeps runtime state outside the project under `%LOCALAPPDATA%\clsp`.

Per-workspace resolution data includes the executable path, version output, and source (`project-local`, `explicit`, `path`, `vscode-extension`, or `installed`).

For broader runtime/state details, see [Architecture](architecture.md).
