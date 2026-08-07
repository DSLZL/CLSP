# CLSP

CLSP 是面向 Codex CLI 的 Windows LSP 与 VS Code companion。npm 包同时包含 `clsp.exe` 和很薄的 `clsp-ide.vsix`：Rust Broker 继续管理 LSP、MCP 和 Codex hooks，扩展只通过 VS Code 公共 API 按需读取当前编辑器与 Problems 状态。

目标平台是本地 `x86_64-pc-windows-msvc` 与 VS Code Desktop。远程窗口、Restricted Mode、虚拟工作区和 Windows GNU 不属于发布支持范围。

## 安装

需要 Windows x64、Node.js/npm，以及已加入 `PATH` 的 VS Code CLI。全局安装 CLSP：

```powershell
npm install -g @dslzl/clsp
```

然后在项目中运行：

```powershell
clsp setup --workspace .
```

`setup` 会验证当前 `PATH` 中的 `clsp`、同目录 VSIX 和本机 `Code.exe`，安装/更新扩展，并合并项目级 `.codex/config.toml` 与 `.codex/hooks.json`。它保留无关 MCP 和 hooks；检测到冲突的 `mcp_servers.clsp` 或 TOML 内联 hooks 时会在写入前停止。完成后请在 Codex `/hooks` 中检查并信任项目 hooks，然后 reload VS Code。

npm 包已同时包含 `clsp.exe` 和 `clsp-ide.vsix`，不需要手工下载 ZIP，也不需要从 Marketplace 获取 CLSP Bridge。VS Code 扩展仍然是读取实时编辑器内存状态所必需的部分，但由 `clsp setup` 本地安装。要复用 Astro、Bash、C# 或 Clojure 的 VS Code Problems，仍需安装对应的 `astro-build.astro-vscode`、`mads-hartmann.bash-ide-vscode`、`ms-dotnettools.csharp` 或 `betterthantomorrow.calva` 语言扩展。

## Language Server 安装

CLSP 先复用项目 `node_modules/.bin`、虚拟环境、项目 `bin`、显式路径或 `PATH` 中版本兼容的服务器。缺失且 `auto_install = true` 时，多数服务器只执行用户本机已有的安装命令并验证结果。唯一的内置下载例外是 Windows x86-64 clangd：CLSP 使用固定官方资源和 SHA-256，并将完成的版本放在用户级 `%LOCALAPPDATA%/clsp/artifacts`，不写工作区。

- npm 型服务器固定按 `bun` > `pnpm` > `npm` 探测，使用第一个版本探测成功的管理器执行全局、精确版本安装。选中后安装或 root 查询失败不会静默改用下一个管理器。
- Astro 与 TypeScript Language Server 会在同一命令中安装固定版本 TypeScript；Bash、Pyright 与 YAML Language Server 只安装自身。
- gopls 使用本地 `go install`，不覆盖 `GOBIN`；rust-analyzer 使用当前 workspace 对应的 `rustup component add rust-analyzer`。
- C# 需要本机已有 .NET 10 SDK。CLSP 通过 `dotnet tool` 复用或安装固定的 `roslyn-language-server@5.9.0-1.26303.1`，并核对全局工具清单、shim 与服务端版本；它不会安装 .NET SDK，也不会读取 C# 扩展的私有 `.roslyn` 目录。
- Clojure 使用 PATH 或显式路径中的 `clojure-lsp`，并需要项目适用的 Clojure 构建工具。Windows 可通过官方 `scoop-clojure` bucket 安装 `clojure-lsp` 和 Clojure CLI；CLSP 不调用 Scoop，也不读取 Calva 的私有下载目录。
- clangd 在上述本地来源之后，优先复用 VS Code Stable/Insiders 的 `llvm-vs-code-extensions.vscode-clangd` 已管理服务器，再复用 CLSP 用户级缓存；仍缺失时固定安装官方 `22.1.6` Windows ZIP。CLSP 不调用 winget、Chocolatey、Scoop 或其他系统包管理器。自定义 VS Code user-data-dir 请通过 `lsp.clangd.executable` 指定。

安装步骤退出成功后，CLSP 仍会重新发现 executable、校验包名/版本并完成 LSP initialize。npm 或 dotnet 全局清单、全局目录或 clangd 归档不完整时不会报告安装成功。npm/工具链网络行为由对应本地工具负责；clangd 下载沿用当前代理环境并强制 HTTPS、大小上限和固定摘要。

## 实时 IDE 能力

