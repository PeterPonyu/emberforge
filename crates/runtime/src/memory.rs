//! Persistent file-based memory system for the emberforge CLI.
//!
//! Memory files are markdown with YAML frontmatter, stored in:
//! - `~/.ember/memory/` (user-level)
//! - `.ember/memory/` (project-level)
//!
//! Each directory may contain an entrypoint `MEMORY.md` and individual
//! memory files categorized by type (user, feedback, project, reference).

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Memory file type taxonomy (closed 4-type system).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    /// Parse a type string (case-insensitive).
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    /// Short label used in the manifest.
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

/// Parsed frontmatter from a memory file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFrontmatter {
    pub name: String,
    pub description: String,
    pub memory_type: MemoryType,
}

/// A discovered memory file with metadata.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub frontmatter: MemoryFrontmatter,
    pub modified: SystemTime,
    /// Content of the file body (after frontmatter).
    pub body: String,
}

/// Entrypoint index (`MEMORY.md`) configuration.
#[derive(Debug, Clone)]
pub struct MemoryIndex {
    pub path: PathBuf,
    pub content: String,
}

/// Memory discovery configuration.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Maximum number of memory files to discover.
    pub max_files: usize,
    /// Maximum lines in `MEMORY.md` entrypoint.
    pub max_entrypoint_lines: usize,
    /// Maximum bytes in `MEMORY.md` entrypoint.
    pub max_entrypoint_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_files: 200,
            max_entrypoint_lines: 200,
            max_entrypoint_bytes: 25_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Strip optional surrounding quotes (single or double) from a value string.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
        && s.len() >= 2
    {
        return &s[1..s.len() - 1];
    }
    s
}

/// Parse YAML frontmatter from markdown content.
///
/// Frontmatter is delimited by `---` on its own line at the very start of the
/// file.  Returns `None` if no valid frontmatter block is found or if required
/// fields (`name`, `description`, `type`) are missing.
///
/// The second element of the tuple is the body text that follows the
/// frontmatter block.
#[must_use]
pub fn parse_frontmatter(content: &str) -> Option<(MemoryFrontmatter, &str)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the opening delimiter line.
    let after_open = &trimmed[3..];
    // The rest of the opening line should be empty (allow trailing whitespace).
    let after_open = after_open.strip_prefix('\n').or_else(|| {
        let line_end = after_open.find('\n')?;
        if after_open[..line_end].trim().is_empty() {
            Some(&after_open[line_end + 1..])
        } else {
            None
        }
    })?;

    // Find the closing `---`.
    let close_pos = after_open.find("\n---")?;
    let yaml_block = &after_open[..close_pos];

    // Body starts after the closing `---` line.
    let after_close = &after_open[close_pos + 4..]; // skip "\n---"
    let body = if let Some(nl) = after_close.find('\n') {
        &after_close[nl + 1..]
    } else {
        ""
    };

    // Parse simple key: value pairs from the YAML block.
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut memory_type: Option<MemoryType> = None;

    for line in yaml_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = strip_quotes(val.trim());
            match key {
                "name" => name = Some(val.to_string()),
                "description" => description = Some(val.to_string()),
                "type" => memory_type = MemoryType::from_str(val),
                _ => {} // ignore unknown keys
            }
        }
    }

    Some((
        MemoryFrontmatter {
            name: name?,
            description: description?,
            memory_type: memory_type?,
        },
        body,
    ))
}

// ---------------------------------------------------------------------------
// Directory scanning
// ---------------------------------------------------------------------------

/// Scan a memory directory for `.md` files with valid frontmatter.
///
/// Returns files sorted by modification time (newest first), capped at
/// `config.max_files`.  `MEMORY.md` (the entrypoint) is always skipped.
pub fn scan_memory_dir(dir: &Path, config: &MemoryConfig) -> io::Result<Vec<MemoryFile>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files: Vec<MemoryFile> = Vec::new();

    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        // Only consider .md files.
        let is_md = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        if !is_md {
            continue;
        }

        // Skip the entrypoint.
        if let Some(fname) = path.file_name().and_then(|f| f.to_str()) {
            if fname.eq_ignore_ascii_case("memory.md") {
                continue;
            }
        }

        // Read and parse.
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("memory: skipping {}: {}", path.display(), e);
                continue;
            }
        };

        let Some((frontmatter, body)) = parse_frontmatter(&content) else {
            eprintln!(
                "memory: skipping {} (invalid or missing frontmatter)",
                path.display()
            );
            continue;
        };

        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        files.push(MemoryFile {
            path,
            frontmatter,
            modified,
            body: body.to_string(),
        });
    }

    sort_memory_files(&mut files);

    // Cap at max_files.
    files.truncate(config.max_files);

    Ok(files)
}

