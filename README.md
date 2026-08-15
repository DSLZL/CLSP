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
| Elixir | ElixirLS | Reuse official VS Code release or manual ElixirLS release |
| ESLint | ESLint Language Server | Reuse official VS Code extension; project-local ESLint required |
| F# | FsAutoComplete | Reuse official Ionide VS Code extension, otherwise `dotnet tool` |
| Gleam | Gleam Language Server | Manual Gleam compiler |
| Go | gopls | `go install` |
| Haskell | Haskell Language Server | Manual GHCup/HLS toolchain |
| Java | Eclipse JDT Language Server | Reuse local JDTLS or official `redhat.java` extension |
| Julia | Julia Language Server | Reuse active Julia environment or official `julialang.language-julia` extension |
| Kotlin | Kotlin Language Server | Reuse standalone server or official `JetBrains.kotlin-server` extension |
| Lua | Lua Language Server | Reuse standalone LuaLS or official `sumneko.lua` extension |
| OCaml | OCaml Language Server | Manual opam switch package |
| Oxlint | Oxlint Language Server | Project-local package or manual executable |
| PHP | Intelephense | Reuse official VS Code extension or npm-compatible manager |
| Prisma | Prisma Language Server | Reuse official VS Code extension or npm-compatible manager |
| Python | Pyright | Reuse official VS Code extension or npm-compatible manager |
| Ruby | Ruby LSP | Reuse local/gem-installed server or `gem install` |
| Rust | rust-analyzer | `rustup component add` |
| Swift / Objective-C / Objective-C++ | SourceKit-LSP | Manual Swift toolchain or Xcode |
| Svelte | Svelte Language Server | Reuse official VS Code extension or npm-compatible manager |
| Terraform | Terraform Language Server | Reuse official VS Code extension, otherwise verified CLSP download |
| Typst | Tinymist | Reuse official VS Code extension, otherwise verified CLSP download |
| TypeScript / JavaScript | TypeScript Language Server | npm wrapper plus project, VS Code, or manager TypeScript SDK |
| Vue | Vue Language Server | Reuse official VS Code extension or npm-compatible manager |
| YAML | YAML Language Server | npm-compatible manager |

CLSP follows a simple rule:

> **Reuse first. Install only when necessary.**

TypeScript support keeps `typescript-language-server` as the LSP wrapper. It prefers the nearest project TypeScript SDK, then the SDK bundled with the built-in `vscode.typescript-language-features` extension located through the Stable or Insiders CLI, and finally the selected npm manager's SDK. The built-in extension itself uses the private `tsserver` protocol and is not an LSP server.

Dart support reuses `dart` from `PATH` or `[lsp.dart].executable`; CLSP does not install the Dart or Flutter SDK.

Deno support is enabled only below `deno.json` or `deno.jsonc`. It reuses `deno` from `PATH` or `[lsp.deno].executable`; CLSP does not install Deno or scan the official VS Code extension for a bundled server.

Elixir support covers `.ex` and `.exs` below the nearest `mix.exs` or `mix.lock`. It requires local Erlang/OTP and Elixir, then reuses an official `JakeBecker.elixir-ls` VS Code release or an explicitly configured ElixirLS `0.31.x` launcher. CLSP does not install the runtime or server; start ElixirLS only in trusted Mix projects because it compiles project and dependency code.

ESLint support covers `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.mts`, `.cts`, and `.vue`. It requires Node.js, project-local `eslint`, and the official `dbaeumer.vscode-eslint` `3.0.x` server from a standard VS Code extension directory or an explicit path. CLSP installs none of them; use ESLint only in trusted projects because configurations and plugins execute project code.

F# support covers `.fs`, `.fsi`, `.fsx`, and `.fsscript` below the nearest solution, F# project, or `global.json`. CLSP reuses the official `Ionide.Ionide-fsharp` extension before an exact global FsAutoComplete tool and can install/update that tool through an existing .NET SDK. Use it only in trusted projects because MSBuild targets can execute project code.

Gleam support covers `.gleam` files below the nearest `gleam.toml`, with the workspace root as the fallback. CLSP reuses a compatible Gleam `1.x` compiler from `PATH` or `[lsp.gleam].executable` and starts its built-in `gleam lsp`; it does not install Gleam, Erlang/OTP, or scan the official `Gleam.gleam` extension for a bundled server. The extension uses the same external compiler and can independently publish VS Code Problems through the IDE bridge.

