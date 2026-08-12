use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use sha2::{Digest, Sha256};

use crate::protocol::{Diagnostic, DiagnosticSeverity, DiagnosticsReport, IdeDiagnostic};

pub(crate) const HOOK_MAX_ERRORS: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Baseline {
    paths: BTreeSet<PathBuf>,
    diagnostics: BTreeSet<String>,
    complete: bool,
}

impl Baseline {
    pub(crate) fn from_ide_diagnostics(
        paths: BTreeSet<PathBuf>,
        diagnostics: &[IdeDiagnostic],
        complete: bool,
    ) -> Self {
        Self {
            paths,
            diagnostics: diagnostics.iter().map(ide_diagnostic_key).collect(),
            complete,
        }
    }

    pub(crate) fn from_keys(
        paths: BTreeSet<PathBuf>,
        diagnostics: BTreeSet<String>,
        complete: bool,
    ) -> Self {
        Self {
            paths,
            diagnostics,
            complete,
        }
    }

    pub(crate) fn covers(&self, requested_paths: &BTreeSet<PathBuf>) -> bool {
        self.complete && requested_paths.is_subset(&self.paths)
    }
}

pub(crate) struct BaselineStore<K: Ord> {
    entries: BTreeMap<K, Baseline>,
    capacity: usize,
}

impl<K: Ord> BaselineStore<K> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            capacity,
        }
    }

    pub(crate) fn insert(&mut self, key: K, baseline: Baseline) {
        if self.capacity == 0 {
            return;
        }
        self.entries.remove(&key);
        while self.entries.len() >= self.capacity {
            self.entries.pop_first();
        }
        self.entries.insert(key, baseline);
    }

    pub(crate) fn take(&mut self, key: &K) -> Option<Baseline> {
        self.entries.remove(key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiagnosticSnapshot {
    paths: BTreeSet<PathBuf>,
    diagnostics: Vec<Diagnostic>,
    identities: Vec<String>,
    fresh: bool,
    complete: bool,
    truncated: bool,
}

impl DiagnosticSnapshot {
    pub(crate) fn new(
        paths: BTreeSet<PathBuf>,
        diagnostics: Vec<Diagnostic>,
        fresh: bool,
        complete: bool,
        truncated: bool,
    ) -> Self {
        let identities = diagnostics.iter().map(diagnostic_key).collect();
        Self {
            paths,
            diagnostics,
            identities,
            fresh,
            complete,
            truncated,
        }
    }

    pub(crate) fn from_ide_identities(
        paths: BTreeSet<PathBuf>,
        diagnostics: Vec<Diagnostic>,
        identities: Vec<String>,
        fresh: bool,
        complete: bool,
        truncated: bool,
    ) -> Self {
        Self {
            paths,
            diagnostics,
            identities,
            fresh,
            complete,
            truncated,
        }
    }
}

pub(crate) trait DiagnosticSource {
    fn snapshot(&self) -> DiagnosticSnapshot;
}

impl DiagnosticSource for DiagnosticSnapshot {
    fn snapshot(&self) -> DiagnosticSnapshot {
        self.clone()
    }
}

pub(crate) struct LspDiagnosticSource<'a> {
    paths: BTreeSet<PathBuf>,
    report: &'a DiagnosticsReport,
}

impl<'a> LspDiagnosticSource<'a> {
    pub(crate) fn new(paths: &[PathBuf], report: &'a DiagnosticsReport) -> Self {
        Self {
            paths: paths.iter().cloned().collect(),
            report,
        }
    }
}

impl DiagnosticSource for LspDiagnosticSource<'_> {
    fn snapshot(&self) -> DiagnosticSnapshot {
        let truncated = self
            .report
            .sources
            .iter()
            .any(|source| source.reason.as_deref() == Some("diagnostics_truncated"));
        let complete = !self.report.sources.iter().any(|source| {
            matches!(
                source.reason.as_deref(),
                Some("no_diagnostics_received" | "diagnostics_truncated")
            )
        });
        DiagnosticSnapshot::new(
            self.paths.clone(),
            self.report.diagnostics.clone(),
            self.report.fresh,
            complete,
            truncated,
        )
    }
}

