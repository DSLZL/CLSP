use super::*;

#[test]
fn builtin_is_the_closed_twenty_seven_server_set() {
    let registry = Registry::builtin().unwrap();
    assert_eq!(registry.server.len(), 27);
    assert_eq!(
        registry
            .server
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>(),
        APPROVED_IDS.into_iter().collect()
    );
}

#[test]
fn matches_only_declared_extensions() {
    let registry = Registry::builtin().unwrap();
    assert_eq!(
        registry
            .matching_extension(".astro")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["astro", "oxlint"]
    );
    assert_eq!(
        registry.matching_extension(".rs").next().unwrap().id,
        "rust"
    );
    assert_eq!(
        registry
            .matching_extension(".SH")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["bash"]
    );
    for extension in [".swift", ".objc", ".objcpp"] {
        assert_eq!(
            registry
                .matching_extension(extension)
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sourcekit-lsp"]
        );
    }
    assert_eq!(
        registry
            .matching_extension(".CLJC")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["clojure-lsp"]
    );
    assert_eq!(
        registry
            .matching_extension(".DART")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["dart"]
    );
    assert_eq!(
        registry
            .matching_extension(".EXS")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["elixir-ls"]
    );
    assert_eq!(
        registry
            .matching_extension(".VUE")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["eslint", "oxlint"]
    );
    assert_eq!(
        registry
            .matching_extension(".FSX")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fsharp"]
    );
    assert_eq!(
        registry
            .matching_extension(".GLEAM")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gleam"]
    );
    assert_eq!(
        registry
            .matching_extension(".LHS")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["hls"]
    );
    assert_eq!(
        registry
            .matching_extension(".JAVA")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["jdtls"]
    );
    assert_eq!(
        registry
            .matching_extension(".JL")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["julials"]
    );
    assert_eq!(
        registry
            .matching_extension(".KTS")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["kotlin-ls"]
    );
    assert_eq!(
        registry
            .matching_extension(".LUA")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["lua-ls"]
    );
    for extension in [".ML", ".MLI"] {
        assert_eq!(
            registry
                .matching_extension(extension)
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ocaml-lsp"]
        );
    }
    assert_eq!(
        registry
            .matching_extension(".PHP")
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        vec!["intelephense"]
    );
    for extension in [".RB", ".RAKE", ".GEMSPEC", ".RU"] {
        assert_eq!(
            registry
                .matching_extension(extension)
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ruby-lsp"]
        );
    }
}

#[test]
fn astro_uses_the_locked_official_language_server() {
    let registry = Registry::builtin().unwrap();
    let astro = registry.server("astro").unwrap();
    assert_eq!(astro.command, "astro-ls");
    assert_eq!(astro.args, ["--stdio"]);
    assert_eq!(
        astro.markers,
        ["astro.config.js", "astro.config.mjs", "astro.config.ts"]
    );
    let InstallRecipe::Npm {
        version,
        package,
        companions,
    } = &astro.install
    else {
        panic!("Astro must use the npm recipe");
    };
    assert_eq!(version, "2.16.13");
    assert_eq!(package, "@astrojs/language-server");
    assert_eq!(companions, &vec!["typescript@5.9.2".to_owned()]);
}

#[test]
fn bash_uses_the_locked_official_language_server() {
    let registry = Registry::builtin().unwrap();
    let bash = registry.server("bash").unwrap();
    assert_eq!(bash.language_id, "shellscript");
    assert_eq!(bash.version_req, ">=5.6.0, <6.0.0");
    assert_eq!(bash.extensions, ["sh", "bash", "zsh", "ksh"]);
    assert_eq!(bash.markers, [".shellcheckrc"]);
    assert_eq!(bash.command, "bash-language-server");
    assert_eq!(bash.args, ["start"]);
    let InstallRecipe::Npm {
        version,
        package,
        companions,
    } = &bash.install
    else {
        panic!("Bash must use the npm recipe");
    };
    assert_eq!(version, "5.6.0");
    assert_eq!(package, "bash-language-server");
    assert!(companions.is_empty());
}