fn sort_memory_files(files: &mut [MemoryFile]) {
    // Sort newest first. Use deterministic tie-breakers because some
    // filesystems expose coarse modification timestamps during fast test writes.
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| b.frontmatter.name.cmp(&a.frontmatter.name))
            .then_with(|| b.path.cmp(&a.path))
    });
}

// ---------------------------------------------------------------------------
// Entrypoint (MEMORY.md)
// ---------------------------------------------------------------------------

/// Read and truncate the `MEMORY.md` entrypoint file.
///
/// The content is truncated to at most `config.max_entrypoint_lines` lines and
/// `config.max_entrypoint_bytes` bytes (whichever limit is hit first).
pub fn load_entrypoint(dir: &Path, config: &MemoryConfig) -> io::Result<Option<MemoryIndex>> {
    let path = dir.join("MEMORY.md");
    if !path.is_file() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path)?;
    let content = truncate_content(&raw, config.max_entrypoint_lines, config.max_entrypoint_bytes);

    Ok(Some(MemoryIndex { path, content }))
}

/// Truncate content to fit within both a line limit and a byte limit.
fn truncate_content(s: &str, max_lines: usize, max_bytes: usize) -> String {
    let mut result = String::new();

    for (line_count, line) in s.lines().enumerate() {
        if line_count >= max_lines {
            break;
        }
        // Check byte budget (include the newline).
        let addition = if result.is_empty() {
            line.len()
        } else {
            line.len() + 1 // for '\n'
        };
        if result.len() + addition > max_bytes {
            break;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }

    result
}

/// Ensure the memory directory and `MEMORY.md` exist, creating them if needed.
///
/// This is idempotent — calling it on an already-initialized directory is a
/// no-op.
pub fn ensure_memory_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;

    let entrypoint = dir.join("MEMORY.md");
    if !entrypoint.exists() {
        fs::write(
            &entrypoint,
            "# Memory\n\nThis file is the entrypoint for the ember memory system.\n",
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Memory manifest
// ---------------------------------------------------------------------------

/// Format a `SystemTime` as `YYYY-MM-DD`.
///
/// Falls back to `"unknown"` if the time cannot be converted.
fn format_date(time: SystemTime) -> String {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            // Simple date calculation (no chrono dependency).
            let days = secs / 86_400;
            let (year, month, day) = days_to_ymd(days);
            format!("{year:04}-{month:02}-{day:02}")
        }
        Err(_) => "unknown".to_string(),
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm adapted from Howard Hinnant's `civil_from_days`.
    // days fits in i64 for any realistic date (< 2^53 days since epoch).
    let days_i64 = i64::try_from(days).unwrap_or(i64::MAX);
    let z = days_i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let z_offset = z - era * 146_097;
    let doe = u64::try_from(z_offset).unwrap_or(0); // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let yoe_i64 = i64::try_from(yoe).unwrap_or(0);
    let y = yoe_i64 + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * day_of_year + 2) / 153; // [0, 11]
    let d = day_of_year - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    let y_u64 = u64::try_from(y).unwrap_or(1970);
    (y_u64, m, d)
}

/// Build a one-line-per-file manifest string for presenting to the LLM.
///
/// Format: `[type] filename.md (YYYY-MM-DD) — description`
#[must_use]
pub fn build_memory_manifest(files: &[MemoryFile]) -> String {
    let mut out = String::new();
    for f in files {
        let fname = f
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        let date = format_date(f.modified);
        let type_label = f.frontmatter.memory_type.label();
        let description = &f.frontmatter.description;
        let line = format!("[{type_label}] {fname} ({date}) \u{2014} {description}\n");
        out.push_str(&line);
    }
    out
}

// ---------------------------------------------------------------------------
// Memory paths
// ---------------------------------------------------------------------------

/// Return the user-level memory directory (`~/.ember/memory/`).
pub fn user_memory_dir() -> io::Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".ember").join("memory"))
}

