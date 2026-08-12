use super::*;

use crate::test_support as support;

#[test]
fn rejects_parent_and_sibling_paths() {
    let parent = support::tempdir().unwrap();
    let root = parent.path().join("workspace");
    support::create_dir(&root).unwrap();
    support::write(root.join("inside.rs"), "fn main() {}").unwrap();
    support::write(parent.path().join("outside.rs"), "").unwrap();
    let workspace = Workspace::open(&root).unwrap();

    assert!(workspace.resolve_file("inside.rs", 1_024).is_ok());
    assert_eq!(
        workspace
            .resolve_file("../outside.rs", 1_024)
            .unwrap_err()
            .code,
        ErrorCode::PathOutsideWorkspace
    );
    assert_eq!(
        workspace
            .resolve_file(parent.path().join("outside.rs"), 1_024)
            .unwrap_err()
            .code,
        ErrorCode::PathOutsideWorkspace
    );
}

#[test]
fn resolves_missing_edit_targets_through_the_nearest_existing_ancestor() {
    let parent = support::tempdir().unwrap();
    let root = parent.path().join("workspace");
    support::create_dir(&root).unwrap();
    let workspace = Workspace::open(&root).unwrap();

    let target = workspace.resolve_candidate("new/nested.rs").unwrap();
    assert_eq!(
        target,
        fs::canonicalize(&root).unwrap().join("new/nested.rs")
    );
    assert_eq!(
        workspace.relative_candidate(&target).unwrap(),
        PathBuf::from("new/nested.rs")
    );
    assert!(workspace.resolve_candidate("../outside.rs").is_err());
    assert!(
        workspace
            .resolve_candidate(parent.path().join("outside.rs"))
            .is_err()
    );
}

#[test]
fn discovers_nearest_monorepo_roots() {
    let root = support::tempdir().unwrap();
    let nested = root.path().join("crate");
    support::create_dir(&nested).unwrap();
    support::write(
        nested.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'",
    )
    .unwrap();
    support::write(nested.join("lib.rs"), "pub fn answer() -> u8 { 42 }").unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let result = workspace.discover(&Registry::builtin().unwrap(), &DiscoveryConfig::default());
    let rust = result
        .matches
        .iter()
        .find(|item| item.server_id == "rust")
        .unwrap();
    assert_eq!(rust.root, fs::canonicalize(nested).unwrap());
}