#[test]
fn pyright_uses_the_locked_opencode_contract() {
    let registry = Registry::builtin().unwrap();
    let pyright = registry.server("pyright").unwrap();
    assert_eq!(pyright.display_name, "Pyright");
    assert_eq!(pyright.language_id, "python");
    assert_eq!(pyright.version_req, ">=1.1.300, <2.0.0");
    assert_eq!(pyright.extensions, ["py", "pyi"]);
    assert_eq!(
        pyright.markers,
        [
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
            "pyrightconfig.json",
        ]
    );
    assert_eq!(pyright.command, "pyright-langserver");
    assert_eq!(pyright.args, ["--stdio"]);
    let InstallRecipe::Npm {
        version,
        package,
        companions,
    } = &pyright.install
    else {
        panic!("Pyright must use the npm recipe");
    };
    assert_eq!(version, "1.1.411");
    assert_eq!(package, "pyright");
    assert!(companions.is_empty());
}

#[test]
fn ruby_lsp_uses_the_locked_shopify_contract() {
    let registry = Registry::builtin().unwrap();
    let ruby = registry.server("ruby-lsp").unwrap();
    assert_eq!(ruby.display_name, "Ruby LSP");
    assert_eq!(ruby.language_id, "ruby");
    assert_eq!(ruby.version_req, ">=0.26.10, <0.27.0");
    assert_eq!(ruby.extensions, ["rb", "rake", "gemspec", "ru"]);
    assert_eq!(ruby.markers, ["Gemfile"]);
    assert_eq!(ruby.command, "ruby-lsp");
    assert!(ruby.args.is_empty());
    assert_eq!(ruby.version_args, ["--version"]);
    let InstallRecipe::Command {
        version,
        program,
        args,
    } = &ruby.install
    else {
        panic!("Ruby LSP must use the gem command recipe");
    };
    assert_eq!(version, "0.26.10");
    assert_eq!(program, "gem");
    assert_eq!(
        args,
        &[
            "install".to_owned(),
            "ruby-lsp".to_owned(),
            "--version".to_owned(),
            "0.26.10".to_owned(),
            "--no-document".to_owned(),
        ]
    );
}

#[test]
fn gopls_uses_the_locked_official_language_server() {
    let registry = Registry::builtin().unwrap();
    let gopls = registry.server("gopls").unwrap();
    assert_eq!(gopls.language_id, "go");
    assert_eq!(gopls.version_req, ">=0.15.0, <1.0.0");
    assert_eq!(gopls.extensions, ["go"]);
    assert_eq!(gopls.markers, ["go.work", "go.mod", "go.sum"]);
    assert_eq!(gopls.command, "gopls");
    assert!(gopls.args.is_empty());
    assert_eq!(gopls.version_args, ["version"]);
    let InstallRecipe::Command {
        version,
        program,
        args,
    } = &gopls.install
    else {
        panic!("gopls must use the go install recipe");
    };
    assert_eq!(version, "v0.23.0");
    assert_eq!(program, "go");
    assert_eq!(args, &["install", "golang.org/x/tools/gopls@v0.23.0"]);
}

#[test]
fn hls_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let hls = registry.server("hls").unwrap();
    assert_eq!(hls.language_id, "haskell");
    assert_eq!(hls.version_req, ">=2.0.0, <3.0.0");
    assert_eq!(hls.extensions, ["hs", "lhs"]);
    assert_eq!(
        hls.markers,
        ["stack.yaml", "cabal.project", "hie.yaml", "*.cabal"]
    );
    assert_eq!(hls.command, "haskell-language-server-wrapper");
    assert_eq!(hls.args, ["--lsp"]);
    assert_eq!(hls.version_args, ["--numeric-version"]);
    let InstallRecipe::Manual { version, hint } = &hls.install else {
        panic!("HLS must use a manual recipe");
    };
    assert_eq!(version, "2.14.0.0");
    assert!(hint.contains("GHCup"));
    assert!(hint.contains("[lsp.hls].executable"));
    assert!(hint.contains("haskell.haskell"));
}

