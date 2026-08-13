use super::*;

use crate::registry::Registry;
use crate::test_support as support;
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn test_resolver(root: &Path) -> ServerResolver {
    let paths = StatePaths {
        workspace_state: root.join("state"),
        logs: root.join("state/logs"),
        artifacts: root.join("artifacts"),
    };
    for path in [&paths.logs, &paths.artifacts] {
        support::create_dir_all(path).unwrap();
    }
    let mut resolver = ServerResolver::new(Config::default(), paths);
    resolver.vscode_app_data = None;
    resolver.vscode_user_home = None;
    resolver.dotnet_cli_home = None;
    resolver
}

#[cfg(windows)]
fn fake_executable(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(format!("{name}.cmd"));
    support::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
    path
}

#[cfg(windows)]
fn compatible_test_executable(path: &Path) {
    std::fs::copy(system_curl().unwrap(), path).unwrap();
}

#[cfg(unix)]
fn compatible_test_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    support::write(path, "#!/bin/sh\necho clangd version 22.1.6\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_test_zip(path: &Path, name: &str, bytes: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    archive
        .start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    archive.write_all(bytes).unwrap();
    archive.finish().unwrap();
}

#[cfg(unix)]
fn fake_executable(root: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = root.join(name);
    support::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn write_elixir_ls_release(extension_root: &Path, version: &str) -> PathBuf {
    let launcher = if cfg!(windows) {
        "language_server.bat"
    } else {
        "language_server.sh"
    };
    let release = extension_root
        .join(format!("jakebecker.elixir-ls-{version}"))
        .join("elixir-ls-release");
    support::create_dir_all(&release).unwrap();
    let executable = release.join(launcher);
    support::write(&executable, b"launcher").unwrap();
    support::write(release.join("VERSION"), version).unwrap();
    executable
}

fn write_eslint_extension(extension_root: &Path, version: &str) -> PathBuf {
    let root = extension_root.join(format!("dbaeumer.vscode-eslint-{version}"));
    let server = root.join("server/out/eslintServer.js");
    support::create_dir_all(server.parent().unwrap()).unwrap();
    support::write(&server, b"server").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "vscode-eslint",
            "publisher": "dbaeumer",
            "version": version,
        }))
        .unwrap(),
    )
    .unwrap();
    server
}

fn write_eslint_dependency(workspace: &Path, version: &str) {
    let package = workspace.join("node_modules/eslint");
    support::create_dir_all(&package).unwrap();
    support::write(
        package.join("package.json"),
        serde_json::to_vec(&serde_json::json!({"name": "eslint", "version": version})).unwrap(),
    )
    .unwrap();
}

fn write_intelephense_extension(extension_root: &Path, version: &str) -> PathBuf {
    let root = extension_root.join(format!("{INTELEPHENSE_EXTENSION_PREFIX}{version}"));
    let server = root.join("node_modules/intelephense/lib/intelephense.js");
    support::create_dir_all(server.parent().unwrap()).unwrap();
    support::write(&server, b"server").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "vscode-intelephense-client",
            "publisher": "bmewburn",
            "version": version,
        }))
        .unwrap(),
    )
    .unwrap();
    support::write(
        root.join("node_modules/intelephense/package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "intelephense",
            "version": version,
        }))
        .unwrap(),
    )
    .unwrap();
    server
}

fn write_prisma_extension(extension_root: &Path, version: &str) -> PathBuf {
    let root = extension_root.join(format!("{PRISMA_EXTENSION_PREFIX}{version}"));
    let server = root.join("dist/language-server/bin.js");
    support::create_dir_all(server.parent().unwrap()).unwrap();
    support::write(&server, b"server").unwrap();
    support::write(
        root.join("dist/language-server/prisma_schema_build_bg.wasm"),
        b"wasm",
    )
    .unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "prisma",
            "publisher": "Prisma",
            "version": version,
        }))
        .unwrap(),
    )
    .unwrap();
    server
}

fn write_pyright_extension(extension_root: &Path, version: &str) -> PathBuf {
    let root = extension_root.join(format!("{PYRIGHT_EXTENSION_PREFIX}{version}"));
    let server = root.join("dist/server.js");
    support::create_dir_all(server.parent().unwrap()).unwrap();
    support::write(&server, b"server").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "pyright",
            "publisher": "ms-pyright",
            "version": version,
        }))
        .unwrap(),
    )
    .unwrap();
    server
}

fn write_fsharp_extension(
    extension_root: &Path,
    extension_version: &str,
    framework: &str,
) -> PathBuf {
    let root = extension_root.join(format!("ionide.ionide-fsharp-{extension_version}"));
    let server = root.join("bin").join(framework).join("fsautocomplete.dll");
    support::create_dir_all(server.parent().unwrap()).unwrap();
    for name in [
        "fsautocomplete.dll",
        "fsautocomplete.deps.json",
        "fsautocomplete.runtimeconfig.json",
    ] {
        support::write(server.with_file_name(name), b"server").unwrap();
    }
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "Ionide-fsharp",
            "publisher": "Ionide",
            "version": extension_version,
        }))
        .unwrap(),
    )
    .unwrap();
    server
}

fn write_kotlin_extension(
    extension_root: &Path,
    directory_version: &str,
    manifest_version: &str,
    server_version: &str,
) -> PathBuf {
    let root = extension_root.join(format!("{KOTLIN_EXTENSION_PREFIX}{directory_version}"));
    let launcher_path = if cfg!(windows) {
        "bin/intellij-server.exe"
    } else {
        "bin/intellij-server"
    };
    let java_path = if cfg!(windows) {
        "jbr/bin/java.exe"
    } else {
        "jbr/bin/java"
    };
    let server = root.join("server");
    let launcher = server.join(launcher_path);
    let java = server.join(java_path);
    support::create_dir_all(launcher.parent().unwrap()).unwrap();
    support::create_dir_all(java.parent().unwrap()).unwrap();
    support::write(&launcher, b"launcher").unwrap();
    support::write(&java, b"java").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "kotlin-server",
            "publisher": "JetBrains",
            "version": manifest_version,
        }))
        .unwrap(),
    )
    .unwrap();
    support::write(server.join("build.txt"), format!("ILS-{server_version}\n")).unwrap();
    support::write(
        server.join("product-info.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "kotlin-server",
            "buildNumber": server_version,
            "productCode": "ILS",
            "productVendor": "JetBrains",
            "minRequiredJavaVersion": 25,
            "launch": [{
                "launcherPath": launcher_path,
                "javaExecutablePath": java_path,
                "stdioRedirectArg": "--stdio",
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    launcher
}

fn write_lua_extension(
    extension_root: &Path,
    directory_version: &str,
    manifest_version: &str,
) -> PathBuf {
    let root = extension_root.join(format!("{LUA_EXTENSION_PREFIX}{directory_version}"));
    let executable = root.join(if cfg!(windows) {
        "server/bin/lua-language-server.exe"
    } else {
        "server/bin/lua-language-server"
    });
    for directory in [
        executable.parent().unwrap().to_path_buf(),
        root.join("server/script"),
        root.join("server/meta"),
        root.join("server/locale"),
    ] {
        support::create_dir_all(directory).unwrap();
    }
    support::write(&executable, b"launcher").unwrap();
    support::write(root.join("server/main.lua"), b"return true").unwrap();
    support::write(root.join("server/bin/main.lua"), b"return true").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "lua",
            "publisher": "sumneko",
            "version": manifest_version,
        }))
        .unwrap(),
    )
    .unwrap();
    executable
}

fn write_jdtls_extension(
    extension_root: &Path,
    directory_version: &str,
    manifest_version: &str,
    core_version: &str,
    java_version: &str,
) -> (PathBuf, PathBuf) {
    let root = extension_root.join(format!("redhat.java-{directory_version}"));
    let plugins = root.join("server/plugins");
    let configuration = root.join("server").join(if cfg!(windows) {
        "config_win"
    } else if cfg!(target_os = "macos") {
        "config_mac"
    } else {
        "config_linux"
    });
    let java_bin = root.join("jre/bin");
    for directory in [&plugins, &configuration, &java_bin] {
        support::create_dir_all(directory).unwrap();
    }
    let launcher = plugins.join("org.eclipse.equinox.launcher_1.7.0.jar");
    support::write(&launcher, b"launcher").unwrap();
    support::write(
        plugins.join(format!("org.eclipse.jdt.ls.core_{core_version}.jar")),
        b"core",
    )
    .unwrap();
    support::write(configuration.join("config.ini"), b"config").unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "java",
            "publisher": "redhat",
            "version": manifest_version,
        }))
        .unwrap(),
    )
    .unwrap();
    let java = fake_executable(
        &java_bin,
        "java",
        &format!("echo openjdk version \"{java_version}\" 1>&2"),
    );
    (launcher, java)
}

