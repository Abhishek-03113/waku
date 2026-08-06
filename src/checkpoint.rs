use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context as _, anyhow, bail};
use uuid::Uuid;

use crate::model::{Checkpoint, CheckpointFile, CheckpointStatus, unix_time};

pub fn checkpoint_ref(session_id: Uuid, turn_count: usize) -> String {
    format!("refs/waku/session-{session_id}-turn-{turn_count}")
}

pub fn capture_turn(cwd: &Path, session_id: Uuid, turn_count: usize) -> anyhow::Result<Checkpoint> {
    let git_ref = checkpoint_ref(session_id, turn_count);
    if !is_git_repository(cwd) {
        return Ok(Checkpoint {
            turn_count,
            git_ref,
            status: CheckpointStatus::Unavailable,
            files: Vec::new(),
            created_at: unix_time(),
        });
    }

    capture_ref(cwd, &git_ref)?;
    let files = if turn_count == 0 {
        Vec::new()
    } else {
        let previous_ref = checkpoint_ref(session_id, turn_count - 1);
        if has_ref(cwd, &previous_ref) {
            diff_files(cwd, &previous_ref, &git_ref)?
        } else {
            Vec::new()
        }
    };
    Ok(Checkpoint {
        turn_count,
        git_ref,
        status: CheckpointStatus::Ready,
        files,
        created_at: unix_time(),
    })
}

pub fn capture_ref(cwd: &Path, git_ref: &str) -> anyhow::Result<()> {
    if !is_git_repository(cwd) {
        bail!("checkpoints require a Git repository");
    }

    let common_dir = git_output(cwd, ["rev-parse", "--git-common-dir"])?
        .trim()
        .to_owned();
    if common_dir.is_empty() {
        bail!("git did not return its common directory");
    }
    let common_dir = PathBuf::from(common_dir);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        cwd.join(common_dir)
    };
    let temporary_index = common_dir.join(format!("waku-checkpoint-index-{}", Uuid::new_v4()));

    let result = (|| {
        if has_head(cwd) {
            git_with_index(cwd, &temporary_index, ["read-tree", "HEAD"])?;
        }
        git_with_index(cwd, &temporary_index, ["add", "-A", "--", "."])?;
        let tree = git_with_index(cwd, &temporary_index, ["write-tree"])?
            .trim()
            .to_owned();
        if tree.is_empty() {
            bail!("git write-tree returned no object id");
        }
        let message = format!("Waku checkpoint ref={git_ref}");
        let commit = git_with_identity_and_index(
            cwd,
            &temporary_index,
            ["commit-tree", &tree, "-m", &message],
        )?
        .trim()
        .to_owned();
        if commit.is_empty() {
            bail!("git commit-tree returned no object id");
        }
        git_output(cwd, ["update-ref", git_ref, &commit])?;
        Ok(())
    })();

    let _ = fs::remove_file(&temporary_index);
    let _ = fs::remove_file(temporary_index.with_extension("lock"));
    result
}

pub fn restore_ref(cwd: &Path, git_ref: &str) -> anyhow::Result<()> {
    let commit = resolve_ref(cwd, git_ref)
        .ok_or_else(|| anyhow!("checkpoint `{git_ref}` is unavailable"))?;
    git_output(
        cwd,
        [
            "restore",
            "--source",
            &commit,
            "--worktree",
            "--staged",
            "--",
            ".",
        ],
    )?;
    git_output(cwd, ["clean", "-fd", "--", "."])?;
    if has_head(cwd) {
        git_output(cwd, ["reset", "--quiet", "--", "."])?;
    }
    Ok(())
}

pub fn has_ref(cwd: &Path, git_ref: &str) -> bool {
    resolve_ref(cwd, git_ref).is_some()
}

/// Every turn count that has a checkpoint ref for `session_id`, resolved with
/// a single `git for-each-ref` instead of one `git rev-parse` per turn.
pub fn session_turn_refs(cwd: &Path, session_id: Uuid) -> HashSet<usize> {
    let prefix = format!("refs/waku/session-{session_id}-turn-");
    git_output(
        cwd,
        ["for-each-ref", "--format=%(refname)", &format!("{prefix}*")],
    )
    .map(|output| {
        output
            .lines()
            .filter_map(|line| line.trim().strip_prefix(prefix.as_str()))
            .filter_map(|turn_count| turn_count.parse().ok())
            .collect()
    })
    .unwrap_or_default()
}

pub fn delete_ref(cwd: &Path, git_ref: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["update-ref", "-d", git_ref])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to delete checkpoint `{git_ref}`"))?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("{}", command_error(&output))
    }
}

