# Troubleshooting

Start with:

```powershell
clsp status --workspace .
```

For a live terminal view:

```powershell
clsp tui --workspace .
```

The sections below cover the failures most likely to appear during setup or normal use.

## `clsp setup` says CLSP is not on PATH

`setup` verifies that the running executable is the same CLSP resolved from `PATH`.

Check:

```powershell
where.exe clsp
clsp --version
```

If you recently upgraded the npm package, open a new terminal so the current environment sees the new global bin location.

A normal installation is:

```powershell
npm install -g @dslzl/clsp
clsp setup --workspace .
```

## `code` is not available on PATH

`clsp setup` installs the bundled VSIX through the local VS Code CLI.

Check:

```powershell
where.exe code
code --version
```

If `code` is missing, enable/install the VS Code shell command, then open a new terminal and rerun setup.

## VS Code extension installation fails

`setup` expects the bundled `clsp-ide.vsix` to live next to the installed CLSP executable and verifies that the bundled extension version matches CLSP.

If the package installation is incomplete, reinstall CLSP:

```powershell
npm uninstall -g @dslzl/clsp
npm install -g @dslzl/clsp
clsp setup --workspace .
```

Do not install a random VSIX from another CLSP version.

## Codex hooks are configured but CLSP appears inactive

After `clsp setup`, run:

```text
/hooks
```

inside Codex and review/trust the project hooks.

Then reload VS Code and start a fresh Codex session.

## `protocol_mismatch`

This usually means old and new CLSP components are still running at the same time.

Restart:

- Codex
- VS Code
- any existing CLSP processes

Then open a fresh integrated terminal and try again.

## `ide_diagnostics` is unavailable

The IDE tool depends on a live eligible VS Code bridge.

Check that:

- the workspace is trusted
- you are using VS Code Desktop
- the window is local, not Remote
- the workspace is not virtual
- `clsp setup --workspace .` has been run for this project
- VS Code has been reloaded since setup

Also inspect the VS Code Output panel and select:

```text
CLSP IDE Bridge
```

for bridge startup messages.

## Multiple VS Code windows cause an ambiguous IDE session

CLSP refuses to guess when multiple live VS Code windows match the same workspace and the Codex process has no reliable session binding.

The preferred fix is:

1. focus the intended VS Code window
2. open a **new integrated terminal** in that window
3. start Codex from that terminal

You can also close the duplicate workspace window.

## Editor context does not match the current VS Code window

The VS Code bridge injects its session ID into newly created integrated terminals.

An old terminal can predate the current bridge/session.

Open a fresh integrated terminal from the intended VS Code window and restart Codex there.

## Selection text is missing

Selection sharing may be disabled.

Run this VS Code command:

```text
CLSP: Toggle Selection Sharing
```

Also note:

- only the primary selection is shared
- empty selections have no text to share
- selections larger than 8 KiB are intentionally omitted

## VS Code Problems is empty

An empty result means VS Code currently has no matching structured diagnostics.

It does **not** mean:

- the project compiles
- tests pass
- Cargo/npm/dotnet commands succeeded
- a language extension has no Output Channel errors

`ide_diagnostics` reads VS Code Problems, not terminal/build logs.

## `lsp_diagnostics` works but `ide_diagnostics` does not

That can be expected.

`lsp_diagnostics` uses CLSP-managed Language Servers.

`ide_diagnostics` requires a live VS Code bridge and reuses diagnostics already published by VS Code extensions.

The two paths are deliberately independent.

## `ide_diagnostics` works but a CLSP Language Server does not

The VS Code extension and CLSP may be using different language-server sources.

For example, VS Code may already have a language extension producing Problems even when CLSP cannot resolve its own server.

Check:

```powershell
clsp status --workspace .
```

Then see [Language Servers](language-servers.md) for the relevant discovery/install path.

## A server is missing and `auto_install = false`

This is expected behavior.

Either install a compatible server yourself, configure an explicit executable, or re-enable installation:

```toml
auto_install = true
```

Explicit example:

```toml
[lsp.pyright]
executable = "C:/tools/pyright-langserver.cmd"
```

## npm Language Server install fails even though another package manager exists

CLSP probes package managers in this order:

```text
bun → pnpm → npm
```

The first working manager is selected for that resolution attempt.

If the selected manager later fails during its global-root query or installation, CLSP reports the error instead of silently changing package managers.

Fix the selected manager or temporarily remove it from the environment so the next probe chooses another one.

## gopls cannot be resolved or uses the wrong root

Check the Go toolchain and server that CLSP can see:

```powershell
where.exe go
go version
go env GOBIN GOPATH GOTOOLCHAIN
where.exe gopls
gopls version
```

