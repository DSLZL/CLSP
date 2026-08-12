# CLSP

**为 Windows 上的 Codex CLI 提供 LSP 与实时 VS Code 诊断能力。**

CLSP 让 Codex CLI 获得接近 IDE 的语言感知能力，包括定义跳转、引用查找、Hover 信息、诊断，以及实时的 VS Code Problems。

它以轻量级的 workspace Broker 运行，优先复用你本机已有且版本兼容的 Language Server；缺失时，也可以按内置规则自动安装支持的服务器。

> 当前正式支持：**Windows x64 + VS Code Desktop**。

[English](README.md) | **简体中文**

## 快速开始

### 环境要求

你需要：

- Windows x64
- Node.js / npm
- VS Code Desktop
- 已加入 `PATH` 的 `code` 命令
- Codex CLI

### 安装

```powershell
npm install -g @dslzl/clsp
```

然后进入需要配合 Codex 使用的项目目录：

```powershell
clsp setup --workspace .
```

`setup` 会安装随 CLSP 提供的 VS Code adapter，并把 CLSP 所需的 MCP 与 hooks 配置合并到项目的 `.codex` 目录中。

完成后：

1. 在项目中启动 Codex。
2. 运行 `/hooks`，检查并信任项目 hooks。
3. Reload VS Code。
4. 正常使用 Codex。

## CLSP 能带来什么

- **代码导航** — 通过 LSP 获取 hover、definition 和 references。
- **语言诊断** — 让 Codex 获取 CLSP 管理的 Language Server 诊断结果。
- **实时 IDE 诊断** — 直接复用 VS Code 当前的 Problems，包括 VS Code 已发布的未保存文档诊断。
- **修改后检查** — 对比 Codex 修改前后的诊断，找出本次编辑新增的错误。
- **修改审阅** — 在 VS Code 中打开原生 `Before Codex ↔ After Codex` Diff。
- **复用本地环境** — 优先使用项目或系统中已经存在的兼容 Language Server。
- **状态查看** — 通过 CLI 或 TUI 检查 Broker、Language Server、IDE Bridge 以及降级状态。

## 支持的语言

| 语言 | Language Server | 缺失时的处理方式 |
| --- | --- | --- |
| Astro | Astro Language Server | npm 兼容包管理器 |
| Bash / Shell | Bash Language Server | npm 兼容包管理器 |
| C# | Roslyn Language Server | `dotnet tool` |
| Clojure | clojure-lsp | 手动安装 |
| C / C++ | clangd | 复用本地 / VS Code 版本，否则由 CLSP 校验后下载 |
| Dart | Dart Language Server | 手动安装 Dart/Flutter SDK |
| Deno | Deno Language Server | 手动安装 Deno CLI |
| Elixir | ElixirLS | 复用官方 VS Code release 或手动安装的 ElixirLS release |
| ESLint | ESLint Language Server | 复用官方 VS Code 扩展；项目需本地安装 ESLint |
| F# | FsAutoComplete | 优先复用官方 Ionide VS Code 扩展，否则使用 `dotnet tool` |
| Gleam | Gleam Language Server | 手动安装 Gleam 编译器 |
| Go | gopls | `go install` |
| Haskell | Haskell Language Server | 手动安装 GHCup/HLS 工具链 |
| Java | Eclipse JDT Language Server | 复用本地 JDTLS 或官方 `redhat.java` 扩展 |
| Julia | Julia Language Server | 复用当前 Julia 环境或官方 `julialang.language-julia` 扩展 |
| Kotlin | Kotlin Language Server | 复用独立服务端或官方 `JetBrains.kotlin-server` 扩展 |
| Lua | Lua Language Server | 复用独立 LuaLS 或官方 `sumneko.lua` 扩展 |
| OCaml | OCaml Language Server | 手动安装到 opam switch |
| Oxlint | Oxlint Language Server | 项目本地包或手动配置可执行文件 |
| PHP | Intelephense | 复用官方 VS Code 扩展或 npm 兼容包管理器 |
| Prisma | Prisma Language Server | 复用官方 VS Code 扩展或 npm 兼容包管理器 |
| Python | Pyright | 复用官方 VS Code 扩展或 npm 兼容包管理器 |
| Ruby | Ruby LSP | 复用本地/gem 服务端或 `gem install` |
| Rust | rust-analyzer | `rustup component add` |
| TypeScript / JavaScript | TypeScript Language Server | npm 兼容包管理器 |
| YAML | YAML Language Server | npm 兼容包管理器 |

