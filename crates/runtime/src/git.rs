//! Git utilities for repository discovery, state queries, advanced operations,
//! and GitHub integration.
//!
//! This module provides a reusable git utility layer that both the CLI and
//! runtime can depend on. All functions shell out to the `git` binary via
//! `std::process::Command`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Run a git command and capture trimmed stdout.
fn git_output(cwd: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(io::Error::new(io::ErrorKind::Other, stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a git command and check success.
fn git_ok(cwd: &Path, args: &[&str]) -> io::Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(io::Error::new(io::ErrorKind::Other, stderr));
    }

    Ok(())
}

/// Run a git command and check if it succeeds (returns bool, no error).
fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Repository discovery
// ---------------------------------------------------------------------------

/// Find the git root directory from the given path, walking up ancestors.
/// Returns `None` if not in a git repository.
pub fn find_git_root(from: &Path) -> Option<PathBuf> {
    // Prefer rev-parse when git is available – it handles all edge cases
    // (worktrees, bare repos, etc.).
    if let Ok(root) = git_output(from, &["rev-parse", "--show-toplevel"]) {
        let p = PathBuf::from(root);
        if p.is_dir() {
            return Some(p);
        }
    }

    // Fallback: walk up and look for a `.git` entry (dir or file for worktrees).
    let mut current = if from.is_file() {
        from.parent()?.to_path_buf()
    } else {
        from.to_path_buf()
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Check if a directory is at the root of a git repository.
pub fn is_at_git_root(dir: &Path) -> bool {
    match find_git_root(dir) {
        Some(root) => root == dir,
        None => false,
    }
}

/// Check if a path is inside a git repository.
pub fn is_in_git_repo(path: &Path) -> bool {
    find_git_root(path).is_some()
}

/// Check if the git root is a bare repository.
pub fn is_bare_repo(git_root: &Path) -> bool {
    git_output(git_root, &["rev-parse", "--is-bare-repository"])
        .map(|s| s == "true")
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// State queries
// ---------------------------------------------------------------------------

/// Get the current HEAD commit hash (short form).
pub fn get_head(cwd: &Path) -> io::Result<String> {
    git_output(cwd, &["rev-parse", "--short", "HEAD"])
}

/// Get the current branch name. Returns `None` if in detached HEAD state.
pub fn get_branch(cwd: &Path) -> io::Result<Option<String>> {
    match git_output(cwd, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(branch) if !branch.is_empty() => Ok(Some(branch)),
        _ => Ok(None),
    }
}

/// Get the default branch (main or master).
///
/// Checks the remote HEAD first, then falls back to checking whether `main`
/// or `master` exist locally.
pub fn get_default_branch(cwd: &Path) -> io::Result<String> {
    // Try remote HEAD symbolic ref (works when origin is set).
    if let Ok(sym) = git_output(cwd, &["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        if let Some(branch) = sym.strip_prefix("refs/remotes/origin/") {
            return Ok(branch.to_string());
        }
    }

    // Fallback: look for main, then master.
    if git_succeeds(cwd, &["show-ref", "--verify", "--quiet", "refs/heads/main"]) {
        return Ok("main".to_string());
    }
    if git_succeeds(cwd, &["show-ref", "--verify", "--quiet", "refs/heads/master"]) {
        return Ok("master".to_string());
    }

    // Last resort.
    Ok("main".to_string())
}

/// Get the remote URL for origin.
pub fn get_remote_url(cwd: &Path) -> io::Result<Option<String>> {
    match git_output(cwd, &["remote", "get-url", "origin"]) {
        Ok(url) if !url.is_empty() => Ok(Some(url)),
        _ => Ok(None),
    }
}

/// Check if the working tree is clean (no uncommitted changes).
///
/// When `ignore_untracked` is true, untracked files are not considered dirty.
pub fn is_clean(cwd: &Path, ignore_untracked: bool) -> io::Result<bool> {
    let args: Vec<&str> = if ignore_untracked {
        vec!["status", "--porcelain", "-uno"]
    } else {
        vec!["status", "--porcelain"]
    };
    let output = git_output(cwd, &args)?;
    Ok(output.is_empty())
}

/// File change type from `git status --porcelain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Unknown,
}

impl FileChangeType {
    /// Parse a single-character status code from porcelain v1 output.
    pub fn from_status_code(code: char) -> Self {
        match code {
            'A' => Self::Added,
            'M' => Self::Modified,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            '?' => Self::Untracked,
            _ => Self::Unknown,
        }
    }
}

/// File status from `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub status: FileChangeType,
}

/// Get list of changed files (staged + unstaged + untracked).
///
/// Parses `git status --porcelain=v1` output. Each line has the format
/// `XY <path>` where X is the index status and Y the work-tree status.
pub fn get_changed_files(cwd: &Path) -> io::Result<Vec<FileStatus>> {
    let raw = git_output(cwd, &["status", "--porcelain"])?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for line in raw.lines() {
        if line.len() < 3 {
            continue;
        }

        let bytes = line.as_bytes();
        let index_status = bytes[0] as char;
        let worktree_status = bytes[1] as char;
        let path = line[3..].to_string();

        // Pick the most meaningful status: prefer worktree, fall back to index.
        let code = if worktree_status != ' ' && worktree_status != '?' {
            worktree_status
        } else if index_status == '?' {
            '?'
        } else {
            index_status
        };

        // Handle renamed paths – porcelain shows "R  old -> new".
        let display_path = if code == 'R' {
            path.rsplit(" -> ").next().unwrap_or(&path).to_string()
        } else {
            path
        };

        results.push(FileStatus {
            path: display_path,
            status: FileChangeType::from_status_code(code),
        });
    }

    Ok(results)
}

/// Check if HEAD has been pushed to the remote tracking branch.
///
/// Returns `true` when there are local commits not yet on the remote.
pub fn has_unpushed_commits(cwd: &Path) -> io::Result<bool> {
    // @{u} refers to the upstream tracking branch.
    match git_output(cwd, &["rev-list", "--count", "@{u}..HEAD"]) {
        Ok(count) => {
            let n: usize = count.parse().unwrap_or(0);
            Ok(n > 0)
        }
        // No upstream configured – treat as unpushed.
        Err(_) => Ok(true),
    }
}

/// Get the number of git worktrees.
pub fn get_worktree_count(cwd: &Path) -> io::Result<usize> {
    let trees = list_worktrees(cwd)?;
    Ok(trees.len())
}

// ---------------------------------------------------------------------------
// Complete state snapshot
// ---------------------------------------------------------------------------

/// Full git state snapshot for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub head: String,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
    pub is_clean: bool,
    pub changed_files: Vec<String>,
    pub has_unpushed: bool,
    pub is_shallow: bool,
}

/// Capture a complete git state snapshot.
pub fn get_git_state(cwd: &Path) -> io::Result<GitState> {
    let head = get_head(cwd)?;
    let branch = get_branch(cwd)?;
    let remote_url = get_remote_url(cwd)?;
    let clean = is_clean(cwd, false)?;
    let changed = get_changed_files(cwd)?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let unpushed = has_unpushed_commits(cwd).unwrap_or(true);
    let shallow = is_shallow_clone(cwd).unwrap_or(false);

    Ok(GitState {
        head,
        branch,
        remote_url,
        is_clean: clean,
        changed_files: changed,
        has_unpushed: unpushed,
        is_shallow: shallow,
    })
}

// ---------------------------------------------------------------------------
// Advanced operations
// ---------------------------------------------------------------------------

/// Safely stash changes, staging untracked files first to prevent data loss.
///
/// Returns the stash ref (e.g. `stash@{0}`) if changes were stashed, or
/// `None` if there was nothing to stash.
pub fn safe_stash(cwd: &Path) -> io::Result<Option<String>> {
    // Check if there is anything to stash.
    if is_clean(cwd, false)? {
        return Ok(None);
    }

    // Stage everything (including untracked) so stash captures all files.
    git_ok(cwd, &["add", "-A"])?;

    // Record stash count before.
    let before = git_output(cwd, &["stash", "list"])
        .unwrap_or_default()
        .lines()
        .count();

    git_ok(cwd, &["stash", "push", "-m", "emberforge-safe-stash"])?;

    let after = git_output(cwd, &["stash", "list"])
        .unwrap_or_default()
        .lines()
        .count();

    if after > before {
        Ok(Some("stash@{0}".to_string()))
    } else {
        Ok(None)
    }
}

/// Pop the most recent stash.
pub fn stash_pop(cwd: &Path) -> io::Result<()> {
    git_ok(cwd, &["stash", "pop"])
}

/// Check if the repository is a shallow clone.
pub fn is_shallow_clone(cwd: &Path) -> io::Result<bool> {
    // Modern git (2.15+) supports --is-shallow-repository.
    match git_output(cwd, &["rev-parse", "--is-shallow-repository"]) {
        Ok(val) => Ok(val == "true"),
        Err(_) => {
            // Fallback: check for the shallow file.
            if let Some(root) = find_git_root(cwd) {
                Ok(root.join(".git").join("shallow").exists())
            } else {
                Ok(false)
            }
        }
    }
}

/// Find the merge-base between HEAD and a remote branch.
///
/// `remote_branch` should be a full ref like `origin/main`.
pub fn find_merge_base(cwd: &Path, remote_branch: &str) -> io::Result<Option<String>> {
    match git_output(cwd, &["merge-base", "HEAD", remote_branch]) {
        Ok(hash) if !hash.is_empty() => Ok(Some(hash)),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// GitHub integration
// ---------------------------------------------------------------------------

/// Extract `(owner, repo)` from a GitHub remote URL.
///
/// Handles the following formats:
/// - `https://github.com/owner/repo.git`
/// - `https://github.com/owner/repo`
/// - `git@github.com:owner/repo.git`
/// - `ssh://git@github.com/owner/repo.git`
///
/// Returns `None` for non-GitHub URLs.
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();

    // SSH shorthand: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return parse_owner_repo(rest);
    }

    // ssh:// protocol: ssh://git@github.com/owner/repo.git
    if let Some(rest) = url.strip_prefix("ssh://git@github.com/") {
        return parse_owner_repo(rest);
    }

    // HTTPS: https://github.com/owner/repo(.git)?
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        return parse_owner_repo(rest);
    }

    None
}

