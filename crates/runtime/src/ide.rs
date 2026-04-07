use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Supported IDE types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdeKind {
    VsCode,
    Cursor,
    Windsurf,
    IntelliJ,
    WebStorm,
    PyCharm,
    GoLand,
    RustRover,
    Unknown,
}

impl IdeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VsCode => "vscode",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::IntelliJ => "intellij",
            Self::WebStorm => "webstorm",
            Self::PyCharm => "pycharm",
            Self::GoLand => "goland",
            Self::RustRover => "rustrover",
            Self::Unknown => "unknown",
        }
    }

    /// Parse from a name string (case-insensitive).
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "vscode" | "vs code" | "visual studio code" | "code" => Self::VsCode,
            "cursor" => Self::Cursor,
            "windsurf" => Self::Windsurf,
            "intellij" | "intellij idea" | "idea" => Self::IntelliJ,
            "webstorm" => Self::WebStorm,
            "pycharm" => Self::PyCharm,
            "goland" => Self::GoLand,
            "rustrover" | "rust rover" => Self::RustRover,
            _ => Self::Unknown,
        }
    }

    /// Whether this IDE supports MCP bridge communication.
    #[must_use]
    pub fn supports_mcp(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

impl std::fmt::Display for IdeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Transport protocol for IDE communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdeTransport {
    WebSocket,
    Sse,
}

/// Information about a detected IDE instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedIde {
    /// IDE type.
    pub kind: IdeKind,
    /// Display name (e.g., "VS Code", "`IntelliJ IDEA`"). Populated from the lockfile.
    pub name: String,
    /// Communication port.
    pub port: u16,
    /// Connection URL (ws:// or http://).
    pub url: String,
    /// Transport protocol.
    pub transport: IdeTransport,
    /// Workspace folders the IDE has open.
    pub workspace_folders: Vec<PathBuf>,
    /// Optional auth token.
    pub auth_token: Option<String>,
    /// PID of the IDE process.
    pub pid: Option<u32>,
    /// Path to the lockfile that was read.
    pub lockfile_path: PathBuf,
}

/// IDE lockfile format (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeLockfile {
    pub ide_name: Option<String>,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub workspace_folders: Option<Vec<String>>,
    pub transport: Option<String>,
    pub auth_token: Option<String>,
    pub url: Option<String>,
}

/// Selection context from an IDE (highlighted code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeSelection {
    /// File path.
    pub file: PathBuf,
    /// Start line (1-based).
    pub start_line: u32,
    /// End line (1-based).
    pub end_line: u32,
    /// Selected text content.
    pub text: String,
    /// Language ID (e.g., "rust", "typescript").
    pub language: Option<String>,
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Return the list of directories to scan for IDE lockfiles.
///
/// The search order is:
///   1. `<cwd>/.ide-lockfiles/`
///   2. `~/.ember/ide-locks/`
///   3. `<tmp>/ember-ide-locks/`
#[must_use]
pub fn lockfile_search_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(3);

    // Project-local lockfiles.
    dirs.push(cwd.join(".ide-lockfiles"));

    // User-global lockfiles.
    if let Some(home) = home_dir() {
        dirs.push(home.join(".ember").join("ide-locks"));
    }

    // Temp dir fallback.
    dirs.push(std::env::temp_dir().join("ember-ide-locks"));

    dirs
}

/// Scan for IDE lockfiles and return detected IDE instances.
///
/// Searches in: `cwd/.ide-lockfiles/`, `~/.ember/ide-locks/`, and temp dir
/// patterns.  Each `*.lock.json` file found is parsed; invalid files are
/// silently skipped.
#[must_use]
pub fn detect_ides(cwd: &Path) -> Vec<DetectedIde> {
    let mut found: Vec<DetectedIde> = Vec::new();

    for dir in lockfile_search_dirs(cwd) {
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !name.ends_with(".lock.json") {
                continue;
            }
            if let Ok(Some(ide)) = parse_lockfile(&path) {
                found.push(ide);
            }
        }
    }

    // Stable sort: VS Code first, then alphabetical by name.
    found.sort_by(|a, b| {
        let a_vscode = a.kind == IdeKind::VsCode;
        let b_vscode = b.kind == IdeKind::VsCode;
        b_vscode.cmp(&a_vscode).then_with(|| a.name.cmp(&b.name))
    });

    found
}

