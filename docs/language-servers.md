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
| `fsharp` | F# | `fsautocomplete` | `=0.83.0` | official Ionide VS Code extension or global `dotnet tool` |
| `gleam` | Gleam | `gleam lsp` | `>=1.0.0, <2.0.0` | manual Gleam compiler |
| `rust` | Rust | `rust-analyzer` | `>=1.75.0` | `rustup component add rust-analyzer` |
| `typescript` | TypeScript / JavaScript | `typescript-language-server` | `>=4.0.0, <5.0.0` | `typescript-language-server@4.4.0` + `typescript@5.9.2` |
| `pyright` | Python | `pyright-langserver --stdio` | `>=1.1.300, <2.0.0` | official VS Code extension or `pyright@1.1.411` |
| `ruby-lsp` | Ruby | `ruby-lsp` | `>=0.26.10, <0.27.0` | `gem install ruby-lsp --version 0.26.10 --no-document` |
| `sourcekit-lsp` | Swift / Objective-C / Objective-C++ | `sourcekit-lsp` | `>=5.9.0` (Swift toolchain) | manual Swift toolchain / Xcode |
| `svelte` | Svelte | `svelteserver --stdio` | `>=0.18.4, <0.19.0` | official `svelte.svelte-vscode` extension or `svelte-language-server@0.18.4` + `typescript@5.9.2` |
| `gopls` | Go | `gopls` | `>=0.15.0, <1.0.0` | `go install golang.org/x/tools/gopls@v0.23.0` |
| `hls` | Haskell | `haskell-language-server-wrapper --lsp` | `>=2.0.0, <3.0.0` | manual GHCup/HLS toolchain |
| `intelephense` | PHP | `intelephense --stdio` | `>=1.18.5, <2.0.0` | official VS Code extension or `intelephense@1.18.5` |
| `prisma` | Prisma | `prisma-language-server --stdio` | `>=6.19.0, <32.0.0` | official VS Code extension or `@prisma/language-server@31.11.0` |
| `jdtls` | Java | `jdtls` or Java + Equinox launcher | `>=1.30.0, <2.0.0` | official `redhat.java` extension or manual JDTLS |
| `julials` | Julia | `julia ... using LanguageServer; runserver()` | `>=5.0.0, <6.0.0` | active Julia environment or official `julialang.language-julia` extension |
| `kotlin-ls` | Kotlin | `kotlin-lsp` or `intellij-server --stdio` | `>=262.4739.0, <264.0.0` | official `JetBrains.kotlin-server` extension or manual standalone server |
| `lua-ls` | Lua | `lua-language-server` | `>=3.19.0, <4.0.0` | official `sumneko.lua` extension or manual standalone server |
| `ocaml-lsp` | OCaml | `ocamllsp` | `>=1.4.1, <2.0.0` | manual `ocaml-lsp-server` opam package |
| `oxlint` | JavaScript / TypeScript / Vue / Astro / Svelte | `oxlint --lsp` | `>=1.78.0, <2.0.0` | project-local package or manual executable |
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
- F#: `bin/net*/fsautocomplete.dll` from the official `Ionide.Ionide-fsharp` Stable/Insiders extension, then the exact global .NET tool
- Intelephense: `node_modules/intelephense/lib/intelephense.js` from the official `bmewburn.vscode-intelephense-client` Stable/Insiders extension, then the selected package manager's global installation
- Prisma: `dist/language-server/bin.js` plus its schema WASM from the official `Prisma.prisma` Stable/Insiders extension, then the selected package manager's global installation
- Pyright: `dist/server.js` from the official `ms-pyright.pyright` Stable/Insiders extension, then the selected package manager's global installation
- Svelte: `node_modules/svelte-language-server/bin/server.js` from the official `svelte.svelte-vscode` Stable/Insiders extension after strict manifest/path validation, then the selected package manager's global installation
- JDTLS: the official `redhat.java` Stable/Insiders extension after local launchers; its manifest, JDTLS core, platform configuration, and Java 21+ runtime are verified
- JuliaLS: the official `julialang.language-julia` Stable/Insiders extension after local Julia environments; its manifest, matching/fallback environment, LanguageServer package, and Julia 1.11+ runtime are verified
- Kotlin: `intellij-server` on `PATH`, then the server bundled with the official `JetBrains.kotlin-server` Stable/Insiders extension; its manifest, product/build metadata, launcher, and JBR 25 are verified
- LuaLS: the complete server bundled with the official `sumneko.lua` Stable/Insiders extension after standalone executables; its manifest, launcher, runtime files, and actual server version are verified
- clangd: the VS Code clangd extension's managed install, then CLSP's user-level artifact cache
- SourceKit-LSP: macOS `xcrun --find sourcekit-lsp` after local/explicit/PATH candidates; CLSP never scans arbitrary VS Code extension directories

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
go install golang.org/x/tools/gopls@v0.23.0
```

CLSP does not override `GOBIN`. It asks Go for `GOBIN` / `GOPATH` and reuses the resulting tool location when possible.

For `.go` files, CLSP first searches up to the workspace boundary for `go.work`. If none exists, the nearest `go.mod` or `go.sum` is used; otherwise the workspace root is the fallback.

The official `golang.Go` VS Code extension uses the same external `gopls` and may install or update it independently. It does not carry a server binary for CLSP to scan, but its Problems remain available through the existing IDE bridge.

### Haskell

Haskell Language Server must match the GHC used by the project. Install a supported GHC and the Cabal or Stack tooling required by the project, then install HLS through GHCup:

```powershell
ghcup install hls 2.14.0.0
```

The current validation baseline is HLS `2.14.0.0` with GHC `9.12.4`. CLSP accepts compatible HLS `2.x` wrappers, recognizes HLS's four-component PVP version output, and does not install GHCup, GHC, Cabal, Stack, HLS, or editor extensions.

CLSP starts `haskell-language-server-wrapper --lsp` for `.hs` and `.lhs` files. It uses the nearest `stack.yaml`, `cabal.project`, `hie.yaml`, or `*.cabal` directory, falling back to the workspace root. Configure a wrapper outside `PATH` explicitly:

```toml
[lsp.hls]
executable = "C:/tools/ghcup/bin/haskell-language-server-wrapper.exe"
```

The official `haskell.haskell` VS Code extension resolves an external HLS through its own explicit path, `PATH`, or GHCup workflow. CLSP does not scan its configurable globalStorage or reproduce its project-GHC version selection; set the extension to use `PATH` when both clients should share the same toolchain. Its Problems remain available through the existing IDE bridge. Start HLS only in trusted projects because Cabal, Stack, and cradle configuration may execute project code.

### Java

JDTLS requires Java 21+. CLSP first checks project-local, explicit, and `PATH` launchers, then the standard Stable/Insiders directories for the official `redhat.java` extension. It validates the extension manifest, the unique Equinox launcher and JDTLS core JAR, the current platform configuration, and an embedded or system Java 21+ runtime. CLSP does not download Java, JDTLS, or the extension.

The current validation baselines are `redhat.java` `1.55.0` and Eclipse JDT Language Server `1.61.0-202608051627`. A standalone launcher outside `PATH` can be configured directly:

```toml
[lsp.jdtls]
executable = "C:/tools/jdtls/bin/jdtls.bat"
```

Root selection follows OpenCode: Gradle settings files take priority over a wrapper and build files; Maven roots climb only through parent POMs that explicitly declare the child module; `.project` and `.classpath` provide the Eclipse fallback. A loose `.java` file with none of these markers does not start CLSP JDTLS. Each selected root gets a separate CLSP-owned `-data` directory.

JDTLS may import Maven or Gradle projects and execute build logic. Use it only with trusted projects.

### Julia

For standalone use, install Julia 1.10+ and LanguageServer.jl 5.x in the Julia environment visible to CLSP:

```powershell
julia -e 'using Pkg; Pkg.add(name="LanguageServer", version="5.0.0")'
julia --startup-file=no --history-file=no -e 'using LanguageServer; println(pkgversion(LanguageServer))'
```

CLSP probes Julia and the package without loading LanguageServer, then starts the OpenCode-compatible `using LanguageServer; runserver()` stdio command. A Julia executable outside `PATH` can be configured directly:

```toml
[lsp.julials]
executable = "C:/tools/Julia/bin/julia.exe"
```

If the active Julia environment has no compatible package, CLSP scans only the standard Stable/Insiders directories for the official `julialang.language-julia` 1.x extension. With Julia 1.11+, it selects the extension's `v<major>.<minor>` LanguageServer environment or its `fallback`, validates the environment and bundled LanguageServer 5.x package, and starts Julia with `--project=<environment>`. CLSP does not install Julia, Juliaup, LanguageServer.jl, or the extension.

Root selection follows OpenCode: the nearest `Project.toml`, `Manifest.toml`, or directory containing `*.jl` wins, otherwise CLSP uses the workspace root. JuliaLS loads Julia environments and package metadata; use it only with trusted projects.

### Kotlin

The standalone Kotlin Language Server requires JDK 25; the official extension bundles its own JBR 25. CLSP first checks project-local, explicit, and `PATH` `kotlin-lsp` launchers, then `intellij-server` on `PATH`, then the bundled server in the official `JetBrains.kotlin-server` Stable/Insiders extension. It accepts server versions from 262.4739.0 through the 263.x line; the standalone validation baseline is 262.9593.0, while the current extension baseline is 0.0.8 with server 263.2689.0. CLSP does not install a JDK, server, or extension.

Configure a standalone launcher outside `PATH` directly:

```toml
[lsp.kotlin-ls]
executable = "C:/tools/kotlin-lsp/bin/intellij-server.exe"
```

Root selection follows OpenCode precedence: Gradle settings files, wrapper scripts, Gradle build files, then Maven `pom.xml`; otherwise CLSP uses the workspace root. Each selected root gets a separate CLSP-owned `--system-path`, and Kotlin receives no server-specific initialization options. `JAVA_HOME` and `GRADLE_USER_HOME` are preserved, and Kotlin alone gets a longer initialize deadline for cold IntelliJ startup. Use only trusted projects because Gradle and Maven imports may execute build logic.

### Lua

CLSP first checks project-local, explicit, and `PATH` `lua-language-server` executables, then the standard Stable/Insiders directories for the official `sumneko.lua` extension. It accepts LuaLS `3.x` from `3.19.0` onward and validates that an extension candidate has the official manifest, complete runtime layout, and a server version matching the extension. CLSP does not install LuaLS or the extension.

Configure a standalone server outside `PATH` directly:

```toml
[lsp.lua-ls]
executable = "C:/tools/lua-language-server/bin/lua-language-server.exe"
```

Root selection follows OpenCode: the nearest `.luarc.json`, `.luarc.jsonc`, `.luacheckrc`, `.stylua.toml`, `stylua.toml`, `selene.toml`, or `selene.yml` wins; otherwise CLSP uses the workspace root. LuaLS receives no extension-private initialization options. Project configuration can enable executable LuaLS plugins, so use only trusted projects.

### OCaml

Install `ocaml-lsp-server` into the same opam switch as the project toolchain. The current validation baseline is OCaml `5.5.0` with `ocaml-lsp-server` `1.27.0`:

```powershell
opam install ocaml-lsp-server.1.27.0
opam exec -- ocamllsp --version
```

Expose that switch's `ocamllsp` through `PATH`, or configure it directly:

```toml
[lsp.ocaml-lsp]
executable = "C:/Users/you/AppData/Local/opam/5.5.0/bin/ocamllsp.exe"
```

CLSP starts the bare `ocamllsp` stdio command for `.ml` and `.mli` files. The nearest `dune-project`, `dune-workspace`, `.merlin`, or `opam` selects the root; otherwise the workspace root is used. CLSP does not install the toolchain or scan `ocamllabs.ocaml-platform`, because the official extension also resolves an external server from its selected sandbox. Its Problems remain available through the existing IDE bridge. Use only trusted project build configuration.

### Oxlint

Add a compatible Oxlint to the project with your normal package manager. The current validation baseline is:

```powershell
bun add --dev --exact oxlint@1.78.0
bunx oxlint --version
```

CLSP starts `oxlint --lsp` for `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.mts`, `.cts`, `.vue`, `.astro`, and `.svelte`. It first checks the selected root's `node_modules/.bin`, then `[lsp.oxlint].executable` and `PATH`; it accepts versions `>=1.78.0, <2.0.0` and does not install or update the package.

The nearest `.oxlintrc.json`, `package-lock.json`, `bun.lockb`, `bun.lock`, `pnpm-lock.yaml`, `yarn.lock`, or `package.json` selects the root, with the workspace root as the fallback. The official `oxc.oxc-vscode` extension independently resolves the same external project tool and can publish Problems through the IDE bridge; it is not a server source for CLSP.

CLSP sends no extension-private `oxc.*` settings. Keep lint behavior in the project's Oxlint configuration. Configuration and JavaScript plugins can execute project code, so use Oxlint only in trusted workspaces.

### PHP

Intelephense `1.18.5` requires Node.js 20 or newer. CLSP first checks the selected project's `node_modules/.bin`, `[lsp.intelephense].executable`, and `PATH`. It then scans only the standard Stable/Insiders directories for the official `bmewburn.vscode-intelephense-client` extension and validates its identity, version, nested `intelephense` package, entry path, and extension-root containment.

If no compatible existing source is found, CLSP checks the selected package manager's global root and, when automatic installation is enabled, installs the exact `intelephense@1.18.5` package. A custom extension directory can be configured directly:

```toml
[lsp.intelephense]
executable = "C:/tools/vscode-intelephense/node_modules/intelephense/lib/intelephense.js"
```

CLSP starts `intelephense --stdio` for `.php` files. The nearest `composer.json`, `composer.lock`, or `.php-version` selects the root, otherwise the workspace root is used. Initialization contains only `{"telemetry":{"enabled":false}}`. The server is proprietary/freemium; premium activation remains Intelephense's responsibility through `%USERPROFILE%/intelephense/licence.txt`, and CLSP does not read, copy, or manage that file or its key.

### Prisma

Prisma Language Server requires Node.js 20 or newer. OpenCode's current built-in entry requires the `prisma` CLI; CLSP instead targets the official standalone language-server package and VS Code extension, and does not exclude a project merely because it contains `package.json`. CLSP first checks the selected project's `node_modules/.bin`, `[lsp.prisma].executable`, and `PATH`. It then scans only the standard Stable/Insiders directories for the official `Prisma.prisma` extension and validates its identity, version, bundled `dist/language-server/bin.js`, schema WASM, and extension-root containment.

If no compatible existing source is found, CLSP checks the selected package manager's global root and, when automatic installation is enabled, installs the exact `@prisma/language-server@31.11.0` package. A custom entry can be configured directly:

```toml
[lsp.prisma]
executable = "C:/tools/prisma/dist/language-server/bin.js"
```

CLSP starts the server with `--stdio` for `.prisma` files and sends no extension-private settings or Prisma-specific initialization options. It answers each standard `workspace/configuration` item with an empty object so diagnostics remain enabled. Prisma omits the optional diagnostic version, so CLSP correlates a push only to the current version of an already-open document; an explicit server version always wins. The nearest `schema.prisma`, `prisma/schema.prisma`, or `prisma` directory selects the root, otherwise the workspace root is used. `package.json` does not disable Prisma detection. Prisma configuration files may execute project code, so use the server only in trusted workspaces.

### Python

Pyright requires Node.js 14 or newer. CLSP first checks the selected project's `node_modules/.bin`, `[lsp.pyright].executable`, and `PATH`. It then scans only the standard Stable/Insiders directories for the official `ms-pyright.pyright` extension and validates its identity, version, bundled `dist/server.js`, and extension-root containment.

If no compatible existing source is found, CLSP checks the selected package manager's global root and, when automatic installation is enabled, installs the exact `pyright@1.1.411` package. CLSP starts `pyright-langserver --stdio` and sends no extension-private settings or Pyright-specific initialization options.

The nearest `pyproject.toml`, `setup.py`, `setup.cfg`, `requirements.txt`, `Pipfile`, or `pyrightconfig.json` selects the root, otherwise the workspace root is used. The official extension disables its own editor client when Pylance is installed, but CLSP's server process and `lsp_diagnostics` remain independent from VS Code Problems.

### Svelte

Svelte Language Server requires Node.js 18 or newer. CLSP starts `svelteserver --stdio` for `.svelte` files and sends no extension-private initialization options. It first checks project-local, explicit, and `PATH` candidates, then validates the embedded `node_modules/svelte-language-server/bin/server.js` and both manifests in the official `svelte.svelte-vscode` Stable/Insiders extension. If those sources are unavailable, CLSP checks the selected package manager's global root and, when automatic installation is enabled, installs the exact `svelte-language-server@0.18.4` package with `typescript@5.9.2`.

The nearest `package-lock.json`, `bun.lockb`, `bun.lock`, `pnpm-lock.yaml`, or `yarn.lock` selects the root, otherwise the workspace root is used. Configure a non-standard entry directly:

```toml
[lsp.svelte]
executable = "C:/tools/svelte-language-server/bin/server.js"
```

The official Svelte extension independently publishes VS Code Problems; CLSP does not install or manage the extension or its private settings. Svelte configuration and preprocessors can execute project code, so use the server only in trusted workspaces.

### Ruby

Ruby LSP requires Ruby 3.0 or newer. CLSP checks project-local, explicit, and `PATH` `ruby-lsp` candidates in that order and validates `ruby-lsp --version` against `>=0.26.10, <0.27.0`. If no compatible candidate exists and automatic installation is enabled, CLSP requires RubyGems and runs exactly:

```powershell
gem install ruby-lsp --version 0.26.10 --no-document
```

The nearest `Gemfile` selects the root; otherwise CLSP uses the workspace root. `BUNDLE_GEMFILE`, Bundler path/group settings, RubyGems paths, and `RUBYGEMS_GEMDEPS` are preserved for the child process and included in resolution identity. CLSP sends no Shopify extension-private initialization options and gives the first composed-bundle startup the existing 300-second initialize budget. The official `Shopify.ruby-lsp` VS Code extension is an independent client of the external gem and can publish Problems through the IDE bridge; CLSP does not scan its extension directory or own `.ruby-lsp` bundles. Use only trusted Ruby projects because Gemfiles and Bundler hooks can execute code.

### C#

CLSP uses the existing `dotnet` CLI and the pinned global tool:

```powershell
dotnet tool install --global roslyn-language-server --version 5.9.0-1.26303.1
```

If the global tool already exists at another version, CLSP can update it to the pinned version and allows a downgrade when needed.

Resolution verifies both the global tool listing and the executable shim/version before accepting the server.

CLSP does not install the .NET SDK itself.

### F#

FsAutoComplete 0.83.0 requires a compatible local .NET SDK/runtime; CLSP does not install .NET. After normal project, explicit, and `PATH` candidates, CLSP scans the standard Stable/Insiders directories for the official `Ionide.Ionide-fsharp` `7.31.x` extension. It accepts only a bounded `bin/net*/fsautocomplete.dll` layout with the official manifest and runtime files, then verifies the actual server version by running the DLL through `dotnet`.

If no compatible extension server exists, CLSP checks and, when enabled, installs or updates the exact global tool:

```powershell
dotnet tool install --global fsautocomplete --version 0.83.0
```

For a custom extension directory, configure either its official DLL or a compatible shim explicitly:

```toml
[lsp.fsharp]
executable = "C:/tools/ionide/bin/net8.0/fsautocomplete.dll"
```

CLSP sends `AutomaticWorkspaceInit = true` and passes a root-specific `--state-directory` below `%LOCALAPPDATA%\clsp\state\workspaces`, so FsAutoComplete state is not written into the project. `.fs`, `.fsi`, `.fsx`, and `.fsscript` files use the nearest `*.slnx`, `*.sln`, `*.fsproj`, or `global.json` root. Start FsAutoComplete only in trusted projects because MSBuild targets can execute project code.

### Gleam

Gleam's Language Server ships in the Gleam compiler. Install a compatible Gleam `1.x` compiler and expose `gleam` through `PATH`, or configure it explicitly:

```toml
[lsp.gleam]
executable = "C:/tools/gleam/gleam.exe"
```

The current validation baseline is Gleam `1.18.1`. The official Windows installer can be invoked manually with `winget install --id Gleam.Gleam`; CLSP never invokes winget or installs Gleam, Erlang/OTP, or editor extensions.

CLSP starts `gleam lsp` for `.gleam` files using the nearest `gleam.toml` directory, with the workspace root as the fallback. The official `Gleam.gleam` VS Code extension is a separate client of the same external compiler: it resolves `gleam` through `gleam.path` or `PATH` and does not carry a server binary for CLSP to reuse. Its Problems remain available through the existing IDE bridge.

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

### Swift / SourceKit-LSP

SourceKit-LSP is supplied by the Swift toolchain or Xcode; CLSP does not download or install either one. The registry accepts Swift toolchain version `>=5.9.0`, because SourceKit-LSP has no stable independent semantic-version command. Candidate validation runs `sourcekit-lsp --help`, then reads the Swift version from `swift --version` (including toolchains whose output also contains a `swift-driver` version).

Swift files use the nearest `Package.swift`, `*.xcodeproj`, `*.xcworkspace`, `compile_commands.json`, or `compile_flags.txt` marker. Discovery checks project-local, explicit, and `PATH` candidates first; on macOS it then tries `xcrun --find sourcekit-lsp`. Configure a non-standard installation explicitly:

```toml
[lsp.sourcekit-lsp]
executable = "C:/Swift/usr/bin/sourcekit-lsp.exe"
```

For `didOpen`, CLSP sends the protocol language IDs `swift`, `objective-c`, and `objective-cpp` for the matching extensions; the registry's single `language_id` is only the default metadata value.

The official [`swiftlang.swift-vscode`](https://marketplace.visualstudio.com/items?itemName=swiftlang.swift-vscode) extension is an independent VS Code client. CLSP does not scan its private bundle or copy its settings. SwiftPM/Xcode indexing and the first build may be slow; only use SourceKit-LSP in trusted workspaces.

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
- Go: `.go`; any ancestor `go.work` takes priority over the nearest `go.mod` or `go.sum`
- Python: `.py` or `.pyi` below the nearest `pyproject.toml`, `setup.py`, `setup.cfg`, `requirements.txt`, `Pipfile`, or `pyrightconfig.json`, with workspace-root fallback
- TypeScript/JavaScript: JS/TS extensions plus `package.json`, `tsconfig.json`, or `jsconfig.json`
- C/C++: C-family extensions plus `compile_commands.json`, `CMakeLists.txt`, or `.clangd`
- Swift/Objective-C: `.swift`, `.objc`, or `.objcpp` plus `Package.swift`, Xcode project/workspace directories, or a compilation database
- Dart: `.dart` plus `pubspec.yaml` or `analysis_options.yaml`
- Deno: `.ts`, `.tsx`, `.js`, `.jsx`, or `.mjs` below `deno.json` or `deno.jsonc`; this takes precedence over the TypeScript server within that root
- Elixir: `.ex` or `.exs` below the nearest `mix.exs` or `mix.lock`
- F#: `.fs`, `.fsi`, `.fsx`, or `.fsscript` below the nearest solution, `*.fsproj`, or `global.json`
- Gleam: `.gleam` below the nearest `gleam.toml`, with workspace-root fallback
- PHP: `.php` below the nearest `composer.json`, `composer.lock`, or `.php-version`, with workspace-root fallback
- OCaml: `.ml` or `.mli` below the nearest `dune-project`, `dune-workspace`, `.merlin`, or `opam`, with workspace-root fallback
- Oxlint: JS/TS and supported framework files below the nearest Oxlint config, package-manager lockfile, or `package.json`, with workspace-root fallback

The registry is deliberately bounded rather than accepting arbitrary server recipes from workspace configuration.

## Installation State

CLSP keeps runtime state outside the project under `%LOCALAPPDATA%\clsp`.

Per-workspace resolution data includes the executable path, version output, and source (`project-local`, `explicit`, `path`, `vscode-extension`, or `installed`).

For broader runtime/state details, see [Architecture](architecture.md).