CLSP 的基本原则很简单：

> **能复用就复用，只有确实缺失时才安装。**

Dart 支持复用 PATH 或 `[lsp.dart].executable` 中的 `dart`；CLSP 不会安装 Dart 或 Flutter SDK。

Deno 支持只会在 `deno.json` 或 `deno.jsonc` 所在目录树中启用。CLSP 复用 PATH 或 `[lsp.deno].executable` 中的 `deno`，不会安装 Deno，也不会把官方 VS Code 扩展当作内置服务器来源。

Elixir 支持 `.ex` 与 `.exs`，根目录取最近的 `mix.exs` 或 `mix.lock`。本机必须已有 Erlang/OTP 和 Elixir；CLSP 会复用官方 `JakeBecker.elixir-ls` VS Code release，或显式配置的 ElixirLS `0.31.x` launcher，但不会安装 runtime 或 server。ElixirLS 会编译项目及依赖代码，因此只应在可信 Mix 项目中启动。

ESLint 支持 `.ts`、`.tsx`、`.js`、`.jsx`、`.mjs`、`.cjs`、`.mts`、`.cts` 与 `.vue`。本机必须已有 Node.js，项目根必须本地安装 `eslint`，并复用标准 VS Code 扩展目录中的官方 `dbaeumer.vscode-eslint` `3.0.x` server 或显式路径；CLSP 不安装这些组件。ESLint 配置和插件会执行项目代码，因此只应在可信项目中启动。

F# 支持 `.fs`、`.fsi`、`.fsx` 与 `.fsscript`，根目录取最近的解决方案、F# 项目或 `global.json`。CLSP 会先复用官方 `Ionide.Ionide-fsharp` 扩展，再检查精确版本的全局 FsAutoComplete，并可通过已有的 .NET SDK 安装或更新该工具。MSBuild target 可能执行项目代码，因此只应在可信项目中启动。

Gleam 支持 `.gleam` 文件，根目录取最近的 `gleam.toml`，找不到时回退到 workspace 根。CLSP 复用 `PATH` 或 `[lsp.gleam].executable` 中兼容的 Gleam `1.x` 编译器并启动其内置 `gleam lsp`；不会安装 Gleam、Erlang/OTP，也不会扫描官方 `Gleam.gleam` 扩展来寻找内置服务器。该扩展使用同一个外部编译器，可通过现有 IDE Bridge 独立提供 VS Code Problems。

Go 支持 `.go` 文件。文件与 workspace 根之间只要存在 `go.work`，它就优先于最近的 `go.mod` 或 `go.sum`；CLSP 会复用兼容的 `gopls`，否则通过本机已有的 Go 工具链安装固定版本。官方 `golang.Go` VS Code 扩展是同一外部服务器的独立客户端，可通过现有 IDE Bridge 独立提供 Problems。

Haskell 支持 `.hs` 与 `.lhs` 文件，根目录取最近的 `stack.yaml`、`cabal.project`、`hie.yaml` 或 `*.cabal`，找不到时回退到 workspace 根。CLSP 复用 `PATH` 或 `[lsp.hls].executable` 中兼容的 HLS `2.x` wrapper，并启动 `haskell-language-server-wrapper --lsp`；不会安装或替用户选择 GHC/HLS 版本。官方 `haskell.haskell` 扩展是外部 HLS 的独立客户端，可通过现有 IDE Bridge 提供 Problems。项目 cradle/构建配置可能执行代码，因此只应在可信项目中启动 HLS。

Java 支持位于 Gradle、Maven 或 Eclipse 项目中的 `.java` 文件。CLSP 复用本地 `jdtls` launcher 或官方 `redhat.java` Stable/Insiders 扩展内的服务器，要求 Java 21+，并按项目根隔离 JDTLS data。没有项目标记的松散 Java 文件不会启动 CLSP JDTLS 客户端。CLSP 不安装这些组件；Maven/Gradle 导入可能执行构建逻辑，因此只应打开可信项目。

Julia 支持 `.jl` 文件，根目录取最近的 `Project.toml`、`Manifest.toml` 或包含 Julia 源文件的目录，找不到时回退到 workspace 根。CLSP 先复用 Julia 1.10+ 当前环境中的 LanguageServer.jl 5.x，再尝试配合 Julia 1.11+ 使用官方 `julialang.language-julia` Stable/Insiders 扩展环境。CLSP 不安装这些组件；JuliaLS 会加载 Julia 环境与包元数据，因此只应在可信项目中使用。