fn write_julials_extension(
    extension_root: &Path,
    extension_version: &str,
    environment_name: &str,
    server_version: &str,
) -> (PathBuf, PathBuf) {
    let root = extension_root.join(format!("{JULIALS_EXTENSION_PREFIX}{extension_version}"));
    let environment = root
        .join("scripts/environments/languageserver")
        .join(environment_name);
    let package = root.join("scripts/packages/LanguageServer");
    let source = package.join("src/LanguageServer.jl");
    support::create_dir_all(&environment).unwrap();
    support::create_dir_all(source.parent().unwrap()).unwrap();
    support::write(
        root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "language-julia",
            "publisher": "julialang",
            "version": extension_version,
        }))
        .unwrap(),
    )
    .unwrap();
    support::write(
        environment.join("Project.toml"),
        format!("[deps]\nLanguageServer = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\n"),
    )
    .unwrap();
    support::write(
        environment.join("Manifest.toml"),
        format!(
            "manifest_format = \"2.0\"\n\n[[deps.LanguageServer]]\npath = \"../../../packages/LanguageServer\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"{server_version}\"\n"
        ),
    )
    .unwrap();
    support::write(
        package.join("Project.toml"),
        format!(
            "name = \"LanguageServer\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"{server_version}\"\n"
        ),
    )
    .unwrap();
    support::write(&source, "module LanguageServer\nend\n").unwrap();
    (environment.join("Project.toml"), source)
}

fn write_fake_julia(
    root: &Path,
    runtime_version: &str,
    server_version: &str,
    package: &Path,
) -> PathBuf {
    support::create_dir_all(root).unwrap();
    #[cfg(windows)]
    let body = format!(
        "if \"%~1\"==\"--version\" (\r\n  echo julia version {runtime_version}\r\n  exit /b 0\r\n)\r\necho {runtime_version}\r\necho {server_version}\r\necho {}",
        package.display()
    );
    #[cfg(unix)]
    let body = format!(
        "if [ \"$1\" = \"--version\" ]; then\n  echo 'julia version {runtime_version}'\n  exit 0\nfi\nprintf '%s\\n' '{runtime_version}' '{server_version}' '{}'",
        package.display()
    );
    fake_executable(root, "julia", &body)
}

#[test]
fn npm_manager_order_and_exact_argv_are_fixed() {
    assert_eq!(
        NpmManager::ALL,
        [NpmManager::Bun, NpmManager::Pnpm, NpmManager::Npm]
    );
    assert_eq!(
        NpmManager::Bun.install_args(),
        ["install", "--global", "--ignore-scripts"]
    );
    assert_eq!(
        NpmManager::Pnpm.install_args(),
        ["add", "--global", "--ignore-scripts"]
    );
    assert_eq!(
        NpmManager::Npm.install_args(),
        [
            "install",
            "--global",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund"
        ]
    );
    assert_eq!(
        npm_install_args(
            NpmManager::Bun,
            "@astrojs/language-server",
            "2.16.13",
            &["typescript@5.9.2".to_owned()]
        ),
        [
            "install",
            "--global",
            "--ignore-scripts",
            "@astrojs/language-server@2.16.13",
            "typescript@5.9.2"
        ]
    );
}

#[test]
fn dotnet_tool_contract_is_exact() {
    let registry = Registry::builtin().unwrap();
    let csharp = registry.server("csharp").unwrap();
    let InstallRecipe::Command { args, .. } = &csharp.install else {
        panic!("C# must use a command recipe");
    };
    assert_eq!(dotnet_tool_command_args(args, false).unwrap(), *args);
    let mut update = args.clone();
    update[1] = "update".to_owned();
    update.push("--allow-downgrade".to_owned());
    assert_eq!(dotnet_tool_command_args(args, true).unwrap(), update);

    let list = b"Package Id Version Commands\n--------------------------------\nroslyn-language-server 5.9.0-1.26303.1 roslyn-language-server\n";
    assert_eq!(
        parse_dotnet_tool_version(list, ROSLYN_LANGUAGE_SERVER_PACKAGE).unwrap(),
        Some("5.9.0-1.26303.1".to_owned())
    );
    assert_eq!(
        parse_dotnet_tool_version(list, "other-package").unwrap(),
        None
    );
    assert!(
        parse_dotnet_tool_version(
            b"roslyn-language-server invalid",
            ROSLYN_LANGUAGE_SERVER_PACKAGE
        )
        .is_err()
    );
    assert!(PRESERVED_ENV.contains(&"DOTNET_CLI_HOME"));
    assert!(PRESERVED_ENV.contains(&"DOTNET_ROOT"));
    assert!(PRESERVED_ENV.contains(&"ProgramFiles(x86)"));
    assert_eq!(
        dotnet_tool_package("csharp"),
        Some(ROSLYN_LANGUAGE_SERVER_PACKAGE)
    );
    assert_eq!(
        dotnet_tool_package(FSHARP_SERVER_ID),
        Some(FSHARP_LANGUAGE_SERVER_PACKAGE)
    );
    assert_eq!(dotnet_tool_package("rust"), None);
}

#[tokio::test]
async fn dotnet_global_resolution_requires_manifest_and_compatible_shim() {
    let root = support::tempdir().unwrap();
    let home = root.path().join("dotnet-home");
    let tools = home.join(".dotnet/tools");
    support::create_dir_all(&tools).unwrap();
    let dotnet = fake_executable(
        root.path(),
        "dotnet",
        "echo Package Id Version Commands\necho roslyn-language-server 5.9.0-1.26303.1 roslyn-language-server",
    );
    let server_executable = fake_executable(
        &tools,
        ROSLYN_LANGUAGE_SERVER_PACKAGE,
        "echo roslyn-language-server 5.9.0-1.26303.1",
    );
    let mut resolver = test_resolver(root.path());
    resolver.dotnet_cli_home = Some(home);
    let server = Registry::builtin()
        .unwrap()
        .server("csharp")
        .unwrap()
        .clone();

    let resolution = resolver
        .resolve_toolchain_candidate(&server, root.path(), &dotnet, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.path, server_executable);
    assert_eq!(resolution.source, ExecutableSource::Path);

    #[cfg(windows)]
    support::write(
        &dotnet,
        "@echo off\r\necho Package Id Version Commands\r\nexit /b 1\r\n",
    )
    .unwrap();
    #[cfg(unix)]
    support::write(
        &dotnet,
        "#!/bin/sh\necho Package Id Version Commands\nexit 1\n",
    )
    .unwrap();
    assert_eq!(
        resolver
            .dotnet_global_tool_version(&dotnet, ROSLYN_LANGUAGE_SERVER_PACKAGE)
            .await
            .unwrap(),
        None
    );

    #[cfg(windows)]
    support::write(
        &dotnet,
        "@echo off\r\necho roslyn-language-server 5.8.0 roslyn-language-server\r\n",
    )
    .unwrap();
    #[cfg(unix)]
    support::write(
        &dotnet,
        "#!/bin/sh\necho roslyn-language-server 5.8.0 roslyn-language-server\n",
    )
    .unwrap();
    assert!(
        resolver
            .resolve_toolchain_candidate(&server, root.path(), &dotnet, false)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn manager_probe_skips_failures_without_reordering() {
    let root = support::tempdir().unwrap();
    let resolver = test_resolver(root.path());
    let failed = fake_executable(root.path(), "bun", "exit /b 9");
    let working = fake_executable(root.path(), "pnpm", "echo 10.0.0");
    let unused = fake_executable(root.path(), "npm", "echo 12.0.0");

    let selected = resolver
        .select_npm_manager_from([
            (NpmManager::Bun, failed),
            (NpmManager::Pnpm, working.clone()),
            (NpmManager::Npm, unused),
        ])
        .await
        .unwrap();
    assert_eq!(selected.manager, NpmManager::Pnpm);
    assert_eq!(selected.executable, working);
    assert_eq!(
        resolver
            .select_npm_manager_from(Vec::new())
            .await
            .unwrap_err()
            .code,
        ErrorCode::RuntimeUnavailable
    );
}

#[tokio::test]
async fn selected_manager_install_failure_is_terminal() {
    let root = support::tempdir().unwrap();
    let resolver = test_resolver(root.path());
    let failed = fake_executable(root.path(), "bun", "exit /b 9");
    let server = Registry::builtin()
        .unwrap()
        .server("pyright")
        .unwrap()
        .clone();
    let manager = NpmManagerSelection {
        manager: NpmManager::Bun,
        executable: failed,
    };

    let error = resolver
        .install_npm(&server, &manager, "pyright", "1.1.405", &[])
        .await
        .unwrap_err();
    assert!(error.message.contains("bun global install"));
}

#[tokio::test]
async fn post_install_missing_manager_roots_fail() {
    let root = support::tempdir().unwrap();
    let resolver = test_resolver(root.path());
    let missing = root.path().join("missing-global-root");
    let manager = NpmManagerSelection {
        manager: NpmManager::Npm,
        executable: fake_executable(
            root.path(),
            "npm-roots",
            &format!("echo {}", missing.display()),
        ),
    };

    let error = match resolver.npm_roots(&manager, true).await {
        Ok(_) => panic!("missing roots must fail"),
        Err(error) => error,
    };
    assert!(error.message.contains("reported missing global roots"));
}

#[tokio::test]
async fn compatible_project_executable_never_starts_install() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let bin = workspace.join("node_modules/.bin");
    let package = workspace.join("node_modules/pyright");
    support::create_dir_all(&bin).unwrap();
    support::create_dir_all(&package).unwrap();
    let server = Registry::builtin()
        .unwrap()
        .server("pyright")
        .unwrap()
        .clone();
    support::write(bin.join(&executable_names(&server.command)[0]), b"wrapper").unwrap();
    support::write(
        package.join("package.json"),
        br#"{"name":"pyright","version":"1.1.405"}"#,
    )
    .unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&called);

    let resolution = test_resolver(root.path())
        .resolve_server(&server, &workspace, None, move || async move {
            callback.store(true, Ordering::Relaxed);
        })
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::ProjectLocal);
    assert!(!called.load(Ordering::Relaxed));
}

