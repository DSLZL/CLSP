# Contributing to CLSP

Thanks for contributing.

CLSP is a Windows-first Rust project with a small TypeScript VS Code bridge. The main repository check intentionally validates both halves together.

## Development Environment

Match CI as closely as practical.

Required:

- Windows x64
- Rust **1.88+**
- `x86_64-pc-windows-msvc` target
- Visual Studio Build Tools
- Windows SDK
- Node.js
- Bun **1.3.14+**
- VS Code CLI (`code`)

CI currently uses Node.js 24 and Bun 1.3.14.

Add the Rust target if needed:

```powershell
rustup target add x86_64-pc-windows-msvc
```

Install VS Code bridge dependencies:

```powershell
bun ci --cwd vscode
```

## Run the Full Check

Before submitting a change:

```powershell
bun run check
```

The root check runs:

```text
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
bun run --cwd vscode check
bun run --cwd vscode test
```

Clean generated output with:

```powershell
bun run clean
```

## Project Layout

```text
.
├── registry/
│   └── servers.toml       Built-in Language Server definitions
├── src/
│   ├── broker.rs          Per-workspace coordinator and server lifecycle
│   ├── cli.rs             CLI surface
│   ├── config.rs          Strict configuration model and defaults
│   ├── hook.rs            Codex lifecycle hooks
│   ├── ide.rs             Broker ↔ VS Code relay host
│   ├── installer.rs       Server discovery / reuse / installation
│   ├── ipc.rs             Windows local IPC and runtime metadata
│   ├── lsp.rs             LSP client/process handling
│   ├── mcp.rs             MCP tool adapter
│   ├── protocol.rs        Internal request/response types and limits
│   ├── registry.rs        Built-in registry validation
│   ├── setup.rs           Codex + VS Code project setup
│   ├── tui.rs             ratatui status UI
│   └── workspace.rs       Workspace detection and path safety
├── vscode/
│   └── src/
│       ├── extension.ts   VS Code bridge implementation
│       └── protocol.ts    Bounded bridge protocol validation
├── docs/
└── .github/workflows/
```

## Rust Changes

Format:

```powershell
cargo fmt --all
```

Test:

```powershell
cargo test --all-targets --locked
```

Lint:

```powershell
cargo clippy --all-targets --locked -- -D warnings
```

For release-style local compilation:

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
```

The release profile is size-focused, so avoid changing release-profile settings casually; they affect the shipped executable.

## VS Code Bridge Changes

From the repository root:

```powershell
bun run --cwd vscode check
bun run --cwd vscode test
```

Build a local VSIX:

```powershell
bun run --cwd vscode package
```

The bridge should remain thin. Prefer VS Code public APIs and keep Language Server management in the Rust Broker.

Do not move full editor-buffer mirroring, Language Server lifecycle, or package installation into the extension.

## Changing the MCP Surface

The MCP implementation lives in `src/mcp.rs`.

Keep tool behavior:

- workspace-bounded
- input-bounded
- structured
- read-only unless the project explicitly changes that contract

If a tool adds a new request shape, update the internal protocol and tests together.

## Changing Configuration

The configuration model uses strict deserialization with unknown-field rejection.

When adding/removing a setting:

1. update `src/config.rs`
2. add/adjust validation
3. add tests for default, valid, and invalid cases
4. update `docs/configuration.md`
5. update README only if the setting belongs in the normal user path

Avoid adding knobs for internal implementation details unless users genuinely need control over them.

## Changing Language Server Support

The built-in registry is intentionally closed and validated.

A server change may require updates in both:

```text
registry/servers.toml
src/registry.rs
```

Adding a brand-new server is not just a TOML edit: `src/registry.rs` validates the approved server IDs/extensions.

For a registry change:

1. choose a stable server ID
2. define extensions and project markers
3. pin/limit compatible versions
4. use one of the supported installation recipe types
5. keep command/program names safe basenames
6. add registry tests
7. add resolver/installer tests when discovery behavior changes
8. update `docs/language-servers.md`
9. update the README support table

Installation should preserve the project policy:

> reuse a compatible local environment before installing another copy.

## Installer Changes

Installer behavior is security-sensitive because it launches local tools and, for clangd, handles a downloaded archive.

Keep the existing constraints in mind:

- bounded command execution
- sanitized child-process environment
- exact package/version checks
- no silent package-manager fallback after a manager has been selected
- HTTPS-only pinned clangd download
- fixed SHA-256 verification
- bounded archive size/entry count
- safe extraction paths
- no workspace writes for managed clangd artifacts

Add focused tests for any changed resolver priority or install command.

## IDE Integration Changes

The VS Code bridge is a local trust boundary.

When changing editor-context or Problems behavior:

- keep the payload bounded
- keep workspace path checks
- preserve the remote/untrusted/virtual-workspace guards
- do not guess among ambiguous VS Code windows
- avoid persisting selection/diagnostic content unless the feature explicitly requires it
- update `docs/ide-integration.md`

For edit-protection behavior, test dirty documents, cancellation, and routing failure separately.

## Documentation Style

README is the landing page, not the complete specification.

Put:

- first-run instructions and core capabilities in `README.md`
- exact Language Server behavior in `docs/language-servers.md`
- full config reference in `docs/configuration.md`
- implementation/runtime details in `docs/architecture.md`
- edge cases in `docs/troubleshooting.md`

Prefer short paragraphs, concrete examples, and tables over dense implementation prose.

## Pull Request Checklist

Before opening a PR:

- [ ] `bun ci --cwd vscode` succeeds
- [ ] `bun run check` succeeds
- [ ] behavior changes have tests
- [ ] user-visible behavior is documented
- [ ] new config fields have validation and docs
- [ ] Language Server changes update the registry docs
- [ ] no generated `dist/`, `target/`, `vscode/out/`, or local VSIX artifacts are committed unintentionally

## Releases

Releases are produced by `.github/workflows/release.yml`.

The workflow expects:

1. an **annotated** tag named `vX.Y.Z`
2. non-empty release notes in the annotated tag body
3. the release version to match the Rust package, root npm package, and VS Code package versions

The workflow then:

- runs the repository checks
- builds the Windows x64 executable
- packages the VS Code extension
- creates the Windows ZIP
- creates/verifies the npm tarball
- writes SHA-256 checksums
- creates a GitHub Release
- publishes `@dslzl/clsp` through npm trusted publishing

Release publishing should stay in CI rather than being recreated as an ad-hoc local publishing script.

## License

By contributing, you agree that your contribution is distributed under the repository's GNU Affero General Public License v3.0 only (`AGPL-3.0-only`).