Kotlin 支持 Gradle 或 Maven 项目中的 `.kt` 与 `.kts` 文件。CLSP 会复用兼容的独立 Kotlin Language Server，或官方 `JetBrains.kotlin-server` Stable/Insiders 扩展内置的服务端与 JBR 25，并按项目根隔离索引。CLSP 不安装这些组件；Gradle/Maven 导入可能执行构建逻辑，因此只应在可信项目中使用。

Lua 支持 `.lua` 文件，根目录取最近的 OpenCode 兼容 Lua 配置标记，找不到时回退到 workspace 根。CLSP 先复用兼容的 LuaLS `3.x` 可执行文件，再尝试官方 `sumneko.lua` Stable/Insiders 扩展内置的完整服务端；两者都不会由 CLSP 安装。LuaLS 插件可能执行代码，因此只应使用可信的项目配置。

OCaml 支持 `.ml` 与 `.mli` 文件，根目录取最近的 `dune-project`、`dune-workspace`、`.merlin` 或 `opam`，找不到时回退到 workspace 根。CLSP 复用 `PATH` 或 `[lsp.ocaml-lsp].executable` 中兼容的 `ocamllsp`；不会安装 opam、OCaml、Dune、服务端或官方 `ocamllabs.ocaml-platform` 扩展。该扩展是同一外部服务端的独立客户端，可通过现有 IDE Bridge 提供 Problems。

PHP 支持 `.php` 文件，根目录取最近的 `composer.json`、`composer.lock` 或 `.php-version`，找不到时回退到 workspace 根。CLSP 依次复用项目本地、显式路径或 `PATH` 中兼容的 Intelephense、官方 `bmewburn.vscode-intelephense-client` Stable/Insiders 扩展内置服务端，以及兼容的全局包；启用自动安装时固定安装 `intelephense@1.18.5`。CLSP 只发送 `telemetry.enabled = false`，不会读取或管理 Intelephense 许可证文件。

Prisma 支持 `.prisma` 文件，根目录取最近的 `schema.prisma`、`prisma/schema.prisma` 或 `prisma` 目录，找不到时回退到 workspace 根。CLSP 依次复用项目本地、显式路径或 `PATH` 中兼容的服务端、官方 `Prisma.prisma` Stable/Insiders 扩展内置服务端，以及兼容的全局包；启用自动安装时固定安装 `@prisma/language-server@31.11.0`。本机需要 Node.js 20+；Prisma 配置可能执行项目代码，因此只应在可信 workspace 中使用。

Python 支持 `.py` 与 `.pyi` 文件，根目录取最近的 OpenCode 兼容 Python 项目标记，找不到时回退到 workspace 根。CLSP 依次复用项目本地、显式路径或 `PATH` 中兼容的 Pyright、官方 `ms-pyright.pyright` Stable/Insiders 扩展内置服务端，以及兼容的全局包；启用自动安装时固定安装 `pyright@1.1.411`。本机需要 Node.js 14+。

Ruby 支持 `.rb`、`.rake`、`.gemspec` 与 `.ru` 文件，根目录取最近的 `Gemfile`，找不到时回退到 workspace 根。本机需要 Ruby 3.0 或更新版本；CLSP 依次复用兼容的项目本地、显式路径或 `PATH` 中的 `ruby-lsp`，启用自动安装时只执行固定命令 `gem install ruby-lsp --version 0.26.10 --no-document`。官方 `Shopify.ruby-lsp` 扩展是独立的 VS Code 外部客户端，CLSP 不扫描其私有 bundle，也不管理 `.ruby-lsp` 内容。Bundler 和 Gemfile 配置可能执行项目代码，因此只应在可信项目中使用。

Oxlint 支持 `.ts`、`.tsx`、`.js`、`.jsx`、`.mjs`、`.cjs`、`.mts`、`.cts`、`.vue`、`.astro` 与 `.svelte`。CLSP 复用所选项目 `node_modules/.bin`、`PATH` 或 `[lsp.oxlint].executable` 中兼容的 Oxlint 1.x，并启动 `oxlint --lsp`；不会安装 npm 包或官方 `oxc.oxc-vscode` 扩展。该扩展会独立使用同一个项目工具，并可通过现有 IDE Bridge 提供 Problems。Oxlint 配置和 JavaScript 插件可能执行项目代码，因此只应在可信项目中使用。

