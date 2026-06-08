use super::checks::{CheckRollupItem, aggregate_checks};
use super::types::{PrSummary, ReviewDecision};
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct PrBatchItem {
    number: u32,
    title: String,
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    url: String,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<CheckRollupItem>,
}

pub(super) fn list_prs_for_branches(
    repo_root: &Path,
    branches: &[String],
) -> Result<HashMap<String, PrSummary>> {
    let mut map = HashMap::new();

    for branch in branches {
        let output = match Command::new("gh")
            .current_dir(repo_root)
            .args([
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "all",
                "--json",
                "number,title,state,isDraft,headRefName,url,statusCheckRollup,reviewDecision",
                "--limit",
                "1",
            ])
            .output()
        {
            Ok(output) => output,
            Err(_) => continue,
        };

        if !output.status.success() {
            continue;
        }

        let prs: Vec<PrBatchItem> = match serde_json::from_slice(&output.stdout) {
            Ok(prs) => prs,
            Err(_) => continue,
        };

        if let Some(pr) = prs.into_iter().next() {
            let (checks, check_meta) = aggregate_checks(&pr.status_check_rollup);
            let review = parse_review_decision(pr.review_decision.as_deref());
            map.insert(
                pr.head_ref_name,
                PrSummary {
                    number: pr.number,
                    title: pr.title,
                    state: pr.state,
                    is_draft: pr.is_draft,
                    checks,
                    check_meta,
                    url: Some(pr.url),
                    review,
                },
            );
        }
    }

    Ok(map)
}

fn parse_review_decision(s: Option<&str>) -> Option<ReviewDecision> {
    match s? {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}
