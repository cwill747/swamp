use super::*;
use crate::worktree::test_support::{run as git, setup};
use std::cell::Cell;

struct Fixture {
    root: PathBuf,
    common: PathBuf,
    wt: Worktree,
}

impl Fixture {
    fn new(pool_size: u32) -> Self {
        let (root, common) = setup();
        git(
            &common,
            &["remote", "add", "origin", common.to_str().unwrap()],
        );
        git(&common, &["update-ref", "refs/remotes/origin/main", "main"]);
        git(
            &common,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let (allocation, _) = super::super::alloc(
            &common,
            "feature",
            &slots::AllocOptions::pool(pool_size),
            &[],
        )
        .unwrap();
        let wt = Worktree {
            path: allocation.path,
            branch: "feature".into(),
        };
        Self { root, common, wt }
    }

    fn commit(&self) {
        std::fs::write(self.wt.path.join("work.txt"), "work").unwrap();
        git(&self.wt.path, &["add", "work.txt"]);
        git(&self.wt.path, &["commit", "-m", "feature work"]);
    }

    fn scan(
        &self,
        cfg: &SlotsConfig,
        records: &HashMap<String, AgentRecord>,
    ) -> (AgentChanges, usize) {
        let closes = Cell::new(0);
        let changes = super::run(
            &self.common,
            cfg,
            &HashMap::new(),
            || records.clone(),
            |wt| {
                assert_eq!(
                    slots::checked_out_branch(&wt.path).as_deref(),
                    Some("feature")
                );
                closes.set(closes.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        (changes, closes.get())
    }

    fn merge(&self) {
        // First observe an unmerged tip. The branch memory survives a daemon
        // restart; each scan below constructs a fresh service invocation.
        assert_eq!(self.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
        git(
            &self.common,
            &["update-ref", "refs/remotes/origin/main", "feature"],
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn merged_cleanup_is_restart_safe_and_idempotent() {
    let f = Fixture::new(5);
    f.commit();
    f.merge();
    let (changes, closes) = f.scan(&SlotsConfig::default(), &HashMap::new());
    assert_eq!(closes, 1);
    assert_eq!(changes.clear, vec![f.wt.name()]);
    assert!(slots::checked_out_branch(&f.wt.path).is_none());
    assert!(!load_branches(&f.common).contains_key("feature"));
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
}

#[test]
fn merge_commit_cleans_feature_and_preserves_default_worktree() {
    let f = Fixture::new(5);
    f.commit();
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
    let main_path = f.root.join("main");
    git(
        &f.common,
        &["worktree", "add", main_path.to_str().unwrap(), "main"],
    );
    git(
        &main_path,
        &["merge", "--no-ff", "-m", "merged feature", "feature"],
    );
    git(
        &f.common,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
    assert_eq!(
        slots::checked_out_branch(&main_path).as_deref(),
        Some("main")
    );
}

#[test]
fn pushed_feature_and_unpushed_local_main_are_not_merge_evidence() {
    let f = Fixture::new(5);
    f.commit();
    git(
        &f.common,
        &["update-ref", "refs/remotes/origin/feature", "feature"],
    );
    git(
        &f.common,
        &["branch", "--set-upstream-to=origin/feature", "feature"],
    );
    git(&f.common, &["update-ref", "refs/heads/main", "feature"]);
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
    assert_eq!(
        slots::checked_out_branch(&f.wt.path).as_deref(),
        Some("feature")
    );
}

#[test]
fn new_branch_at_destination_is_not_cleaned() {
    let f = Fixture::new(5);
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
}

#[test]
fn new_branch_on_a_custom_base_is_retained_when_the_base_merges() {
    let f = Fixture::new(5);
    f.commit();
    let (child, _) = super::super::alloc(
        &f.common,
        "child",
        &slots::AllocOptions {
            pool_size: 5,
            base: Some("feature".into()),
        },
        &[],
    )
    .unwrap();
    f.merge();
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
    assert_eq!(
        slots::checked_out_branch(&child.path).as_deref(),
        Some("child")
    );
}

#[test]
fn stale_feature_tracking_ref_does_not_block_a_proven_merge() {
    let f = Fixture::new(5);
    git(
        &f.common,
        &["update-ref", "refs/remotes/origin/feature", "main"],
    );
    git(
        &f.common,
        &["branch", "--set-upstream-to=origin/feature", "feature"],
    );
    f.commit();
    f.merge();
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
}

#[test]
fn merged_tip_with_new_local_work_is_not_cleaned() {
    let f = Fixture::new(5);
    f.commit();
    f.merge();
    git(
        &f.wt.path,
        &["commit", "--allow-empty", "-m", "unfinished work"],
    );
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
    assert!(slots::checked_out_branch(&f.wt.path).is_some());
}

#[test]
fn agent_becoming_active_during_hooks_defers_cleanup() {
    let f = Fixture::new(5);
    f.commit();
    f.merge();
    let reads = Cell::new(0);
    let changes = super::run(
        &f.common,
        &SlotsConfig::default(),
        &HashMap::new(),
        || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                HashMap::new()
            } else {
                HashMap::from([(
                    f.wt.name(),
                    AgentRecord {
                        status: AgentStatus::Working,
                        ..Default::default()
                    },
                )])
            }
        },
        |_| panic!("active agent must not be closed"),
    )
    .unwrap();
    assert!(changes.is_empty());
    assert!(slots::checked_out_branch(&f.wt.path).is_some());
}

#[test]
fn disabled_autoclean_leaves_merged_branch() {
    let f = Fixture::new(5);
    f.commit();
    f.merge();
    let cfg = SlotsConfig {
        autoclean: false,
        ..Default::default()
    };
    assert_eq!(f.scan(&cfg, &HashMap::new()).1, 0);
    assert!(slots::checked_out_branch(&f.wt.path).is_some());
}

#[test]
fn busy_waiting_dirty_pinned_locked_and_rebasing_worktrees_are_preserved() {
    for condition in [
        "working",
        "waiting",
        "dirty",
        "pinned",
        "locked",
        "merge",
        "corrupt",
        "corrupt_memory",
        "unreadable_status",
    ] {
        let f = Fixture::new(5);
        f.commit();
        f.merge();
        let mut agents = HashMap::new();
        match condition {
            "working" | "waiting" => {
                agents.insert(
                    f.wt.name(),
                    AgentRecord {
                        status: if condition == "working" {
                            AgentStatus::Working
                        } else {
                            AgentStatus::Waiting
                        },
                        ..Default::default()
                    },
                );
            }
            "dirty" => std::fs::write(f.wt.path.join("wip.txt"), "unfinished").unwrap(),
            "pinned" => {
                let path = f.common.join("swamp/pins").join(f.wt.name());
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "keep").unwrap();
            }
            "locked" => git(
                &f.common,
                &["worktree", "lock", f.wt.path.to_str().unwrap()],
            ),
            "merge" => {
                let repo = Repository::open(&f.wt.path).unwrap();
                std::fs::write(
                    repo.path().join("MERGE_HEAD"),
                    repo.head().unwrap().target().unwrap().to_string(),
                )
                .unwrap();
            }
            "corrupt" => std::fs::write(super::super::status_path(&f.common), "{broken").unwrap(),
            "corrupt_memory" => std::fs::write(branches_path(&f.common), "{broken").unwrap(),
            "unreadable_status" => {
                let repo = Repository::open(&f.wt.path).unwrap();
                std::fs::write(repo.path().join("index"), "broken index").unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(f.scan(&SlotsConfig::default(), &agents).1, 0, "{condition}");
        assert_eq!(
            slots::checked_out_branch(&f.wt.path).as_deref(),
            Some("feature"),
            "{condition}"
        );
    }
}

#[test]
fn hooks_cannot_add_work_or_change_branches_under_cleanup() {
    for command in [
        "touch unfinished",
        "git commit --allow-empty -m new-work",
        "git checkout -b replacement",
        "exit 1",
    ] {
        let f = Fixture::new(5);
        f.commit();
        f.merge();
        let mut cfg = SlotsConfig::default();
        cfg.hooks.pre_release.push(HookCommand {
            run: command.into(),
            timeout_secs: None,
        });
        assert_eq!(f.scan(&cfg, &HashMap::new()).1, 0, "{command}");
        assert!(slots::checked_out_branch(&f.wt.path).is_some());
    }
}

#[test]
fn failed_session_close_can_be_retried_without_releasing_worktree() {
    let f = Fixture::new(5);
    f.commit();
    f.merge();
    let changes = super::run(
        &f.common,
        &SlotsConfig::default(),
        &HashMap::new(),
        HashMap::new,
        |_| anyhow::bail!("close failed"),
    )
    .unwrap();
    assert!(changes.is_empty());
    assert!(slots::checked_out_branch(&f.wt.path).is_some());
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
}

#[test]
fn overflow_worktree_is_removed() {
    let f = Fixture::new(0);
    f.commit();
    f.merge();
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
    assert!(!f.wt.path.exists());
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 0);
}

#[test]
fn squash_requires_merged_pr_with_exact_head() {
    for (state, matches, expected) in [
        ("MERGED", true, 1),
        ("CLOSED", true, 0),
        ("MERGED", false, 0),
    ] {
        let f = Fixture::new(5);
        f.commit();
        let repo = Repository::open(&f.wt.path).unwrap();
        let tip = repo.head().unwrap().target().unwrap();
        let facts = PrFacts {
            branch: "feature".into(),
            state: state.into(),
            head_oid: Some(if matches {
                tip
            } else {
                repo.find_commit(tip).unwrap().parent_id(0).unwrap()
            }),
        };
        let closes = Cell::new(0);
        super::run(
            &f.common,
            &SlotsConfig::default(),
            &HashMap::from([("feature".into(), facts)]),
            HashMap::new,
            |_| {
                closes.set(closes.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(closes.get(), expected, "{state} {matches}");
    }
}

#[test]
fn config_defaults_and_repository_override() {
    let user: crate::config::SwampConfig = toml::from_str("").unwrap();
    assert!(user.slots.autoclean);
    let repo: crate::config::RepoConfig = toml::from_str("[slots]\nautoclean = false").unwrap();
    assert!(!user.slots.merged_with(&repo.slots).autoclean);
    let user: crate::config::SwampConfig = toml::from_str("[slots]\nautoclean = false").unwrap();
    let repo: crate::config::RepoConfig = toml::from_str("[slots]\nautoclean = true").unwrap();
    assert!(user.slots.merged_with(&repo.slots).autoclean);
}

#[test]
fn reopened_squash_merge_is_protected_only_with_exact_pr_evidence() {
    for pool_size in [0, 2] {
        for exact in [false, true] {
            let f = Fixture::new(pool_size);
            f.commit();
            let tip = Repository::open(&f.wt.path)
                .unwrap()
                .head()
                .unwrap()
                .target()
                .unwrap();
            let pr = PrFacts {
                branch: "feature".into(),
                state: "MERGED".into(),
                head_oid: Some(if exact { tip } else { Oid::ZERO_SHA1 }),
            };
            super::super::alloc_with_pr(
                &f.common,
                "feature",
                &slots::AllocOptions::pool(pool_size),
                &[],
                Some(&pr),
            )
            .unwrap();
            let prs = HashMap::from([(
                "feature".into(),
                PrFacts {
                    head_oid: Some(tip),
                    ..pr
                },
            )]);
            let closes = Cell::new(0);
            for _ in 0..2 {
                super::run(
                    &f.common,
                    &SlotsConfig::default(),
                    &prs,
                    HashMap::new,
                    |_| {
                        closes.set(closes.get() + 1);
                        Ok(())
                    },
                )
                .unwrap();
            }
            assert_eq!(closes.get(), if exact { 0 } else { 1 });
        }
    }
}

#[test]
fn allocating_unmerged_work_does_not_prevent_later_cleanup() {
    let f = Fixture::new(2);
    f.commit();
    super::super::alloc(&f.common, "feature", &slots::AllocOptions::pool(2), &[]).unwrap();
    f.merge();
    assert_eq!(f.scan(&SlotsConfig::default(), &HashMap::new()).1, 1);
}