CLSP reuses any compatible `gopls` and otherwise runs `go install golang.org/x/tools/gopls@v0.23.0` without overriding `GOBIN`. The pinned server declares Go 1.26; update Go or allow Go's own toolchain selection if installation reports an older toolchain.

For root selection, any `go.work` between the file and workspace root wins over a nested `go.mod` or `go.sum`. If no marker exists, CLSP uses the workspace root. The official `golang.Go` extension resolves the same external server independently; installing the extension alone does not provide CLSP with a bundled copy.

## Haskell Language Server cannot be resolved

CLSP does not install GHCup, GHC, Cabal, Stack, or HLS. Check the toolchain and wrapper visible to the current process:

```powershell
where.exe ghcup
where.exe ghc
where.exe cabal
where.exe stack
where.exe haskell-language-server-wrapper
ghc --numeric-version
haskell-language-server-wrapper --numeric-version
haskell-language-server-wrapper --probe-tools
code --list-extensions --show-versions | Select-String haskell.haskell
```

HLS must include a binary compatible with the project's GHC. The registry accepts HLS `2.x` and the current validation baseline is HLS `2.14.0.0` with GHC `9.12.4`. If the wrapper is outside `PATH`, configure it directly:

```toml
[lsp.hls]
executable = "C:/tools/ghcup/bin/haskell-language-server-wrapper.exe"
```

The nearest `stack.yaml`, `cabal.project`, `hie.yaml`, or `*.cabal` selects the root; otherwise CLSP uses the workspace root. The official `haskell.haskell` extension resolves external HLS versions independently and may place them in configurable globalStorage that CLSP does not scan. Configure the extension for `PATH` or give both clients explicit paths when they must share the same toolchain. Run probes and HLS only in trusted projects because cradle/build configuration may execute code.

## JDTLS cannot be resolved or Java files have no CLSP client

JDTLS requires Java 21+. Check the runtime, standalone launcher, and official VS Code extension visible to the current process:

```powershell
where.exe java
java -version
where.exe jdtls
jdtls --help
code --list-extensions --show-versions | Select-String redhat.java
```

CLSP checks compatible project, explicit, and `PATH` launchers first. It then scans only the standard Stable/Insiders extension directories for official `redhat.java` installs and validates their manifest, launcher/core JARs, platform configuration, and embedded or system Java runtime. It never downloads Java or JDTLS.

Configure a custom standalone launcher explicitly when needed:

```toml
[lsp.jdtls]
executable = "C:/tools/jdtls/bin/jdtls.bat"
```

CLSP deliberately skips loose `.java` files. Add a recognized Gradle marker (`settings.gradle[.kts]`, `gradlew[.bat]`, or `build.gradle[.kts]`), Maven `pom.xml`, or Eclipse `.project` / `.classpath`. Maven parent roots are selected only when `<modules>` declares the child. Use only trusted Maven/Gradle projects because imports may execute build logic.

## C# server cannot be resolved

CLSP does not install the .NET SDK.

Check that `dotnet` is available:

```powershell
where.exe dotnet
dotnet --info
dotnet tool list --global
```

CLSP expects the pinned `roslyn-language-server` global tool version defined in the registry and verifies the global-tool entry plus executable shim.

If your global tool state is broken, repair it with `dotnet tool` and rerun the CLSP operation.

## F# server cannot be resolved

CLSP requires a compatible local .NET SDK/runtime and accepts FsAutoComplete `0.83.0`. Check both the global tool and the official Ionide extension:

```powershell
dotnet --info
dotnet tool list --global fsautocomplete
fsautocomplete --version
code --list-extensions --show-versions | Select-String Ionide.Ionide-fsharp
```

The supported standard extension layout is the official `Ionide.Ionide-fsharp` `7.31.x` `bin/net*/fsautocomplete.dll` plus its runtime files. For a custom extensions directory, point CLSP at that DLL explicitly:

```toml
[lsp.fsharp]
executable = "C:/tools/ionide/bin/net8.0/fsautocomplete.dll"
```

If neither source is usable and automatic installation is enabled, CLSP installs or updates the exact global tool through `dotnet`. Run FsAutoComplete only in trusted projects because MSBuild targets can execute project code.

## Dart server cannot be resolved

CLSP does not install the Dart or Flutter SDK. Check that a compatible Dart SDK is available:

```powershell
where.exe dart
dart --version
dart language-server --protocol=lsp --help
```

CLSP requires Dart SDK 2.12.0 or newer. If `dart` is not on `PATH`, configure the SDK executable directly:

```toml
[lsp.dart]
executable = "C:/tools/dart-sdk/bin/dart.exe"
```

CLSP does not read VS Code's `dart.sdkPath`; Dart Code and CLSP resolve their SDKs independently.

## Deno server cannot be resolved

CLSP does not install Deno. Check that Deno 1.40.0 or newer is available:

```powershell
where.exe deno
deno --version
deno lsp --help
```