#[tokio::test]
async fn existing_resolution_pass_repeats_after_a_new_candidate_appears() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let mut server = Registry::builtin()
        .unwrap()
        .server(ELIXIR_LS_SERVER_ID)
        .unwrap()
        .clone();
    server.command = "missing-elixir-ls-for-rediscovery".into();
    let mut resolver = test_resolver(root.path());
    resolver.vscode_user_home = Some(root.path().to_path_buf());

    assert!(
        resolver
            .resolve_existing(&server, &workspace, None)
            .await
            .unwrap()
            .is_none()
    );

    let executable = write_elixir_ls_release(&root.path().join(".vscode/extensions"), "0.31.1");
    let resolution = resolver
        .resolve_existing(&server, &workspace, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "0.31.1");
}

#[test]
fn vscode_clangd_candidates_are_newest_first() {
    let root = support::tempdir().unwrap();
    let older = root
        .path()
        .join("Code/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/18.1.8/clangd_18.1.8/bin/clangd.exe");
    let newer = root
        .path()
        .join("Code - Insiders/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/22.1.6/clangd_22.1.6/bin/clangd.exe");
    for path in [&older, &newer] {
        support::create_dir_all(path.parent().unwrap()).unwrap();
        support::write(path, b"candidate").unwrap();
    }

    assert_eq!(vscode_clangd_candidates_from(root.path()), [newer, older]);
}

#[test]
fn elixir_ls_release_probe_requires_official_bounded_version() {
    let root = support::tempdir().unwrap();
    let executable = write_elixir_ls_release(root.path(), "0.31.1");
    let version_file = executable.parent().unwrap().join("VERSION");

    assert_eq!(
        probe_elixir_ls_release(&executable, ">=0.31.1, <0.32.0").unwrap(),
        "0.31.1"
    );

    let wrong_launcher = executable.parent().unwrap().join("debug_adapter.bat");
    support::write(&wrong_launcher, b"launcher").unwrap();
    assert!(probe_elixir_ls_release(&wrong_launcher, ">=0.31.1").is_err());

    std::fs::remove_file(&version_file).unwrap();
    assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

    support::write(
        &version_file,
        vec![b'1'; ELIXIR_LS_VERSION_FILE_LIMIT as usize + 1],
    )
    .unwrap();
    assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

    support::write(&version_file, b"not-a-version").unwrap();
    assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

    support::write(&version_file, b"0.30.0").unwrap();
    assert!(probe_elixir_ls_release(&executable, ">=0.31.1, <0.32.0").is_err());
}

#[test]
fn vscode_elixir_ls_candidates_are_newest_first_and_official() {
    let root = support::tempdir().unwrap();
    let older = write_elixir_ls_release(&root.path().join(".vscode/extensions"), "0.30.0");
    let newer = write_elixir_ls_release(&root.path().join(".vscode-insiders/extensions"), "0.31.1");
    let fake = root
        .path()
        .join(".vscode/extensions/not-official.elixir-ls-9.9.9/elixir-ls-release");
    support::create_dir_all(&fake).unwrap();
    support::write(
        fake.join(if cfg!(windows) {
            "language_server.bat"
        } else {
            "language_server.sh"
        }),
        b"launcher",
    )
    .unwrap();

    assert_eq!(
        vscode_elixir_ls_candidates_from(root.path()),
        [newer, older]
    );
}

#[tokio::test]
async fn elixir_ls_reuses_the_official_vscode_release() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let executable = write_elixir_ls_release(&root.path().join(".vscode/extensions"), "0.31.1");
    let mut resolver = test_resolver(root.path());
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(ELIXIR_LS_SERVER_ID)
        .unwrap()
        .clone();

    let resolution = resolver
        .resolve_vscode_elixir_ls(&server, &workspace)
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "0.31.1");
}

#[test]
fn vscode_eslint_candidates_are_newest_first_and_official() {
    let root = support::tempdir().unwrap();
    let older = write_eslint_extension(&root.path().join(".vscode/extensions"), "3.0.33");
    let newer = write_eslint_extension(&root.path().join(".vscode-insiders/extensions"), "3.0.34");
    let fake = root
        .path()
        .join(".vscode/extensions/not-official.vscode-eslint-9.9.9/server/out/eslintServer.js");
    support::create_dir_all(fake.parent().unwrap()).unwrap();
    support::write(fake, b"server").unwrap();

    assert_eq!(vscode_eslint_candidates_from(root.path()), [newer, older]);
}

#[test]
fn vscode_intelephense_candidates_are_newest_first_and_official() {
    let root = support::tempdir().unwrap();
    let older = write_intelephense_extension(&root.path().join(".vscode/extensions"), "1.18.4");
    let newer =
        write_intelephense_extension(&root.path().join(".vscode-insiders/extensions"), "1.18.5");
    let fake = root.path().join(
        ".vscode/extensions/not-official.vscode-intelephense-client-9.9.9/node_modules/intelephense/lib/intelephense.js",
    );
    support::create_dir_all(fake.parent().unwrap()).unwrap();
    support::write(fake, b"server").unwrap();

    assert_eq!(
        vscode_intelephense_candidates_from(root.path()),
        [newer.clone(), older.clone()]
    );
    assert!(validate_vscode_intelephense_extension(&newer, ">=1.18.5, <2.0.0").is_ok());
    assert!(validate_vscode_intelephense_extension(&older, ">=1.18.5, <2.0.0").is_err());
}

#[test]
fn vscode_prisma_candidates_are_newest_first_and_official() {
    let root = support::tempdir().unwrap();
    let older = write_prisma_extension(&root.path().join(".vscode/extensions"), "6.19.0");
    let newer = write_prisma_extension(&root.path().join(".vscode-insiders/extensions"), "31.11.0");
    let fake = root
        .path()
        .join(".vscode/extensions/not-official.prisma-99.0.0/dist/language-server/bin.js");
    support::create_dir_all(fake.parent().unwrap()).unwrap();
    support::write(fake, b"server").unwrap();

    assert_eq!(
        vscode_prisma_candidates_from(root.path()),
        [newer.clone(), older.clone()]
    );
    assert!(validate_vscode_prisma_extension(&newer, ">=6.19.0, <32.0.0").is_ok());
    assert!(validate_vscode_prisma_extension(&older, ">=7.0.0, <32.0.0").is_err());
}

