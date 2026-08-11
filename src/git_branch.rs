//! Git branch discovery and checkout operations for workspace selectors.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`BranchSnapshot`] values they return.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, anyhow, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchEntry {
    pub name: String,
    /// Git refuses to check out one branch in two worktrees. Keep such rows
    /// visible so the list remains truthful, but let the picker draw them
    /// disabled.
    pub checked_out_elsewhere: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchSnapshot {
    pub repository: PathBuf,
    pub current: Option<String>,
    pub detached_head: Option<String>,
    pub default_branch: Option<String>,
    pub branches: Vec<BranchEntry>,
    /// Tracked working-tree changes against `HEAD`, cached with the branch
    /// snapshot so environment UI never shells out from a render path.
    pub additions: u64,
    pub deletions: u64,
}

impl BranchSnapshot {
    pub fn display_branch(&self) -> Option<&str> {
        self.current.as_deref().or(self.detached_head.as_deref())
    }
}

/// Inspect local branches and which worktree, if any, currently owns each.
/// `Ok(None)` means `cwd` is not inside a Git repository.
pub fn inspect(cwd: &Path) -> anyhow::Result<Option<BranchSnapshot>> {
    let repository_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !repository_output.status.success() {
        return Ok(None);
    }
    let repository = PathBuf::from(
        String::from_utf8_lossy(&repository_output.stdout)
            .trim()
            .to_owned(),
    );
    let repository = fs::canonicalize(&repository).unwrap_or(repository);

    let current =
        optional_stdout(cwd, &["branch", "--show-current"])?.filter(|branch| !branch.is_empty());
    let detached_head = if current.is_none() {
        Some(git_stdout(cwd, &["rev-parse", "--short", "HEAD"])?).filter(|head| !head.is_empty())
    } else {
        None
    };

    // `%(worktreepath)` is empty for an available branch and points at the
    // owning checkout otherwise. A NUL separates it from the branch name so
    // paths containing spaces need no shell-style decoding.
    let refs = git_stdout(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(worktreepath)",
            "refs/heads",
        ],
    )?;
    let mut branches = refs
        .lines()
        .filter_map(|line| {
            let (name, worktree_path) = line.split_once('\0')?;
            if name.is_empty() {
                return None;
            }
            let checked_out_elsewhere = if worktree_path.is_empty() {
                false
            } else {
                let worktree_path = PathBuf::from(worktree_path);
                let worktree_path = fs::canonicalize(&worktree_path).unwrap_or(worktree_path);
                worktree_path != repository
            };
            Some(BranchEntry {
                name: name.to_owned(),
                checked_out_elsewhere,
            })
        })
        .collect::<Vec<_>>();

    branches.sort_by(|left, right| {
        let left_current = current.as_deref() == Some(left.name.as_str());
        let right_current = current.as_deref() == Some(right.name.as_str());
        right_current
            .cmp(&left_current)
            .then_with(|| left.name.cmp(&right.name))
    });

    let remote_default = optional_stdout(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )?;
    let default_branch = remote_default
        .as_deref()
        .and_then(|branch| branch.strip_prefix("origin/"))
        .filter(|branch| branches.iter().any(|entry| entry.name == *branch))
        .map(str::to_owned)
        .or_else(|| current.clone());
    let (additions, deletions) = worktree_line_counts(cwd);

    Ok(Some(BranchSnapshot {
        repository,
        current,
        detached_head,
        default_branch,
        branches,
        additions,
        deletions,
    }))
}

fn worktree_line_counts(cwd: &Path) -> (u64, u64) {
    let Ok(output) = Command::new("git")
        .args(["diff", "--numstat", "HEAD", "--"])
        .current_dir(cwd)
        .output()
    else {
        return (0, 0);
    };
    if !output.status.success() {
        return (0, 0);
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .fold((0, 0), |(additions, deletions), line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next().and_then(|value| value.parse::<u64>().ok());
            let deleted = fields.next().and_then(|value| value.parse::<u64>().ok());
            match (added, deleted) {
                (Some(added), Some(deleted)) => (additions + added, deletions + deleted),
                // Binary files are reported as `-\t-` and have no line count.
                _ => (additions, deletions),
            }
        })
}

pub fn checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let output = Command::new("git")
        .args(["switch", "--"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to execute git switch")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

pub fn create_and_checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("enter a branch name");
    }
    let validation = Command::new("git")
        .args(["check-ref-format", "--branch"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to validate the branch name")?;
    if !validation.status.success() {
        bail!("{}", command_error(&validation));
    }
    let output = Command::new("git")
        .args(["switch", "-c"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to execute git switch")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

fn git_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn optional_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!("{}", command_error(&output))
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("waku-branch-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-b", "main"]);
        fs::write(root.join("README.md"), "main\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(&root, &["branch", "feature"]);
        root
    }

    #[test]
    fn discovers_current_and_worktree_owned_branches() {
        let repository = repository();
        let other = repository.with_extension("other-worktree");
        run_git(
            &repository,
            &["worktree", "add", "-b", "occupied", other.to_str().unwrap()],
        );

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!(snapshot.current.as_deref(), Some("main"));
        assert_eq!(snapshot.branches[0].name, "main");
        assert!(
            snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "occupied")
                .unwrap()
                .checked_out_elsewhere
        );
        assert!(
            !snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "feature")
                .unwrap()
                .checked_out_elsewhere
        );
    }

    #[test]
    fn switches_and_creates_branches() {
        let repository = repository();
        let switched = checkout(&repository, "feature").unwrap();
        assert_eq!(switched.current.as_deref(), Some("feature"));

        let created = create_and_checkout(&repository, "topic/new-picker").unwrap();
        assert_eq!(created.current.as_deref(), Some("topic/new-picker"));
        assert!(
            created
                .branches
                .iter()
                .any(|branch| branch.name == "topic/new-picker")
        );
    }

    #[test]
    fn counts_tracked_worktree_changes() {
        let repository = repository();
        fs::write(repository.join("README.md"), "main\nsecond\n").unwrap();

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
    }
}
