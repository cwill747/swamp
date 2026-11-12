//! Slot operations together with the swamp state that follows a branch.
//!
//! [`crate::worktree::slots`] changes git state only. This layer adds:
//!
//! - Branch memory: `<common>/swamp/branches.json` maps a branch to its Claude
//!   session, harness override, and user title. Release moves a slot's record
//!   there, and alloc restores it, so `--resume` follows a branch across slots.
//! - Agent records: the per-worktree records in `.swamp-status.json`. Alloc and
//!   release return [`AgentChanges`] for the caller to apply, because the
//!   daemon owns that map while it runs and the CLI edits the file otherwise.
//! - Agent safety: a release refuses to pull a slot out from under a working
//!   agent unless the caller runs inside that slot.
//!
//! Branch memory is written while the slot lock is held.

use crate::config::{Harness, HookCommand};
use crate::daemon::state::{AgentRecord, AgentStatus};
use crate::slot_hooks::{self, HookEnv};
use crate::util::now_unix;
use crate::worktree::slots::{
    self, AllocOptions, Allocation, PrFacts, Release, ReleaseOptions, ReleaseOutcome,
    ReleaseRefused, ReleaseRefusedReason,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Swamp state that belongs to a branch rather than to the worktree it sits in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    /// User-set title (`swamp title`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl BranchRecord {
    fn is_empty(&self) -> bool {
        self == &BranchRecord::default()
    }
}

pub fn branches_path(common: &Path) -> PathBuf {
    common.join("swamp").join("branches.json")
}

/// Load branch memory. A missing or malformed file yields an empty map.
pub fn load_branches(common: &Path) -> HashMap<String, BranchRecord> {
    std::fs::read(branches_path(common))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write branch memory atomically. Callers hold the slot lock.
pub fn save_branches(common: &Path, map: &HashMap<String, BranchRecord>) -> Result<()> {
    let path = branches_path(common);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))
}

/// User titles from branch memory, keyed by branch.
pub fn user_titles(common: &Path) -> HashMap<String, String> {
    load_branches(common)
        .into_iter()
        .filter_map(|(branch, rec)| rec.title.map(|t| (branch, t)))
        .collect()
}

/// Set (or, with `None` or a blank string, clear) the user title of `branch`.
/// The longest user title, in UTF-8 bytes.
pub const MAX_TITLE_BYTES: usize = 200;

pub fn set_title(common: &Path, branch: &str, title: Option<&str>) -> Result<()> {
    if let Some(t) = title {
        if t.len() > MAX_TITLE_BYTES {
            anyhow::bail!("title exceeds {MAX_TITLE_BYTES} bytes");
        }
        if t.chars().any(char::is_control) {
            anyhow::bail!("title contains a control character");
        }
    }
    let _guard = slots::lock(common)?;
    let mut mem = load_branches(common);
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    match title {
        Some(t) => mem.entry(branch.to_string()).or_default().title = Some(t.to_string()),
        None => {
            if let Some(rec) = mem.get_mut(branch) {
                rec.title = None;
                if rec.is_empty() {
                    mem.remove(branch);
                }
            }
        }
    }
    save_branches(common, &mem)
}

/// Edits to the per-worktree agent records that a slot operation implies.
#[derive(Debug, Clone, Default)]
pub struct AgentChanges {
    /// Records to set (a restored session for a reopened branch).
    pub set: Vec<(String, AgentRecord)>,
    /// Records to drop (a released slot, or a slot that takes a new branch).
    pub clear: Vec<String>,
}

impl AgentChanges {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.clear.is_empty()
    }

    pub fn apply(&self, agents: &mut HashMap<String, AgentRecord>) {
        for name in &self.clear {
            agents.remove(name);
        }
        for (name, rec) in &self.set {
            agents.insert(name.clone(), rec.clone());
        }
    }
}

/// Allocate a worktree for `branch` and work out the agent record it starts
/// with: the branch's remembered session, or none. `post_alloc` hooks run
/// after the slot lock is released, so a slow hook does not block other
/// allocations; a failed hook becomes a warning.
pub fn alloc(
    common: &Path,
    branch: &str,
    opts: &AllocOptions,
    post_alloc: &[HookCommand],
) -> Result<(Allocation, AgentChanges)> {
    let (a, changes) = {
        let guard = slots::lock(common)?;
        alloc_under(&guard, common, branch, opts)?
    };
    Ok((run_post_alloc(common, a, post_alloc), changes))
}