#[test]
fn intelephense_uses_the_locked_opencode_contract() {
    let registry = Registry::builtin().unwrap();
    let intelephense = registry.server("intelephense").unwrap();
    assert_eq!(intelephense.display_name, "PHP Intelephense");
    assert_eq!(intelephense.language_id, "php");
    assert_eq!(intelephense.version_req, ">=1.18.5, <2.0.0");
    assert_eq!(intelephense.extensions, ["php"]);
    assert_eq!(
        intelephense.markers,
        ["composer.json", "composer.lock", ".php-version"]
    );
    assert_eq!(intelephense.command, "intelephense");
    assert_eq!(intelephense.args, ["--stdio"]);
    assert!(intelephense.version_args.is_empty());
    let InstallRecipe::Npm {
        version,
        package,
        companions,
    } = &intelephense.install
    else {
        panic!("Intelephense must use the npm recipe");
    };
    assert_eq!(version, "1.18.5");
    assert_eq!(package, "intelephense");
    assert!(companions.is_empty());
}

#[test]
fn prisma_uses_the_verified_official_contract() {
    let registry = Registry::builtin().unwrap();
    let prisma = registry.server("prisma").unwrap();
    assert_eq!(prisma.display_name, "Prisma Language Server");
    assert_eq!(prisma.language_id, "prisma");
    assert_eq!(prisma.version_req, ">=6.19.0, <32.0.0");
    assert_eq!(prisma.extensions, ["prisma"]);
    assert_eq!(
        prisma.markers,
        ["schema.prisma", "prisma/schema.prisma", "prisma"]
    );
    assert_eq!(prisma.command, "prisma-language-server");
    assert_eq!(prisma.args, ["--stdio"]);
    assert!(prisma.version_args.is_empty());
    let InstallRecipe::Npm {
        version,
        package,
        companions,
    } = &prisma.install
    else {
        panic!("Prisma must use the npm recipe");
    };
    assert_eq!(version, "31.11.0");
    assert_eq!(package, "@prisma/language-server");
    assert!(companions.is_empty());
}

#[test]
fn jdtls_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let jdtls = registry.server("jdtls").unwrap();
    assert_eq!(jdtls.language_id, "java");
    assert_eq!(jdtls.version_req, ">=1.30.0, <2.0.0");
    assert_eq!(jdtls.extensions, ["java"]);
    assert_eq!(
        jdtls.markers,
        [
            "settings.gradle",
            "settings.gradle.kts",
            "gradlew",
            "gradlew.bat",
            "build.gradle",
            "build.gradle.kts",
            "pom.xml",
            ".project",
            ".classpath",
        ]
    );
    assert_eq!(jdtls.command, "jdtls");
    assert!(jdtls.args.is_empty());
    assert!(jdtls.version_args.is_empty());
    let InstallRecipe::Manual { version, hint } = &jdtls.install else {
        panic!("JDTLS must use a manual recipe");
    };
    assert_eq!(version, "1.61.0-202608051627");
    assert!(hint.contains("Java 21+"));
    assert!(hint.contains("[lsp.jdtls].executable"));
    assert!(hint.contains("redhat.java"));
}

#[test]
fn julials_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let julials = registry.server("julials").unwrap();
    assert_eq!(julials.language_id, "julia");
    assert_eq!(julials.version_req, ">=5.0.0, <6.0.0");
    assert_eq!(julials.extensions, ["jl"]);
    assert_eq!(julials.markers, ["Project.toml", "Manifest.toml", "*.jl"]);
    assert_eq!(julials.command, "julia");
    assert_eq!(
        julials.args,
        [
            "--startup-file=no",
            "--history-file=no",
            "-e",
            "using LanguageServer; runserver()",
        ]
    );
    assert_eq!(julials.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &julials.install else {
        panic!("JuliaLS must use a manual recipe");
    };
    assert_eq!(version, "5.0.0");
    assert!(hint.contains("Julia 1.10+"));
    assert!(hint.contains("[lsp.julials].executable"));
    assert!(hint.contains("julialang.language-julia"));
}