#[test]
fn vscode_pyright_candidates_are_newest_first_and_official() {
    let root = support::tempdir().unwrap();
    let older = write_pyright_extension(&root.path().join(".vscode/extensions"), "1.1.410");
    let newer =
        write_pyright_extension(&root.path().join(".vscode-insiders/extensions"), "1.1.411");
    let fake = root
        .path()
        .join(".vscode/extensions/not-official.pyright-9.9.9/dist/server.js");
    support::create_dir_all(fake.parent().unwrap()).unwrap();
    support::write(fake, b"server").unwrap();

    assert_eq!(
        vscode_pyright_candidates_from(root.path()),
        [newer.clone(), older.clone()]
    );
    assert!(validate_vscode_pyright_extension(&newer, ">=1.1.300, <2.0.0").is_ok());
    assert!(validate_vscode_pyright_extension(&older, ">=1.1.411, <2.0.0").is_err());
}

#[test]
fn vscode_fsharp_candidates_and_manifest_are_official_and_bounded() {
    let root = support::tempdir().unwrap();
    let older = write_fsharp_extension(&root.path().join(".vscode/extensions"), "7.30.0", "net8.0");
    let newer_net8 = write_fsharp_extension(
        &root.path().join(".vscode-insiders/extensions"),
        "7.31.1",
        "net8.0",
    );
    let newer_net9 = write_fsharp_extension(
        &root.path().join(".vscode-insiders/extensions"),
        "7.31.1",
        "net9.0",
    );
    let fake = root
        .path()
        .join(".vscode/extensions/not-official.ionide-fsharp-9.9.9/bin/net9.0");
    support::create_dir_all(&fake).unwrap();
    support::write(fake.join("fsautocomplete.dll"), b"server").unwrap();

    assert_eq!(
        vscode_fsharp_candidates_from(root.path()),
        [newer_net8.clone(), newer_net9, older.clone()]
    );
    validate_vscode_fsharp_extension(&newer_net8).unwrap();
    assert!(validate_vscode_fsharp_extension(&older).is_err());

    support::write(
        vscode_fsharp_extension_root(&newer_net8)
            .unwrap()
            .join("package.json"),
        br#"{"name":"Ionide-fsharp","publisher":"other","version":"7.31.1"}"#,
    )
    .unwrap();
    assert!(validate_vscode_fsharp_extension(&newer_net8).is_err());
}

#[test]
fn vscode_kotlin_candidates_validate_official_bundled_servers() {
    let root = support::tempdir().unwrap();
    let older = write_kotlin_extension(
        &root.path().join(".vscode/extensions"),
        "0.0.6-win32-x64",
        "0.0.6",
        "262.9593.0",
    );
    let newer = write_kotlin_extension(
        &root.path().join(".vscode-insiders/extensions"),
        "0.0.8-win32-x64",
        "0.0.8",
        "263.2689.0",
    );

    assert_eq!(
        vscode_kotlin_candidates_from(root.path()),
        [newer.clone(), older]
    );
    let layout = validate_vscode_kotlin_extension(&newer, ">=262.4739.0, <264.0.0").unwrap();
    assert!(layout.bundled_java.is_file());

    let incompatible = write_kotlin_extension(
        &root.path().join("other"),
        "0.0.8-win32-x64",
        "0.0.8",
        "264.1.0",
    );
    assert!(validate_vscode_kotlin_extension(&incompatible, ">=262.4739.0, <264.0.0").is_err());

    support::write(
        layout.manifest,
        br#"{"name":"kotlin-server","publisher":"other","version":"0.0.8"}"#,
    )
    .unwrap();
    assert!(validate_vscode_kotlin_extension(&newer, ">=262.4739.0, <264.0.0").is_err());
    assert!(PRESERVED_ENV.contains(&"GRADLE_USER_HOME"));
}