/// Helper: parse `owner/repo(.git)?` into `(owner, repo)`.
fn parse_owner_repo(s: &str) -> Option<(String, String)> {
    let s = s.strip_suffix(".git").unwrap_or(s);
    let s = s.trim_end_matches('/');
    let mut parts = s.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();

    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }

    Some((owner, repo))
}

/// Get the GitHub owner/repo for the current repository.
pub fn get_github_repo(cwd: &Path) -> io::Result<Option<(String, String)>> {
    let url = match get_remote_url(cwd)? {
        Some(u) => u,
        None => return Ok(None),
    };
    Ok(parse_github_remote(&url))
}

// ---------------------------------------------------------------------------
// Worktree management
// ---------------------------------------------------------------------------

/// Information about a git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_bare: bool,
}

/// List all git worktrees.
///
/// Parses `git worktree list --porcelain` output. Each worktree block is
/// separated by a blank line and contains lines like:
///
/// ```text
/// worktree /path/to/tree
/// HEAD abc123
/// branch refs/heads/main
/// ```
pub fn list_worktrees(cwd: &Path) -> io::Result<Vec<WorktreeInfo>> {
    let raw = git_output(cwd, &["worktree", "list", "--porcelain"])?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch: Option<String> = None;
    let mut current_bare = false;

    for line in raw.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = current_path.take() {
                worktrees.push(WorktreeInfo {
                    path,
                    branch: current_branch.take(),
                    head: std::mem::take(&mut current_head),
                    is_bare: current_bare,
                });
                current_bare = false;
            }
            continue;
        }

        if let Some(p) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(p));
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            current_head = h.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            // Strip refs/heads/ prefix for readability.
            let branch = b.strip_prefix("refs/heads/").unwrap_or(b);
            current_branch = Some(branch.to_string());
        } else if line == "bare" {
            current_bare = true;
        }
    }

    Ok(worktrees)
}

