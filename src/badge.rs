//! Attention badges in Zellij tab names.
//!
//! The daemon marks the tab of a worktree whose agent waits for the user
//! (`● feature`), and shows on the dashboard tab the number of waiting
//! worktrees in other repositories (`dashboard ●2`). Every lookup of a tab by
//! name compares canonical names, which are the names without a badge.

use crate::zellij::TabInfo;
use std::collections::BTreeSet;

/// Name of the first tab of a repo session.
pub const DASHBOARD_TAB: &str = "dashboard";

/// Badge glyphs that swamp can have written: the default glyph and the glyph
/// for `SWAMP_ASCII`. Both are recognized, whatever the current mode is.
const GLYPHS: [char; 2] = ['●', '*'];

/// The glyph that swamp writes in the current mode.
pub fn glyph() -> char {
    if crate::util::ascii_mode() {
        GLYPHS[1]
    } else {
        GLYPHS[0]
    }
}

/// `name` without a trailing ` <glyph><digits>` count badge.
fn strip_count(name: &str) -> Option<&str> {
    let (base, badge) = name.rsplit_once(' ')?;
    let digits = badge.strip_prefix(GLYPHS)?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(base)
}

/// The tab name without an attention badge. A worktree tab loses a leading
/// `<glyph> `. The dashboard tab loses a trailing ` <glyph><digits>`.
pub fn canonical_tab_name(name: &str) -> &str {
    if strip_count(name) == Some(DASHBOARD_TAB) {
        return DASHBOARD_TAB;
    }
    match name.strip_prefix(GLYPHS).and_then(|r| r.strip_prefix(' ')) {
        Some(rest) if !rest.is_empty() => rest,
        _ => name,
    }
}

pub fn worktree_tab_name(worktree: &str, waiting: bool, glyph: char) -> String {
    if waiting {
        format!("{glyph} {worktree}")
    } else {
        worktree.to_string()
    }
}

pub fn dashboard_tab_name(external_count: usize, glyph: char) -> String {
    if external_count > 0 {
        format!("{DASHBOARD_TAB} {glyph}{external_count}")
    } else {
        DASHBOARD_TAB.to_string()
    }
}

/// The state that decides the badges of one repo session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs {
    /// Name of each worktree of the repository.
    pub worktrees: BTreeSet<String>,
    /// Name of each worktree whose agent waits for the user.
    pub waiting: BTreeSet<String>,
    /// Number of waiting worktrees in other repositories.
    pub external: usize,
}

/// The renames that make the tab names agree with `inputs`: the ID of each
/// tab whose name must change, with the new name.
///
/// A tab that is not the dashboard tab or a worktree tab is not touched.
pub fn plan_renames(tabs: &[TabInfo], inputs: &Inputs, glyph: char) -> Vec<(u32, String)> {
    tabs.iter()
        .filter_map(|tab| {
            let canonical = canonical_tab_name(&tab.name);
            let wanted = if canonical == DASHBOARD_TAB {
                dashboard_tab_name(inputs.external, glyph)
            } else if inputs.worktrees.contains(canonical) {
                // A worktree whose own name looks like a badged name cannot
                // be told apart from its badged form, so it gets no badge.
                if canonical_tab_name(canonical) != canonical {
                    return None;
                }
                worktree_tab_name(canonical, inputs.waiting.contains(canonical), glyph)
            } else {
                return None;
            };
            (wanted != tab.name).then_some((tab.tab_id, wanted))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(names: &[&str]) -> Vec<TabInfo> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| TabInfo {
                tab_id: i as u32,
                position: i,
                name: name.to_string(),
            })
            .collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn canonical_name_removes_a_badge_and_keeps_a_plain_name() {
        assert_eq!(canonical_tab_name("feature"), "feature");
        assert_eq!(canonical_tab_name("● feature"), "feature");
        assert_eq!(canonical_tab_name("* feature"), "feature");
        assert_eq!(canonical_tab_name("dashboard"), "dashboard");
        assert_eq!(canonical_tab_name("dashboard ●2"), "dashboard");
        assert_eq!(canonical_tab_name("dashboard ●12"), "dashboard");
        assert_eq!(canonical_tab_name("dashboard *3"), "dashboard");
    }

    #[test]
    fn canonical_name_keeps_text_that_only_looks_like_a_badge() {
        // No space after the glyph, and a glyph with nothing after it.
        assert_eq!(canonical_tab_name("●feature"), "●feature");
        assert_eq!(canonical_tab_name("● "), "● ");
        assert_eq!(canonical_tab_name("●"), "●");
        // A count suffix belongs only to the dashboard tab.
        assert_eq!(canonical_tab_name("feature ●2"), "feature ●2");
        assert_eq!(canonical_tab_name("dashboard ●"), "dashboard ●");
        assert_eq!(canonical_tab_name("dashboard ●2x"), "dashboard ●2x");
        // Only one badge is removed.
        assert_eq!(canonical_tab_name("● ● feature"), "● feature");
    }

    #[test]
    fn wanted_names_carry_the_badge() {
        assert_eq!(worktree_tab_name("feature", true, '●'), "● feature");
        assert_eq!(worktree_tab_name("feature", false, '●'), "feature");
        assert_eq!(worktree_tab_name("feature", true, '*'), "* feature");
        assert_eq!(dashboard_tab_name(0, '●'), "dashboard");
        assert_eq!(dashboard_tab_name(2, '●'), "dashboard ●2");
        assert_eq!(dashboard_tab_name(12, '*'), "dashboard *12");
    }

    #[test]
    fn plan_adds_and_removes_badges() {
        let inputs = Inputs {
            worktrees: set(&["main", "feature", "done"]),
            waiting: set(&["feature"]),
            external: 2,
        };
        let open = tabs(&["dashboard", "main", "feature", "● done", "scratch"]);
        assert_eq!(
            plan_renames(&open, &inputs, '●'),
            [
                (0, "dashboard ●2".to_string()),
                (2, "● feature".to_string()),
                (3, "done".to_string()),
            ]
        );

        // Names that already agree need no rename.
        let open = tabs(&["dashboard ●2", "main", "● feature", "done"]);
        assert!(plan_renames(&open, &inputs, '●').is_empty());
    }

    #[test]
    fn plan_leaves_tabs_that_swamp_does_not_own() {
        let inputs = Inputs {
            worktrees: set(&["feature"]),
            waiting: set(&["feature", "notes"]),
            external: 0,
        };
        let open = tabs(&["● notes", "notes", "dashboard ●9 old"]);
        assert!(plan_renames(&open, &inputs, '●').is_empty());
    }

    #[test]
    fn plan_replaces_a_badge_from_the_other_glyph_mode() {
        let inputs = Inputs {
            worktrees: set(&["feature"]),
            waiting: set(&["feature"]),
            external: 1,
        };
        let open = tabs(&["dashboard *1", "* feature"]);
        assert_eq!(
            plan_renames(&open, &inputs, '●'),
            [
                (0, "dashboard ●1".to_string()),
                (1, "● feature".to_string())
            ]
        );
    }

    #[test]
    fn a_worktree_name_that_starts_with_the_glyph_gets_no_badge() {
        let inputs = Inputs {
            worktrees: set(&["● odd"]),
            waiting: set(&["● odd"]),
            external: 0,
        };
        // The tab reads as the badged form of `odd`, which is not a worktree.
        assert!(plan_renames(&tabs(&["● odd"]), &inputs, '●').is_empty());
    }
}