#[test]
fn vscode_lua_candidates_validate_official_bounded_runtime() {
    let root = support::tempdir().unwrap();
    let older = write_lua_extension(
        &root.path().join(".vscode/extensions"),
        "3.18.2-win32-x64",
        "3.18.2",
    );
    let newer = write_lua_extension(
        &root.path().join(".vscode-insiders/extensions"),
        "3.19.0-win32-x64",
        "3.19.0",
    );
    assert_eq!(
        vscode_lua_candidates_from(root.path()),
        [newer.clone(), older]
    );

    let (layout, version) = validate_vscode_lua_extension(&newer, ">=3.19.0, <4.0.0").unwrap();
    assert_eq!(version, Version::new(3, 19, 0));
    validate_lua_server_version("3.19.0", &version).unwrap();
    assert!(validate_lua_server_version("3.19.1", &version).is_err());

    let mismatched = write_lua_extension(
        &root.path().join("mismatched"),
        "3.19.1-win32-x64",
        "3.19.0",
    );
    assert!(validate_vscode_lua_extension(&mismatched, ">=3.19.0, <4.0.0").is_err());

    let forged = write_lua_extension(&root.path().join("forged"), "3.19.0-win32-x64", "3.19.0");
    let forged_manifest = lua_extension_layout(&forged).unwrap().manifest;
    support::write(
        forged_manifest,
        br#"{"name":"lua","publisher":"other","version":"3.19.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_lua_extension(&forged, ">=3.19.0, <4.0.0").is_err());

    let incomplete = write_lua_extension(
        &root.path().join("incomplete"),
        "3.19.0-win32-x64",
        "3.19.0",
    );
    std::fs::remove_dir_all(lua_extension_layout(&incomplete).unwrap().locale).unwrap();
    assert!(validate_vscode_lua_extension(&incomplete, ">=3.19.0, <4.0.0").is_err());

    support::write(
        layout.manifest,
        vec![b' '; LUA_EXTENSION_FILE_LIMIT as usize + 1],
    )
    .unwrap();
    assert!(validate_vscode_lua_extension(&newer, ">=3.19.0, <4.0.0").is_err());

    let outside = root.path().join(if cfg!(windows) {
        "outside/lua-language-server.exe"
    } else {
        "outside/lua-language-server"
    });
    support::create_dir_all(outside.parent().unwrap()).unwrap();
    support::write(&outside, b"launcher").unwrap();
    assert!(lua_extension_layout(&outside).is_err());
}

#[tokio::test]
async fn vscode_jdtls_candidates_validate_layout_java_and_platform_suffixes() {
    let root = support::tempdir().unwrap();
    let extensions = root.path().join(".vscode/extensions");
    let (older, _) = write_jdtls_extension(
        &extensions,
        "1.54.0-win32-x64",
        "1.54.0",
        "1.59.0.202606010000",
        "21.0.3",
    );
    let (newer, java) = write_jdtls_extension(
        &extensions,
        "1.55.0-win32-x64",
        "1.55.0",
        "1.60.0.202607010000",
        "21.0.3",
    );

    assert_eq!(
        vscode_jdtls_candidates_from(root.path()),
        [newer.clone(), older.clone()]
    );
    let (layout, version) = validate_vscode_jdtls_extension(&newer, ">=1.30.0, <2.0.0").unwrap();
    assert_eq!(version, Version::new(1, 60, 0));
    assert_eq!(
        jdtls_java_for_launcher(&newer, root.path(), Duration::from_secs(5))
            .await
            .unwrap(),
        (java, 21)
    );
    assert_eq!(java_major_version("openjdk version \"21.0.3\""), Some(21));
    assert_eq!(java_major_version("openjdk version \"20.0.2\""), Some(20));

    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let mut resolver = test_resolver(root.path());
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(JDTLS_SERVER_ID)
        .unwrap()
        .clone();
    let resolution = resolver
        .resolve_vscode_jdtls(&server, &workspace)
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, newer);
    assert!(resolution.version_output.contains("Eclipse JDT LS 1.60.0"));

    support::write(
        layout.extension_root.join("package.json"),
        br#"{"name":"java","publisher":"other","version":"1.55.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_jdtls_extension(&resolution.path, ">=1.30.0").is_err());

    support::write(
        jdtls_extension_layout(&older)
            .unwrap()
            .core
            .with_file_name("org.eclipse.jdt.ls.core_1.59.1.jar"),
        b"duplicate",
    )
    .unwrap();
    assert!(jdtls_extension_layout(&older).is_err());
}

#[tokio::test]
async fn julials_probe_checks_julia_and_language_server_versions() {
    let root = support::tempdir().unwrap();
    let package = root.path().join("LanguageServer/src/LanguageServer.jl");
    support::create_dir_all(package.parent().unwrap()).unwrap();
    support::write(&package, "module LanguageServer\nend\n").unwrap();
    let julia = write_fake_julia(&root.path().join("bin"), "1.11.9", "5.0.0", &package);

    assert_eq!(
        julials_probe_args(None),
        [
            "--startup-file=no",
            "--history-file=no",
            "-e",
            JULIALS_PROBE_SCRIPT,
        ]
    );
    assert_eq!(
        julials_probe_timeout(Duration::from_millis(1_500)),
        JULIALS_PROBE_TIMEOUT
    );
    assert_eq!(
        julials_probe_timeout(Duration::from_secs(10)),
        Duration::from_secs(10)
    );
    let environment = root.path().join("environment");
    assert_eq!(
        julials_probe_args(Some(&environment)),
        [
            "--startup-file=no".to_owned(),
            "--history-file=no".to_owned(),
            format!("--project={}", environment.to_string_lossy()),
            "-e".to_owned(),
            JULIALS_PROBE_SCRIPT.to_owned(),
        ]
    );
    let probe = probe_julials(
        &julia,
        None,
        root.path(),
        ">=5.0.0, <6.0.0",
        ">=1.10.0",
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(probe.julia_version, Version::new(1, 11, 9));
    assert_eq!(probe.server_version, Version::new(5, 0, 0));
    assert_eq!(probe.package_path, package);
    assert!(
        probe_julia_version(&julia, root.path(), Duration::from_secs(5), ">=1.12.0")
            .await
            .is_err()
    );
    assert!(
        parse_julials_probe_output(
            format!("1.9.4\n5.0.0\n{}", package.display()).as_bytes(),
            ">=5.0.0, <6.0.0",
            ">=1.10.0",
        )
        .is_err()
    );
    assert!(
        parse_julials_probe_output(
            format!("1.11.9\n4.5.0\n{}", package.display()).as_bytes(),
            ">=5.0.0, <6.0.0",
            ">=1.10.0",
        )
        .is_err()
    );
    assert!(PRESERVED_ENV.contains(&"JULIA_DEPOT_PATH"));
    assert!(PRESERVED_ENV.contains(&"JULIA_LOAD_PATH"));
    assert!(PRESERVED_ENV.contains(&"JULIA_PROJECT"));
}

#[tokio::test]
async fn vscode_julials_uses_official_exact_or_fallback_environment() {
    let root = support::tempdir().unwrap();
    let stable = root.path().join(".vscode/extensions");
    let insiders = root.path().join(".vscode-insiders/extensions");
    let (fallback, _) = write_julials_extension(&stable, "1.230.0", "fallback", "5.0.0");
    let (matching, source) = write_julials_extension(&insiders, "1.231.1", "v1.11", "5.1.0");
    let julia_version = Version::new(1, 11, 9);

    assert_eq!(
        vscode_julials_candidates_from(root.path(), &julia_version),
        [matching.clone(), fallback.clone()]
    );
    assert_eq!(
        vscode_julials_candidates_from(root.path(), &Version::new(1, 12, 1)),
        [fallback]
    );
    let (layout, extension_version, server_version) =
        validate_vscode_julials_environment(&matching, ">=5.0.0, <6.0.0").unwrap();
    assert_eq!(extension_version, Version::new(1, 231, 1));
    assert_eq!(server_version, Version::new(5, 1, 0));
    assert_eq!(
        julials_extension_environment(&matching).unwrap(),
        layout.environment
    );

    let julia = write_fake_julia(&root.path().join("bin"), "1.11.9", "5.1.0", &source);
    let output = probe_vscode_julials(
        &julia,
        &matching,
        root.path(),
        ">=5.0.0, <6.0.0",
        &julia_version,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(output.contains("Julia 1.11.9; LanguageServer 5.1.0"));
    assert!(output.contains("julialang.language-julia 1.231.1"));
}

#[test]
fn vscode_julials_rejects_forged_oversized_and_escaping_metadata() {
    let root = support::tempdir().unwrap();
    let extensions = root.path().join(".vscode/extensions");

    let (forged, _) = write_julials_extension(&extensions, "1.231.0", "v1.11", "5.1.0");
    let forged_root = julials_extension_layout(&forged).unwrap().extension_root;
    support::write(
        forged_root.join("package.json"),
        br#"{"name":"language-julia","publisher":"other","version":"1.231.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_julials_environment(&forged, ">=5.0.0, <6.0.0").is_err());

    let (mismatched, _) = write_julials_extension(&extensions, "1.231.1", "v1.11", "5.1.0");
    let mismatched_root = julials_extension_layout(&mismatched)
        .unwrap()
        .extension_root;
    support::write(
        mismatched_root.join("package.json"),
        br#"{"name":"language-julia","publisher":"julialang","version":"1.231.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_julials_environment(&mismatched, ">=5.0.0, <6.0.0").is_err());

    let (oversized, _) = write_julials_extension(&extensions, "1.232.0", "v1.11", "5.1.0");
    let oversized_root = julials_extension_layout(&oversized).unwrap().extension_root;
    support::write(
        oversized_root.join("package.json"),
        vec![b'x'; JULIALS_FILE_LIMIT as usize + 1],
    )
    .unwrap();
    assert!(validate_vscode_julials_environment(&oversized, ">=5.0.0, <6.0.0").is_err());

    let (escaping, _) = write_julials_extension(&extensions, "1.233.0", "v1.11", "5.1.0");
    let outside = extensions.join("outside");
    support::create_dir(&outside).unwrap();
    let layout = julials_extension_layout(&escaping).unwrap();
    support::write(
        &layout.manifest,
        format!(
            "[[deps.LanguageServer]]\npath = \"../../../../../outside\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"5.1.0\"\n"
        ),
    )
    .unwrap();
    assert!(validate_vscode_julials_environment(&escaping, ">=5.0.0, <6.0.0").is_err());
}

#[tokio::test]
async fn eslint_probe_requires_the_official_extension_and_project_dependency() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    write_eslint_dependency(&workspace, "9.32.0");
    let executable = write_eslint_extension(root.path(), "3.0.34");

    let probe = probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
        .await
        .unwrap();
    assert_eq!(probe.version_output, "vscode-eslint 3.0.34");
    assert_eq!(probe.npm_modules_root, Some(workspace.join("node_modules")));

    let extension_root = vscode_eslint_extension_root(&executable).unwrap();
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"vscode-eslint","publisher":"other","version":"3.0.34"}"#,
    )
    .unwrap();
    assert!(
        probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
            .await
            .is_err()
    );

    write_eslint_extension(root.path(), "3.0.34");
    std::fs::remove_file(workspace.join("node_modules/eslint/package.json")).unwrap();
    assert!(
        probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn eslint_reuses_the_official_vscode_server() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    write_eslint_dependency(&workspace, "9.32.0");
    let executable = write_eslint_extension(&root.path().join(".vscode/extensions"), "3.0.34");
    let mut resolver = test_resolver(root.path());
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(ESLINT_SERVER_ID)
        .unwrap()
        .clone();

    let resolution = resolver
        .resolve_vscode_eslint(&server, &workspace)
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "vscode-eslint 3.0.34");
}

#[test]
fn intelephense_extension_probe_requires_official_matching_manifests() {
    let root = support::tempdir().unwrap();
    let executable = write_intelephense_extension(root.path(), "1.18.5");
    let probe = validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").unwrap();
    assert_eq!(probe.version_output, "intelephense 1.18.5");
    assert_eq!(
        probe.modules_root,
        std::fs::canonicalize(root.path().join(format!(
            "{INTELEPHENSE_EXTENSION_PREFIX}1.18.5/node_modules"
        )))
        .unwrap()
    );

    let extension_root = vscode_intelephense_extension_root(&executable).unwrap();
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"vscode-intelephense-client","publisher":"other","version":"1.18.5"}"#,
    )
    .unwrap();
    assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

    write_intelephense_extension(root.path(), "1.18.5");
    support::write(
        extension_root.join("node_modules/intelephense/package.json"),
        br#"{"name":"intelephense","version":"1.18.4"}"#,
    )
    .unwrap();
    assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

    write_intelephense_extension(root.path(), "1.18.5");
    support::write(
        extension_root.join("package.json"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .unwrap();
    assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

    write_intelephense_extension(root.path(), "1.18.5");
    std::fs::remove_file(&executable).unwrap();
    assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());
}

#[tokio::test]
async fn intelephense_reuses_the_official_vscode_server() {
    let root = support::tempdir().unwrap();
    let executable =
        write_intelephense_extension(&root.path().join(".vscode/extensions"), "1.18.5");
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(INTELEPHENSE_SERVER_ID)
        .unwrap()
        .clone();

    let resolution = resolver.resolve_vscode_intelephense(&server).await.unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "intelephense 1.18.5");
}

