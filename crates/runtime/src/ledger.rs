//! Append-only JSONL ledgers for decisions and release-verification receipts.
//!
//! Ported from the canonical append-only ledger discipline proven in
//! oh-my-antigravity's `src/lib/ledger.ts`: records are appended one JSON object
//! per line and the file is never rewritten. Reads are torn-line-tolerant —
//! blank or partially written lines are skipped and a missing file yields an
//! empty result — so a crash mid-append can never corrupt history.
//!
//! Two ledgers live under `.ember/`:
//! - [`decisions.jsonl`](decisions_file) — a [`DecisionRecord`] log (EFRUST-9).
//! - [`release-receipts.jsonl`](receipts_file) — a [`ReleaseReceipt`] log used
//!   by the verification-receipt release gate (EFRUST-10).
//!
//! Everything is offline and local; no network access is performed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// An append-only decision record (EFRUST-9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
    pub ts: String,
    /// Decision category, e.g. `"architecture"` or `"process"`.
    pub kind: String,
    /// One-line summary of what was decided.
    pub summary: String,
    /// Why the decision was made.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
    /// Lifecycle status, e.g. `"accepted"`, `"proposed"`, `"superseded"`.
    pub status: String,
}

impl DecisionRecord {
    /// Build a decision stamped with the current UTC time and `status` of
    /// `"accepted"`.
    #[must_use]
    pub fn now(
        kind: impl Into<String>,
        summary: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            ts: iso8601_now(),
            kind: kind.into(),
            summary: summary.into(),
            rationale: rationale.into(),
            status: "accepted".to_string(),
        }
    }
}

/// A single gate within a release receipt (e.g. build, clippy, tests, doctor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptCheck {
    /// Gate name, e.g. `"build"`, `"clippy"`, `"tests"`, `"doctor"`.
    pub name: String,
    /// Whether the gate passed.
    pub pass: bool,
}

impl ReceiptCheck {
    /// Construct a named check result.
    #[must_use]
    pub fn new(name: impl Into<String>, pass: bool) -> Self {
        Self {
            name: name.into(),
            pass,
        }
    }
}

/// An append-only verification receipt gating a release (EFRUST-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseReceipt {
    /// The version this receipt attests to, e.g. `"0.1.0"`.
    pub version: String,
    /// ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
    pub ts: String,
    /// The local gates that were run.
    pub checks: Vec<ReceiptCheck>,
    /// Overall pass — true only when every check passed.
    pub pass: bool,
    /// Who recorded the receipt.
    pub committer: String,
}

impl ReleaseReceipt {
    /// Build a receipt for `version` from `checks`, stamped with the current UTC
    /// time. `pass` is derived as the conjunction of every check.
    #[must_use]
    pub fn now(
        version: impl Into<String>,
        committer: impl Into<String>,
        checks: Vec<ReceiptCheck>,
    ) -> Self {
        let pass = !checks.is_empty() && checks.iter().all(|c| c.pass);
        Self {
            version: version.into(),
            ts: iso8601_now(),
            checks,
            pass,
            committer: committer.into(),
        }
    }

    /// A one-line human summary suitable for release notes.
    #[must_use]
    pub fn summary(&self) -> String {
        let verdict = if self.pass { "PASS" } else { "FAIL" };
        let checks = self
            .checks
            .iter()
            .map(|c| format!("{}={}", c.name, if c.pass { "ok" } else { "fail" }))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{verdict} v{} [{checks}] by {} at {}",
            self.version, self.committer, self.ts
        )
    }
}

/// Path to the decision ledger within a project directory.
#[must_use]
pub fn decisions_file(project_dir: &Path) -> PathBuf {
    project_dir.join(".ember").join("decisions.jsonl")
}

/// Path to the release-receipt ledger within a project directory.
#[must_use]
pub fn receipts_file(project_dir: &Path) -> PathBuf {
    project_dir.join(".ember").join("release-receipts.jsonl")
}

/// Append a decision to `.ember/decisions.jsonl`, creating the directory and
/// file if needed. The file is opened in append mode so existing history is
/// never rewritten.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the `.ember` directory cannot be created or
/// the ledger cannot be opened for appending or written.
pub fn append_decision(project_dir: &Path, record: &DecisionRecord) -> std::io::Result<()> {
    append_jsonl(&decisions_file(project_dir), record)
}

/// Read every decision from `.ember/decisions.jsonl`, skipping torn or blank
/// lines. A missing file yields an empty vector.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the ledger exists but cannot be read.
pub fn read_decisions(project_dir: &Path) -> std::io::Result<Vec<DecisionRecord>> {
    read_jsonl(&decisions_file(project_dir))
}

/// Append a release receipt to `.ember/release-receipts.jsonl`, creating the
/// directory and file if needed.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the `.ember` directory cannot be created or
/// the ledger cannot be opened for appending or written.
pub fn append_receipt(project_dir: &Path, receipt: &ReleaseReceipt) -> std::io::Result<()> {
    append_jsonl(&receipts_file(project_dir), receipt)
}