#[test]
fn file_detection_precedence_matrix_is_stable() {
    let root = support::tempdir().unwrap();
    let rust_root = root.path().join("rust");
    let deno_root = root.path().join("deno");
    let java_root = root.path().join("java");
    let kotlin_root = root.path().join("kotlin");
    let go_module = root.path().join("go/module");
    let scripts = root.path().join("scripts");
    let loose = root.path().join("loose");
    for directory in [
        &rust_root,
        &deno_root,
        &java_root.join("src"),
        &kotlin_root.join("src"),
        &go_module,
        &scripts,
        &loose,
    ] {
        support::create_dir_all(directory).unwrap();
    }
    support::write(rust_root.join("Cargo.toml"), "[workspace]\n").unwrap();
    support::write(deno_root.join("deno.json"), "{}").unwrap();
    support::write(java_root.join("settings.gradle"), "").unwrap();
    support::write(kotlin_root.join("settings.gradle.kts"), "").unwrap();
    support::write(root.path().join("go.work"), "go 1.26\n").unwrap();
    support::write(go_module.join("go.mod"), "module example.com/demo\n").unwrap();

    let rust_file = rust_root.join("lib.rs");
    let deno_file = deno_root.join("main.ts");
    let java_file = java_root.join("src/Main.java");
    let kotlin_file = kotlin_root.join("src/Main.kt");
    let go_file = go_module.join("main.go");
    let ruby_file = scripts.join("task.rb");
    let loose_java = loose.join("Loose.java");
    for file in [
        &rust_file,
        &deno_file,
        &java_file,
        &kotlin_file,
        &go_file,
        &ruby_file,
        &loose_java,
    ] {
        support::write(file, "").unwrap();
    }

    let workspace = Workspace::open(root.path()).unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace_root = workspace.root().to_path_buf();
    let cases = [
        ("nested marker", &rust_file, "rust", Some(&rust_root)),
        (
            "workspace fallback",
            &ruby_file,
            "ruby-lsp",
            Some(&workspace_root),
        ),
        ("deno selected", &deno_file, "deno", Some(&deno_root)),
        ("typescript suppressed", &deno_file, "typescript", None),
        ("gradle java", &java_file, JDTLS_SERVER_ID, Some(&java_root)),
        ("loose java skipped", &loose_java, JDTLS_SERVER_ID, None),
        (
            "kotlin settings",
            &kotlin_file,
            KOTLIN_LS_SERVER_ID,
            Some(&kotlin_root),
        ),
        (
            "go.work precedence",
            &go_file,
            GOPLS_SERVER_ID,
            Some(&workspace_root),
        ),
    ];
    for (name, file, server_id, expected_root) in cases {
        let extension = file.extension().unwrap().to_str().unwrap();
        let selected = workspace
            .detect_file(file, extension, &registry)
            .into_iter()
            .find(|(_, detection)| detection.server_id == server_id)
            .map(|(_, detection)| fs::canonicalize(detection.root).unwrap());
        let expected_root = expected_root.map(|root| fs::canonicalize(root).unwrap());
        assert_eq!(selected, expected_root, "{name}");
    }
}

#[test]
fn elixir_files_use_the_nearest_mix_root() {
    let root = support::tempdir().unwrap();
    let nested = root.path().join("apps/example");
    support::create_dir_all(&nested).unwrap();
    support::write(
        nested.join("mix.exs"),
        "defmodule Example.MixProject do\nend",
    )
    .unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();

    for name in ["example.ex", "example.exs"] {
        let file = nested.join(name);
        support::write(&file, "defmodule Example do\nend").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["elixir-ls"]
        );
        assert_eq!(
            workspace.root_for_file(&file, registry.server("elixir-ls").unwrap()),
            nested
        );
    }
}

#[test]
fn wildcard_markers_find_the_nearest_csharp_project() {
    let root = support::tempdir().unwrap();
    let nested = root.path().join("src/Demo");
    support::create_dir_all(&nested).unwrap();
    support::write(nested.join("Demo.csproj"), "<Project />").unwrap();
    let source = nested.join("Program.cs");
    support::write(&source, "class Program {}").unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let registry = Registry::builtin().unwrap();

    assert_eq!(
        workspace.root_for_file(&source, registry.server("csharp").unwrap()),
        nested
    );
    assert_eq!(
        workspace.root_for_file(
            &root.path().join("Loose.cs"),
            registry.server("csharp").unwrap()
        ),
        fs::canonicalize(root.path()).unwrap()
    );
    assert!(!marker_exists(root.path(), "global.json"));
    support::write(root.path().join("global.json"), "{}").unwrap();
    assert!(marker_exists(root.path(), "global.json"));
}

#[test]
fn fsharp_files_use_the_nearest_project_root() {
    let root = support::tempdir().unwrap();
    let nested = root.path().join("src/Demo");
    support::create_dir_all(&nested).unwrap();
    support::write(nested.join("Demo.fsproj"), "<Project />").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();

    for name in ["Demo.fs", "Types.fsi", "Script.fsx", "Build.fsscript"] {
        let file = nested.join(name);
        support::write(&file, "module Demo").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["fsharp"]
        );
        assert_eq!(
            workspace.root_for_file(&file, registry.server("fsharp").unwrap()),
            nested
        );
    }
}