#[test]
fn prisma_extension_probe_requires_official_manifest_and_wasm() {
    let root = support::tempdir().unwrap();
    let executable = write_prisma_extension(root.path(), "31.11.0");
    assert_eq!(
        validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").unwrap(),
        "@prisma/language-server 31.11.0"
    );

    let extension_root = vscode_prisma_extension_root(&executable).unwrap();
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"prisma","publisher":"other","version":"31.11.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

    write_prisma_extension(root.path(), "31.11.0");
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"prisma","publisher":"Prisma","version":"31.10.0"}"#,
    )
    .unwrap();
    assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

    write_prisma_extension(root.path(), "31.11.0");
    std::fs::remove_file(extension_root.join("dist/language-server/prisma_schema_build_bg.wasm"))
        .unwrap();
    assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

    write_prisma_extension(root.path(), "31.11.0");
    std::fs::remove_file(&executable).unwrap();
    assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());
}

#[tokio::test]
async fn prisma_reuses_the_official_vscode_server() {
    let root = support::tempdir().unwrap();
    let executable = write_prisma_extension(&root.path().join(".vscode/extensions"), "31.11.0");
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(PRISMA_SERVER_ID)
        .unwrap()
        .clone();

    let resolution = resolver.resolve_vscode_prisma(&server).await.unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "@prisma/language-server 31.11.0");
    assert!(resolution.npm_modules_root.is_none());
}

#[test]
fn pyright_extension_probe_requires_official_matching_manifest() {
    let root = support::tempdir().unwrap();
    let executable = write_pyright_extension(root.path(), "1.1.411");
    assert_eq!(
        validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").unwrap(),
        "pyright 1.1.411"
    );

    let extension_root = vscode_pyright_extension_root(&executable).unwrap();
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"pyright","publisher":"other","version":"1.1.411"}"#,
    )
    .unwrap();
    assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());

    write_pyright_extension(root.path(), "1.1.411");
    support::write(
        extension_root.join("package.json"),
        br#"{"name":"pyright","publisher":"ms-pyright","version":"1.1.410"}"#,
    )
    .unwrap();
    assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());

    write_pyright_extension(root.path(), "1.1.411");
    std::fs::remove_file(&executable).unwrap();
    assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());
}

#[tokio::test]
async fn pyright_reuses_the_official_vscode_server() {
    let root = support::tempdir().unwrap();
    let executable = write_pyright_extension(&root.path().join(".vscode/extensions"), "1.1.411");
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    resolver.vscode_user_home = Some(root.path().to_path_buf());
    let server = Registry::builtin()
        .unwrap()
        .server(PYRIGHT_SERVER_ID)
        .unwrap()
        .clone();

    let resolution = resolver.resolve_vscode_pyright(&server).await.unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, executable);
    assert_eq!(resolution.version_output, "pyright 1.1.411");
    assert!(resolution.npm_modules_root.is_none());
}

#[tokio::test]
async fn github_zip_resolution_prefers_vscode_then_cache() {
    let root = support::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let app_data = root.path().join("appdata");
    let extension = app_data
        .join("Code/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/22.1.6/clangd_22.1.6/bin/clangd.exe");
    support::create_dir_all(extension.parent().unwrap()).unwrap();
    compatible_test_executable(&extension);

    let mut resolver = test_resolver(root.path());
    resolver.vscode_app_data = Some(app_data);
    let mut server = Registry::builtin()
        .unwrap()
        .server("clangd")
        .unwrap()
        .clone();
    server.version_req = ">=1.0.0".into();
    let (version, executable) = match &server.install {
        InstallRecipe::GithubZip {
            version,
            executable,
            ..
        } => (version.clone(), executable.clone()),
        _ => unreachable!(),
    };
    let cached = github_zip_candidate(&resolver.paths.artifacts, &server.id, &version, &executable);
    support::create_dir_all(cached.parent().unwrap()).unwrap();
    compatible_test_executable(&cached);
    let extension_candidates =
        vscode_clangd_candidates_from(resolver.vscode_app_data.as_deref().unwrap());
    assert_eq!(
        extension_candidates.as_slice(),
        std::slice::from_ref(&extension)
    );
    resolver
        .probe_server(&server, &extension, &workspace)
        .await
        .unwrap();

    let resolution = resolver
        .resolve_github_zip_existing(&server, &workspace, &version, &executable)
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
    assert_eq!(resolution.path, extension);

    std::fs::remove_file(&resolution.path).unwrap();
    let resolution = resolver
        .resolve_github_zip_existing(&server, &workspace, &version, &executable)
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::Installed);
    assert_eq!(resolution.path, cached);
}

