//! Rank the worktrees that need the user, across all repositories.
//!
//! The ranking is a pure function of the daemon snapshots, so that unit tests
//! cover it without a daemon.

use crate::daemon::state::{AgentStatus, PrSnapshot, Snapshot, WorktreeRow};
use crate::github::{CheckState, ReviewDecision};
use std::path::PathBuf;

/// Why a worktree needs the user. The order of the variants is the order of
/// the tiers, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    Waiting,
    ChecksFailed,
    ChangesRequested,
    Conflict,
    Rebase,
    IdleDirty,
}

impl Reason {
    pub fn tier(self) -> u8 {
        match self {
            Reason::Waiting => 1,
            Reason::ChecksFailed => 2,
            Reason::ChangesRequested => 3,
            Reason::Conflict | Reason::Rebase => 4,
            Reason::IdleDirty => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reason::Waiting => "agent waits for you",
            Reason::ChecksFailed => "checks failed",
            Reason::ChangesRequested => "changes requested",
            Reason::Conflict => "merge conflict",
            Reason::Rebase => "rebase in progress",
            Reason::IdleDirty => "idle with changes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Socket file stem of the daemon that reports the worktree.
    pub repo_id: String,
    /// Repository name for display.
    pub repo: String,
    pub worktree: String,
    pub path: PathBuf,
    pub reason: Reason,
    /// Unix time at which the state started, when the daemon reports it.
    pub since: Option<u64>,
}

/// The state of one repository, as input to [`rank`].
pub struct RepoState<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub snapshot: &'a Snapshot,
    pub prs: &'a PrSnapshot,
}

fn reason_for(row: &WorktreeRow, prs: &PrSnapshot) -> Option<Reason> {
    if row.deleting {
        return None;
    }
    if row.agent == AgentStatus::Waiting {
        return Some(Reason::Waiting);
    }
    // A merged or closed pull request needs no action.
    let pr = prs.prs.get(&row.branch).filter(|pr| pr.state == "OPEN");
    if pr.is_some_and(|pr| matches!(pr.checks, Some(CheckState::Failure { .. }))) {
        return Some(Reason::ChecksFailed);
    }
    if pr.is_some_and(|pr| pr.review == Some(ReviewDecision::ChangesRequested)) {
        return Some(Reason::ChangesRequested);
    }
    if row.conflict {
        return Some(Reason::Conflict);
    }
    if row.rebase {
        return Some(Reason::Rebase);
    }
    let dirty = row.staged + row.unstaged + row.untracked > 0;
    if row.agent == AgentStatus::Idle && dirty && !row.is_default {
        return Some(Reason::IdleDirty);
    }
    None
}

/// List each worktree that needs the user, one time, most urgent first.
pub fn rank(repos: &[RepoState<'_>]) -> Vec<Item> {
    let mut items: Vec<Item> = repos
        .iter()
        .flat_map(|repo| {
            repo.snapshot.rows.iter().filter_map(|row| {
                let reason = reason_for(row, repo.prs)?;
                let timed = matches!(reason, Reason::Waiting | Reason::IdleDirty);
                Some(Item {
                    repo_id: repo.id.to_string(),
                    repo: repo.name.to_string(),
                    worktree: row.name.clone(),
                    path: row.path.clone(),
                    reason,
                    since: (timed && row.agent_ts > 0).then_some(row.agent_ts),
                })
            })
        })
        .collect();
    items.sort_by(|a, b| {
        let by_name =
            || (&a.repo, &a.worktree, &a.repo_id).cmp(&(&b.repo, &b.worktree, &b.repo_id));
        a.reason
            .tier()
            .cmp(&b.reason.tier())
            .then_with(|| match a.reason.tier() {
                // The agent that has waited longest is first.
                1 => a.since.cmp(&b.since).then_with(by_name),
                // The agent that stopped most recently is first.
                5 => b.since.cmp(&a.since).then_with(by_name),
                _ => by_name(),
            })
    });
    items
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::github::{CheckState, PrSummary, ReviewDecision};

    pub(crate) fn pr(
        state: &str,
        checks: Option<CheckState>,
        review: Option<ReviewDecision>,
    ) -> PrSummary {
        PrSummary {
            number: 1,
            title: "A change".into(),
            state: state.into(),
            head_oid: None,
            is_draft: false,
            checks,
            check_meta: None,
            url: None,
            comment_count: 0,
            review,
            reviews_partial: false,
        }
    }

    pub(crate) fn failed() -> Option<CheckState> {
        Some(CheckState::Failure {
            passed: 1,
            total: 2,
            failed: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::discover::test_support::row;
    use std::path::Path;

    fn wt(name: &str) -> WorktreeRow {
        row(name, Path::new("/repos/x").join(name).as_path())
    }

    fn waiting(name: &str, ts: u64) -> WorktreeRow {
        WorktreeRow {
            agent: AgentStatus::Waiting,
            agent_ts: ts,
            ..wt(name)
        }
    }

    fn idle_dirty(name: &str, ts: u64) -> WorktreeRow {
        WorktreeRow {
            unstaged: 1,
            agent_ts: ts,
            ..wt(name)
        }
    }

    fn snapshot(rows: Vec<WorktreeRow>) -> Snapshot {
        Snapshot {
            rows,
            default_known: true,
        }
    }

    fn prs(entries: Vec<(&str, crate::github::PrSummary)>) -> PrSnapshot {
        PrSnapshot {
            prs: entries
                .into_iter()
                .map(|(branch, pr)| (branch.to_string(), pr))
                .collect(),
            ..PrSnapshot::default()
        }
    }

    fn rank_one(snapshot: &Snapshot, prs: &PrSnapshot) -> Vec<Item> {
        rank(&[RepoState {
            id: "id",
            name: "repo",
            snapshot,
            prs,
        }])
    }

    fn summary(items: &[Item]) -> Vec<(&str, &str, Reason)> {
        items
            .iter()
            .map(|i| (i.repo.as_str(), i.worktree.as_str(), i.reason))
            .collect()
    }

    #[test]
    fn each_tier_produces_its_reason() {
        let snap = snapshot(vec![
            waiting("waits", 10),
            wt("ci"),
            wt("review"),
            WorktreeRow {
                conflict: true,
                ..wt("conflict")
            },
            WorktreeRow {
                rebase: true,
                ..wt("rebase")
            },
            idle_dirty("dirty", 20),
            WorktreeRow {
                agent: AgentStatus::Working,
                unstaged: 3,
                ..wt("busy")
            },
            wt("clean"),
        ]);
        let prs = prs(vec![
            ("ci", pr("OPEN", failed(), None)),
            (
                "review",
                pr(
                    "OPEN",
                    Some(CheckState::Success),
                    Some(ReviewDecision::ChangesRequested),
                ),
            ),
            ("clean", pr("OPEN", Some(CheckState::Success), None)),
        ]);
        let items = rank_one(&snap, &prs);
        assert_eq!(
            summary(&items),
            [
                ("repo", "waits", Reason::Waiting),
                ("repo", "ci", Reason::ChecksFailed),
                ("repo", "review", Reason::ChangesRequested),
                ("repo", "conflict", Reason::Conflict),
                ("repo", "rebase", Reason::Rebase),
                ("repo", "dirty", Reason::IdleDirty),
            ]
        );
        assert_eq!(items[0].since, Some(10));
        assert_eq!(items[1].since, None);
        assert_eq!(items[5].since, Some(20));
    }

    #[test]
    fn a_worktree_in_two_tiers_appears_one_time_at_the_highest() {
        let snap = snapshot(vec![WorktreeRow {
            conflict: true,
            unstaged: 2,
            ..waiting("both", 5)
        }]);
        let prs = prs(vec![("both", pr("OPEN", failed(), None))]);
        assert_eq!(
            summary(&rank_one(&snap, &prs)),
            [("repo", "both", Reason::Waiting)]
        );
    }

    #[test]
    fn a_merged_pull_request_with_failed_checks_is_not_listed() {
        let snap = snapshot(vec![wt("done")]);
        let prs = prs(vec![(
            "done",
            pr("MERGED", failed(), Some(ReviewDecision::ChangesRequested)),
        )]);
        assert!(rank_one(&snap, &prs).is_empty());
    }

    #[test]
    fn the_default_branch_is_excluded_from_tier_five_only() {
        let dirty_default = WorktreeRow {
            is_default: true,
            ..idle_dirty("main", 9)
        };
        let none = PrSnapshot::default();
        assert!(rank_one(&snapshot(vec![dirty_default.clone()]), &none).is_empty());

        let conflicted = WorktreeRow {
            conflict: true,
            ..dirty_default
        };
        assert_eq!(
            summary(&rank_one(&snapshot(vec![conflicted]), &none)),
            [("repo", "main", Reason::Conflict)]
        );
    }

    #[test]
    fn a_worktree_with_a_removal_in_flight_is_excluded() {
        let snap = snapshot(vec![WorktreeRow {
            deleting: true,
            ..waiting("going", 1)
        }]);
        assert!(rank_one(&snap, &PrSnapshot::default()).is_empty());
    }

    #[test]
    fn order_across_repositories() {
        let a = snapshot(vec![
            waiting("late", 300),
            idle_dirty("old", 100),
            wt("zeta"),
        ]);
        let b = snapshot(vec![
            waiting("early", 200),
            idle_dirty("new", 400),
            wt("alpha"),
        ]);
        let a_prs = prs(vec![("zeta", pr("OPEN", failed(), None))]);
        let b_prs = prs(vec![("alpha", pr("OPEN", failed(), None))]);
        let items = rank(&[
            RepoState {
                id: "1",
                name: "beta",
                snapshot: &a,
                prs: &a_prs,
            },
            RepoState {
                id: "2",
                name: "acme",
                snapshot: &b,
                prs: &b_prs,
            },
        ]);
        assert_eq!(
            summary(&items),
            [
                // Tier 1: the oldest wait is first.
                ("acme", "early", Reason::Waiting),
                ("beta", "late", Reason::Waiting),
                // Tier 2: by repository, then worktree.
                ("acme", "alpha", Reason::ChecksFailed),
                ("beta", "zeta", Reason::ChecksFailed),
                // Tier 5: the newest agent timestamp is first.
                ("acme", "new", Reason::IdleDirty),
                ("beta", "old", Reason::IdleDirty),
            ]
        );
    }
}
