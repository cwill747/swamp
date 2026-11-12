//! Slot lifecycle hooks: user-configured shell commands that swamp runs after
//! an allocation (`post-alloc`) and before a release (`pre-release`).
//!
//! Each hook runs with `sh -c` in the worktree, with its stdout and stderr
//! captured together. Swamp returns the output to the caller instead of
//! printing it, because a hook can run inside the daemon, whose stderr is the
//! diagnostic log. The CLI prints the output on its own stderr so that stdout
//! keeps only the path.

use crate::config::{DEFAULT_HOOK_TIMEOUT_SECS, HookCommand};
use crate::worktree::WorktreeKind;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The facts a hook receives as `SWAMP_*` environment variables.
#[derive(Debug, Clone)]
pub struct HookEnv {
    pub worktree: PathBuf,
    pub name: String,
    pub kind: WorktreeKind,
    pub branch: String,
    pub repo_root: PathBuf,
}

impl HookEnv {
    pub fn new(
        common: &Path,
        worktree: &Path,
        name: &str,
        kind: WorktreeKind,
        branch: &str,
    ) -> Self {
        Self {
            worktree: worktree.to_path_buf(),
            name: name.to_string(),
            kind,
            branch: branch.to_string(),
            repo_root: common
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| common.to_path_buf()),
        }
    }
}

/// The result of one hook command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    pub command: String,
    pub success: bool,
    /// Combined stdout and stderr.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// Why the command failed: its exit status, a timeout, or a spawn error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Run `hooks` in order and stop at the first failure. The last entry of the
/// result is the failed hook, if one failed.
pub fn run_hooks(hooks: &[HookCommand], env: &HookEnv) -> Vec<HookRun> {
    let mut runs = Vec::new();
    for hook in hooks {
        let run = run_one(hook, env);
        let failed = !run.success;
        runs.push(run);
        if failed {
            break;
        }
    }
    runs
}

/// The failed run in `runs`, if any.
pub fn failure(runs: &[HookRun]) -> Option<&HookRun> {
    runs.iter().find(|r| !r.success)
}

fn kind_label(kind: WorktreeKind) -> &'static str {
    match kind {
        WorktreeKind::Default => "default",
        WorktreeKind::Slot => "slot",
        WorktreeKind::Overflow => "overflow",
    }
}

fn run_one(hook: &HookCommand, env: &HookEnv) -> HookRun {
    let timeout = Duration::from_secs(hook.timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS));
    let fail = |output: String, error: String| HookRun {
        command: hook.run.clone(),
        success: false,
        output,
        error: Some(error),
    };

    let (mut reader, writer) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => return fail(String::new(), format!("create output pipe: {e}")),
    };
    let writer_err = match writer.try_clone() {
        Ok(w) => w,
        Err(e) => return fail(String::new(), format!("create output pipe: {e}")),
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&hook.run)
        .current_dir(&env.worktree)
        .env("SWAMP_WORKTREE", &env.worktree)
        .env("SWAMP_NAME", &env.name)
        .env("SWAMP_KIND", kind_label(env.kind))
        .env("SWAMP_BRANCH", &env.branch)
        .env("SWAMP_REPO_ROOT", &env.repo_root)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(writer_err)
        // Its own process group, so a timeout can stop everything it started.
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(String::new(), format!("start sh: {e}")),
    };
    // Drop our copies of the write end so the reader sees EOF when the hook
    // and its children exit.
    drop(cmd);
    // The collector reads in chunks into a shared buffer, so a timeout can
    // still report what the hook printed. It signals on `done` at EOF.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (done_tx, done) = std::sync::mpsc::channel::<()>();
    {
        let buf = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
            let _ = done_tx.send(());
        });
    }

    // The deadline covers the shell and every process that still holds the
    // output pipe: a hook that leaves `sleep 600 &` behind must not hang the
    // slot operation after `sh` exits.
    let deadline = Instant::now() + timeout;
    let kill_group = |child: &mut std::process::Child| {
        // SAFETY: plain syscall on the child's process group id.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    };
    let timed_out = || format!("timed out after {}s", timeout.as_secs());
    let mut status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                kill_group(&mut child);
                let _ = child.wait();
                break Err(timed_out());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(format!("wait for hook: {e}")),
        }
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if done.recv_timeout(remaining).is_err() {
        // A background process still holds the pipe after the deadline.
        kill_group(&mut child);
        let _ = done.recv_timeout(Duration::from_secs(1));
        if status.is_ok() {
            status = Err(timed_out());
        }
    }
    let output = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    match status {
        Ok(s) if s.success() => HookRun {
            command: hook.run.clone(),
            success: true,
            output,
            error: None,
        },
        Ok(s) => fail(
            output,
            match s.code() {
                Some(code) => format!("exit code {code}"),
                None => "killed by a signal".to_string(),
            },
        ),
        Err(e) => fail(output, e),
    }
}