#[test]
fn gleam_files_use_the_nearest_project_root_or_workspace() {
    let root = support::tempdir().unwrap();
    let nested = root.path().join("apps/example");
    let source_dir = nested.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(nested.join("gleam.toml"), "name = \"example\"").unwrap();
    let file = source_dir.join("main.gleam");
    support::write(&file, "pub fn main() { Nil }").unwrap();
    let loose = root.path().join("loose.gleam");
    support::write(&loose, "pub fn loose() { Nil }").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();

    assert_eq!(
        workspace
            .matching_servers(&file, "gleam", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["gleam"]
    );
    let gleam = registry.server("gleam").unwrap();
    assert_eq!(workspace.root_for_file(&file, gleam), nested);
    assert_eq!(
        workspace.root_for_file(&loose, gleam),
        fs::canonicalize(root.path()).unwrap()
    );
}

#[test]
fn gopls_prefers_go_work_then_the_nearest_module_marker_or_workspace() {
    let root = support::tempdir().unwrap();
    let module = root.path().join("nested/module");
    support::create_dir_all(&module).unwrap();
    let source = module.join("main.go");
    support::write(&source, "package main").unwrap();
    support::write(root.path().join("go.work"), "go 1.26\nuse ./nested/module").unwrap();
    support::write(module.join("go.mod"), "module example.com/demo\ngo 1.26").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let gopls = registry.server(GOPLS_SERVER_ID).unwrap();
    let selected_root = || fs::canonicalize(workspace.root_for_file(&source, gopls)).unwrap();

    assert_eq!(selected_root(), fs::canonicalize(root.path()).unwrap());
    fs::remove_file(root.path().join("go.work")).unwrap();
    assert_eq!(selected_root(), fs::canonicalize(&module).unwrap());
    fs::remove_file(module.join("go.mod")).unwrap();
    support::write(module.join("go.sum"), "").unwrap();
    assert_eq!(selected_root(), fs::canonicalize(&module).unwrap());
    fs::remove_file(module.join("go.sum")).unwrap();
    assert_eq!(selected_root(), fs::canonicalize(root.path()).unwrap());
}

#[test]
fn jdtls_uses_opencode_gradle_precedence() {
    let root = support::tempdir().unwrap();
    let app = root.path().join("app");
    let module = app.join("module");
    let source_dir = module.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(
        root.path().join("settings.gradle"),
        "rootProject.name = 'demo'",
    )
    .unwrap();
    support::write(app.join("gradlew"), "").unwrap();
    support::write(module.join("build.gradle"), "").unwrap();
    let source = source_dir.join("Main.java");
    support::write(&source, "class Main {}").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let jdtls = registry.server(JDTLS_SERVER_ID).unwrap();

    assert_eq!(workspace.root_for_file(&source, jdtls), root.path());
    fs::remove_file(root.path().join("settings.gradle")).unwrap();
    assert_eq!(workspace.root_for_file(&source, jdtls), app);
    fs::remove_file(app.join("gradlew")).unwrap();
    assert_eq!(workspace.root_for_file(&source, jdtls), module);
}

#[test]
fn jdtls_climbs_only_declared_maven_modules() {
    let root = support::tempdir().unwrap();
    let module = root.path().join("apps/demo");
    let source_dir = module.join("src/main/java");
    support::create_dir_all(&source_dir).unwrap();
    support::write(module.join("pom.xml"), "<project />").unwrap();
    support::write(
        root.path().join("pom.xml"),
        "<project><modules><!-- <module>apps/demo</module> --><module>apps/sibling</module></modules></project>",
    )
    .unwrap();
    let source = source_dir.join("Main.java");
    support::write(&source, "class Main {}").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let jdtls = registry.server(JDTLS_SERVER_ID).unwrap();

    assert_eq!(workspace.root_for_file(&source, jdtls), module);
    support::write(
        root.path().join("pom.xml"),
        "<project><modules><module>apps/demo</module></modules></project>",
    )
    .unwrap();
    assert_eq!(workspace.root_for_file(&source, jdtls), root.path());
}

#[test]
fn jdtls_uses_eclipse_markers_but_skips_loose_java_files() {
    let root = support::tempdir().unwrap();
    let project = root.path().join("eclipse-project");
    support::create_dir(&project).unwrap();
    support::write(project.join(".classpath"), "<classpath />").unwrap();
    let source = project.join("Main.java");
    let loose = root.path().join("Loose.java");
    support::write(&source, "class Main {}").unwrap();
    support::write(&loose, "class Loose {}").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();

    assert_eq!(
        workspace
            .matching_servers(&source, "java", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        [JDTLS_SERVER_ID]
    );
    assert_eq!(
        workspace.root_for_file(&source, registry.server(JDTLS_SERVER_ID).unwrap()),
        project
    );
    assert!(
        workspace
            .matching_servers(&loose, "java", &registry)
            .is_empty()
    );
}

#[test]
fn jdtls_never_uses_markers_above_the_workspace() {
    let parent = support::tempdir().unwrap();
    let root = parent.path().join("workspace");
    support::create_dir(&root).unwrap();
    support::write(parent.path().join("settings.gradle"), "").unwrap();
    let source = root.join("Loose.java");
    support::write(&source, "class Loose {}").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&root).unwrap();

    assert!(
        workspace
            .matching_servers(&source, "java", &registry)
            .is_empty()
    );
}

#[test]
fn kotlin_uses_opencode_root_precedence_and_workspace_boundary() {
    let parent = support::tempdir().unwrap();
    let root = parent.path().join("workspace");
    let app = root.join("app");
    let module = app.join("module");
    let project = module.join("project");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(parent.path().join("settings.gradle"), "").unwrap();
    support::write(root.join("settings.gradle.kts"), "").unwrap();
    support::write(app.join("gradlew.bat"), "").unwrap();
    support::write(module.join("build.gradle.kts"), "").unwrap();
    support::write(project.join("pom.xml"), "<project />").unwrap();
    let source = source_dir.join("Main.kt");
    support::write(&source, "fun main() = Unit").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&root).unwrap();
    let kotlin = registry.server(KOTLIN_LS_SERVER_ID).unwrap();

    assert_eq!(workspace.root_for_file(&source, kotlin), root);
    fs::remove_file(root.join("settings.gradle.kts")).unwrap();
    assert_eq!(workspace.root_for_file(&source, kotlin), app);
    fs::remove_file(app.join("gradlew.bat")).unwrap();
    assert_eq!(workspace.root_for_file(&source, kotlin), module);
    fs::remove_file(module.join("build.gradle.kts")).unwrap();
    assert_eq!(workspace.root_for_file(&source, kotlin), project);
    fs::remove_file(project.join("pom.xml")).unwrap();
    assert_eq!(
        workspace.root_for_file(&source, kotlin),
        fs::canonicalize(&root).unwrap()
    );
}