/// [`alloc`] for the CLI when no daemon runs: it applies the agent changes to
/// `.swamp-status.json` before it releases the slot lock, so a concurrent
/// operation cannot reuse the slot between the two steps.
pub fn alloc_with_status_file(
    common: &Path,
    branch: &str,
    opts: &AllocOptions,
    post_alloc: &[HookCommand],
) -> Result<Allocation> {
    let a = {
        let guard = slots::lock(common)?;
        let (a, changes) = alloc_under(&guard, common, branch, opts)?;
        apply_to_status_file(common, &changes)?;
        a
    };
    Ok(run_post_alloc(common, a, post_alloc))
}

fn alloc_under(
    guard: &slots::SlotLock,
    common: &Path,
    branch: &str,
    opts: &AllocOptions,
) -> Result<(Allocation, AgentChanges)> {
    let a = slots::alloc_locked(guard, common, branch, opts)?;
    let mut changes = AgentChanges::default();
    if !a.existing {
        match load_branches(common).get(&a.branch) {
            Some(mem) if mem.session_id.is_some() || mem.harness.is_some() => {
                changes.set.push((
                    a.name.clone(),
                    AgentRecord {
                        status: AgentStatus::Idle,
                        ts: now_unix(),
                        session_name: mem.session_name.clone(),
                        session_id: mem.session_id.clone(),
                        harness: mem.harness,
                    },
                ));
            }
            _ => changes.clear.push(a.name.clone()),
        }
    }
    Ok((a, changes))
}

/// Run `post_alloc` hooks for a new allocation. Callers run them after they
/// release the slot lock.
fn run_post_alloc(common: &Path, mut a: Allocation, post_alloc: &[HookCommand]) -> Allocation {
    if !a.existing && !post_alloc.is_empty() {
        let env = HookEnv::new(common, &a.path, &a.name, a.kind, &a.branch);
        a.hooks = slot_hooks::run_hooks(post_alloc, &env);
        if let Some(failed) = slot_hooks::failure(&a.hooks) {
            a.warnings.push(format!(
                "post-alloc hook `{}` failed ({}); the worktree is ready anyway",
                failed.command,
                failed.error.as_deref().unwrap_or("error")
            ));
        }
    }
    a
}

/// Release `target` (see [`slots::release`]). `agents` is the current view of
/// the agent records; the caller applies the returned changes to it.
/// `pre_release` hooks run after the safety checks and before the detach; a
/// failed hook stops the release unless `opts.force` is set.
pub fn release(
    common: &Path,
    target: Option<&str>,
    cwd: &Path,
    opts: &ReleaseOptions,
    agents: &HashMap<String, AgentRecord>,
    pre_release: &[HookCommand],
) -> Result<(Release, AgentChanges)> {
    let guard = slots::lock(common)?;
    release_under(&guard, common, target, cwd, opts, agents, pre_release)
}

/// [`release`] for the CLI when no daemon runs: it reads the agent records
/// and applies the changes to `.swamp-status.json` while it holds the slot
/// lock, so a concurrent alloc cannot swap the slot's record in between.
pub fn release_with_status_file(
    common: &Path,
    target: Option<&str>,
    cwd: &Path,
    opts: &ReleaseOptions,
    pre_release: &[HookCommand],
) -> Result<Release> {
    let guard = slots::lock(common)?;
    let agents = load_agents(common);
    let (r, changes) = release_under(&guard, common, target, cwd, opts, &agents, pre_release)?;
    apply_to_status_file(common, &changes)?;
    Ok(r)
}