/// Print hook runs to stderr for the CLI.
pub fn print_runs(event: &str, runs: &[HookRun]) {
    for r in runs {
        if !r.output.is_empty() {
            eprint!("{}", r.output);
            if !r.output.ends_with('\n') {
                eprintln!();
            }
        }
        if let Some(err) = &r.error {
            eprintln!("swamp: {event} hook `{}` failed: {err}", r.command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(run: &str) -> HookCommand {
        HookCommand {
            run: run.into(),
            timeout_secs: None,
        }
    }

    fn env(dir: &Path) -> HookEnv {
        HookEnv {
            worktree: dir.to_path_buf(),
            name: "work2".into(),
            kind: WorktreeKind::Slot,
            branch: "fix-ci".into(),
            repo_root: dir.parent().unwrap().to_path_buf(),
        }
    }

    #[test]
    fn hooks_get_swamp_env_and_run_in_worktree() {
        let dir = std::env::temp_dir();
        let runs = run_hooks(
            &[hook(
                "echo \"$SWAMP_NAME $SWAMP_KIND $SWAMP_BRANCH\"; pwd; echo err >&2",
            )],
            &env(&dir),
        );

        assert_eq!(runs.len(), 1);
        assert!(runs[0].success);
        let out = &runs[0].output;
        assert!(out.contains("work2 slot fix-ci"), "{out}");
        assert!(out.contains("err"), "stderr is captured: {out}");
        let canon = dir.canonicalize().unwrap();
        assert!(out.contains(canon.to_str().unwrap()) || out.contains(dir.to_str().unwrap()));
    }

    #[test]
    fn first_failure_stops_the_chain() {
        let dir = std::env::temp_dir();
        let runs = run_hooks(
            &[hook("true"), hook("echo nope; exit 3"), hook("echo never")],
            &env(&dir),
        );

        assert_eq!(runs.len(), 2);
        let failed = failure(&runs).unwrap();
        assert_eq!(failed.command, "echo nope; exit 3");
        assert!(failed.output.contains("nope"));
        assert_eq!(failed.error.as_deref(), Some("exit code 3"));
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("swamp-hook-timeout-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let slow = HookCommand {
            // The background child would write the marker after 2s if the
            // process group survived the kill.
            run: format!("(sleep 2; touch {}) & sleep 30", marker.display()),
            timeout_secs: Some(1),
        };

        let started = Instant::now();
        let runs = run_hooks(&[slow], &env(&dir));

        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!runs[0].success);
        assert!(runs[0].error.as_deref().unwrap().contains("timed out"));
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "child in the hook's group must be killed");
    }

    #[test]
    fn background_child_holding_output_cannot_outlast_timeout() {
        let dir = std::env::temp_dir();
        let lingering = HookCommand {
            run: "echo started; sleep 30 &".into(),
            timeout_secs: Some(1),
        };

        let started = Instant::now();
        let runs = run_hooks(&[lingering], &env(&dir));

        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!runs[0].success);
        assert!(runs[0].error.as_deref().unwrap().contains("timed out"));
        assert!(runs[0].output.contains("started"));
    }

    #[test]
    fn detached_background_child_does_not_block() {
        let dir = std::env::temp_dir();
        let detached = HookCommand {
            run: "sleep 30 >/dev/null 2>&1 &".into(),
            timeout_secs: Some(5),
        };

        let started = Instant::now();
        let runs = run_hooks(&[detached], &env(&dir));

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(runs[0].success, "{:?}", runs[0].error);
    }
}