Go support covers `.go` files. A `go.work` anywhere between the file and workspace root takes priority over the nearest `go.mod` or `go.sum`; CLSP reuses a compatible `gopls` or installs the pinned version through the existing Go toolchain. The official `golang.Go` VS Code extension is a separate client of the same external server and can independently publish Problems through the IDE bridge.

Haskell support covers `.hs` and `.lhs` files below the nearest `stack.yaml`, `cabal.project`, `hie.yaml`, or `*.cabal`, with the workspace root as the fallback. CLSP reuses a compatible HLS `2.x` wrapper from `PATH` or `[lsp.hls].executable` and starts `haskell-language-server-wrapper --lsp`; it does not install or select GHC/HLS versions. The official `haskell.haskell` extension is an independent client of an external HLS and can publish Problems through the IDE bridge. Start HLS only in trusted projects because project cradle/build configuration may execute code.

Java support covers `.java` files inside recognized Gradle, Maven, or Eclipse projects. CLSP reuses a local `jdtls` launcher or the server bundled with the official `redhat.java` Stable/Insiders extension, requires Java 21+, and keeps JDTLS data isolated per project root. Loose Java files without a project marker do not start a CLSP JDTLS client. CLSP installs none of these components; open only trusted Maven/Gradle projects because project import may execute build logic.

Julia support covers `.jl` files below the nearest `Project.toml`, `Manifest.toml`, or directory containing Julia source, with the workspace root as the fallback. CLSP first reuses Julia 1.10+ with LanguageServer.jl 5.x in its active environment, then an official `julialang.language-julia` Stable/Insiders environment with Julia 1.11+. CLSP installs none of these components; use JuliaLS only in trusted projects because it loads Julia environments and package metadata.

Kotlin support covers `.kt` and `.kts` files in Gradle or Maven projects. CLSP reuses a compatible standalone Kotlin Language Server or the server and JBR 25 bundled with the official `JetBrains.kotlin-server` Stable/Insiders extension, and isolates server indexes per project root. CLSP installs none of these components; use it only in trusted projects because Gradle and Maven imports may execute build logic.

Lua support covers `.lua` files below the nearest OpenCode-compatible Lua configuration marker, with the workspace root as the fallback. CLSP first reuses a compatible LuaLS `3.x` executable, then the complete server bundled with the official `sumneko.lua` Stable/Insiders extension. CLSP installs neither source; use trusted project configuration because LuaLS plugins may execute code.

OCaml support covers `.ml` and `.mli` files below the nearest `dune-project`, `dune-workspace`, `.merlin`, or `opam`, with the workspace root as the fallback. CLSP reuses a compatible `ocamllsp` from `PATH` or `[lsp.ocaml-lsp].executable`; it does not install opam, OCaml, Dune, the server, or the official `ocamllabs.ocaml-platform` extension. The extension is an independent client of the same external server and can publish Problems through the IDE bridge.

PHP support covers `.php` files below the nearest `composer.json`, `composer.lock`, or `.php-version`, with the workspace root as the fallback. CLSP reuses a compatible project, explicit, or `PATH` Intelephense, then the server bundled with the official `bmewburn.vscode-intelephense-client` Stable/Insiders extension, then a compatible global package; if enabled, automatic installation pins `intelephense@1.18.5`. CLSP sends only `telemetry.enabled = false` and never reads or manages Intelephense licence files.

Prisma support covers `.prisma` files below the nearest `schema.prisma`, `prisma/schema.prisma`, or `prisma` directory, with the workspace root as the fallback. CLSP reuses a compatible project, explicit, or `PATH` server, then the bundle in the official `Prisma.prisma` Stable/Insiders extension, then a compatible global package; if enabled, automatic installation pins `@prisma/language-server@31.11.0`. Node.js 20+ is required. Use only trusted workspaces because Prisma configuration can execute project code.

Python support covers `.py` and `.pyi` files below the nearest OpenCode-compatible Python project marker, with the workspace root as the fallback. CLSP reuses a compatible project, explicit, or `PATH` Pyright, then the server bundled with the official `ms-pyright.pyright` Stable/Insiders extension, then a compatible global package; if enabled, automatic installation pins `pyright@1.1.411`. Node.js 14+ is required.