#[test]
fn haskell_files_use_the_nearest_project_root_or_workspace() {
    let root = support::tempdir().unwrap();
    let project = root.path().join("apps/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(project.join("demo.cabal"), "cabal-version: 3.0").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let hls = registry.server("hls").unwrap();

    for name in ["Main.hs", "Notes.lhs"] {
        let file = source_dir.join(name);
        support::write(&file, "module Demo where").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["hls"]
        );
        assert_eq!(workspace.root_for_file(&file, hls), project);
    }

    fs::remove_file(project.join("demo.cabal")).unwrap();
    assert_eq!(
        workspace.root_for_file(&source_dir.join("Main.hs"), hls),
        fs::canonicalize(root.path()).unwrap()
    );
}

#[test]
fn python_files_use_the_nearest_opencode_marker_within_the_workspace() {
    let root = support::tempdir().unwrap();
    let workspace_root = root.path().join("workspace");
    let project = workspace_root.join("packages/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(root.path().join("pyproject.toml"), "").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let pyright = registry.server("pyright").unwrap();

    for name in ["demo.py", "demo.pyi"] {
        let file = source_dir.join(name);
        support::write(&file, "value: str = 42").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["pyright"]
        );
        for marker in &pyright.markers {
            let marker = project.join(marker);
            support::write(&marker, "").unwrap();
            assert_eq!(workspace.root_for_file(&file, pyright), project);
            fs::remove_file(marker).unwrap();
        }
        assert_eq!(
            workspace.root_for_file(&file, pyright),
            fs::canonicalize(&workspace_root).unwrap()
        );
    }
}