/// Return the project-level memory directory (`.ember/memory/` relative to cwd).
pub fn project_memory_dir() -> io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(".ember").join("memory"))
}

/// Resolve the user's home directory.
fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "could not determine home directory"))
}

// ---------------------------------------------------------------------------
// System prompt integration
// ---------------------------------------------------------------------------

/// Build the full memory context block for injection into the system prompt.
///
/// Includes `MEMORY.md` content and memory manifests from both the user-level
/// and project-level directories.  Returns `None` if no memory content is
/// found anywhere.
pub fn build_memory_prompt(config: &MemoryConfig) -> io::Result<Option<String>> {
    let mut sections: Vec<String> = Vec::new();

    // --- User-level ---
    if let Ok(user_dir) = user_memory_dir() {
        if user_dir.is_dir() {
            if let Some(idx) = load_entrypoint(&user_dir, config)? {
                sections.push(format!(
                    "## User Memory ({})\n\n{}",
                    idx.path.display(),
                    idx.content,
                ));
            }
            let files = scan_memory_dir(&user_dir, config)?;
            if !files.is_empty() {
                let manifest = build_memory_manifest(&files);
                sections.push(format!("### User Memory Files\n\n{manifest}"));
            }
        }
    }

    // --- Project-level ---
    if let Ok(proj_dir) = project_memory_dir() {
        if proj_dir.is_dir() {
            if let Some(idx) = load_entrypoint(&proj_dir, config)? {
                sections.push(format!(
                    "## Project Memory ({})\n\n{}",
                    idx.path.display(),
                    idx.content,
                ));
            }
            let files = scan_memory_dir(&proj_dir, config)?;
            if !files.is_empty() {
                let manifest = build_memory_manifest(&files);
                sections.push(format!("### Project Memory Files\n\n{manifest}"));
            }
        }
    }

    if sections.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sections.join("\n\n")))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    /// Create a unique temp directory for a test.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("emberforge_memory_tests")
            .join(name);
        // Clean up from any previous run.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    // -----------------------------------------------------------------------
    // Frontmatter parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_valid_frontmatter() {
        let content = "\
---
name: test-memory
description: A test memory file
type: user
---
# Body content