/// Parse a single IDE lockfile.
///
/// Returns `Ok(None)` when the file exists but is not a valid lockfile (e.g.
/// missing required fields).  Returns `Err` only on I/O failures.
pub fn parse_lockfile(path: &Path) -> io::Result<Option<DetectedIde>> {
    let data = std::fs::read_to_string(path)?;
    let lockfile: IdeLockfile = match serde_json::from_str(&data) {
        Ok(lf) => lf,
        Err(_) => return Ok(None),
    };

    // Port is required.
    let Some(port) = lockfile.port else { return Ok(None) };

    let ide_name = lockfile.ide_name.unwrap_or_default();
    let kind = IdeKind::from_name(&ide_name);

    let transport = match lockfile.transport.as_deref() {
        Some("sse") => IdeTransport::Sse,
        _ => IdeTransport::WebSocket,
    };

    let url = lockfile.url.unwrap_or_else(|| match transport {
        IdeTransport::WebSocket => format!("ws://localhost:{port}"),
        IdeTransport::Sse => format!("http://localhost:{port}"),
    });

    let display_name = if ide_name.is_empty() {
        kind.as_str().to_owned()
    } else {
        ide_name
    };

    let workspace_folders = lockfile
        .workspace_folders
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect();

    Ok(Some(DetectedIde {
        kind,
        name: display_name,
        port,
        url,
        transport,
        workspace_folders,
        auth_token: lockfile.auth_token,
        pid: lockfile.pid,
        lockfile_path: path.to_path_buf(),
    }))
}

/// Check if a specific IDE kind is running (based on lockfile detection).
#[must_use]
pub fn is_ide_running(cwd: &Path, kind: IdeKind) -> bool {
    detect_ides(cwd).iter().any(|ide| ide.kind == kind)
}

/// Get the primary detected IDE (first found, preferring VS Code).
#[must_use]
pub fn primary_ide(cwd: &Path) -> Option<DetectedIde> {
    detect_ides(cwd).into_iter().next()
}

// ---------------------------------------------------------------------------
// Communication
// ---------------------------------------------------------------------------

/// Build a connection URL for an IDE instance.
#[must_use]
pub fn build_connection_url(ide: &DetectedIde) -> String {
    let base = match ide.transport {
        IdeTransport::WebSocket => format!("ws://localhost:{}", ide.port),
        IdeTransport::Sse => format!("http://localhost:{}", ide.port),
    };

    match &ide.auth_token {
        Some(token) => format!("{base}?token={token}"),
        None => base,
    }
}

/// Format a diff for IDE preview display.
///
/// Returns a JSON structure suitable for sending to IDE extensions:
/// ```json
/// {
///   "file": "<path>",
///   "hunks": [
///     { "old_start": 1, "new_start": 1, "lines": [" ctx", "-old", "+new", ...] }
///   ]
/// }
/// ```
///
/// Uses a simple line-by-line comparison (not a full Myers diff).
#[must_use]
pub fn format_diff_for_ide(
    file_path: &str,
    old_content: &str,
    new_content: &str,
) -> serde_json::Value {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    let mut hunks: Vec<serde_json::Value> = Vec::new();
    let mut diff_lines: Vec<String> = Vec::new();

    let max_len = old_lines.len().max(new_lines.len());
    let mut i = 0;
    while i < max_len {
        let old_line = old_lines.get(i).copied();
        let new_line = new_lines.get(i).copied();

        match (old_line, new_line) {
            (Some(o), Some(n)) if o == n => {
                diff_lines.push(format!(" {o}"));
            }
            (Some(o), Some(n)) => {
                diff_lines.push(format!("-{o}"));
                diff_lines.push(format!("+{n}"));
            }
            (Some(o), None) => {
                diff_lines.push(format!("-{o}"));
            }
            (None, Some(n)) => {
                diff_lines.push(format!("+{n}"));
            }
            (None, None) => break,
        }
        i += 1;
    }

    if !diff_lines.is_empty() {
        hunks.push(serde_json::json!({
            "old_start": 1,
            "new_start": 1,
            "lines": diff_lines,
        }));
    }

    serde_json::json!({
        "file": file_path,
        "hunks": hunks,
    })
}