fn release_under(
    guard: &slots::SlotLock,
    common: &Path,
    target: Option<&str>,
    cwd: &Path,
    opts: &ReleaseOptions,
    agents: &HashMap<String, AgentRecord>,
    pre_release: &[HookCommand],
) -> Result<(Release, AgentChanges)> {
    let wt = slots::resolve_target(common, target, cwd)?;
    let name = wt.name();
    let record = agents.get(&name);

    if !opts.force
        && slots::slot_number(&name).is_some()
        && record.is_some_and(|r| r.status == AgentStatus::Working)
        && !path_contains(&wt.path, cwd)
    {
        return Err(ReleaseRefused {
            name,
            branch: slots::checked_out_branch(&wt.path),
            reason: ReleaseRefusedReason::AgentWorking,
        }
        .into());
    }

    let mut runs = Vec::new();
    let mut r = slots::release_locked(guard, common, &wt, opts, |branch| {
        if pre_release.is_empty() {
            return Ok(());
        }
        let env = HookEnv::new(
            common,
            &wt.path,
            &name,
            crate::worktree::WorktreeKind::Slot,
            branch,
        );
        runs = slot_hooks::run_hooks(pre_release, &env);
        match slot_hooks::failure(&runs) {
            Some(failed) if !opts.force => Err(ReleaseRefused {
                name: name.clone(),
                branch: Some(branch.to_string()),
                reason: ReleaseRefusedReason::HookFailed {
                    hook: format!(
                        "`{}` ({})",
                        failed.command,
                        failed.error.as_deref().unwrap_or("error")
                    ),
                    output: failed.output.clone(),
                },
            }
            .into()),
            _ => Ok(()),
        }
    })?;
    r.hooks = runs;
    let mut changes = AgentChanges::default();
    if let (ReleaseOutcome::Detached { branch_deleted }, Some(branch)) = (r.outcome, &r.branch) {
        let mut mem = load_branches(common);
        if branch_deleted {
            mem.remove(branch);
        } else if let Some(rec) = record {
            let entry = mem.entry(branch.clone()).or_default();
            if rec.session_id.is_some() {
                entry.session_id = rec.session_id.clone();
            }
            if rec.session_name.is_some() {
                entry.session_name = rec.session_name.clone();
            }
            if rec.harness.is_some() {
                entry.harness = rec.harness;
            }
            if entry.is_empty() {
                mem.remove(branch);
            }
        }
        save_branches(common, &mem)?;
        changes.clear.push(name);
    }
    Ok((r, changes))
}

/// Look up the PR for the branch that `target` resolves to, before release
/// takes the slot lock: the lookup is a network call. Any failure (no `gh`, no
/// auth, offline) yields `None`, and release then relies on git state only.
pub fn pr_facts(common: &Path, target: Option<&str>, cwd: &Path) -> Option<PrFacts> {
    let wt = slots::resolve_target(common, target, cwd).ok()?;
    let branch = slots::checked_out_branch(&wt.path)?;
    let mut prs = crate::github::list_prs_for_branches(&wt.path, std::slice::from_ref(&branch))
        .map_err(|e| tracing::debug!("PR lookup for {branch} failed: {e:#}"))
        .ok()?;
    let pr = prs.remove(&branch)?;
    Some(PrFacts {
        branch,
        state: pr.state,
        head_oid: pr.head_oid.and_then(|o| o.parse().ok()),
    })
}

fn path_contains(root: &Path, child: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let child = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    child.starts_with(root)
}

// ---------------------------------------------------------------------------
// Status file access for the CLI when no daemon runs.

fn status_path(common: &Path) -> PathBuf {
    common.join(".swamp-status.json")
}