pub(crate) struct IdeDiagnosticSource {
    snapshot: DiagnosticSnapshot,
}

impl IdeDiagnosticSource {
    pub(crate) fn new(
        paths: BTreeSet<PathBuf>,
        diagnostics: Vec<Diagnostic>,
        identities: Vec<String>,
        fresh: bool,
        truncated: bool,
    ) -> Self {
        Self {
            snapshot: DiagnosticSnapshot::from_ide_identities(
                paths,
                diagnostics,
                identities,
                fresh,
                !truncated,
                truncated,
            ),
        }
    }
}

impl DiagnosticSource for IdeDiagnosticSource {
    fn snapshot(&self) -> DiagnosticSnapshot {
        self.snapshot.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verification {
    pub(crate) fresh: bool,
    pub(crate) baseline_available: bool,
    pub(crate) new_errors: Vec<Diagnostic>,
}

pub(crate) fn verify<S: DiagnosticSource>(
    baseline: Option<&Baseline>,
    requested_paths: &BTreeSet<PathBuf>,
    source: &S,
    max_errors: usize,
) -> Verification {
    let Some(baseline) = baseline else {
        return Verification {
            fresh: false,
            baseline_available: false,
            new_errors: Vec::new(),
        };
    };
    if !baseline.complete || !requested_paths.is_subset(&baseline.paths) {
        return Verification {
            fresh: false,
            baseline_available: false,
            new_errors: Vec::new(),
        };
    }

    let snapshot = source.snapshot();
    if !snapshot.complete || !requested_paths.is_subset(&snapshot.paths) {
        return Verification {
            fresh: false,
            baseline_available: false,
            new_errors: Vec::new(),
        };
    }
    if snapshot.truncated {
        return Verification {
            fresh: snapshot.fresh,
            baseline_available: false,
            new_errors: Vec::new(),
        };
    }
    if !snapshot.fresh {
        return Verification {
            fresh: false,
            baseline_available: true,
            new_errors: Vec::new(),
        };
    }

    let new_errors = snapshot
        .diagnostics
        .into_iter()
        .zip(snapshot.identities)
        .filter(|(diagnostic, identity)| {
            diagnostic.severity == DiagnosticSeverity::Error
                && !baseline.diagnostics.contains(identity)
        })
        .map(|(diagnostic, _)| diagnostic)
        .take(max_errors)
        .collect();
    Verification {
        fresh: true,
        baseline_available: true,
        new_errors,
    }
}

pub(crate) fn correlation_hash(codex_session_id: &str, tool_use_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"clsp-ide-review-v1\0");
    digest.update(codex_session_id.as_bytes());
    digest.update([0]);
    digest.update(tool_use_id.as_bytes());
    hex::encode(digest.finalize())
}

pub(crate) fn diagnostic_key(diagnostic: &Diagnostic) -> String {
    format!(
        "{}:{}:{}:{}:{}:{:?}:{}:{}:{}",
        diagnostic.path.display(),
        diagnostic.range.start.line,
        diagnostic.range.start.column,
        diagnostic.range.end.line,
        diagnostic.range.end.column,
        diagnostic.severity,
        diagnostic.code.as_deref().unwrap_or_default(),
        diagnostic.source.as_deref().unwrap_or_default(),
        diagnostic.message
    )
}

pub(crate) fn ide_diagnostic_key(diagnostic: &IdeDiagnostic) -> String {
    format!(
        "{}:{}:{}:{}:{}:{:?}:{}:{}:{}",
        diagnostic.path.display(),
        diagnostic.range.start.line,
        diagnostic.range.start.character,
        diagnostic.range.end.line,
        diagnostic.range.end.character,
        diagnostic.severity,
        diagnostic.code.as_deref().unwrap_or_default(),
        diagnostic.source.as_deref().unwrap_or_default(),
        diagnostic.message
    )
}

#[cfg(test)]
#[path = "../tests/unit/edit_diagnostics.rs"]
mod tests;