// ---------------------------------------------------------------------------
// Path conversion (WSL)
// ---------------------------------------------------------------------------

/// Detect if running inside WSL.
///
/// Checks `/proc/version` for the strings "microsoft" or "WSL".
#[must_use]
pub fn is_wsl() -> bool {
    match std::fs::read_to_string("/proc/version") {
        Ok(contents) => {
            let lower = contents.to_ascii_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        }
        Err(_) => false,
    }
}

/// Convert a Unix path to a Windows path (for WSL scenarios).
///
/// `/mnt/c/Users/foo` -> `C:\Users\foo`
#[must_use]
pub fn unix_to_windows_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/mnt/") {
        if let Some((drive, remainder)) = rest.split_once('/') {
            if drive.len() == 1 && drive.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            {
                let win_remainder = remainder.replace('/', "\\");
                return format!("{}:\\{}", drive.to_ascii_uppercase(), win_remainder);
            }
        }
        // Single drive letter with no further path.
        if rest.len() == 1 && rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return format!("{}:\\", rest.to_ascii_uppercase());
        }
    }
    // Fallback: just swap slashes.
    path.replace('/', "\\")
}

/// Convert a Windows path to a Unix path (for WSL scenarios).
///
/// `C:\Users\foo` -> `/mnt/c/Users/foo`
#[must_use]
pub fn windows_to_unix_path(path: &str) -> String {
    // Handle `C:\...` or `C:/...`
    let bytes = path.as_bytes();
    if bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':')
    {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = if bytes.len() > 2 { &path[2..] } else { "" };
        let unix_rest = rest.replace('\\', "/");
        return format!("/mnt/{drive}{unix_rest}");
    }
    path.replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Build IDE context information for system prompt injection.