#[test]
fn kotlin_ls_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let kotlin = registry.server("kotlin-ls").unwrap();
    assert_eq!(kotlin.language_id, "kotlin");
    assert_eq!(kotlin.version_req, ">=262.4739.0, <264.0.0");
    assert_eq!(kotlin.extensions, ["kt", "kts"]);
    assert_eq!(
        kotlin.markers,
        [
            "settings.gradle.kts",
            "settings.gradle",
            "gradlew",
            "gradlew.bat",
            "build.gradle.kts",
            "build.gradle",
            "pom.xml",
        ]
    );
    assert_eq!(kotlin.command, "kotlin-lsp");
    assert_eq!(kotlin.args, ["--stdio"]);
    assert_eq!(kotlin.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &kotlin.install else {
        panic!("KotlinLS must use a manual recipe");
    };
    assert_eq!(version, "262.9593.0");
    assert!(hint.contains("JDK 25"));
    assert!(hint.contains("[lsp.kotlin-ls].executable"));
    assert!(hint.contains("JetBrains.kotlin-server"));
}

#[test]
fn lua_ls_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let lua = registry.server("lua-ls").unwrap();
    assert_eq!(lua.language_id, "lua");
    assert_eq!(lua.version_req, ">=3.19.0, <4.0.0");
    assert_eq!(lua.extensions, ["lua"]);
    assert_eq!(
        lua.markers,
        [
            ".luarc.json",
            ".luarc.jsonc",
            ".luacheckrc",
            ".stylua.toml",
            "stylua.toml",
            "selene.toml",
            "selene.yml",
        ]
    );
    assert_eq!(lua.command, "lua-language-server");
    assert!(lua.args.is_empty());
    assert_eq!(lua.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &lua.install else {
        panic!("LuaLS must use a manual recipe");
    };
    assert_eq!(version, "3.19.0");
    assert!(hint.contains("[lsp.lua-ls].executable"));
    assert!(hint.contains("sumneko.lua"));
}

#[test]
fn ocaml_lsp_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let ocaml = registry.server("ocaml-lsp").unwrap();
    assert_eq!(ocaml.language_id, "ocaml");
    assert_eq!(ocaml.version_req, ">=1.4.1, <2.0.0");
    assert_eq!(ocaml.extensions, ["ml", "mli"]);
    assert_eq!(
        ocaml.markers,
        ["dune-project", "dune-workspace", ".merlin", "opam"]
    );
    assert_eq!(ocaml.command, "ocamllsp");
    assert!(ocaml.args.is_empty());
    assert_eq!(ocaml.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &ocaml.install else {
        panic!("OCaml LSP must use a manual recipe");
    };
    assert_eq!(version, "1.27.0");
    assert!(hint.contains("OCaml 5.5.0"));
    assert!(hint.contains("[lsp.ocaml-lsp].executable"));
    assert!(hint.contains("ocamllabs.ocaml-platform"));
}

#[test]
fn oxlint_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let oxlint = registry.server("oxlint").unwrap();
    assert_eq!(oxlint.language_id, "javascript");
    assert_eq!(oxlint.version_req, ">=1.78.0, <2.0.0");
    assert_eq!(
        oxlint.extensions,
        [
            "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "astro", "svelte",
        ]
    );
    assert_eq!(
        oxlint.markers,
        [
            ".oxlintrc.json",
            "package-lock.json",
            "bun.lockb",
            "bun.lock",
            "pnpm-lock.yaml",
            "yarn.lock",
            "package.json",
        ]
    );
    assert_eq!(oxlint.command, "oxlint");
    assert_eq!(oxlint.args, ["--lsp"]);
    assert_eq!(oxlint.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &oxlint.install else {
        panic!("Oxlint must use a manual recipe");
    };
    assert_eq!(version, "1.78.0");
    assert!(hint.contains("bun add --dev --exact oxlint@1.78.0"));
    assert!(hint.contains("[lsp.oxlint].executable"));
    assert!(hint.contains("oxc.oxc-vscode"));

    for extension in &oxlint.extensions {
        assert!(
            registry
                .matching_extension(extension)
                .any(|server| server.id == "oxlint"),
            "Oxlint must match .{extension}"
        );
    }
}

