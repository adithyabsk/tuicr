pub mod context;
pub mod diff;
pub mod repository;
pub mod staging;

use git2::Repository;
use std::path::Path;

use crate::error::{Result, TuicrError};
use crate::model::{DiffFile, DiffLine, FileStatus};
use crate::syntax::SyntaxHighlighter;

use super::traits::{CommitInfo, VcsBackend, VcsInfo, VcsType};

// Re-export commonly used functions
pub use context::{calculate_gap, fetch_context_lines};
pub use diff::{
    get_commit_range_diff, get_staged_diff, get_unstaged_diff, get_working_tree_diff,
    get_working_tree_with_commits_diff,
};

/// Git backend implementation using git2 library
pub struct GitBackend {
    repo: Repository,
    info: VcsInfo,
}

impl GitBackend {
    /// Discover a git repository from the current directory
    pub fn discover() -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|_| TuicrError::NotARepository)?;
        let repo = Repository::discover(&cwd).map_err(|_| TuicrError::NotARepository)?;

        let root_path = repo
            .workdir()
            .ok_or(TuicrError::NotARepository)?
            .to_path_buf();

        let head_commit = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| c.id().to_string())
            .unwrap_or_else(|| "HEAD".to_string());

        let branch_name = repo.head().ok().and_then(|h| {
            if h.is_branch() {
                h.shorthand().map(|s| s.to_string())
            } else {
                None
            }
        });

        let info = VcsInfo {
            root_path,
            head_commit,
            branch_name,
            vcs_type: VcsType::Git,
        };

        Ok(Self { repo, info })
    }
}

impl VcsBackend for GitBackend {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        get_working_tree_diff(&self.repo, highlighter)
    }

    fn get_staged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        get_staged_diff(&self.repo, highlighter)
    }

    fn get_unstaged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        get_unstaged_diff(&self.repo, highlighter)
    }

    fn fetch_context_lines(
        &self,
        file_path: &Path,
        file_status: FileStatus,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        fetch_context_lines(&self.repo, file_path, file_status, start_line, end_line)
    }

    fn get_recent_commits(&self, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        let git_commits = repository::get_recent_commits(&self.repo, offset, limit)?;
        Ok(git_commits
            .into_iter()
            .map(|c| CommitInfo {
                id: c.id,
                short_id: c.short_id,
                branch_name: c.branch_name,
                summary: c.summary,
                body: c.body,
                author: c.author,
                time: c.time,
            })
            .collect())
    }

    fn resolve_revisions(&self, revisions: &str) -> Result<Vec<String>> {
        repository::resolve_revisions(&self.repo, revisions)
    }

    fn get_commit_range_diff(
        &self,
        commit_ids: &[String],
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        get_commit_range_diff(&self.repo, commit_ids, highlighter)
    }

    fn get_commits_info(&self, ids: &[String]) -> Result<Vec<CommitInfo>> {
        let git_commits = repository::get_commits_info(&self.repo, ids)?;
        Ok(git_commits
            .into_iter()
            .map(|c| CommitInfo {
                id: c.id,
                short_id: c.short_id,
                branch_name: c.branch_name,
                summary: c.summary,
                body: c.body,
                author: c.author,
                time: c.time,
            })
            .collect())
    }

    fn get_working_tree_with_commits_diff(
        &self,
        commit_ids: &[String],
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        get_working_tree_with_commits_diff(&self.repo, commit_ids, highlighter)
    }

    fn stage_file(&self, path: &Path) -> Result<()> {
        staging::stage_file(&self.repo, path)
    }

    fn has_staged_changes(&self) -> Result<bool> {
        // Use git CLI instead of git2's diff_tree_to_index which triggers
        // a full ref enumeration of .git/refs/ (thousands of syscalls).
        // `git diff --cached --quiet` exits 1 if there are staged changes.
        let work_dir = self
            .repo
            .workdir()
            .ok_or(TuicrError::NotARepository)?;
        let status = std::process::Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(work_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| TuicrError::VcsCommand(format!("git diff --cached: {}", e)))?;
        Ok(!status.success())
    }

    fn has_unstaged_changes(&self) -> Result<bool> {
        // Use git CLI instead of git2's diff_index_to_workdir which stats
        // every tracked file — too slow on network FS with large repos.
        let work_dir = self
            .repo
            .workdir()
            .ok_or(TuicrError::NotARepository)?;
        // `git diff --quiet` exits 1 if there are modified tracked files.
        let tracked = std::process::Command::new("git")
            .args(["diff", "--quiet"])
            .current_dir(work_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| TuicrError::VcsCommand(format!("git diff: {}", e)))?;
        if !tracked.success() {
            return Ok(true);
        }
        // Also check for untracked files so a repo with *only* new files
        // is not incorrectly treated as having no changes.
        let untracked = std::process::Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(work_dir)
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| TuicrError::VcsCommand(format!("git ls-files: {}", e)))?;
        Ok(!untracked.stdout.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temporary git repo with one initial commit.
    fn tmp_git_repo() -> (tempfile::TempDir, GitBackend) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // git init + initial commit
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .status()
            .unwrap();
        fs::write(path.join("init.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();

        let repo = Repository::open(path).unwrap();
        let info = VcsInfo {
            root_path: path.to_path_buf(),
            head_commit: "test".to_string(),
            branch_name: Some("main".to_string()),
            vcs_type: VcsType::Git,
        };
        (dir, GitBackend { repo, info })
    }

    #[test]
    fn has_unstaged_changes_false_on_clean_repo() {
        let (_dir, backend) = tmp_git_repo();
        assert!(!backend.has_unstaged_changes().unwrap());
    }

    #[test]
    fn has_unstaged_changes_true_on_modified_tracked_file() {
        let (dir, backend) = tmp_git_repo();
        fs::write(dir.path().join("init.txt"), "modified").unwrap();
        assert!(backend.has_unstaged_changes().unwrap());
    }

    #[test]
    fn has_unstaged_changes_true_on_untracked_file() {
        let (dir, backend) = tmp_git_repo();
        fs::write(dir.path().join("new_file.txt"), "new").unwrap();
        assert!(backend.has_unstaged_changes().unwrap());
    }

    #[test]
    fn has_staged_changes_false_on_clean_repo() {
        let (_dir, backend) = tmp_git_repo();
        assert!(!backend.has_staged_changes().unwrap());
    }

    #[test]
    fn has_staged_changes_true_when_file_staged() {
        let (dir, backend) = tmp_git_repo();
        fs::write(dir.path().join("init.txt"), "staged change").unwrap();
        std::process::Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(backend.has_staged_changes().unwrap());
    }
}