///
/// Returns a human-readable string such as
/// `"IDE: VS Code (port 3000, ws://localhost:3000)"`, or `None` when no IDE is
/// detected.
#[must_use]
pub fn build_ide_context(cwd: &Path) -> Option<String> {
    let ide = primary_ide(cwd)?;
    let proto = match ide.transport {
        IdeTransport::WebSocket => "ws",
        IdeTransport::Sse => "sse",
    };
    Some(format!(
        "IDE: {} (port {}, {}, {})",
        ide.name, ide.port, proto, ide.url,
    ))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Portable home directory lookup (avoids pulling in the `dirs` crate).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique temporary directory for a test.
    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("emberforge_ide_tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // 1. IdeKind::from_name parsing
    #[test]
    fn test_ide_kind_from_name() {
        assert_eq!(IdeKind::from_name("vscode"), IdeKind::VsCode);
        assert_eq!(IdeKind::from_name("VS Code"), IdeKind::VsCode);
        assert_eq!(IdeKind::from_name("Visual Studio Code"), IdeKind::VsCode);
        assert_eq!(IdeKind::from_name("Code"), IdeKind::VsCode);
        assert_eq!(IdeKind::from_name("CURSOR"), IdeKind::Cursor);
        assert_eq!(IdeKind::from_name("Windsurf"), IdeKind::Windsurf);
        assert_eq!(IdeKind::from_name("IntelliJ"), IdeKind::IntelliJ);
        assert_eq!(IdeKind::from_name("IntelliJ IDEA"), IdeKind::IntelliJ);
        assert_eq!(IdeKind::from_name("IDEA"), IdeKind::IntelliJ);
        assert_eq!(IdeKind::from_name("WebStorm"), IdeKind::WebStorm);
        assert_eq!(IdeKind::from_name("PyCharm"), IdeKind::PyCharm);
        assert_eq!(IdeKind::from_name("GoLand"), IdeKind::GoLand);
        assert_eq!(IdeKind::from_name("RustRover"), IdeKind::RustRover);
        assert_eq!(IdeKind::from_name("Rust Rover"), IdeKind::RustRover);
        assert_eq!(IdeKind::from_name("emacs"), IdeKind::Unknown);
        assert_eq!(IdeKind::from_name(""), IdeKind::Unknown);
    }

    // 2. IdeKind::supports_mcp
    #[test]
    fn test_supports_mcp() {
        assert!(IdeKind::VsCode.supports_mcp());
        assert!(IdeKind::Cursor.supports_mcp());
        assert!(IdeKind::IntelliJ.supports_mcp());
        assert!(IdeKind::RustRover.supports_mcp());
        assert!(!IdeKind::Unknown.supports_mcp());
    }

    // 3. Parse valid lockfile JSON
    #[test]
    fn test_parse_valid_lockfile() {
        let dir = tmp_dir("valid_lockfile");
        let lock_path = dir.join("vscode.lock.json");
        fs::write(
            &lock_path,
            serde_json::json!({
                "ideName": "VS Code",
                "port": 3000,
                "pid": 12345,
                "workspaceFolders": ["/home/user/project"],
                "transport": "websocket",
                "authToken": "secret123",
                "url": "ws://localhost:3000"
            })
            .to_string(),
        )
        .unwrap();

        let ide = parse_lockfile(&lock_path).unwrap().unwrap();
        assert_eq!(ide.kind, IdeKind::VsCode);
        assert_eq!(ide.name, "VS Code");
        assert_eq!(ide.port, 3000);
        assert_eq!(ide.pid, Some(12345));
        assert_eq!(ide.auth_token, Some("secret123".into()));
        assert_eq!(ide.transport, IdeTransport::WebSocket);
        assert_eq!(ide.workspace_folders, vec![PathBuf::from("/home/user/project")]);
        assert_eq!(ide.url, "ws://localhost:3000");
    }

    // 4. Parse lockfile with missing optional fields
    #[test]
    fn test_parse_lockfile_minimal() {
        let dir = tmp_dir("minimal_lockfile");
        let lock_path = dir.join("ide.lock.json");
        fs::write(
            &lock_path,
            serde_json::json!({ "port": 8080 }).to_string(),
        )
        .unwrap();

        let ide = parse_lockfile(&lock_path).unwrap().unwrap();
        assert_eq!(ide.kind, IdeKind::Unknown);
        assert_eq!(ide.port, 8080);
        assert_eq!(ide.pid, None);
        assert_eq!(ide.auth_token, None);
        assert!(ide.workspace_folders.is_empty());
        // Default URL for WebSocket transport.
        assert_eq!(ide.url, "ws://localhost:8080");
    }

    // 5. Parse invalid JSON returns None
    #[test]
    fn test_parse_invalid_json() {
        let dir = tmp_dir("invalid_json");
        let lock_path = dir.join("bad.lock.json");
        fs::write(&lock_path, "not json at all {{{").unwrap();

        let result = parse_lockfile(&lock_path).unwrap();
        assert!(result.is_none());
    }

    // 6. unix_to_windows_path
    #[test]
    fn test_unix_to_windows_path() {
        assert_eq!(
            unix_to_windows_path("/mnt/c/Users/foo/file.txt"),
            "C:\\Users\\foo\\file.txt"
        );
        assert_eq!(
            unix_to_windows_path("/mnt/d/projects"),
            "D:\\projects"
        );
        assert_eq!(
            unix_to_windows_path("/home/user/file"),
            "\\home\\user\\file"
        );
    }

    // 7. windows_to_unix_path
    #[test]
    fn test_windows_to_unix_path() {
        assert_eq!(
            windows_to_unix_path("C:\\Users\\foo\\file.txt"),
            "/mnt/c/Users/foo/file.txt"
        );
        assert_eq!(
            windows_to_unix_path("D:\\projects"),
            "/mnt/d/projects"
        );
        assert_eq!(
            windows_to_unix_path("C:"),
            "/mnt/c"
        );
    }

    // 8. format_diff_for_ide produces valid JSON
    #[test]
    fn test_format_diff_for_ide() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nchanged\nline3\nline4\n";

        let diff = format_diff_for_ide("src/main.rs", old, new);
        assert_eq!(diff["file"], "src/main.rs");

        let hunks = diff["hunks"].as_array().unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["old_start"], 1);
        assert_eq!(hunks[0]["new_start"], 1);

        let lines = hunks[0]["lines"].as_array().unwrap();
        assert!(lines.iter().any(|l| l.as_str().unwrap() == "-line2"));
        assert!(lines.iter().any(|l| l.as_str().unwrap() == "+changed"));
        assert!(lines.iter().any(|l| l.as_str().unwrap() == " line1"));
    }

    // 9. build_connection_url formats correctly
    #[test]
    fn test_build_connection_url() {
        let ide = DetectedIde {
            kind: IdeKind::VsCode,
            name: "VS Code".into(),
            port: 3000,
            url: "ws://localhost:3000".into(),
            transport: IdeTransport::WebSocket,
            workspace_folders: vec![],
            auth_token: Some("tok123".into()),
            pid: None,
            lockfile_path: PathBuf::from("/tmp/test.lock.json"),
        };
        assert_eq!(
            build_connection_url(&ide),
            "ws://localhost:3000?token=tok123"
        );

        let ide_no_auth = DetectedIde {
            auth_token: None,
            transport: IdeTransport::Sse,
            port: 8080,
            ..ide.clone()
        };
        assert_eq!(build_connection_url(&ide_no_auth), "http://localhost:8080");
    }

    // 10. DetectedIde serialization round-trip
    #[test]
    fn test_detected_ide_roundtrip() {
        let ide = DetectedIde {
            kind: IdeKind::Cursor,
            name: "Cursor".into(),
            port: 4000,
            url: "ws://localhost:4000".into(),
            transport: IdeTransport::WebSocket,
            workspace_folders: vec![PathBuf::from("/proj")],
            auth_token: Some("abc".into()),
            pid: Some(999),
            lockfile_path: PathBuf::from("/tmp/cursor.lock.json"),
        };
        let json = serde_json::to_string(&ide).unwrap();
        let deser: DetectedIde = serde_json::from_str(&json).unwrap();
        assert_eq!(ide, deser);
    }

    // 11. lockfile_search_dirs includes cwd
    #[test]
    fn test_lockfile_search_dirs_includes_cwd() {
        let cwd = PathBuf::from("/some/project");
        let dirs = lockfile_search_dirs(&cwd);
        assert!(dirs.contains(&cwd.join(".ide-lockfiles")));
        assert!(dirs.len() >= 2); // at least cwd + temp
    }

    // 12. IdeSelection serialization
    #[test]
    fn test_ide_selection_serialization() {
        let sel = IdeSelection {
            file: PathBuf::from("/src/main.rs"),
            start_line: 10,
            end_line: 20,
            text: "fn main() {}".into(),
            language: Some("rust".into()),
        };
        let json = serde_json::to_string(&sel).unwrap();
        let deser: IdeSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, deser);
    }
}