#[test]
fn csharp_uses_the_locked_official_language_server() {
    let registry = Registry::builtin().unwrap();
    let csharp = registry.server("csharp").unwrap();
    assert_eq!(csharp.language_id, "csharp");
    assert_eq!(csharp.version_req, "=5.9.0-1.26303.1");
    assert_eq!(csharp.extensions, ["cs", "csx"]);
    assert_eq!(
        csharp.markers,
        ["*.slnx", "*.sln", "*.csproj", "global.json"]
    );
    assert_eq!(csharp.command, "roslyn-language-server");
    assert_eq!(csharp.args, ["--stdio", "--autoLoadProjects"]);
    let InstallRecipe::Command {
        version,
        program,
        args,
    } = &csharp.install
    else {
        panic!("C# must use the dotnet tool recipe");
    };
    assert_eq!(version, "5.9.0-1.26303.1");
    assert_eq!(program, "dotnet");
    assert_eq!(
        args,
        &[
            "tool",
            "install",
            "--global",
            "roslyn-language-server",
            "--version",
            "5.9.0-1.26303.1",
        ]
    );
}

#[test]
fn fsharp_uses_the_locked_official_language_server() {
    let registry = Registry::builtin().unwrap();
    let fsharp = registry.server("fsharp").unwrap();
    assert_eq!(fsharp.language_id, "fsharp");
    assert_eq!(fsharp.version_req, "=0.83.0");
    assert_eq!(fsharp.extensions, ["fs", "fsi", "fsx", "fsscript"]);
    assert_eq!(
        fsharp.markers,
        ["*.slnx", "*.sln", "*.fsproj", "global.json"]
    );
    assert_eq!(fsharp.command, "fsautocomplete");
    assert!(fsharp.args.is_empty());
    assert_eq!(fsharp.version_args, ["--version"]);
    let InstallRecipe::Command {
        version,
        program,
        args,
    } = &fsharp.install
    else {
        panic!("F# must use the dotnet tool recipe");
    };
    assert_eq!(version, "0.83.0");
    assert_eq!(program, "dotnet");
    assert_eq!(
        args,
        &[
            "tool",
            "install",
            "--global",
            "fsautocomplete",
            "--version",
            "0.83.0",
        ]
    );
}

#[test]
fn gleam_uses_the_compiler_lsp_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let gleam = registry.server("gleam").unwrap();
    assert_eq!(gleam.language_id, "gleam");
    assert_eq!(gleam.version_req, ">=1.0.0, <2.0.0");
    assert_eq!(gleam.extensions, ["gleam"]);
    assert_eq!(gleam.markers, ["gleam.toml"]);
    assert_eq!(gleam.command, "gleam");
    assert_eq!(gleam.args, ["lsp"]);
    assert_eq!(gleam.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &gleam.install else {
        panic!("Gleam must use a manual recipe");
    };
    assert_eq!(version, "Gleam 1.18.1");
    assert!(hint.contains("Gleam 1.x"));
    assert!(hint.contains("[lsp.gleam].executable"));
    assert!(hint.contains("Gleam.gleam"));
}

#[test]
fn clojure_uses_the_opencode_contract_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let clojure = registry.server("clojure-lsp").unwrap();
    assert_eq!(clojure.language_id, "clojure");
    assert_eq!(clojure.version_req, ">=2026.7.6, <2027.0.0");
    assert_eq!(clojure.extensions, ["clj", "cljs", "cljc", "edn"]);
    assert_eq!(
        clojure.markers,
        [
            "deps.edn",
            "project.clj",
            "shadow-cljs.edn",
            "bb.edn",
            "build.boot",
        ]
    );
    assert_eq!(clojure.command, "clojure-lsp");
    assert_eq!(clojure.args, ["listen"]);
    let InstallRecipe::Manual { version, hint } = &clojure.install else {
        panic!("Clojure must use a manual recipe");
    };
    assert_eq!(version, "2026.07.06-14.34.19");
    assert!(hint.contains("scoop-clojure"));
}