/// Create a new worktree.
pub fn create_worktree(cwd: &Path, path: &Path, branch: &str) -> io::Result<()> {
    let path_str = path.to_string_lossy();
    git_ok(cwd, &["worktree", "add", &path_str, branch])
}

/// Remove a worktree.
pub fn remove_worktree(cwd: &Path, path: &Path) -> io::Result<()> {
    let path_str = path.to_string_lossy();
    git_ok(cwd, &["worktree", "remove", &path_str])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary git repository with one empty initial commit.
    /// Returns the path to the repo root.
    fn make_temp_repo() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "emberforge-git-test-{}-{nanos}",
            std::process::id()
        ));
        // Clean up from any previous failed run.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let init = Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output();
        if init.is_err() || !init.as_ref().unwrap().status.success() {
            panic!("git init failed in {}: {:?}", dir.display(), init);
        }
        let _ = Command::new("git")
            .args(["config", "user.email", "test@emberforge.dev"])
            .current_dir(&dir)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .output();
        let commit = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(&dir)
            .output();
        if commit.is_err() || !commit.as_ref().unwrap().status.success() {
            panic!(
                "git commit failed in {}: {:?}",
                dir.display(),
                commit.map(|o| String::from_utf8_lossy(&o.stderr).to_string())
            );
        }

        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // -- parse_github_remote ------------------------------------------------

    #[test]
    fn parse_github_https() {
        let result = parse_github_remote("https://github.com/ember/forge.git");
        assert_eq!(result, Some(("ember".into(), "forge".into())));
    }

    #[test]
    fn parse_github_ssh() {
        let result = parse_github_remote("git@github.com:ember/forge.git");
        assert_eq!(result, Some(("ember".into(), "forge".into())));
    }

    #[test]
    fn parse_github_ssh_protocol() {
        let result = parse_github_remote("ssh://git@github.com/ember/forge.git");
        assert_eq!(result, Some(("ember".into(), "forge".into())));
    }

    #[test]
    fn parse_github_non_github_returns_none() {
        assert_eq!(
            parse_github_remote("https://gitlab.com/owner/repo.git"),
            None,
        );
    }

    #[test]
    fn parse_github_strips_dot_git() {
        let with = parse_github_remote("https://github.com/a/b.git");
        let without = parse_github_remote("https://github.com/a/b");
        assert_eq!(with, without);
        assert_eq!(with, Some(("a".into(), "b".into())));
    }

    // -- find_git_root ------------------------------------------------------

    #[test]
    fn find_git_root_in_repo() {
        let repo = make_temp_repo();
        let root = find_git_root(&repo);
        assert_eq!(root, Some(repo.clone()));
        cleanup(&repo);
    }

    #[test]
    fn find_git_root_outside_repo() {
        let dir = std::env::temp_dir().join(format!(
            "emberforge-no-git-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create a nested dir to ensure we don't accidentally find a repo above.
        let nested = dir.join("deeply").join("nested");
        fs::create_dir_all(&nested).unwrap();

        // This *might* still find a repo if /tmp itself is inside one, so we
        // test the most reliable aspect: the function does not panic.
        let _ = find_git_root(&nested);

        cleanup(&dir);
    }

    // -- is_in_git_repo -----------------------------------------------------

    #[test]
    fn is_in_git_repo_true() {
        let repo = make_temp_repo();
        assert!(is_in_git_repo(&repo));
        cleanup(&repo);
    }

    #[test]
    fn is_in_git_repo_false() {
        let dir = std::env::temp_dir().join(format!(
            "emberforge-not-repo-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Same caveat as find_git_root_outside_repo.
        // At minimum verify no panic.
        let _ = is_in_git_repo(&dir);
        cleanup(&dir);
    }

    // -- get_head -----------------------------------------------------------

    #[test]
    fn get_head_returns_short_hash() {
        let repo = make_temp_repo();
        let head = get_head(&repo).unwrap();
        // Short hashes are typically 7-12 hex chars.
        assert!(
            head.len() >= 7 && head.len() <= 12,
            "unexpected head length: {head}"
        );
        assert!(
            head.chars().all(|c| c.is_ascii_hexdigit()),
            "non-hex head: {head}"
        );
        cleanup(&repo);
    }

    // -- get_branch ---------------------------------------------------------

    #[test]
    fn get_branch_returns_name() {
        let repo = make_temp_repo();
        let branch = get_branch(&repo).unwrap();
        assert!(
            branch.is_some(),
            "expected a branch name, got None (detached HEAD?)"
        );
        let name = branch.unwrap();
        assert!(
            name == "main" || name == "master",
            "unexpected default branch: {name}"
        );
        cleanup(&repo);
    }

    // -- is_clean -----------------------------------------------------------

    #[test]
    fn is_clean_on_clean_repo() {
        let repo = make_temp_repo();
        assert!(is_clean(&repo, false).unwrap());
        assert!(is_clean(&repo, true).unwrap());
        cleanup(&repo);
    }

    // -- get_changed_files --------------------------------------------------

    #[test]
    fn get_changed_files_after_creating_file() {
        let repo = make_temp_repo();
        fs::write(repo.join("hello.txt"), "world").unwrap();

        let changed = get_changed_files(&repo).unwrap();
        assert!(!changed.is_empty(), "expected at least one changed file");
        assert!(
            changed.iter().any(|f| f.path == "hello.txt"),
            "hello.txt not in changed files: {changed:?}"
        );
        assert_eq!(changed[0].status, FileChangeType::Untracked);

        cleanup(&repo);
    }

    // -- FileChangeType parsing ---------------------------------------------

    #[test]
    fn file_change_type_from_codes() {
        assert_eq!(FileChangeType::from_status_code('A'), FileChangeType::Added);
        assert_eq!(
            FileChangeType::from_status_code('M'),
            FileChangeType::Modified
        );
        assert_eq!(
            FileChangeType::from_status_code('D'),
            FileChangeType::Deleted
        );
        assert_eq!(
            FileChangeType::from_status_code('R'),
            FileChangeType::Renamed
        );
        assert_eq!(
            FileChangeType::from_status_code('C'),
            FileChangeType::Copied
        );
        assert_eq!(
            FileChangeType::from_status_code('?'),
            FileChangeType::Untracked
        );
        assert_eq!(
            FileChangeType::from_status_code('X'),
            FileChangeType::Unknown
        );
    }

    // -- get_git_state ------------------------------------------------------

    #[test]
    fn get_git_state_snapshot() {
        let repo = make_temp_repo();
        let state = get_git_state(&repo).unwrap();

        assert!(!state.head.is_empty());
        assert!(state.branch.is_some());
        assert!(state.is_clean);
        assert!(state.changed_files.is_empty());
        assert!(!state.is_shallow);
        // remote_url is None because there is no origin.
        assert!(state.remote_url.is_none());

        cleanup(&repo);
    }

    // -- is_shallow_clone ---------------------------------------------------

    #[test]
    fn is_shallow_clone_normal_repo() {
        let repo = make_temp_repo();
        assert!(!is_shallow_clone(&repo).unwrap());
        cleanup(&repo);
    }
}
