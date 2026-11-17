//! Conservative, restart-safe cleanup using the same slot lock and release
//! checks as an explicit release. No network operations run in this path.
use super::*;
use crate::config::SlotsConfig;
use crate::worktree::{self, Worktree, WorktreeKind};
use git2::{Oid, Repository, RepositoryState, WorktreeLockStatus};

#[cfg(test)]
mod tests;

pub(super) fn load_memory(common: &Path) -> Result<HashMap<String, BranchRecord>> {
    match std::fs::read(branches_path(common)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// Re-evaluate from current refs on every pass, including after hooks. Missing
/// refs and unreadable status fail closed. Absent agent records mean this
/// worktree has no known harness session; explicit active states always defer.
fn eligible(
    common: &Path,
    wt: &Worktree,
    agents: &HashMap<String, AgentRecord>,
    pr: Option<&PrFacts>,
) -> Result<Option<(String, Oid)>> {
    let name = wt.name();
    if common.join("swamp").join("pins").join(&name).try_exists()? {
        return Ok(None);
    }
    let repo = Repository::open(&wt.path)?;
    if repo.head_detached()? || repo.state() != RepositoryState::Clean {
        return Ok(None);
    }
    let head = repo.head()?;
    let branch = head.shorthand()?;
    let tip = head.peel_to_commit()?.id();
    let Some((default, destination)) = worktree::remote_default_tip(common) else {
        return Ok(None);
    };
    let common_repo = Repository::open(common)?;
    if branch == default || common_repo.workdir().is_some_and(|p| p == wt.path) {
        return Ok(None);
    }
    // libgit2 names can differ from the worktree directory name.
    let mut linked_found = false;
    for id in common_repo.worktrees()?.iter().flatten().flatten() {
        let linked = common_repo.find_worktree(id)?;
        if linked.path() == wt.path {
            linked_found = true;
            if !matches!(linked.is_locked()?, WorktreeLockStatus::Unlocked) {
                return Ok(None);
            }
        }
    }
    if !linked_found {
        return Ok(None);
    }
    let merged_pr =
        pr.is_some_and(|p| p.branch == branch && p.state == "MERGED" && p.head_oid == Some(tip));
    // A just-created branch at main is already reachable from main. Require
    // evidence it actually acquired commits before inferring a merge from the
    // graph. If reflogs were disabled/pruned, defer to authoritative PR facts.
    let reachable = tip == destination || common_repo.graph_descendant_of(destination, tip)?;
    let mut memory = load_memory(common)?;
    if memory
        .get(branch)
        .and_then(|r| r.autoclean_initial_tip.as_ref())
        .is_some_and(|initial| initial == &tip.to_string())
    {
        return Ok(None);
    }
    if !reachable && !merged_pr {
        let entry = memory.entry(branch.to_string()).or_default();
        if entry.autoclean_unmerged_tip.as_deref() != Some(&tip.to_string()) {
            entry.autoclean_unmerged_tip = Some(tip.to_string());
            save_branches(common, &memory)?;
        }
        return Ok(None);
    }
    if agents
        .get(&name)
        .is_some_and(|r| r.status != AgentStatus::Idle)
        || worktree::git_info(&wt.path)?.is_dirty()
    {
        return Ok(None);
    }
    let observed_unmerged = memory
        .get(branch)
        .and_then(|r| r.autoclean_unmerged_tip.as_ref())
        .is_some_and(|oid| oid == &tip.to_string());
    let acquired_commits = observed_unmerged
        || common_repo
            .reflog(&format!("refs/heads/{branch}"))
            .ok()
            .is_some_and(|log| {
                log.len()
                    .checked_sub(1)
                    .and_then(|i| log.get(i))
                    .is_some_and(|entry| entry.id_old().is_zero() && entry.id_new() != tip)
            });
    let merged_git = acquired_commits && reachable;
    Ok((merged_pr || merged_git).then(|| (branch.to_string(), tip)))
}

/// `agents` reads live state, rather than a scan's stale copy. `end_session`
/// closes the worktree's tab (and its idle harness process) synchronously.
/// It runs under the slot lock, before detach/removal: a crash or a failed
/// close leaves a busy worktree that the next pass can safely retry. No late
/// closer can kill a newly allocated slot's session.
pub fn run(
    common: &Path,
    cfg: &SlotsConfig,
    prs: &HashMap<String, PrFacts>,
    agents: impl Fn() -> HashMap<String, AgentRecord>,
    mut end_session: impl FnMut(&Worktree) -> Result<()>,
) -> Result<AgentChanges> {
    let mut changes = AgentChanges::default();
    if !cfg.autoclean {
        return Ok(changes);
    }
    for wt in worktree::list_worktrees(common)? {
        let result = (|| -> Result<()> {
            let guard = slots::lock(common)?;
            // The normal status loader tolerates corruption for display. A
            // destructive operation must not interpret corrupt state as idle.
            match std::fs::read(super::status_path(common)) {
                Ok(bytes) => {
                    serde_json::from_slice::<HashMap<String, AgentRecord>>(&bytes)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let branch = slots::checked_out_branch(&wt.path).unwrap_or_default();
            let pr = prs.get(&branch);
            let Some(identity) = eligible(common, &wt, &agents(), pr)? else {
                return Ok(());
            };
            let name = wt.name();
            let opts = ReleaseOptions {
                pr: pr
                    .filter(|p| p.state == "MERGED" && p.head_oid == Some(identity.1))
                    .cloned(),
                ..Default::default()
            };
            let kind = slots::worktree_kind(&name, &identity.0, &worktree::default_branch(common));
            let mut before_detach = |_: &str| -> Result<()> {
                let env = HookEnv::new(common, &wt.path, &name, kind, &identity.0);
                let runs = slot_hooks::run_hooks(&cfg.hooks.pre_release, &env);
                if let Some(failed) = slot_hooks::failure(&runs) {
                    anyhow::bail!("pre-release hook failed: {}", failed.command);
                }
                if eligible(common, &wt, &agents(), pr)? != Some(identity.clone()) {
                    anyhow::bail!("worktree changed during autoclean hooks");
                }
                // Mark before any release, matching the explicit release flow.
                crate::launch::mark_tab_closing(common, &name, std::process::id());
                let result = end_session(&wt);
                crate::launch::clear_tab_closing(common, &name);
                result?;
                if eligible(common, &wt, &agents(), pr)? != Some(identity.clone()) {
                    anyhow::bail!("worktree changed while ending its session");
                }
                Ok(())
            };
            match kind {
                WorktreeKind::Slot => {
                    slots::release_locked(&guard, common, &wt, &opts, before_detach)?;
                }
                WorktreeKind::Overflow => {
                    // Explicit release keeps overflow trees. Autoclean uses the
                    // existing non-forced removal path to reclaim these too.
                    worktree::check_worktree_removal(
                        common,
                        &name,
                        opts.pr.as_ref().map(|p| p.state.clone()),
                        opts.pr.as_ref().and_then(|p| p.head_oid),
                    )?;
                    before_detach(&identity.0)?;
                    worktree::remove_worktree(
                        common,
                        &name,
                        false,
                        false,
                        opts.pr.as_ref().map(|p| p.state.clone()),
                        opts.pr.as_ref().and_then(|p| p.head_oid),
                    )?;
                }
                WorktreeKind::Default => return Ok(()),
            }
            // The completed harness session must not be restored on reallocation.
            changes.clear.push(name.clone());
            let mut memory = load_memory(common)?;
            memory.remove(&identity.0);
            save_branches(common, &memory)?;
            tracing::info!(worktree = %name, branch = %identity.0, "autocleaned merged branch");
            Ok(())
        })();
        if let Err(e) = result {
            tracing::debug!(worktree = %wt.name(), "autoclean deferred: {e:#}");
        }
    }
    Ok(changes)
}