/// Load the agent records from `.swamp-status.json`. A missing or malformed
/// file yields an empty map, the same as the daemon's loader.
pub fn load_agents(common: &Path) -> HashMap<String, AgentRecord> {
    std::fs::read(status_path(common))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Apply `changes` to `.swamp-status.json` in place.
pub fn apply_to_status_file(common: &Path, changes: &AgentChanges) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut agents = load_agents(common);
    changes.apply(&mut agents);
    let path = status_path(common);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&agents)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::test_support::{git_available, setup};
    use git2::Repository;

    fn detach(path: &Path) {
        let repo = Repository::open(path).unwrap();
        let oid = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(oid).unwrap();
    }

    fn record(session: &str, status: AgentStatus) -> AgentRecord {
        AgentRecord {
            status,
            ts: 1,
            session_name: Some(format!("{session} name")),
            session_id: Some(session.to_string()),
            harness: None,
        }
    }

    #[test]
    fn reopened_branch_restores_its_session() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(5);
        let (a, _) = alloc(&bare, "fix-ci", &opts, &[]).unwrap();
        // `fix-ci` equals main, so release finds its work safe.
        let mut agents = HashMap::from([(a.name.clone(), record("abc-123", AgentStatus::Idle))]);
        let (_, changes) = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &ReleaseOptions::default(),
            &agents,
            &[],
        )
        .unwrap();
        changes.apply(&mut agents);
        assert!(!agents.contains_key("work1"));
        // Take work1 with another branch so fix-ci lands in work2.
        alloc(&bare, "other", &opts, &[]).unwrap();

        let (b, changes) = alloc(&bare, "fix-ci", &opts, &[]).unwrap();

        assert_eq!(b.name, "work2");
        assert_eq!(changes.set.len(), 1);
        assert_eq!(changes.set[0].0, "work2");
        assert_eq!(changes.set[0].1.session_id.as_deref(), Some("abc-123"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_branch_in_reused_slot_starts_without_session() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(5);
        let (a, _) = alloc(&bare, "first", &opts, &[]).unwrap();
        // Freed by hand (no release), so its stale record is still present.
        detach(&a.path);

        let (b, changes) = alloc(&bare, "second", &opts, &[]).unwrap();

        assert_eq!(b.name, "work1");
        assert_eq!(changes.clear, vec!["work1".to_string()]);
        assert!(changes.set.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_allocation_changes_no_records() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(5);
        alloc(&bare, "same", &opts, &[]).unwrap();

        let (again, changes) = alloc(&bare, "same", &opts, &[]).unwrap();

        assert!(again.existing);
        assert!(changes.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn release_refuses_another_slots_working_agent() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let (a, _) = alloc(&bare, "busy", &AllocOptions::pool(5), &[]).unwrap();
        let agents = HashMap::from([(a.name.clone(), record("s", AgentStatus::Working))]);

        let err = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &ReleaseOptions::default(),
            &agents,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            err.downcast::<ReleaseRefused>().unwrap().reason,
            ReleaseRefusedReason::AgentWorking
        );

        // The agent may release its own slot.
        let (r, _) = release(
            &bare,
            None,
            &a.path,
            &ReleaseOptions::default(),
            &agents,
            &[],
        )
        .unwrap();
        assert!(matches!(r.outcome, ReleaseOutcome::Detached { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn deleted_branch_drops_its_memory() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let (a, _) = alloc(&bare, "done", &AllocOptions::pool(5), &[]).unwrap();
        let mut mem = HashMap::new();
        mem.insert(
            "done".to_string(),
            BranchRecord {
                session_id: Some("old".into()),
                ..Default::default()
            },
        );
        save_branches(&bare, &mem).unwrap();
        let tip = Repository::open(&a.path)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();
        let opts = ReleaseOptions {
            pr: Some(slots::PrFacts {
                branch: "done".into(),
                state: "MERGED".into(),
                head_oid: Some(tip),
            }),
            ..Default::default()
        };

        let (r, _) = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &opts,
            &HashMap::new(),
            &[],
        )
        .unwrap();

        assert_eq!(
            r.outcome,
            ReleaseOutcome::Detached {
                branch_deleted: true
            }
        );
        assert!(!load_branches(&bare).contains_key("done"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn hook(run: &str) -> HookCommand {
        HookCommand {
            run: run.into(),
            timeout_secs: None,
        }
    }

    #[test]
    fn failed_post_alloc_hook_keeps_the_allocation() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(5);

        let (a, _) = alloc(&bare, "hooked", &opts, &[hook("echo setting up; exit 1")]).unwrap();

        assert_eq!(
            slots::checked_out_branch(&a.path).as_deref(),
            Some("hooked")
        );
        assert_eq!(a.hooks.len(), 1);
        assert!(a.hooks[0].output.contains("setting up"));
        assert!(a.warnings.iter().any(|w| w.contains("post-alloc hook")));

        // An existing allocation changes nothing, so its hooks do not run.
        let (again, _) = alloc(&bare, "hooked", &opts, &[hook("exit 1")]).unwrap();
        assert!(again.existing && again.hooks.is_empty() && again.warnings.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_pre_release_hook_stops_release_unless_forced() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let (a, _) = alloc(&bare, "keep", &AllocOptions::pool(5), &[]).unwrap();
        let agents = HashMap::new();
        let failing = [hook("echo still running; exit 2")];

        let err = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &ReleaseOptions::default(),
            &agents,
            &failing,
        )
        .unwrap_err();
        let refused = err.downcast::<ReleaseRefused>().unwrap();
        match refused.reason {
            ReleaseRefusedReason::HookFailed { hook, output } => {
                assert!(hook.contains("exit code 2"), "{hook}");
                assert!(output.contains("still running"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(slots::checked_out_branch(&a.path).as_deref(), Some("keep"));

        let forced = ReleaseOptions {
            force: true,
            ..Default::default()
        };
        let (r, _) = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &forced,
            &agents,
            &failing,
        )
        .unwrap();
        assert!(matches!(r.outcome, ReleaseOutcome::Detached { .. }));
        assert!(!r.hooks[0].success);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pre_release_hook_does_not_run_when_checks_refuse() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let (a, _) = alloc(&bare, "dirty", &AllocOptions::pool(5), &[]).unwrap();
        std::fs::write(a.path.join("wip.txt"), "x").unwrap();
        let marker = root.join("hook-ran");

        let err = release(
            &bare,
            Some("work1"),
            Path::new("/"),
            &ReleaseOptions::default(),
            &HashMap::new(),
            &[hook(&format!("touch {}", marker.display()))],
        )
        .unwrap_err();

        assert_eq!(
            err.downcast::<ReleaseRefused>().unwrap().reason,
            ReleaseRefusedReason::Dirty
        );
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn titles_follow_the_branch_and_clear_cleanly() {
        let (root, bare) = if git_available() {
            setup()
        } else {
            return;
        };
        set_title(&bare, "zellij-bump", Some("Bump zellij")).unwrap();
        assert_eq!(user_titles(&bare)["zellij-bump"], "Bump zellij");

        // A release that saves a session keeps the title.
        let mut mem = load_branches(&bare);
        mem.get_mut("zellij-bump").unwrap().session_id = Some("s".into());
        save_branches(&bare, &mem).unwrap();
        set_title(&bare, "zellij-bump", None).unwrap();
        assert!(user_titles(&bare).is_empty());
        assert_eq!(
            load_branches(&bare)["zellij-bump"].session_id.as_deref(),
            Some("s")
        );

        // Clearing the last field drops the record; a blank title clears too.
        set_title(&bare, "other", Some("x")).unwrap();
        set_title(&bare, "other", Some("   ")).unwrap();
        assert!(!load_branches(&bare).contains_key("other"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_or_malformed_branch_memory_is_empty() {
        let (root, bare) = if git_available() {
            setup()
        } else {
            return;
        };
        assert!(load_branches(&bare).is_empty());
        std::fs::create_dir_all(bare.join("swamp")).unwrap();
        std::fs::write(branches_path(&bare), "{not json").unwrap();
        assert!(load_branches(&bare).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The status file keeps its flat worktree-name map; branch memory lives in
    /// its own file, so an older swamp still reads `.swamp-status.json`.
    #[test]
    fn status_file_stays_a_flat_agent_map() {
        let (root, bare) = if git_available() {
            setup()
        } else {
            return;
        };
        std::fs::write(
            bare.join(".swamp-status.json"),
            r#"{"main":{"status":"idle","ts":1,"session_id":"keep"}}"#,
        )
        .unwrap();
        save_branches(
            &bare,
            &HashMap::from([("b".to_string(), BranchRecord::default())]),
        )
        .unwrap();

        let agents = load_agents(&bare);

        assert_eq!(agents["main"].session_id.as_deref(), Some("keep"));
        assert_eq!(agents.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_file_changes_round_trip() {
        let (root, bare) = if git_available() {
            setup()
        } else {
            return;
        };
        let changes = AgentChanges {
            set: vec![("work1".into(), record("x", AgentStatus::Idle))],
            clear: vec![],
        };
        apply_to_status_file(&bare, &changes).unwrap();
        assert_eq!(load_agents(&bare)["work1"].session_id.as_deref(), Some("x"));
        apply_to_status_file(
            &bare,
            &AgentChanges {
                set: vec![],
                clear: vec!["work1".into()],
            },
        )
        .unwrap();
        assert!(load_agents(&bare).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