pub fn delete_turn_refs_after(
    cwd: &Path,
    session_id: Uuid,
    retained_turn_count: usize,
    previous_turn_count: usize,
) -> anyhow::Result<()> {
    update_refs(
        cwd,
        (retained_turn_count + 1..=previous_turn_count)
            .map(|turn_count| format!("delete {}\n", checkpoint_ref(session_id, turn_count)))
            .collect::<String>(),
    )
}

pub fn delete_session_refs(
    cwd: &Path,
    session_id: Uuid,
    last_turn_count: usize,
) -> anyhow::Result<()> {
    update_refs(
        cwd,
        (0..=last_turn_count)
            .map(|turn_count| format!("delete {}\n", checkpoint_ref(session_id, turn_count)))
            .collect::<String>(),
    )
}

pub fn copy_session_refs(
    cwd: &Path,
    source_session_id: Uuid,
    target_session_id: Uuid,
    through_turn_count: usize,
) -> anyhow::Result<()> {
    if !is_git_repository(cwd) {
        return Ok(());
    }

    let source = session_turn_ref_commits(cwd, source_session_id);
    update_refs(
        cwd,
        (0..=through_turn_count)
            .filter_map(|turn_count| {
                let commit = source.get(&turn_count)?;
                Some(format!(
                    "update {} {commit}\n",
                    checkpoint_ref(target_session_id, turn_count)
                ))
            })
            .collect::<String>(),
    )
}