Some notes here.
";
        let (fm, body) = parse_frontmatter(content).expect("should parse");
        assert_eq!(fm.name, "test-memory");
        assert_eq!(fm.description, "A test memory file");
        assert_eq!(fm.memory_type, MemoryType::User);
        assert!(body.contains("# Body content"));
        assert!(body.contains("Some notes here."));
    }

    #[test]
    fn parse_frontmatter_with_quotes() {
        let content = "\
---
name: \"quoted name\"
description: 'single quoted desc'
type: project
---
body
";
        let (fm, body) = parse_frontmatter(content).expect("should parse");
        assert_eq!(fm.name, "quoted name");
        assert_eq!(fm.description, "single quoted desc");
        assert_eq!(fm.memory_type, MemoryType::Project);
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn parse_frontmatter_missing_field() {
        let content = "\
---
name: only-name
type: feedback
---
body
";
        // Missing description -> None.
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nNo frontmatter here.\n";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn parse_frontmatter_invalid_type() {
        let content = "\
---
name: foo
description: bar
type: invalid
---
body
";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn parse_frontmatter_all_types() {
        for (type_str, expected) in [
            ("user", MemoryType::User),
            ("feedback", MemoryType::Feedback),
            ("project", MemoryType::Project),
            ("reference", MemoryType::Reference),
        ] {
            let content = format!(
                "---\nname: n\ndescription: d\ntype: {}\n---\nbody\n",
                type_str
            );
            let (fm, _) = parse_frontmatter(&content).expect("should parse");
            assert_eq!(fm.memory_type, expected, "failed for type: {}", type_str);
        }
    }

    #[test]
    fn parse_frontmatter_extra_whitespace() {
        let content = "\
---
name:   spaced-name
description:  spaced description
type:  reference
---
body
";
        let (fm, _) = parse_frontmatter(content).expect("should parse");
        assert_eq!(fm.name, "spaced-name");
        assert_eq!(fm.description, "spaced description");
        assert_eq!(fm.memory_type, MemoryType::Reference);
    }

    #[test]
    fn parse_frontmatter_unknown_keys_ignored() {
        let content = "\
---
name: n
description: d
type: user
author: someone
version: 1.0
---
body
";
        let (fm, _) = parse_frontmatter(content).expect("should parse");
        assert_eq!(fm.name, "n");
    }

    // -----------------------------------------------------------------------
    // Directory scanning
    // -----------------------------------------------------------------------

    fn write_memory_file(dir: &Path, name: &str, mem_type: &str, desc: &str, body: &str) {
        let content = format!(
            "---\nname: {}\ndescription: {}\ntype: {}\n---\n{}",
            name, desc, mem_type, body
        );
        fs::write(dir.join(format!("{}.md", name)), content).expect("write file");
    }

    #[test]
    fn scan_finds_valid_files() {
        let dir = test_dir("scan_valid");
        write_memory_file(&dir, "alpha", "user", "First file", "Alpha body");
        // Small delay to ensure different mtimes.
        thread::sleep(Duration::from_millis(50));
        write_memory_file(&dir, "beta", "project", "Second file", "Beta body");

        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");

        assert_eq!(files.len(), 2);
        // Newest first -> beta before alpha.
        assert_eq!(files[0].frontmatter.name, "beta");
        assert_eq!(files[1].frontmatter.name, "alpha");
    }

    #[test]
    fn scan_skips_memory_md() {
        let dir = test_dir("scan_skip_memory");
        write_memory_file(&dir, "keep", "user", "Kept", "body");
        // Write a MEMORY.md that would parse fine.
        let entrypoint = "---\nname: index\ndescription: index\ntype: user\n---\nindex body";
        fs::write(dir.join("MEMORY.md"), entrypoint).expect("write");

        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].frontmatter.name, "keep");
    }

    #[test]
    fn scan_skips_non_md_files() {
        let dir = test_dir("scan_skip_non_md");
        write_memory_file(&dir, "valid", "user", "Valid", "body");
        fs::write(dir.join("notes.txt"), "not markdown").expect("write");
        fs::write(dir.join("data.json"), "{}").expect("write");

        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn scan_caps_at_max_files() {
        let dir = test_dir("scan_cap");
        for i in 0..10 {
            write_memory_file(
                &dir,
                &format!("file{:02}", i),
                "user",
                &format!("File {}", i),
                "body",
            );
            thread::sleep(Duration::from_millis(20));
        }

        let config = MemoryConfig {
            max_files: 3,
            ..Default::default()
        };
        let files = scan_memory_dir(&dir, &config).expect("scan");
        assert_eq!(files.len(), 3);
        // Should be the 3 newest.
        assert_eq!(files[0].frontmatter.name, "file09");
        assert_eq!(files[1].frontmatter.name, "file08");
        assert_eq!(files[2].frontmatter.name, "file07");
    }

    #[test]
    fn sort_memory_files_breaks_equal_mtime_ties_deterministically() {
        let modified = SystemTime::UNIX_EPOCH;
        let mut files = vec![
            MemoryFile {
                path: PathBuf::from("b.md"),
                frontmatter: MemoryFrontmatter {
                    name: "beta".to_string(),
                    description: "beta".to_string(),
                    memory_type: MemoryType::User,
                },
                modified,
                body: "beta".to_string(),
            },
            MemoryFile {
                path: PathBuf::from("c.md"),
                frontmatter: MemoryFrontmatter {
                    name: "gamma".to_string(),
                    description: "gamma".to_string(),
                    memory_type: MemoryType::User,
                },
                modified,
                body: "gamma".to_string(),
            },
            MemoryFile {
                path: PathBuf::from("a.md"),
                frontmatter: MemoryFrontmatter {
                    name: "alpha".to_string(),
                    description: "alpha".to_string(),
                    memory_type: MemoryType::User,
                },
                modified,
                body: "alpha".to_string(),
            },
        ];

        sort_memory_files(&mut files);

        assert_eq!(files[0].frontmatter.name, "gamma");
        assert_eq!(files[1].frontmatter.name, "beta");
        assert_eq!(files[2].frontmatter.name, "alpha");
    }

    #[test]
    fn scan_skips_invalid_frontmatter() {
        let dir = test_dir("scan_skip_invalid");
        write_memory_file(&dir, "good", "user", "Good one", "body");
        // Write a file with bad frontmatter (missing type).
        fs::write(
            dir.join("bad.md"),
            "---\nname: bad\ndescription: oops\n---\nbody",
        )
        .expect("write");

        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].frontmatter.name, "good");
    }

    #[test]
    fn scan_nonexistent_dir() {
        let dir = std::env::temp_dir().join("emberforge_memory_tests").join("no_such_dir");
        let _ = fs::remove_dir_all(&dir);
        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");
        assert!(files.is_empty());
    }

    // -----------------------------------------------------------------------
    // Entrypoint loading and truncation
    // -----------------------------------------------------------------------

    #[test]
    fn load_entrypoint_basic() {
        let dir = test_dir("entrypoint_basic");
        let content = "# Memory\n\nSome context here.\n";
        fs::write(dir.join("MEMORY.md"), content).expect("write");

        let config = MemoryConfig::default();
        let idx = load_entrypoint(&dir, &config)
            .expect("load")
            .expect("should find");
        assert!(idx.content.contains("# Memory"));
        assert!(idx.content.contains("Some context here."));
    }

    #[test]
    fn load_entrypoint_missing() {
        let dir = test_dir("entrypoint_missing");
        let config = MemoryConfig::default();
        let idx = load_entrypoint(&dir, &config).expect("load");
        assert!(idx.is_none());
    }

    #[test]
    fn load_entrypoint_truncates_lines() {
        let dir = test_dir("entrypoint_trunc_lines");
        let content: String = (0..500).map(|i| format!("Line {}\n", i)).collect();
        fs::write(dir.join("MEMORY.md"), &content).expect("write");

        let config = MemoryConfig {
            max_entrypoint_lines: 10,
            max_entrypoint_bytes: 100_000,
            ..Default::default()
        };
        let idx = load_entrypoint(&dir, &config)
            .expect("load")
            .expect("should find");
        let line_count = idx.content.lines().count();
        assert_eq!(line_count, 10);
        assert!(idx.content.starts_with("Line 0"));
    }

    #[test]
    fn load_entrypoint_truncates_bytes() {
        let dir = test_dir("entrypoint_trunc_bytes");
        let content: String = (0..100).map(|i| format!("Line {:04}\n", i)).collect();
        fs::write(dir.join("MEMORY.md"), &content).expect("write");

        let config = MemoryConfig {
            max_entrypoint_lines: 1000,
            max_entrypoint_bytes: 50,
            ..Default::default()
        };
        let idx = load_entrypoint(&dir, &config)
            .expect("load")
            .expect("should find");
        assert!(idx.content.len() <= 50);
    }

    // -----------------------------------------------------------------------
    // Memory manifest
    // -----------------------------------------------------------------------

    #[test]
    fn manifest_formatting() {
        let files = vec![
            MemoryFile {
                path: PathBuf::from("/mem/alpha.md"),
                frontmatter: MemoryFrontmatter {
                    name: "alpha".to_string(),
                    description: "First memory".to_string(),
                    memory_type: MemoryType::User,
                },
                // 2025-01-15 00:00:00 UTC
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_736_899_200),
                body: "body".to_string(),
            },
            MemoryFile {
                path: PathBuf::from("/mem/beta.md"),
                frontmatter: MemoryFrontmatter {
                    name: "beta".to_string(),
                    description: "Second memory".to_string(),
                    memory_type: MemoryType::Project,
                },
                // 2025-03-20 00:00:00 UTC
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_742_428_800),
                body: "body".to_string(),
            },
        ];

        let manifest = build_memory_manifest(&files);
        let lines: Vec<&str> = manifest.lines().collect();
        assert_eq!(lines.len(), 2);

        assert!(lines[0].starts_with("[user] alpha.md ("));
        assert!(lines[0].contains("\u{2014} First memory"));
        assert!(lines[1].starts_with("[project] beta.md ("));
        assert!(lines[1].contains("\u{2014} Second memory"));
    }

    #[test]
    fn manifest_empty() {
        let manifest = build_memory_manifest(&[]);
        assert!(manifest.is_empty());
    }

    // -----------------------------------------------------------------------
    // ensure_memory_dir
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_memory_dir_creates_fresh() {
        let dir = test_dir("ensure_fresh").join("memory");
        assert!(!dir.exists());

        ensure_memory_dir(&dir).expect("ensure");
        assert!(dir.is_dir());
        assert!(dir.join("MEMORY.md").is_file());

        let content = fs::read_to_string(dir.join("MEMORY.md")).expect("read");
        assert!(content.contains("# Memory"));
    }

    #[test]
    fn ensure_memory_dir_idempotent() {
        let dir = test_dir("ensure_idempotent").join("memory");

        ensure_memory_dir(&dir).expect("first call");
        // Write custom content to MEMORY.md.
        fs::write(dir.join("MEMORY.md"), "# Custom\n").expect("write");

        ensure_memory_dir(&dir).expect("second call");
        // Custom content should be preserved (not overwritten).
        let content = fs::read_to_string(dir.join("MEMORY.md")).expect("read");
        assert_eq!(content, "# Custom\n");
    }

    #[test]
    fn ensure_memory_dir_nested() {
        let dir = test_dir("ensure_nested")
            .join("a")
            .join("b")
            .join("memory");
        ensure_memory_dir(&dir).expect("ensure nested");
        assert!(dir.join("MEMORY.md").is_file());
    }

    // -----------------------------------------------------------------------
    // Integration: scan + manifest
    // -----------------------------------------------------------------------

    #[test]
    fn scan_and_manifest_roundtrip() {
        let dir = test_dir("roundtrip");
        write_memory_file(&dir, "note1", "feedback", "Bug report", "details...");
        thread::sleep(Duration::from_millis(50));
        write_memory_file(&dir, "note2", "reference", "API docs", "docs...");

        let config = MemoryConfig::default();
        let files = scan_memory_dir(&dir, &config).expect("scan");
        let manifest = build_memory_manifest(&files);

        assert!(manifest.contains("[reference] note2.md"));
        assert!(manifest.contains("[feedback] note1.md"));

        // Verify ordering in the manifest (newest first).
        let ref_pos = manifest.find("[reference]").unwrap();
        let fb_pos = manifest.find("[feedback]").unwrap();
        assert!(ref_pos < fb_pos, "reference (newer) should come first");
    }

    // -----------------------------------------------------------------------
    // Date formatting
    // -----------------------------------------------------------------------

    #[test]
    fn format_date_known_epoch() {
        // 2024-06-15 is day 19889 since epoch.
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_718_409_600);
        let date = format_date(t);
        assert_eq!(date, "2024-06-15");
    }

    #[test]
    fn format_date_unix_epoch() {
        let date = format_date(SystemTime::UNIX_EPOCH);
        assert_eq!(date, "1970-01-01");
    }

    // -----------------------------------------------------------------------
    // Truncation helper
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_respects_both_limits() {
        let input = "aaa\nbbb\nccc\nddd\neee\n";
        // Line limit first.
        let out = truncate_content(input, 2, 10000);
        assert_eq!(out, "aaa\nbbb");
        // Byte limit first.
        let out = truncate_content(input, 100, 7);
        assert_eq!(out, "aaa\nbbb");
    }

    // -----------------------------------------------------------------------
    // strip_quotes helper
    // -----------------------------------------------------------------------

    #[test]
    fn strip_quotes_cases() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("'hello'"), "hello");
        assert_eq!(strip_quotes("hello"), "hello");
        assert_eq!(strip_quotes("\"mixed'"), "\"mixed'");
        assert_eq!(strip_quotes("  \"spaced\"  "), "spaced");
    }
}