/// Read every release receipt from `.ember/release-receipts.jsonl`, skipping
/// torn or blank lines. A missing file yields an empty vector.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the ledger exists but cannot be read.
pub fn read_receipts(project_dir: &Path) -> std::io::Result<Vec<ReleaseReceipt>> {
    read_jsonl(&receipts_file(project_dir))
}

/// The most recent receipt recorded for `version`, if any.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the ledger exists but cannot be read.
pub fn latest_receipt_for(
    project_dir: &Path,
    version: &str,
) -> std::io::Result<Option<ReleaseReceipt>> {
    let receipts = read_receipts(project_dir)?;
    Ok(receipts.into_iter().rev().find(|r| r.version == version))
}

/// The release gate: returns `true` when the latest receipt for `version`
/// exists and passed. Use this to gate tagging/publishing on a passing receipt.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the ledger exists but cannot be read.
pub fn is_release_gated(project_dir: &Path, version: &str) -> std::io::Result<bool> {
    Ok(latest_receipt_for(project_dir, version)?.is_some_and(|r| r.pass))
}

// ---------------------------------------------------------------------------
// Internal JSONL helpers
// ---------------------------------------------------------------------------

/// Append one JSON object per line, never rewriting prior content.
fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    file.flush()
}

/// Read every record, tolerating torn/blank lines and a missing file.
fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> std::io::Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(path)?;
    let mut records = Vec::new();
    for line in data.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Torn lines (e.g. a crash mid-append) fail to parse and are skipped.
        if let Ok(record) = serde_json::from_str::<T>(trimmed) {
            records.push(record);
        }
    }
    Ok(records)
}

/// Current UTC time as an ISO-8601 `YYYY-MM-DDTHH:MM:SSZ` string.
fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil-from-days (Howard Hinnant's algorithm), UTC.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u64::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ember-ledger-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn decisions_append_then_read_roundtrip() {
        let dir = temp_dir("decisions");
        assert!(read_decisions(&dir).unwrap().is_empty());

        let a = DecisionRecord::now(
            "architecture",
            "port ledger to rust",
            "parity with antigravity",
        );
        let b = DecisionRecord::now("process", "gate releases on receipts", "no silent releases");
        append_decision(&dir, &a).unwrap();
        append_decision(&dir, &b).unwrap();

        let read = read_decisions(&dir).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].summary, "port ledger to rust");
        assert_eq!(read[1].kind, "process");
        assert_eq!(read[0].status, "accepted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_skips_torn_and_blank_lines() {
        let dir = temp_dir("torn");
        let good = DecisionRecord::now("architecture", "keep it", "valid");
        append_decision(&dir, &good).unwrap();

        // Simulate a crash mid-append: a blank line and a half-written record.
        let path = decisions_file(&dir);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f).unwrap();
        write!(f, "{{\"ts\":\"2026-01-01T00:00:00Z\",\"kind\":\"arch").unwrap();
        drop(f);

        let read = read_decisions(&dir).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].summary, "keep it");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_reads_empty() {
        let dir = temp_dir("missing");
        assert!(read_decisions(&dir).unwrap().is_empty());
        assert!(read_receipts(&dir).unwrap().is_empty());
        assert!(latest_receipt_for(&dir, "0.1.0").unwrap().is_none());
        assert!(!is_release_gated(&dir, "0.1.0").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn receipt_pass_derivation_and_gate() {
        let passing = ReleaseReceipt::now(
            "0.1.0",
            "ci",
            vec![
                ReceiptCheck::new("build", true),
                ReceiptCheck::new("clippy", true),
                ReceiptCheck::new("tests", true),
                ReceiptCheck::new("doctor", true),
            ],
        );
        assert!(passing.pass);
        assert!(passing.summary().starts_with("PASS v0.1.0"));

        let failing = ReleaseReceipt::now(
            "0.1.0",
            "ci",
            vec![
                ReceiptCheck::new("build", true),
                ReceiptCheck::new("tests", false),
            ],
        );
        assert!(!failing.pass);

        // An empty check set is never a pass.
        let empty = ReleaseReceipt::now("0.1.0", "ci", vec![]);
        assert!(!empty.pass);
    }

    #[test]
    fn gate_uses_latest_receipt_for_version() {
        let dir = temp_dir("gate");

        // First receipt fails; gate is closed.
        append_receipt(
            &dir,
            &ReleaseReceipt::now("0.1.0", "ci", vec![ReceiptCheck::new("tests", false)]),
        )
        .unwrap();
        assert!(!is_release_gated(&dir, "0.1.0").unwrap());

        // A later passing receipt opens the gate for that version.
        append_receipt(
            &dir,
            &ReleaseReceipt::now("0.1.0", "ci", vec![ReceiptCheck::new("tests", true)]),
        )
        .unwrap();
        assert!(is_release_gated(&dir, "0.1.0").unwrap());

        // A different version is unaffected and stays gated-closed.
        assert!(!is_release_gated(&dir, "0.2.0").unwrap());

        let latest = latest_receipt_for(&dir, "0.1.0").unwrap().unwrap();
        assert!(latest.pass);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn iso8601_now_is_well_formed() {
        let ts = iso8601_now();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