#[test]
fn ruby_files_use_the_nearest_gemfile_within_the_workspace() {
    let parent = support::tempdir().unwrap();
    let workspace_root = parent.path().join("workspace");
    let project = workspace_root.join("packages/demo");
    let source_dir = project.join("lib");
    support::create_dir_all(&source_dir).unwrap();
    support::write(parent.path().join("Gemfile"), "").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let ruby = registry.server("ruby-lsp").unwrap();

    for name in ["demo.rb", "Rakefile.rake", "demo.gemspec", "config.ru"] {
        let file = source_dir.join(name);
        support::write(&file, "value = 42").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["ruby-lsp"]
        );
        assert_eq!(
            workspace.root_for_file(&file, ruby),
            fs::canonicalize(&workspace_root).unwrap()
        );
        support::write(project.join("Gemfile"), "").unwrap();
        assert_eq!(workspace.root_for_file(&file, ruby), project);
        fs::remove_file(project.join("Gemfile")).unwrap();
    }
}

#[test]
fn julia_files_use_the_nearest_opencode_marker_or_workspace() {
    let root = support::tempdir().unwrap();
    let project = root.path().join("packages/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(root.path().join("Project.toml"), "[deps]").unwrap();
    support::write(project.join("Manifest.toml"), "manifest_format = \"2.0\"").unwrap();
    let file = source_dir.join("Demo.jl");
    support::write(&file, "module Demo\nend").unwrap();
    let pending = project.join("pending/Demo.jl");
    support::create_dir_all(pending.parent().unwrap()).unwrap();
    let loose = root.path().join("loose/Loose.jl");
    support::create_dir_all(loose.parent().unwrap()).unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let julials = registry.server("julials").unwrap();

    assert_eq!(
        workspace
            .matching_servers(&file, "jl", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["julials"]
    );
    assert_eq!(workspace.root_for_file(&file, julials), source_dir);
    assert_eq!(workspace.root_for_file(&pending, julials), project);
    fs::remove_file(project.join("Manifest.toml")).unwrap();
    assert_eq!(workspace.root_for_file(&pending, julials), root.path());
    fs::remove_file(root.path().join("Project.toml")).unwrap();
    assert_eq!(
        workspace.root_for_file(&loose, julials),
        fs::canonicalize(root.path()).unwrap()
    );
}

#[test]
fn lua_files_use_the_nearest_opencode_marker_within_the_workspace() {
    let root = support::tempdir().unwrap();
    let workspace_root = root.path().join("workspace");
    let project = workspace_root.join("packages/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(root.path().join(".luarc.json"), "{}").unwrap();
    let file = source_dir.join("main.lua");
    support::write(&file, "local value = 1").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let lua = registry.server("lua-ls").unwrap();

    assert_eq!(
        workspace
            .matching_servers(&file, "lua", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["lua-ls"]
    );
    for marker in &lua.markers {
        let marker = project.join(marker);
        support::write(&marker, "{}").unwrap();
        assert_eq!(workspace.root_for_file(&file, lua), project);
        fs::remove_file(marker).unwrap();
    }
    assert_eq!(
        workspace.root_for_file(&file, lua),
        fs::canonicalize(&workspace_root).unwrap()
    );
}

#[test]
fn ocaml_files_use_the_nearest_opencode_marker_within_the_workspace() {
    let root = support::tempdir().unwrap();
    let workspace_root = root.path().join("workspace");
    let project = workspace_root.join("packages/demo");
    let source_dir = project.join("lib");
    support::create_dir_all(&source_dir).unwrap();
    support::write(root.path().join("opam"), "").unwrap();
    support::write(workspace_root.join("dune-project"), "(lang dune 3.0)").unwrap();
    support::write(project.join(".merlin"), "B _build/default").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let ocaml = registry.server("ocaml-lsp").unwrap();

    for name in ["demo.ml", "demo.mli"] {
        let file = source_dir.join(name);
        support::write(&file, "let value = 1").unwrap();
        assert_eq!(
            workspace
                .matching_servers(
                    &file,
                    file.extension().unwrap().to_str().unwrap(),
                    &registry
                )
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["ocaml-lsp"]
        );
        assert_eq!(workspace.root_for_file(&file, ocaml), project);
    }

    fs::remove_file(project.join(".merlin")).unwrap();
    assert_eq!(
        workspace.root_for_file(&source_dir.join("demo.ml"), ocaml),
        workspace_root
    );
    fs::remove_file(workspace_root.join("dune-project")).unwrap();
    assert_eq!(
        workspace.root_for_file(&source_dir.join("demo.ml"), ocaml),
        fs::canonicalize(&workspace_root).unwrap()
    );
}

#[test]
fn php_files_use_the_nearest_opencode_marker_within_the_workspace() {
    let parent = support::tempdir().unwrap();
    let workspace_root = parent.path().join("workspace");
    let project = workspace_root.join("apps/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(parent.path().join("composer.json"), "{}").unwrap();
    let file = source_dir.join("index.php");
    support::write(&file, "<?php echo $missing;").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let intelephense = registry.server("intelephense").unwrap();

    assert_eq!(
        workspace
            .matching_servers(&file, "php", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["intelephense"]
    );
    assert_eq!(
        workspace.root_for_file(&file, intelephense),
        fs::canonicalize(&workspace_root).unwrap()
    );
    for marker in &intelephense.markers {
        let marker = project.join(marker);
        support::write(&marker, "{}").unwrap();
        assert_eq!(workspace.root_for_file(&file, intelephense), project);
        fs::remove_file(marker).unwrap();
    }
}

#[test]
fn prisma_files_use_the_nearest_schema_root_within_the_workspace() {
    let parent = support::tempdir().unwrap();
    let workspace_root = parent.path().join("workspace");
    let project = workspace_root.join("apps/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(parent.path().join("package.json"), "{}").unwrap();
    support::write(project.join("package.json"), "{}").unwrap();
    let file = source_dir.join("model.prisma");
    support::write(&file, "model User { id Int @id }").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let prisma = registry.server("prisma").unwrap();

    assert_eq!(
        workspace
            .matching_servers(&file, "prisma", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["prisma"]
    );
    assert_eq!(
        workspace.root_for_file(&file, prisma),
        fs::canonicalize(&workspace_root).unwrap()
    );

    let schema = project.join("prisma/schema.prisma");
    support::create_dir_all(schema.parent().unwrap()).unwrap();
    support::write(&schema, "datasource db { provider = \"sqlite\" }").unwrap();
    assert_eq!(workspace.root_for_file(&file, prisma), project);

    let nested = project.join("prisma/models/user.prisma");
    support::create_dir_all(nested.parent().unwrap()).unwrap();
    support::write(&nested, "model User { id Int @id }").unwrap();
    assert_eq!(
        workspace.root_for_file(&nested, prisma),
        project.join("prisma")
    );
    fs::remove_file(project.join("prisma/schema.prisma")).unwrap();
    assert_eq!(workspace.root_for_file(&nested, prisma), project);
    support::write(project.join("prisma/schema.prisma"), "").unwrap();
    assert_eq!(
        workspace.root_for_file(&nested, prisma),
        project.join("prisma")
    );
}

#[test]
fn oxlint_uses_the_nearest_opencode_marker_and_coexists_with_astro() {
    let parent = support::tempdir().unwrap();
    let workspace_root = parent.path().join("workspace");
    let project = workspace_root.join("apps/demo");
    let source_dir = project.join("src");
    support::create_dir_all(&source_dir).unwrap();
    support::write(parent.path().join("package.json"), "{}").unwrap();
    let script = source_dir.join("main.ts");
    let astro = source_dir.join("page.astro");
    support::write(&script, "export const value = 1;").unwrap();
    support::write(&astro, "---\nconst value = 1;\n---").unwrap();
    let registry = Registry::builtin().unwrap();
    let workspace = Workspace::open(&workspace_root).unwrap();
    let oxlint = registry.server("oxlint").unwrap();

    assert_eq!(
        workspace
            .matching_servers(&script, "ts", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["eslint", "typescript", "oxlint"]
    );
    assert_eq!(
        workspace
            .matching_servers(&astro, "astro", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["astro", "oxlint"]
    );
    assert_eq!(
        workspace.root_for_file(&script, oxlint),
        fs::canonicalize(&workspace_root).unwrap()
    );

    for marker in &oxlint.markers {
        let marker = project.join(marker);
        support::write(&marker, "{}").unwrap();
        assert_eq!(workspace.root_for_file(&script, oxlint), project);
        fs::remove_file(marker).unwrap();
    }
}

#[test]
fn deno_replaces_typescript_while_eslint_coexists_at_the_nearest_lock_root() {
    let root = support::tempdir().unwrap();
    let deno_root = root.path().join("deno-app");
    let node_root = root.path().join("node-app");
    support::create_dir_all(&deno_root).unwrap();
    support::create_dir_all(&node_root).unwrap();
    support::write(deno_root.join("deno.json"), "{}").unwrap();
    support::write(deno_root.join("package.json"), "{}").unwrap();
    support::write(deno_root.join("bun.lock"), "").unwrap();
    support::write(node_root.join("package.json"), "{}").unwrap();
    support::write(node_root.join("bun.lock"), "").unwrap();
    let deno_file = deno_root.join("main.ts");
    let deno_js_file = deno_root.join("main.js");
    let node_file = node_root.join("main.ts");
    support::write(&deno_file, "export const deno = true;").unwrap();
    support::write(&deno_js_file, "export const deno = true;").unwrap();
    support::write(&node_file, "export const node = true;").unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    let registry = Registry::builtin().unwrap();

    assert_eq!(
        workspace
            .matching_servers(&deno_file, "ts", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["deno", "eslint", "oxlint"]
    );
    assert_eq!(
        workspace
            .matching_servers(&deno_js_file, "js", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["deno", "eslint", "oxlint"]
    );
    assert_eq!(
        workspace.root_for_file(&deno_file, registry.server("deno").unwrap()),
        deno_root
    );
    assert_eq!(
        workspace.root_for_file(&deno_file, registry.server("eslint").unwrap()),
        deno_root
    );
    assert_eq!(
        workspace.root_for_file(&deno_file, registry.server("oxlint").unwrap()),
        deno_root
    );
    assert_eq!(
        workspace
            .matching_servers(&node_file, "ts", &registry)
            .into_iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["eslint", "typescript", "oxlint"]
    );
    assert_eq!(
        workspace.root_for_file(&node_file, registry.server("eslint").unwrap()),
        node_root
    );
    assert_eq!(
        workspace.root_for_file(&node_file, registry.server("oxlint").unwrap()),
        node_root
    );
}

#[test]
fn path_comparison_requires_a_component_boundary() {
    let root = normalize_for_comparison(Path::new("C:/work/project"));
    assert!(path_is_within(
        &root,
        Path::new("C:/work/project/src/lib.rs")
    ));
    assert!(!path_is_within(
        &root,
        Path::new("C:/work/project-other/lib.rs")
    ));
}