/// Every turn count that has a checkpoint ref for `session_id`, with the commit
/// it points at — the same single `git for-each-ref` as [`session_turn_refs`].
fn session_turn_ref_commits(cwd: &Path, session_id: Uuid) -> HashMap<usize, String> {
    let prefix = format!("refs/waku/session-{session_id}-turn-");
    git_output(
        cwd,
        [
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            &format!("{prefix}*"),
        ],
    )
    .map(|output| {
        output
            .lines()
            .filter_map(|line| {
                let (refname, commit) = line.trim().split_once(' ')?;
                let turn_count = refname.strip_prefix(prefix.as_str())?.parse().ok()?;
                Some((turn_count, commit.to_owned()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Applies a batch of ref updates through one `git update-ref --stdin`.
///
/// Deleting a fifty-turn session's checkpoints was fifty `git` invocations run
/// serially on the thread that had just been asked to remove the session. The
/// batch form is one process, and it is atomic: either the whole set applies or
/// none of it does. Deleting a ref that is already gone is not an error.
///
/// Checkpoint ref names are generated, never user text, so they cannot contain
/// the space or newline this line-oriented format delimits on.
fn update_refs(cwd: &Path, commands: String) -> anyhow::Result<()> {
    if commands.is_empty() {
        return Ok(());
    }
    let mut child = Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to execute git")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("git update-ref stdin is unavailable"))?
        .write_all(commands.as_bytes())
        .context("failed to send ref updates to git")?;
    let output = child.wait_with_output().context("failed to execute git")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn diff_files(cwd: &Path, from_ref: &str, to_ref: &str) -> anyhow::Result<Vec<CheckpointFile>> {
    let output = git_output(cwd, ["diff", "--numstat", from_ref, to_ref, "--", "."])?;
    let mut files = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.splitn(3, '\t');
        let additions = columns.next().unwrap_or("0").parse().unwrap_or(0);
        let deletions = columns.next().unwrap_or("0").parse().unwrap_or(0);
        let Some(path) = columns.next() else {
            continue;
        };
        files.push(CheckpointFile {
            path: path.to_owned(),
            additions,
            deletions,
        });
    }
    Ok(files)
}

fn is_git_repository(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn has_head(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn resolve_ref(cwd: &Path, git_ref: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")])
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_output<I, S>(cwd: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn git_with_index<I, S>(cwd: &Path, index: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_with_environment(cwd, index, args, false)
}

fn git_with_identity_and_index<I, S>(cwd: &Path, index: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_with_environment(cwd, index, args, true)
}

fn git_with_environment<I, S>(
    cwd: &Path,
    index: &Path,
    args: I,
    identity: bool,
) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_INDEX_FILE", index);
    if identity {
        command
            .env("GIT_AUTHOR_NAME", "Waku")
            .env("GIT_AUTHOR_EMAIL", "waku@localhost")
            .env("GIT_COMMITTER_NAME", "Waku")
            .env("GIT_COMMITTER_EMAIL", "waku@localhost");
    }
    let output = command.output().context("failed to execute git")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
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

    fn git_ok(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_text(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn session_turn_refs_lists_the_sessions_checkpoints_in_one_call() {
        let directory = std::env::temp_dir().join(format!("waku-checkpoints-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        git_ok(&directory, &["init", "--quiet"]);
        fs::write(directory.join("tracked.txt"), "baseline\n").unwrap();
        git_ok(&directory, &["add", "tracked.txt"]);
        git_ok(
            &directory,
            &[
                "-c",
                "user.name=Waku Test",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );

        let session = Uuid::new_v4();
        let other = Uuid::new_v4();
        capture_turn(&directory, session, 0).unwrap();
        capture_turn(&directory, session, 2).unwrap();
        capture_turn(&directory, other, 5).unwrap();

        assert_eq!(
            session_turn_refs(&directory, session),
            HashSet::from([0, 2])
        );
        assert_eq!(session_turn_refs(&directory, other), HashSet::from([5]));
        assert!(session_turn_refs(&directory, Uuid::new_v4()).is_empty());
        fs::remove_dir_all(&directory).ok();
    }

    /// Deletes and copies go through one batched `git update-ref --stdin`
    /// rather than a process per turn, so the semantics the loop used to give
    /// for free — gaps are skipped, an already-missing ref is not an error —
    /// are worth pinning down.
    #[test]
    fn refs_are_deleted_and_copied_in_batches() {
        let directory = std::env::temp_dir().join(format!("waku-checkpoints-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        git_ok(&directory, &["init", "--quiet"]);
        fs::write(directory.join("tracked.txt"), "baseline\n").unwrap();
        git_ok(&directory, &["add", "tracked.txt"]);
        git_ok(
            &directory,
            &[
                "-c",
                "user.name=Waku Test",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );

        let session = Uuid::new_v4();
        for turn_count in [0, 1, 3] {
            capture_turn(&directory, session, turn_count).unwrap();
        }

        // Turn 2 was never captured; the batch must tolerate the gap.
        let fork = Uuid::new_v4();
        copy_session_refs(&directory, session, fork, 3).unwrap();
        assert_eq!(
            session_turn_refs(&directory, fork),
            HashSet::from([0, 1, 3]),
            "a copy carries every ref the source actually has"
        );
        assert_eq!(
            resolve_ref(&directory, &checkpoint_ref(fork, 1)),
            resolve_ref(&directory, &checkpoint_ref(session, 1)),
            "and points at the same commit"
        );

        delete_turn_refs_after(&directory, session, 1, 3).unwrap();
        assert_eq!(
            session_turn_refs(&directory, session),
            HashSet::from([0, 1]),
            "everything after the retained turn goes, missing ones included"
        );

        delete_session_refs(&directory, fork, 3).unwrap();
        assert!(
            session_turn_refs(&directory, fork).is_empty(),
            "and a session's whole set goes in one call"
        );

        // Nothing left to remove is a no-op, not a failure.
        delete_session_refs(&directory, fork, 3).unwrap();
        fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn captures_diffs_and_restores_tracked_and_untracked_files() {
        let directory = std::env::temp_dir().join(format!("waku-checkpoints-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        git_ok(&directory, &["init", "--quiet"]);
        fs::write(directory.join("tracked.txt"), "baseline\n").unwrap();
        git_ok(&directory, &["add", "tracked.txt"]);
        git_ok(
            &directory,
            &[
                "-c",
                "user.name=Waku Test",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );

        let session_id = Uuid::new_v4();
        let baseline = capture_turn(&directory, session_id, 0).unwrap();
        assert_eq!(baseline.status, CheckpointStatus::Ready);

        fs::write(directory.join("tracked.txt"), "changed\n").unwrap();
        fs::write(directory.join("new.txt"), "new\n").unwrap();
        fs::write(directory.join("already-staged.txt"), "staged\n").unwrap();
        git_ok(&directory, &["add", "already-staged.txt"]);
        let turn = capture_turn(&directory, session_id, 1).unwrap();
        assert_eq!(turn.files.len(), 3);
        assert_eq!(
            git_text(&directory, &["diff", "--cached", "--name-only"]),
            "already-staged.txt"
        );

        let fork_session_id = Uuid::new_v4();
        copy_session_refs(&directory, session_id, fork_session_id, 1).unwrap();
        assert_eq!(
            resolve_ref(&directory, &checkpoint_ref(fork_session_id, 0)),
            resolve_ref(&directory, &checkpoint_ref(session_id, 0))
        );
        assert_eq!(
            resolve_ref(&directory, &checkpoint_ref(fork_session_id, 1)),
            resolve_ref(&directory, &checkpoint_ref(session_id, 1))
        );

        fs::write(directory.join("tracked.txt"), "later\n").unwrap();
        fs::remove_file(directory.join("new.txt")).unwrap();
        fs::write(directory.join("discard.txt"), "discard\n").unwrap();
        restore_ref(&directory, &turn.git_ref).unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("tracked.txt")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            fs::read_to_string(directory.join("new.txt")).unwrap(),
            "new\n"
        );
        assert!(!directory.join("discard.txt").exists());

        fs::remove_dir_all(directory).ok();
    }
}