如果你需要精确的版本范围、查找顺序和安装策略，请查看 [Language Servers](docs/language-servers.md)。

## VS Code 集成

随 CLSP 提供的 **CLSP IDE Bridge** 有意保持得很轻。

它不会重新实现一套 Language Server，而是通过 VS Code 的公开 API，把对 Codex 有用的实时编辑器状态提供给 CLSP，包括：

- 当前活动文件
- 文档版本和 dirty 状态
- 当前主 selection（启用 selection sharing 时）
- 当前 VS Code Problems
- 修改前对 dirty 文件的确认
- 修改后的原生 Diff

可以通过 VS Code Command Palette 切换 selection sharing：

```text
CLSP: Toggle Selection Sharing
```

关于窗口路由、数据限制、隐私行为以及编辑生命周期，请查看 [IDE Integration](docs/ide-integration.md)。

## MCP 工具

CLSP 提供 4 个只读 MCP 工具：

| Tool | 用途 |
| --- | --- |
| `lsp_query` | 查询 Hover、Definition 和 References |
| `lsp_diagnostics` | 获取 CLSP 管理的 Language Server 诊断 |
| `lsp_status` | 查看 Broker、Server、Hooks 和集成状态 |
| `ide_diagnostics` | 获取当前 VS Code Problems |

`lsp_diagnostics` 和 `ide_diagnostics` 是两条不同的数据路径：

- `lsp_diagnostics` 使用 CLSP 自己管理的 Language Server。
- `ide_diagnostics` 复用 VS Code 中已经发布的诊断。

## CLI

```text
clsp setup --workspace <path>
clsp status [--workspace <path>]
clsp tui [--workspace <path>]
```

`mcp`、`hook`、`broker` 和 `ide-host` 属于集成与运行时命令。

正常情况下，只需要执行 `clsp setup`，CLSP 就会完成 Codex 和 VS Code 所需的配置。

## 配置

大多数用户都不需要手动创建 `.clsp.toml`。

一个比较常见的用途，是关闭 Language Server 自动安装：

```toml
auto_install = false
```

CLSP 也支持用户级配置：

```text
%APPDATA%\clsp\config.toml
```

项目级 `.clsp.toml` 的配置优先级高于用户级配置。

完整配置项与默认值请查看 [Configuration](docs/configuration.md)。

## 平台支持

当前正式支持：

- Windows x64
- `x86_64-pc-windows-msvc`
- VS Code Desktop

当前 bundled VS Code Bridge 暂不支持：

- VS Code Remote
- 未信任工作区
- Virtual Workspaces
- 非 Desktop VS Code UI
- Windows GNU 作为发布目标

当 IDE Bridge 不可用时，CLSP 会尽可能保留独立的 LSP / MCP 能力，而不是阻塞 Codex 的普通使用。

## 文档

- [Language Servers](docs/language-servers.md) — Language Server 发现、版本、安装与覆盖配置
- [IDE Integration](docs/ide-integration.md) — VS Code Problems、Selection Sharing、修改检查与隐私行为
- [Configuration](docs/configuration.md) — `.clsp.toml`、用户级配置、默认值与限制
- [Troubleshooting](docs/troubleshooting.md) — 常见 setup、运行时、IDE 与 Language Server 问题
- [Architecture](docs/architecture.md) — Broker、MCP、Hooks、LSP、IPC 与生命周期
- [Contributing](CONTRIBUTING.md) — 本地开发、测试、Registry 修改与 Release 流程

## 查看状态

输出当前 Broker 状态：

```powershell
clsp status --workspace .
```

打开终端 TUI：

```powershell
clsp tui --workspace .
```

如果出现异常，建议先查看 [Troubleshooting](docs/troubleshooting.md)。

## 卸载

卸载 VS Code adapter：

```powershell
code --uninstall-extension clsp.clsp-ide
```

如果希望从项目中完全移除 CLSP，还需要删除 `clsp setup` 写入的 CLSP MCP 与 hooks 配置，然后重启 Codex 和 VS Code。

## 参与开发

开发说明请查看 [CONTRIBUTING.md](CONTRIBUTING.md)。

## License

GNU Affero General Public License v3.0 only (`AGPL-3.0-only`)