#[test]
fn dart_uses_the_sdk_language_server_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let dart = registry.server("dart").unwrap();
    assert_eq!(dart.language_id, "dart");
    assert_eq!(dart.version_req, ">=2.12.0");
    assert_eq!(dart.extensions, ["dart"]);
    assert_eq!(dart.markers, ["pubspec.yaml", "analysis_options.yaml"]);
    assert_eq!(dart.command, "dart");
    assert_eq!(dart.args, ["language-server", "--protocol=lsp"]);
    assert_eq!(dart.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &dart.install else {
        panic!("Dart must use a manual recipe");
    };
    assert_eq!(version, "Dart SDK 2.12.0+");
    assert!(hint.contains("Dart SDK 2.12.0"));
    assert!(hint.contains("[lsp.dart].executable"));
}

#[test]
fn deno_uses_the_cli_language_server_and_manual_recipe() {
    let registry = Registry::builtin().unwrap();
    let deno = registry.server("deno").unwrap();
    assert_eq!(deno.language_id, "typescript");
    assert_eq!(deno.version_req, ">=1.40.0");
    assert_eq!(deno.extensions, ["ts", "tsx", "js", "jsx", "mjs"]);
    assert_eq!(deno.markers, ["deno.json", "deno.jsonc"]);
    assert_eq!(deno.command, "deno");
    assert_eq!(deno.args, ["lsp"]);
    assert_eq!(deno.version_args, ["--version"]);
    let InstallRecipe::Manual { version, hint } = &deno.install else {
        panic!("Deno must use a manual recipe");
    };
    assert_eq!(version, "Deno 1.40.0+");
    assert!(hint.contains("Deno 1.40.0"));
    assert!(hint.contains("[lsp.deno].executable"));
}

#[test]
fn elixir_uses_the_opencode_contract_and_official_manual_release() {
    let registry = Registry::builtin().unwrap();
    let elixir = registry.server("elixir-ls").unwrap();
    assert_eq!(elixir.language_id, "elixir");
    assert_eq!(elixir.version_req, ">=0.31.1, <0.32.0");
    assert_eq!(elixir.extensions, ["ex", "exs"]);
    assert_eq!(elixir.markers, ["mix.exs", "mix.lock"]);
    assert_eq!(elixir.command, "language_server");
    assert!(elixir.args.is_empty());
    assert!(elixir.version_args.is_empty());
    let InstallRecipe::Manual { version, hint } = &elixir.install else {
        panic!("ElixirLS must use a manual recipe");
    };
    assert_eq!(version, "0.31.1");
    assert!(hint.contains("JakeBecker.elixir-ls"));
    assert!(hint.contains("[lsp.elixir-ls].executable"));
}

#[test]
fn eslint_uses_the_opencode_contract_and_official_manual_server() {
    let registry = Registry::builtin().unwrap();
    let eslint = registry.server("eslint").unwrap();
    assert_eq!(eslint.language_id, "javascript");
    assert_eq!(eslint.version_req, ">=3.0.34, <3.1.0");
    assert_eq!(
        eslint.extensions,
        ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue"]
    );
    assert_eq!(
        eslint.markers,
        [
            "package-lock.json",
            "bun.lockb",
            "bun.lock",
            "pnpm-lock.yaml",
            "yarn.lock",
        ]
    );
    assert_eq!(eslint.command, "eslintServer");
    assert_eq!(eslint.args, ["--stdio"]);
    assert!(eslint.version_args.is_empty());
    let InstallRecipe::Manual { version, hint } = &eslint.install else {
        panic!("ESLint must use a manual recipe");
    };
    assert_eq!(version, "3.0.34");
    assert!(hint.contains("dbaeumer.vscode-eslint"));
    assert!(hint.contains("[lsp.eslint].executable"));
}