#[tokio::test]
async fn auto_install_false_does_not_create_github_zip_artifacts() {
    let root = support::tempdir().unwrap();
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    let mut server = Registry::builtin()
        .unwrap()
        .server("clangd")
        .unwrap()
        .clone();
    server.command = "missing-clangd-for-auto-install-test".into();
    let called = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&called);

    let error = resolver
        .resolve_server(&server, root.path(), None, move || async move {
            callback.store(true, Ordering::Relaxed);
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
    assert!(!called.load(Ordering::Relaxed));
    assert!(!resolver.paths.artifacts.join("clangd").exists());
}

#[tokio::test]
async fn github_zip_checksum_and_extraction_are_bounded() {
    let root = support::tempdir().unwrap();
    let archive = root.path().join("clangd.zip");
    write_test_zip(&archive, "clangd/bin/clangd.exe", b"binary");
    let expected = hex::encode(Sha256::digest(std::fs::read(&archive).unwrap()));
    verify_file_sha256(&archive, &expected).await.unwrap();
    assert!(verify_file_sha256(&archive, &"0".repeat(64)).await.is_err());

    let destination = root.path().join("expanded");
    extract_zip(&archive, &destination, 6).unwrap();
    assert_eq!(
        std::fs::read(destination.join("clangd/bin/clangd.exe")).unwrap(),
        b"binary"
    );
    assert!(extract_zip(&archive, &root.path().join("too-large"), 5).is_err());

    let unsafe_archive = root.path().join("unsafe.zip");
    write_test_zip(&unsafe_archive, "../outside.exe", b"bad");
    assert!(extract_zip(&unsafe_archive, &root.path().join("unsafe"), 16).is_err());
}

#[tokio::test]
async fn command_runner_bounds_output_and_reports_nonzero() {
    let root = support::tempdir().unwrap();
    #[cfg(windows)]
    let body = "for /L %%i in (1,1,5000) do <nul set /p \"=x\"\r\n>&2 echo failed\r\nexit /b 7";
    #[cfg(unix)]
    let body = "head -c 5000 /dev/zero | tr '\\0' x\necho failed >&2\nexit 7";
    let executable = fake_executable(root.path(), "bounded", body);
    let output = run_command(&executable, &[], root.path(), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout.len(), OUTPUT_LIMIT);
    assert!(bounded_text(&output.stderr).contains("failed"));
}

#[tokio::test]
async fn command_runner_times_out_and_reaps() {
    let root = support::tempdir().unwrap();
    #[cfg(windows)]
    let body = ":loop\r\ngoto loop";
    #[cfg(unix)]
    let body = "while :; do :; done";
    let executable = fake_executable(root.path(), "timeout", body);
    let error = run_command(&executable, &[], root.path(), Duration::from_millis(25))
        .await
        .unwrap_err();
    assert!(error.message.contains("timed out"));
}

#[test]
fn locates_project_and_global_npm_package_manifests() {
    let project = Path::new("C:/work/node_modules/.bin/pyright-langserver.cmd");
    assert!(
        npm_package_manifest_candidates(project, "pyright").contains(&(
            PathBuf::from("C:/work/node_modules/pyright/package.json"),
            PathBuf::from("C:/work/node_modules")
        ))
    );

    let global = Path::new("C:/Users/me/AppData/Roaming/npm/pyright-langserver.cmd");
    assert!(
        npm_package_manifest_candidates(global, "pyright").contains(&(
            PathBuf::from("C:/Users/me/AppData/Roaming/npm/node_modules/pyright/package.json"),
            PathBuf::from("C:/Users/me/AppData/Roaming/npm/node_modules")
        ))
    );
}

#[tokio::test]
async fn npm_server_version_comes_from_named_manifest_without_running_wrapper() {
    let root = support::tempdir().unwrap();
    let bin = root.path().join("node_modules/.bin");
    let package = root.path().join("node_modules/pyright");
    support::create_dir_all(&bin).unwrap();
    support::create_dir_all(&package).unwrap();
    let executable = bin.join("pyright-langserver.cmd");
    support::write(&executable, b"@exit /b 99").unwrap();
    support::write(
        package.join("package.json"),
        br#"{"name":"pyright","version":"1.1.405"}"#,
    )
    .unwrap();

    let probe = probe_npm_package(&executable, "pyright", ">=1.1.300, <2.0.0")
        .await
        .unwrap();
    assert_eq!(probe.version_output, "pyright 1.1.405");
    assert_eq!(probe.modules_root, root.path().join("node_modules"));
}

#[tokio::test]
async fn npm_global_resolution_uses_the_manager_modules_root() {
    let root = support::tempdir().unwrap();
    let bin = root.path().join("global-bin");
    let modules = root.path().join("global-store/node_modules");
    let package = modules.join("pyright");
    support::create_dir_all(&bin).unwrap();
    support::create_dir_all(&package).unwrap();
    let server = Registry::builtin()
        .unwrap()
        .server("pyright")
        .unwrap()
        .clone();
    support::write(bin.join(&executable_names(&server.command)[0]), b"wrapper").unwrap();
    support::write(
        package.join("package.json"),
        br#"{"name":"pyright","version":"1.1.405"}"#,
    )
    .unwrap();

    let resolution = test_resolver(root.path())
        .resolve_npm_in_roots(
            &server,
            &NpmRoots {
                bin,
                modules: modules.clone(),
            },
            false,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.npm_modules_root, Some(modules));
    assert_eq!(resolution.version_output, "pyright 1.1.405");
}

#[tokio::test]
async fn exact_npm_manifest_rejects_wrong_name_or_version() {
    let root = support::tempdir().unwrap();
    let package = root.path().join("pyright");
    support::create_dir(&package).unwrap();
    support::write(
        package.join("package.json"),
        br#"{"name":"other","version":"1.1.405"}"#,
    )
    .unwrap();
    assert!(
        verify_exact_npm_manifest(root.path(), "pyright", "1.1.405")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rustup_candidate_uses_reported_component_path() {
    let root = support::tempdir().unwrap();
    let resolver = test_resolver(root.path());
    let analyzer = root.path().join(&executable_names("rust-analyzer")[0]);
    support::write(&analyzer, b"binary").unwrap();
    let rustup = fake_executable(
        root.path(),
        "rustup",
        &format!("echo {}", analyzer.display()),
    );

    assert_eq!(
        resolver
            .rustup_candidate(&rustup, root.path(), true)
            .await
            .unwrap(),
        Some(analyzer)
    );
}

#[tokio::test]
async fn manual_recipe_blocks_without_starting_install() {
    let root = support::tempdir().unwrap();
    let mut server = Registry::builtin()
        .unwrap()
        .server("clangd")
        .unwrap()
        .clone();
    server.command = "missing-clangd-for-test".to_owned();
    server.install = InstallRecipe::Manual {
        version: "system".into(),
        hint: "install manually".into(),
    };
    let called = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&called);

    let error = test_resolver(root.path())
        .resolve_server(&server, root.path(), None, move || async move {
            callback.store(true, Ordering::Relaxed);
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
    assert!(!called.load(Ordering::Relaxed));
}

#[tokio::test]
async fn manual_clojure_reuses_an_explicit_compatible_server() {
    let root = support::tempdir().unwrap();
    let executable = fake_executable(
        root.path(),
        "clojure-lsp",
        "echo clojure-lsp 2026.07.06-14.34.19",
    );
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    let server = Registry::builtin()
        .unwrap()
        .server("clojure-lsp")
        .unwrap()
        .clone();
    let called = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&called);

    let resolution = resolver
        .resolve_server(
            &server,
            root.path(),
            Some(&executable),
            move || async move {
                callback.store(true, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::Explicit);
    assert_eq!(resolution.version_output, "clojure-lsp 2026.07.06-14.34.19");
    assert!(!called.load(Ordering::Relaxed));
}

#[tokio::test]
async fn sourcekit_probe_requires_help_and_a_compatible_swift_toolchain() {
    let root = support::tempdir().unwrap();
    let sourcekit = fake_executable(root.path(), "sourcekit-lsp", "echo sourcekit-help");
    let swift = fake_executable(
        root.path(),
        "swift",
        "echo swift-driver version: 1.120.2 Apple Swift version 6.1.2 (swift-6.1.2-RELEASE)",
    );
    let mut resolver = test_resolver(root.path());
    resolver.config.auto_install = false;
    let server = Registry::builtin()
        .unwrap()
        .server("sourcekit-lsp")
        .unwrap()
        .clone();

    let called = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&called);
    let resolution = resolver
        .resolve_server(&server, root.path(), Some(&sourcekit), move || async move {
            callback.store(true, Ordering::Relaxed);
        })
        .await
        .unwrap();
    assert_eq!(resolution.source, ExecutableSource::Explicit);
    assert!(!called.load(Ordering::Relaxed));

    let probe = resolver
        .probe_server(&server, &sourcekit, root.path())
        .await
        .unwrap();
    assert!(probe.version_output.contains("Swift version 6.1.2"));
    assert!(sourcekit_swift_candidates(&sourcekit).contains(&swift));

    let old_root = root.path().join("old");
    support::create_dir(&old_root).unwrap();
    let old_swift = fake_executable(
        &old_root,
        "swift-old",
        "echo Apple Swift version 5.8.1 (swift-5.8.1-RELEASE)",
    );
    let old_sourcekit = fake_executable(&old_root, "sourcekit-old", "echo sourcekit-help");
    let old_swift_target =
        old_sourcekit
            .parent()
            .unwrap()
            .join(if cfg!(windows) { "swift.cmd" } else { "swift" });
    std::fs::rename(old_swift, old_swift_target).unwrap();
    assert!(
        resolver
            .probe_server(&server, &old_sourcekit, root.path())
            .await
            .is_err()
    );
}

#[test]
fn sourcekit_swift_version_parser_ignores_swift_driver_version() {
    assert!(
        validate_sourcekit_swift_output(
            "swift-driver version: 1.120.2 Apple Swift version 6.1.2 (swift-6.1.2-RELEASE)",
            ">=5.9.0"
        )
        .is_ok()
    );
    assert!(validate_sourcekit_swift_output("Apple Swift version 5.8.1", ">=5.9.0").is_err());
    assert!(validate_sourcekit_swift_output("Swift version 6.3-dev", ">=5.9.0").is_ok());
    assert!(validate_sourcekit_swift_output("sourcekit-lsp help", ">=5.9.0").is_err());
}

#[test]
fn parses_bun_and_go_roots() {
    let bun = if cfg!(windows) {
        br#"C:\Users\me\.bun\install\global node_modules (3)"#.as_slice()
    } else {
        b"/home/me/.bun/install/global node_modules (3)"
    };
    let expected = if cfg!(windows) {
        PathBuf::from(r"C:\Users\me\.bun\install\global\node_modules")
    } else {
        PathBuf::from("/home/me/.bun/install/global/node_modules")
    };
    assert_eq!(bun_modules_root(bun).unwrap(), expected);

    let go = if cfg!(windows) {
        b"\r\nC:\\Users\\me\\go\r\n".as_slice()
    } else {
        b"\n/home/me/go\n".as_slice()
    };
    let expected = if cfg!(windows) {
        PathBuf::from(r"C:\Users\me\go\bin")
    } else {
        PathBuf::from("/home/me/go/bin")
    };
    assert_eq!(go_bin_from_env_output(go).unwrap(), Some(expected));
}

#[test]
fn relative_explicit_executables_are_workspace_relative() {
    let registry = Registry::builtin().unwrap();
    let candidates = local_candidates(
        registry.server("rust").unwrap(),
        Path::new("C:/work"),
        Some(Path::new("tools/rust-analyzer.exe")),
    );
    assert!(candidates.contains(&(
        ExecutableSource::Explicit,
        PathBuf::from("C:/work/tools/rust-analyzer.exe")
    )));
}

#[test]
fn parses_common_language_server_versions_and_enforces_ranges() {
    for (output, expected) in [
        ("v26.5.0", Version::new(26, 5, 0)),
        ("golang.org/x/tools/gopls v0.23.0", Version::new(0, 23, 0)),
        ("clangd version 18.1.8", Version::new(18, 1, 8)),
        (
            "rust-analyzer 1.88.0 (6b00bc388 2025-06-23)",
            Version::new(1, 88, 0),
        ),
        ("clojure-lsp 2026.07.06-14.34.19", Version::new(2026, 7, 6)),
        (
            "Dart SDK version: 3.8.1 (stable) on windows_x64",
            Version::new(3, 8, 1),
        ),
        (
            "deno 2.8.1 (stable, release, x86_64-pc-windows-msvc)\nv8 14.2.231.17-rusty\ntypescript 5.9.2",
            Version::new(2, 8, 1),
        ),
        ("gleam 1.18.1", Version::new(1, 18, 1)),
        ("LS-262.9593.0", Version::new(262, 9593, 0)),
        ("ILS-263.2689.0", Version::new(263, 2689, 0)),
        ("2.14.0.0", Version::new(2, 14, 0)),
        ("1.27.0", Version::new(1, 27, 0)),
    ] {
        assert_eq!(parse_version(output), Some(expected));
    }
    assert!(validate_version_output("tool v1.4.0", ">=1.0.0, <2.0.0").is_ok());
    assert!(validate_version_output("tool v2.0.0", ">=1.0.0, <2.0.0").is_err());
    assert!(validate_version_output("v20.0.0", ">=20.0.0").is_ok());
    assert!(validate_version_output("v19.9.0", ">=20.0.0").is_err());
    assert!(
        validate_version_output("clojure-lsp 2026.07.06-14.34.19", ">=2026.7.6, <2027.0.0").is_ok()
    );
    assert!(validate_version_output("2.14.0.0", ">=2.0.0, <3.0.0").is_ok());
    assert!(validate_version_output("3.0.0.0", ">=2.0.0, <3.0.0").is_err());
    assert_eq!(parse_version("2.14.0.0.1"), None);
    for invalid in [
        "2026.02.29-14.34.19",
        "2026.13.06-14.34.19",
        "2026.07.06-24.34.19",
        "2026.07.06-14.34",
    ] {
        assert_eq!(parse_version(invalid), None);
    }
    assert!(PRESERVED_ENV.contains(&"JAVA_HOME"));
    assert!(PRESERVED_ENV.contains(&"SystemDrive"));
    assert!(PRESERVED_ENV.contains(&"GEM_HOME"));
    assert!(PRESERVED_ENV.contains(&"RUBYOPT"));
    assert!(PRESERVED_ENV.contains(&"DEVELOPER_DIR"));
    assert!(PRESERVED_ENV.contains(&"TOOLCHAINS"));
    assert!(PRESERVED_ENV.contains(&"SDKROOT"));
    assert!(PRESERVED_ENV.contains(&"VCToolsInstallDir"));
}

#[test]
fn oxlint_version_output_is_supported() {
    assert_eq!(
        parse_version("Version: 1.78.0"),
        Some(Version::new(1, 78, 0))
    );
    assert!(validate_version_output("Version: 1.78.0", ">=1.78.0, <2.0.0").is_ok());
}

#[test]
fn executable_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let bin = directory.path().join("bin");
    support::create_dir(&bin).unwrap();
    let registry = Registry::builtin().unwrap();
    let server = registry.server("rust").unwrap();
    let executable = bin.join(&executable_names(&server.command)[0]);
    support::write(&executable, b"one").unwrap();
    let first = resolution_fingerprint(server, directory.path(), None);
    support::write(executable, b"different-size").unwrap();
    let second = resolution_fingerprint(server, directory.path(), None);
    assert_ne!(first, second);
}

#[cfg(windows)]
#[test]
fn windows_executable_candidates_include_batch_launchers() {
    assert_eq!(
        executable_names("language_server"),
        [
            "language_server.exe",
            "language_server.cmd",
            "language_server.bat",
            "language_server",
        ]
    );
}

#[test]
fn elixir_ls_version_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let executable = write_elixir_ls_release(directory.path(), "0.31.1");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(ELIXIR_LS_SERVER_ID).unwrap();
    let first = resolution_fingerprint(server, directory.path(), Some(&executable));
    support::write(executable.parent().unwrap().join("VERSION"), b"0.31.2").unwrap();
    let second = resolution_fingerprint(server, directory.path(), Some(&executable));
    assert_ne!(first, second);
}

#[test]
fn eslint_manifest_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    write_eslint_dependency(&workspace, "9.32.0");
    let executable = write_eslint_extension(directory.path(), "3.0.34");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(ESLINT_SERVER_ID).unwrap();
    let first = resolution_fingerprint(server, &workspace, Some(&executable));
    write_eslint_dependency(&workspace, "9.33.0");
    let second = resolution_fingerprint(server, &workspace, Some(&executable));
    assert_ne!(first, second);
}

#[test]
fn intelephense_manifest_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let executable = write_intelephense_extension(directory.path(), "1.18.5");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(INTELEPHENSE_SERVER_ID).unwrap();
    let manifest = vscode_intelephense_extension_root(&executable)
        .unwrap()
        .join("node_modules/intelephense/package.json");
    let first = resolution_fingerprint(server, &workspace, Some(&executable));
    support::write(manifest, br#"{"name":"intelephense","version":"1.18.6"}"#).unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&executable));
    assert_ne!(first, second);
}

#[test]
fn prisma_wasm_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let executable = write_prisma_extension(directory.path(), "31.11.0");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(PRISMA_SERVER_ID).unwrap();
    let wasm = vscode_prisma_extension_root(&executable)
        .unwrap()
        .join("dist/language-server/prisma_schema_build_bg.wasm");
    let first = resolution_fingerprint(server, &workspace, Some(&executable));
    support::write(wasm, b"different-wasm").unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&executable));
    assert_ne!(first, second);
}