- 每个 VS Code 窗口生成一个临时 session ID，并只绑定该窗口新开的 integrated terminals。
- 每次 `UserPromptSubmit` 才读取当前活动文件、版本、dirty 状态和主 selection；不监听或缓存编辑器文本。
- selection 最多 8 KiB，完整 hook 输出最多 12 KiB，并明确标记为不可信工作区数据。
- `CLSP: Toggle Selection Sharing` 可停止分享选择文本，同时保留允许的活动文件元数据。
- `ide_diagnostics` 读取 VS Code Problems（`Ctrl+Shift+M`）当前内容，包括未保存文档的诊断；它不会伪装成 CLSP 的磁盘 LSP 诊断。
- `apply_patch` 前会在内存中记录目标文件的 Problems error 基线；编辑后通过同一 VS Code session 再读一次，只把新增 error 注入 Codex。这条自动路径会复用 rust-analyzer、Astro、Bash、C#、Clojure 等语言扩展已经发布的诊断，不会再启动同语言的 CLSP LSP。
- `apply_patch` 前会检查全部目标文档。存在 dirty buffer 时，VS Code 原生确认框只在用户选择 `Save and continue` 后保存这些目标。
- 编辑成功后最多打开五个 `Before Codex <-> After Codex` 原生 diff。关闭 diff 不会回滚编辑。

默认拒绝把 `.git/**`、`.env`、`.env.*`、PEM、KEY、P12 和 PFX 路径作为 IDE 上下文或 Problems 返回。可在项目 `.clsp.toml` 的 `[ide].denied_paths` 中整体替换该列表。

## MCP 工具

CLSP 向模型开放四个只读工具：

- `lsp_query`：hover、definition 和 references。
- `lsp_diagnostics`：CLSP language server 的有界独立检查；显式调用时仍可能启动 CLSP 自有服务器。
- `lsp_status`：Broker、server、hook 与降级状态。
- `ide_diagnostics`：当前 VS Code Problems，仅在能安全选择唯一 live 窗口时可用。

两个 VS Code 窗口同时打开同一项目，而调用进程没有 session 绑定时，CLSP 会返回歧义而不会猜测焦点窗口。官方 Codex IDE chat 没有公开的 client-surface 标识；只有一个匹配窗口时，CLSP 的有界上下文可能与官方扩展自身上下文重复。

## 手工配置

通常应使用 `clsp setup`。旧的 `[mcp_servers.lsp]` 若 command 是 CLSP，会被原位接管，不会创建第二个 CLSP MCP。

项目 `.clsp.toml` 仍可配置探测、命令执行和诊断：

```toml
enabled = true
auto_install = true
prewarm = true

[runtime]
probe_timeout_ms = 1500

[install]
command_timeout_seconds = 180

[diagnostics]
minimum_severity = "warning"

[lifecycle]
session_lease_seconds = 300
server_idle_seconds = 600
broker_idle_seconds = 900
```

`auto_install = false` 只复用已经可发现的兼容服务器，包括 dotnet 全局工具、VS Code 扩展已管理的 clangd 和完整的 CLSP clangd 缓存；它不执行安装、更新或下载。旧的 `runtime.policy`、per-LSP `policy` 和下载配置已移除，继续配置会返回明确的无效配置错误。

## 状态与降级

```powershell
clsp status --workspace .
clsp tui --workspace .
```

扩展未安装、窗口不受信任、远程窗口、Broker 暂不可用或 session 歧义都不会阻止普通 Codex 工作。PostTool 找不到唯一可路由 IDE session 时回退到原有 CLSP LSP；已路由 IDE 但基线缺失、Problems 截断或读取失败时则跳过自动注入，避免为同一语言启动第二个服务器。`ide_diagnostics` 返回结构化 unavailable；原有 watcher、`lsp_query`、`lsp_diagnostics` 和 `lsp_status` 保持可用。

Problems 为空只表示 VS Code 当前没有发布结构化诊断，不代表语言服务器、Cargo 或构建子进程健康。Output Channel（`Ctrl+Shift+U`）、stderr 和构建日志不属于这条 LSP 诊断复用路径。

状态位于 `%LOCALAPPDATA%\clsp`。Broker token、安装 locator、事件日志和最多保留 24 小时的 diff baseline 仅允许当前用户与 `SYSTEM` 访问。selection、Problems 内容、IDE action 和 baseline 内容不会写入 `events.jsonl` 或状态快照。升级协议后若看到 `protocol_mismatch`，请重启现有 CLSP/Codex/VS Code 进程。

## 构建与检查

需要 Rust 1.88+、Visual Studio Build Tools、Windows SDK、Node.js、Bun 1.3.14+ 和 VS Code CLI：

```powershell
bun ci --cwd vscode
bun run check
bun run clean
```

带发布说明正文的注释标签会触发 GitHub Actions：检查通过后构建 Windows ZIP 与 npm tarball，再创建 GitHub Release，并使用 npm Trusted Publishing 发布 `@dslzl/clsp`。本地仓库不保留发布组装脚本。

## 回滚

```powershell
code --uninstall-extension clsp.clsp-ide
```

然后从项目配置移除带 `# clsp-ide-bridge: managed-v1` 的 MCP 表及五个 `clsp hook ...` handler，并重启 Codex 与 VS Code。仅禁用扩展不会影响 CLSP 原有 LSP/MCP 能力；IDE session 会在约六秒内自动过期。