Ruby support covers `.rb`, `.rake`, `.gemspec`, and `.ru` files below the nearest `Gemfile`, with the workspace root as the fallback. CLSP requires Ruby 3.0 or newer, reuses a compatible local, explicit, or `PATH` `ruby-lsp`, and, when automatic installation is enabled, runs the fixed `gem install ruby-lsp --version 0.26.10 --no-document` command. The official `Shopify.ruby-lsp` extension is an independent VS Code client of the external server; CLSP does not scan its private bundle or manage `.ruby-lsp` contents. Use Ruby LSP only in trusted projects because Bundler and Gemfile configuration can execute project code.

Oxlint support covers `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.mts`, `.cts`, `.vue`, `.astro`, and `.svelte`. CLSP reuses compatible `oxlint` 1.x from the selected project's `node_modules/.bin`, `PATH`, or `[lsp.oxlint].executable` and starts `oxlint --lsp`; it does not install packages or the official `oxc.oxc-vscode` extension. The extension independently uses the same external project tool and can publish Problems through the IDE bridge. Use Oxlint only in trusted projects because configuration and JavaScript plugins may execute project code.

Svelte support covers `.svelte` files below the nearest `package-lock.json`, `bun.lockb`, `bun.lock`, `pnpm-lock.yaml`, or `yarn.lock`, with the workspace root as fallback. CLSP first reuses a compatible project, explicit, or `PATH` server, then the verified `svelte-language-server` bundled in the official `svelte.svelte-vscode` Stable/Insiders extension, then a compatible global package; automatic installation pins `svelte-language-server@0.18.4` with `typescript@5.9.2`. Node.js 18+ is required for the JavaScript entry. The Svelte extension independently publishes VS Code Problems; CLSP does not manage the extension or its private settings.

Vue support covers `.vue` files below the nearest npm, Bun, pnpm, or Yarn lockfile, with the workspace root as fallback. CLSP first reuses a compatible project, explicit, or `PATH` server, then the verified `dist/language-server.js` bundled in the official `Vue.volar` Stable/Insiders extension, then a compatible global package; automatic installation pins `@vue/language-server@3.3.9` with `typescript@5.9.2`. CLSP passes a verified TypeScript SDK through `--tsdk` and sends no private initialization options. It acknowledges Vue's standalone project-info notification with the selected root config, but does not reproduce the official extension's full private TypeScript bridge; template diagnostics and document symbols work independently, while bridge-dependent semantic features remain with the official extension. Vue, ESLint, and Oxlint may run together by design.

Terraform support covers `.tf` and `.tfvars` files below the nearest `.terraform.lock.hcl`, `terraform.tfstate`, or directory containing `.tf` files, with the workspace root as fallback. CLSP reuses a compatible project, explicit, or `PATH` `terraform-ls`, then the verified server bundled in the official `HashiCorp.terraform` Stable/Insiders extension, then its managed cache; on Windows x86-64, automatic installation can download the pinned, checksum-verified `terraform-ls` 0.39.0 archive. CLSP does not install Terraform CLI, providers, modules, or the VS Code extension. `.tfvars` uses the protocol language ID `terraform-vars`.

Typst support covers `.typ` and `.typc` files below the nearest `typst.toml`, with the workspace root as fallback. CLSP reuses a compatible project, explicit, or `PATH` Tinymist, then the verified server bundled in the official `myriad-dreamin.tinymist` Stable/Insiders extension, then its managed cache; on Windows x86-64, automatic installation can download the pinned, checksum-verified Tinymist 0.15.2 archive. CLSP starts `tinymist lsp` and does not install the VS Code extension or a separate Typst CLI. `.typc` uses the protocol language ID `typst-code`.

SourceKit-LSP support covers `.swift`, `.objc`, and `.objcpp` below the nearest `Package.swift`, Xcode project/workspace directory, or compilation database, with the workspace root as the fallback. CLSP reuses `sourcekit-lsp` from the project, an explicit path, or `PATH`, and on macOS can resolve it through `xcrun`; it validates a short `--help` probe and Swift 5.9+. It sends the protocol language IDs `swift`, `objective-c`, and `objective-cpp` for those extensions. CLSP does not install Swift, Xcode, or the official `swiftlang.swift-vscode` extension. SwiftPM/Xcode indexing can take several minutes and may execute trusted project build configuration, so use SourceKit-LSP only in trusted workspaces.

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