#[test]
fn pyright_manifest_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let executable = write_pyright_extension(directory.path(), "1.1.411");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(PYRIGHT_SERVER_ID).unwrap();
    let manifest = vscode_pyright_extension_root(&executable)
        .unwrap()
        .join("package.json");
    let first = resolution_fingerprint(server, &workspace, Some(&executable));
    support::write(
        manifest,
        br#"{"name":"pyright","publisher":"ms-pyright","version":"1.1.410"}"#,
    )
    .unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&executable));
    assert_ne!(first, second);
}

#[test]
fn jdtls_core_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let (launcher, _) = write_jdtls_extension(
        directory.path(),
        "1.55.0-win32-x64",
        "1.55.0",
        "1.60.0.202607010000",
        "21.0.3",
    );
    let registry = Registry::builtin().unwrap();
    let server = registry.server(JDTLS_SERVER_ID).unwrap();
    let core = jdtls_extension_layout(&launcher).unwrap().core;
    let first = resolution_fingerprint(server, &workspace, Some(&launcher));
    support::write(core, b"different-core").unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&launcher));
    assert_ne!(first, second);
}

#[test]
fn julials_environment_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let (project, _) = write_julials_extension(directory.path(), "1.231.1", "v1.11", "5.1.0");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(JULIALS_SERVER_ID).unwrap();
    let manifest = julials_extension_layout(&project).unwrap().manifest;
    let first = resolution_fingerprint(server, &workspace, Some(&project));
    let mut contents = std::fs::read_to_string(&manifest).unwrap();
    contents.push_str("\n# changed\n");
    support::write(manifest, contents).unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&project));
    assert_ne!(first, second);
}

#[test]
fn kotlin_bundle_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let launcher =
        write_kotlin_extension(directory.path(), "0.0.8-win32-x64", "0.0.8", "263.2689.0");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(KOTLIN_LS_SERVER_ID).unwrap();
    let build_file = kotlin_extension_layout(&launcher).unwrap().build_file;
    let first = resolution_fingerprint(server, &workspace, Some(&launcher));
    support::write(build_file, b"ILS-263.2689.1\n").unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&launcher));
    assert_ne!(first, second);
}

#[test]
fn lua_runtime_identity_changes_the_resolution_fingerprint() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    support::create_dir(&workspace).unwrap();
    let executable = write_lua_extension(directory.path(), "3.19.0-win32-x64", "3.19.0");
    let registry = Registry::builtin().unwrap();
    let server = registry.server(LUA_LS_SERVER_ID).unwrap();
    let server_main = lua_extension_layout(&executable).unwrap().server_main;
    let first = resolution_fingerprint(server, &workspace, Some(&executable));
    support::write(server_main, b"return false -- changed").unwrap();
    let second = resolution_fingerprint(server, &workspace, Some(&executable));
    assert_ne!(first, second);
}