#[test]
fn clangd_uses_the_locked_official_windows_archive() {
    let registry = Registry::builtin().unwrap();
    let clangd = registry.server("clangd").unwrap();
    let InstallRecipe::GithubZip {
        version,
        url,
        sha256,
        executable,
    } = &clangd.install
    else {
        panic!("clangd must use the fixed GitHub ZIP recipe");
    };
    assert_eq!(version, "22.1.6");
    assert_eq!(
        url,
        "https://github.com/clangd/clangd/releases/download/22.1.6/clangd-windows-22.1.6.zip"
    );
    assert_eq!(
        sha256,
        "ce54f16e0b4fd76d450eeda9664420b195360b73febcfe40e661108fa57f2ce1"
    );
    assert_eq!(executable, "clangd_22.1.6/bin/clangd.exe");
}

#[test]
fn sourcekit_lsp_uses_the_swift_toolchain_contract() {
    let registry = Registry::builtin().unwrap();
    let sourcekit = registry.server("sourcekit-lsp").unwrap();
    assert_eq!(sourcekit.display_name, "SourceKit-LSP");
    assert_eq!(sourcekit.language_id, "swift");
    assert_eq!(sourcekit.version_req, ">=5.9.0");
    assert_eq!(sourcekit.extensions, ["swift", "objc", "objcpp"]);
    assert_eq!(
        sourcekit.markers,
        [
            "Package.swift",
            "*.xcodeproj",
            "*.xcworkspace",
            "compile_commands.json",
            "compile_flags.txt",
        ]
    );
    assert_eq!(sourcekit.command, "sourcekit-lsp");
    assert!(sourcekit.args.is_empty());
    assert_eq!(sourcekit.version_args, ["--help"]);
    let InstallRecipe::Manual { version, hint } = &sourcekit.install else {
        panic!("SourceKit-LSP must use a manual recipe");
    };
    assert_eq!(version, "5.9.0");
    assert!(hint.contains("Swift 5.9+"));
    assert!(hint.contains("[lsp.sourcekit-lsp].executable"));
}

#[test]
fn sourcekit_uses_protocol_language_ids_for_c_family_files() {
    let sourcekit = Registry::builtin()
        .unwrap()
        .server("sourcekit-lsp")
        .unwrap()
        .clone();
    assert_eq!(
        sourcekit.language_id_for_file(Path::new("main.swift")),
        "swift"
    );
    assert_eq!(
        sourcekit.language_id_for_file(Path::new("main.objc")),
        "objective-c"
    );
    assert_eq!(
        sourcekit.language_id_for_file(Path::new("main.OBJC")),
        "objective-c"
    );
    assert_eq!(
        sourcekit.language_id_for_file(Path::new("main.objcpp")),
        "objective-cpp"
    );
}

#[test]
fn unsafe_github_zip_recipes_are_rejected() {
    let valid_hash = "a".repeat(64);
    for recipe in [
        InstallRecipe::GithubZip {
            version: "22.1.6".into(),
            url: "http://github.com/clangd/clangd/archive.zip".into(),
            sha256: valid_hash.clone(),
            executable: "clangd/bin/clangd.exe".into(),
        },
        InstallRecipe::GithubZip {
            version: "22.1.6".into(),
            url: "https://github.com/clangd/clangd/archive.zip".into(),
            sha256: "not-a-hash".into(),
            executable: "clangd/bin/clangd.exe".into(),
        },
        InstallRecipe::GithubZip {
            version: "22.1.6".into(),
            url: "https://github.com/clangd/clangd/archive.zip".into(),
            sha256: valid_hash.clone(),
            executable: "../clangd.exe".into(),
        },
    ] {
        assert!(validate_recipe(&recipe, "clangd").is_err());
    }
}

#[test]
fn removed_archive_recipes_are_rejected() {
    assert!(
        toml::from_str::<InstallRecipe>(
            "kind = 'archive'\nversion = '1.0.0'\nurl = 'https://example.test/a.zip'\n"
        )
        .is_err()
    );
}
