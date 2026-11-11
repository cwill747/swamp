//! Work titles: what a worktree row is *doing*, because a slot's directory
//! name (`work3`) says nothing about the branch it holds.
//!
//! The first available source wins: the user's title for the branch, the
//! branch's PR title, the Claude session name, the one openspec change the
//! branch adds, the last commit subject, and finally the branch name. A free
//! slot reads `free`, and a pinned slot shows its pin reason.

use crate::worktree::SlotState;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where a row's title came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    User,
    Pr,
    Session,
    Openspec,
    Commit,
    Branch,
    /// A free or pinned slot.
    Slot,
}

/// The candidate values for one row, highest priority first.
#[derive(Debug, Default, Clone, Copy)]
pub struct TitleInputs<'a> {
    pub user: Option<&'a str>,
    pub pr: Option<&'a str>,
    pub session: Option<&'a str>,
    pub openspec: Option<&'a str>,
    pub commit: Option<&'a str>,
    pub branch: &'a str,
    pub slot_state: Option<SlotState>,
    pub pin: Option<&'a str>,
}

pub fn resolve(i: TitleInputs<'_>) -> (String, TitleSource) {
    match i.slot_state {
        Some(SlotState::Free) => return ("free".into(), TitleSource::Slot),
        Some(SlotState::Pinned) => {
            return (i.pin.unwrap_or("pinned").to_string(), TitleSource::Slot);
        }
        _ => {}
    }
    let candidates = [
        (i.user, TitleSource::User),
        (i.pr, TitleSource::Pr),
        (i.session, TitleSource::Session),
        (i.openspec, TitleSource::Openspec),
        (i.commit, TitleSource::Commit),
    ];
    candidates
        .into_iter()
        .find_map(|(v, src)| {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| (s.to_string(), src))
        })
        .unwrap_or_else(|| (i.branch.to_string(), TitleSource::Branch))
}

/// The one openspec change that the worktree at `worktree` adds compared with
/// the default branch tip: the directories under `openspec/changes/` (except
/// `archive`) that the default branch's tree lacks. `None` when there are none
/// or more than one. Reads the working tree, so a change that is not committed
/// yet counts. Costs one directory read when the worktree has no openspec
/// changes directory.
pub fn openspec_change(
    common: &Path,
    worktree: &Path,
    default_tip: Option<git2::Oid>,
) -> Option<String> {
    let dir = worktree.join("openspec").join("changes");
    let entries = std::fs::read_dir(&dir).ok()?;
    let names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "archive" && !n.starts_with('.'))
        .collect();
    if names.is_empty() {
        return None;
    }
    let repo = git2::Repository::open(common).ok();
    let default_tree = default_tip
        .and_then(|oid| repo.as_ref()?.find_commit(oid).ok())
        .and_then(|c| c.tree().ok());
    let added: Vec<String> = names
        .into_iter()
        .filter(|n| {
            default_tree.as_ref().is_none_or(|t| {
                t.get_path(&Path::new("openspec").join("changes").join(n))
                    .is_err()
            })
        })
        .collect();
    match added.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TitleInputs<'static> {
        TitleInputs {
            branch: "fix-ci",
            ..Default::default()
        }
    }

    #[test]
    fn user_title_beats_pr_title() {
        let (t, s) = resolve(TitleInputs {
            user: Some("Bump zellij"),
            pr: Some("Fix flaky CI retry logic"),
            ..base()
        });
        assert_eq!((t.as_str(), s), ("Bump zellij", TitleSource::User));
    }

    #[test]
    fn each_source_falls_back_in_order() {
        let pr = resolve(TitleInputs {
            pr: Some("Fix flaky CI retry logic"),
            session: Some("s"),
            ..base()
        });
        assert_eq!(pr.1, TitleSource::Pr);
        let session = resolve(TitleInputs {
            session: Some("Worktree slots exploration"),
            commit: Some("c"),
            ..base()
        });
        assert_eq!(
            session,
            ("Worktree slots exploration".into(), TitleSource::Session)
        );
        let spec = resolve(TitleInputs {
            openspec: Some("worktree-slots"),
            commit: Some("c"),
            ..base()
        });
        assert_eq!(spec, ("worktree-slots".into(), TitleSource::Openspec));
        let commit = resolve(TitleInputs {
            commit: Some("feat: add clone"),
            ..base()
        });
        assert_eq!(commit, ("feat: add clone".into(), TitleSource::Commit));
        assert_eq!(resolve(base()), ("fix-ci".into(), TitleSource::Branch));
    }

    #[test]
    fn blank_sources_are_skipped() {
        let (t, s) = resolve(TitleInputs {
            user: Some("  "),
            pr: Some("Real PR"),
            ..base()
        });
        assert_eq!((t.as_str(), s), ("Real PR", TitleSource::Pr));
    }

    #[test]
    fn free_and_pinned_slots() {
        let free = resolve(TitleInputs {
            slot_state: Some(SlotState::Free),
            user: Some("ignored"),
            ..base()
        });
        assert_eq!(free, ("free".into(), TitleSource::Slot));
        let pinned = resolve(TitleInputs {
            slot_state: Some(SlotState::Pinned),
            pin: Some("review #172"),
            ..base()
        });
        assert_eq!(pinned, ("review #172".into(), TitleSource::Slot));
        let busy = resolve(TitleInputs {
            slot_state: Some(SlotState::Busy),
            ..base()
        });
        assert_eq!(busy.1, TitleSource::Branch);
    }

    #[test]
    fn openspec_change_counts_only_changes_the_default_lacks() {
        use crate::worktree::test_support::{git_available, run, setup};
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = root.join("wt");
        run(
            &bare,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "topic",
                wt.to_str().unwrap(),
                "main",
            ],
        );
        run(&wt, &["config", "user.email", "t@example.com"]);
        run(&wt, &["config", "user.name", "t"]);
        // `existing` is on main; `archive` never counts.
        std::fs::create_dir_all(wt.join("openspec/changes/existing")).unwrap();
        std::fs::write(wt.join("openspec/changes/existing/proposal.md"), "x").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-q", "-m", "existing change"]);
        run(&bare, &["branch", "-f", "main", "topic"]);
        let tip = git2::Repository::open(&bare)
            .unwrap()
            .find_branch("main", git2::BranchType::Local)
            .unwrap()
            .get()
            .target();
        std::fs::create_dir_all(wt.join("openspec/changes/archive/old")).unwrap();
        assert_eq!(openspec_change(&bare, &wt, tip), None);

        std::fs::create_dir_all(wt.join("openspec/changes/worktree-slots")).unwrap();
        assert_eq!(
            openspec_change(&bare, &wt, tip).as_deref(),
            Some("worktree-slots")
        );

        std::fs::create_dir_all(wt.join("openspec/changes/second")).unwrap();
        assert_eq!(openspec_change(&bare, &wt, tip), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
