use super::*;

#[derive(Clone)]
struct FakeSource {
    snapshot: DiagnosticSnapshot,
}

impl DiagnosticSource for FakeSource {
    fn snapshot(&self) -> DiagnosticSnapshot {
        self.snapshot.clone()
    }
}

fn diagnostic(message: &str, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        path: "src/lib.rs".into(),
        range: crate::protocol::TextRange {
            start: crate::protocol::Position { line: 1, column: 1 },
            end: crate::protocol::Position { line: 1, column: 2 },
        },
        severity,
        code: None,
        source: None,
        message: message.into(),
        server_id: "fake".into(),
    }
}

fn baseline(complete: bool) -> Baseline {
    let error = diagnostic("old", DiagnosticSeverity::Error);
    Baseline::from_keys(
        BTreeSet::from([PathBuf::from("src/lib.rs")]),
        BTreeSet::from([diagnostic_key(&error)]),
        complete,
    )
}

fn source(fresh: bool, complete: bool, truncated: bool) -> FakeSource {
    FakeSource {
        snapshot: DiagnosticSnapshot::new(
            BTreeSet::from([PathBuf::from("src/lib.rs")]),
            vec![
                diagnostic("old", DiagnosticSeverity::Error),
                diagnostic("new", DiagnosticSeverity::Error),
                diagnostic("warning", DiagnosticSeverity::Warning),
            ],
            fresh,
            complete,
            truncated,
        ),
    }
}

#[test]
fn verification_matrix_never_injects_untrusted_deltas() {
    let paths = BTreeSet::from([PathBuf::from("src/lib.rs")]);
    let cases = [
        (
            "success",
            Some(baseline(true)),
            source(true, true, false),
            true,
            true,
            1,
        ),
        (
            "no baseline",
            None,
            source(true, true, false),
            false,
            false,
            0,
        ),
        (
            "incomplete baseline",
            Some(baseline(false)),
            source(true, true, false),
            false,
            false,
            0,
        ),
        (
            "stale result",
            Some(baseline(true)),
            source(false, true, false),
            false,
            true,
            0,
        ),
        (
            "incomplete result",
            Some(baseline(true)),
            source(true, false, false),
            false,
            false,
            0,
        ),
        (
            "truncated result",
            Some(baseline(true)),
            source(true, true, true),
            true,
            false,
            0,
        ),
    ];
    for (name, baseline, source, fresh, available, errors) in cases {
        let result = verify(baseline.as_ref(), &paths, &source, HOOK_MAX_ERRORS);
        assert_eq!(result.fresh, fresh, "{name}");
        assert_eq!(result.baseline_available, available, "{name}");
        assert_eq!(result.new_errors.len(), errors, "{name}");
    }
}

#[test]
fn baseline_store_is_bounded_and_one_shot() {
    let mut store = BaselineStore::new(1);
    store.insert("first", baseline(true));
    store.insert("second", baseline(true));
    assert_eq!(store.len(), 1);
    assert!(store.take(&"first").is_none());
    assert!(store.take(&"second").is_some());
    assert!(store.take(&"second").is_none());
}

#[test]
fn ide_identity_uses_source_coordinates_before_protocol_conversion() {
    let ide = IdeDiagnostic {
        path: "src/lib.rs".into(),
        range: crate::protocol::IdeRange {
            start: crate::protocol::IdePosition {
                line: 0,
                character: 3,
            },
            end: crate::protocol::IdePosition {
                line: 0,
                character: 4,
            },
        },
        severity: DiagnosticSeverity::Error,
        message: "existing".into(),
        source: Some("rust-analyzer".into()),
        code: Some("E1".into()),
    };
    let paths = BTreeSet::from([PathBuf::from("src/lib.rs")]);
    let baseline = Baseline::from_ide_diagnostics(paths.clone(), std::slice::from_ref(&ide), true);
    let current = Diagnostic {
        path: ide.path.clone(),
        range: crate::protocol::TextRange {
            start: crate::protocol::Position { line: 1, column: 9 },
            end: crate::protocol::Position {
                line: 1,
                column: 10,
            },
        },
        severity: DiagnosticSeverity::Error,
        code: ide.code.clone(),
        source: ide.source.clone(),
        message: ide.message.clone(),
        server_id: "vscode_problems".into(),
    };
    let source = IdeDiagnosticSource::new(
        paths.clone(),
        vec![current],
        vec![ide_diagnostic_key(&ide)],
        true,
        false,
    );
    let result = verify(Some(&baseline), &paths, &source, HOOK_MAX_ERRORS);
    assert!(result.new_errors.is_empty());
    assert!(result.fresh && result.baseline_available);
}

impl<K: Ord> BaselineStore<K> {
    fn len(&self) -> usize {
        self.entries.len()
    }
}