If `deno` is not on `PATH`, configure it directly:

```toml
[lsp.deno]
executable = "C:/tools/deno/deno.exe"
```

Deno selection also requires a `deno.json` or `deno.jsonc` between the file and workspace root. Within that Deno root, CLSP selects Deno instead of the TypeScript Language Server.

The official `denoland.vscode-deno` extension and CLSP resolve the Deno CLI independently. Installing the extension alone does not provide CLSP with a server; configure the extension's `deno.path` and CLSP's `[lsp.deno].executable` separately when `PATH` is insufficient.

## Gleam server cannot be resolved

CLSP does not install Gleam or Erlang/OTP. Check that a compatible Gleam `1.x` compiler is available:

```powershell
where.exe gleam
gleam --version
gleam lsp --help
code --list-extensions --show-versions | Select-String Gleam.gleam
```

If `gleam` is not on `PATH`, configure it directly:

```toml
[lsp.gleam]
executable = "C:/tools/gleam/gleam.exe"
```

The official `Gleam.gleam` extension and CLSP resolve the compiler independently. Installing the extension alone does not provide a bundled server; configure the extension's `gleam.path` and CLSP's `[lsp.gleam].executable` separately when `PATH` is insufficient. A nearby `gleam.toml` selects the project root; otherwise CLSP uses the workspace root and the language server may provide only degraded features.

## ElixirLS cannot be resolved or takes a long time to start

CLSP does not install Erlang/OTP, Elixir, or ElixirLS. Check the local runtime first:

```powershell
where.exe erl
where.exe elixir
where.exe mix
elixir --version
```

Install the official `JakeBecker.elixir-ls` extension in a standard VS Code Stable/Insiders extension directory, or point CLSP at an official ElixirLS `0.31.x` release launcher:

```toml
[lsp.elixir-ls]
executable = "C:/tools/elixir-ls/language_server.bat"
```

The launcher must have its official sibling `VERSION` file. A custom VS Code `--extensions-dir`, missing/incompatible `VERSION`, or absent `elixir` runtime is intentionally not guessed.

The first launch can take several minutes while the official script prepares its Mix cache. Later requests retain the normal timeout. ElixirLS compiles Mix project and dependency code, so do not start it in an untrusted workspace.

## ESLint cannot be resolved or returns no diagnostics

CLSP does not install Node.js, the ESLint server, or the project's ESLint package. Check the prerequisites from the project root:

```powershell
node --version
Test-Path node_modules/eslint/package.json
code --list-extensions --show-versions | Select-String dbaeumer.vscode-eslint
```

The standard Stable/Insiders extension must contain `server/out/eslintServer.js`, and its official manifest version must satisfy `>=3.0.34, <3.1.0`. For a custom extensions directory, configure that server file explicitly:

```toml
[lsp.eslint]
executable = "C:/tools/vscode-eslint/server/out/eslintServer.js"
```

The selected project root must contain its own compatible `node_modules/eslint/package.json`; a global ESLint installation is intentionally ignored. If the server starts but reports no issues, confirm that the file is covered by the project's ESLint configuration and that a rule is enabled.

Only run ESLint in a trusted project. Configurations and plugins may execute project code.

## clangd is not found

CLSP checks:

1. project-local candidates
2. an explicit `[lsp.clangd].executable`
3. `PATH`
4. compatible clangd managed by the VS Code clangd extension
5. CLSP's cached artifact
6. a verified managed download, when `auto_install = true`

For custom VS Code user-data layouts, prefer an explicit path:

```toml
[lsp.clangd]
executable = "C:/LLVM/bin/clangd.exe"
```

## Codex asks to save files before `apply_patch`

This is intentional.

When CLSP can route the edit to VS Code, it checks the target documents before the patch. If a target is dirty, VS Code asks whether it should save the file first.

Choose **Save and continue** only when you want the current unsaved buffer persisted before Codex edits the file.

Cancelling prevents CLSP from treating the dirty file as safe to overwrite.

## A post-edit diff did not open

Diff review is best-effort.

Common reasons include:

- no live IDE route
- the relevant review baseline was unavailable
- too many files changed
- a temporary file for the comparison could not be prepared
- VS Code rejected the diff action

The code edit itself is not rolled back just because a review diff cannot open.

## Where CLSP keeps state

CLSP runtime state is under:

```text
%LOCALAPPDATA%\clsp
```

User configuration is under:

```text
%APPDATA%\clsp\config.toml
```

Project configuration is:

```text
.clsp.toml
```

Codex integration is written under the project's:

```text
.codex\
```

## Still stuck?

Capture:

```powershell
clsp status --workspace .
clsp --version
code --version
```

Then include the relevant Language Server/toolchain version and the `CLSP IDE Bridge` Output Channel message when opening an issue.
