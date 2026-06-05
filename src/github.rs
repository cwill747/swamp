use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckState {
    Success,
    Failure { passed: u32, total: u32 },
    Pending { passed: u32, total: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    Commented,
    ReviewRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CheckMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrSummary {
    pub number: u32,
    pub title: String,
    pub state: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(default)]
    pub checks: Option<CheckState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_meta: Option<CheckMeta>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewDecision>,
}

// --- Internal types ---

#[derive(Debug, Deserialize)]
struct CheckRollupItem {
    #[serde(alias = "state")]
    status: Option<String>,
    conclusion: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepositoryOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RepoContext {
    #[allow(dead_code)]
    name: String,
    owner: RepositoryOwner,
    url: String,
}

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

// --- GraphQL response types ---

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    data: Option<GraphqlData>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    repository: HashMap<String, GraphqlPrConnection>,
}

#[derive(Debug, Deserialize)]
struct GraphqlPrConnection {
    nodes: Vec<GraphqlPrNode>,
}

#[derive(Debug, Deserialize)]
struct GraphqlPrNode {
    number: u32,
    title: String,
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    url: String,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(rename = "latestReviews")]
    latest_reviews: Option<GraphqlReviews>,
    commits: GraphqlCommits,
}

#[derive(Debug, Deserialize)]
struct GraphqlReviews {
    nodes: Vec<GraphqlReviewNode>,
}

#[derive(Debug, Deserialize)]
struct GraphqlReviewNode {
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphqlCommits {
    nodes: Vec<GraphqlCommitNode>,
}

#[derive(Debug, Deserialize)]
struct GraphqlCommitNode {
    commit: GraphqlCommit,
}

#[derive(Debug, Deserialize)]
struct GraphqlCommit {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<GraphqlCheckRollup>,
}

#[derive(Debug, Deserialize)]
struct GraphqlCheckRollup {
    contexts: GraphqlCheckContexts,
}

#[derive(Debug, Deserialize)]
struct GraphqlCheckContexts {
    nodes: Vec<GraphqlCheckNode>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum GraphqlCheckNode {
    CheckRun {
        name: Option<String>,
        status: Option<String>,
        conclusion: Option<String>,
        #[serde(rename = "startedAt")]
        started_at: Option<String>,
    },
    StatusContext {
        context: Option<String>,
        state: Option<String>,
        #[serde(rename = "createdAt")]
        created_at: Option<String>,
    },
}

impl GraphqlCheckNode {
    fn to_rollup_item(&self) -> CheckRollupItem {
        match self {
            GraphqlCheckNode::CheckRun {
                name,
                status,
                conclusion,
                started_at,
            } => CheckRollupItem {
                status: status.clone(),
                conclusion: conclusion.clone(),
                name: name.clone(),
                started_at: started_at.clone(),
            },
            GraphqlCheckNode::StatusContext {
                context,
                state,
                created_at,
            } => CheckRollupItem {
                status: state.clone(),
                conclusion: None,
                name: context.clone(),
                started_at: created_at.clone(),
            },
        }
    }
}

// --- Public API ---

pub fn list_prs_for_branches(
    repo_root: &Path,
    branches: &[String],
) -> Result<HashMap<String, PrSummary>> {
    if branches.is_empty() {
        return Ok(HashMap::new());
    }

    match list_prs_for_branches_graphql(repo_root, branches) {
        Ok(map) => Ok(map),
        Err(e) => {
            debug!("github:graphql batch failed, falling back to per-branch REST: {e}");
            list_prs_for_branches_rest(repo_root, branches)
        }
    }
}

// --- Internals ---

fn get_repo_context(repo_root: &Path) -> Result<(String, String, String)> {
    let output = Command::new("gh")
        .current_dir(repo_root)
        .args(["repo", "view", "--json", "owner,name,url"])
        .output()
        .context("Failed to run gh repo view")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("gh repo view failed: {stderr}"));
    }

    let ctx: RepoContext =
        serde_json::from_slice(&output.stdout).context("Failed to parse gh repo view output")?;

    let hostname = ctx
        .url
        .strip_prefix("https://")
        .or_else(|| ctx.url.strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("github.com")
        .to_string();

    Ok((ctx.owner.login, ctx.name, hostname))
}

fn branch_to_alias(index: usize, branch: &str) -> String {
    let sanitized: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("br{}_{}", index, sanitized)
}

fn build_branch_fragment(alias: &str, branch: &str) -> String {
    let escaped = branch.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"    {alias}: pullRequests(headRefName: "{escaped}", first: 1, states: [OPEN, MERGED, CLOSED], orderBy: {{field: CREATED_AT, direction: DESC}}) {{
      nodes {{
        number title state isDraft headRefName url reviewDecision
        latestReviews(first: 10) {{ nodes {{ state }} }}
        commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 100) {{
          nodes {{ __typename ... on CheckRun {{ name status conclusion startedAt }} ... on StatusContext {{ context state createdAt }} }}
        }} }} }} }} }}
      }}
    }}"#
    )
}

fn list_prs_for_branches_graphql(
    repo_root: &Path,
    branches: &[String],
) -> Result<HashMap<String, PrSummary>> {
    let (owner, repo_name, hostname) = get_repo_context(repo_root)?;

    let fragments: Vec<String> = branches
        .iter()
        .enumerate()
        .map(|(i, branch)| {
            let alias = branch_to_alias(i, branch);
            build_branch_fragment(&alias, branch)
        })
        .collect();

    let query = format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{\n{}\n  }} }}",
        fragments.join("\n")
    );

    let body = serde_json::to_vec(&serde_json::json!({
        "query": query,
        "variables": {
            "owner": owner,
            "name": repo_name,
        }
    }))
    .context("JSON serialize")?;

    let mut child = Command::new("gh")
        .current_dir(repo_root)
        .args(["api", "graphql", "--hostname", &hostname, "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn gh api graphql")?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&body)
        .context("Failed to write to gh stdin")?;

    let output = child
        .wait_with_output()
        .context("Failed to wait for gh api graphql")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("gh api graphql failed: {stderr}"));
    }

    let response: GraphqlResponse =
        serde_json::from_slice(&output.stdout).context("Failed to parse GraphQL response")?;

    if let Some(errors) = &response.errors {
        if !errors.is_empty() {
            let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
            return Err(anyhow!("GraphQL errors: {}", msgs.join("; ")));
        }
    }

    let data = response
        .data
        .ok_or_else(|| anyhow!("No data in GraphQL response"))?;
    let repo = data.repository;

    let mut map = HashMap::new();
    for (_alias, connection) in repo {
        for node in connection.nodes {
            let check_items: Vec<CheckRollupItem> = node
                .commits
                .nodes
                .first()
                .and_then(|c| c.commit.status_check_rollup.as_ref())
                .map(|rollup| {
                    rollup
                        .contexts
                        .nodes
                        .iter()
                        .map(|n| n.to_rollup_item())
                        .collect()
                })
                .unwrap_or_default();

            let (checks, check_meta) = aggregate_checks(&check_items);
            let review = compute_review(
                node.review_decision.as_deref(),
                node.latest_reviews.as_ref(),
            );
            map.insert(
                node.head_ref_name,
                PrSummary {
                    number: node.number,
                    title: node.title,
                    state: node.state,
                    is_draft: node.is_draft,
                    checks,
                    check_meta,
                    url: Some(node.url),
                    review,
                },
            );
        }
    }

    Ok(map)
}

fn list_prs_for_branches_rest(
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

fn parse_github_timestamp(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 20 || !s.ends_with('Z') {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year: u64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;
    let hour: u64 = s[11..13].parse().ok()?;
    let min: u64 = s[14..16].parse().ok()?;
    let sec: u64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }

    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[(m - 1) as usize] as u64;
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap_year(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn compute_review(
    decision: Option<&str>,
    latest_reviews: Option<&GraphqlReviews>,
) -> Option<ReviewDecision> {
    match decision {
        Some("APPROVED") => return Some(ReviewDecision::Approved),
        Some("CHANGES_REQUESTED") => return Some(ReviewDecision::ChangesRequested),
        _ => {}
    }

    if let Some(reviews) = latest_reviews {
        let mut has_approved = false;
        let mut has_changes = false;
        let mut has_commented = false;

        for r in &reviews.nodes {
            match r.state.as_deref() {
                Some("APPROVED") => has_approved = true,
                Some("CHANGES_REQUESTED") => has_changes = true,
                Some("COMMENTED" | "DISMISSED") => has_commented = true,
                _ => {}
            }
        }

        if has_changes {
            return Some(ReviewDecision::ChangesRequested);
        }
        if has_approved {
            return Some(ReviewDecision::Approved);
        }
        if has_commented {
            return Some(ReviewDecision::Commented);
        }
    }

    match decision {
        Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

fn parse_review_decision(s: Option<&str>) -> Option<ReviewDecision> {
    match s? {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

fn aggregate_checks(checks: &[CheckRollupItem]) -> (Option<CheckState>, Option<CheckMeta>) {
    if checks.is_empty() {
        return (None, None);
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut pending = 0u32;
    let mut skipped = 0u32;
    let mut earliest_pending_start: Option<u64> = None;
    let mut earliest_any_start: Option<u64> = None;
    let mut latest_any_start: Option<u64> = None;
    let mut failing_name: Option<String> = None;

    for check in checks {
        let status = check.status.as_deref().unwrap_or("");
        let conclusion = check.conclusion.as_deref().unwrap_or("");
        let ts = check.started_at.as_deref().and_then(parse_github_timestamp);

        if let Some(t) = ts {
            earliest_any_start = Some(earliest_any_start.map_or(t, |prev: u64| prev.min(t)));
            latest_any_start = Some(latest_any_start.map_or(t, |prev: u64| prev.max(t)));
        }

        match (status, conclusion) {
            (_, "SUCCESS") | ("SUCCESS", _) => passed += 1,
            (_, "FAILURE" | "CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED")
            | ("FAILURE" | "ERROR", _) => {
                failed += 1;
                if failing_name.is_none() {
                    failing_name = check.name.clone();
                }
            }
            (_, "NEUTRAL" | "SKIPPED") => skipped += 1,
            ("IN_PROGRESS" | "QUEUED" | "PENDING" | "REQUESTED" | "WAITING", _) => {
                pending += 1;
                if let Some(t) = ts {
                    earliest_pending_start =
                        Some(earliest_pending_start.map_or(t, |prev: u64| prev.min(t)));
                }
            }
            _ => {}
        }
    }

    let total = passed + failed + pending;

    if total == 0 {
        return if skipped > 0 {
            (Some(CheckState::Success), None)
        } else {
            (None, None)
        };
    }

    let state = if failed > 0 {
        CheckState::Failure { passed, total }
    } else if pending > 0 {
        CheckState::Pending { passed, total }
    } else {
        CheckState::Success
    };

    let meta = if pending > 0 {
        let started = earliest_pending_start.or(earliest_any_start);
        if started.is_some() || failing_name.is_some() {
            Some(CheckMeta {
                started_at: started,
                duration_secs: None,
                failing_name,
            })
        } else {
            None
        }
    } else if failed > 0 {
        let duration_secs = match (earliest_any_start, current_unix_timestamp()) {
            (Some(start), Some(now)) => Some(now.saturating_sub(start)),
            _ => None,
        };
        if failing_name.is_some() || duration_secs.is_some() {
            Some(CheckMeta {
                started_at: earliest_any_start,
                duration_secs,
                failing_name,
            })
        } else {
            None
        }
    } else {
        None
    };

    (Some(state), meta)
}

fn current_unix_timestamp() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_item(status: Option<&str>, conclusion: Option<&str>) -> CheckRollupItem {
        CheckRollupItem {
            status: status.map(String::from),
            conclusion: conclusion.map(String::from),
            name: None,
            started_at: None,
        }
    }

    #[test]
    fn aggregate_checks_empty() {
        assert_eq!(aggregate_checks(&[]).0, None);
    }

    #[test]
    fn aggregate_checks_all_success() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SUCCESS")),
            check_item(Some("COMPLETED"), Some("SUCCESS")),
        ];
        assert_eq!(aggregate_checks(&checks).0, Some(CheckState::Success));
    }

    #[test]
    fn aggregate_checks_with_failure() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SUCCESS")),
            check_item(Some("COMPLETED"), Some("FAILURE")),
        ];
        assert_eq!(
            aggregate_checks(&checks).0,
            Some(CheckState::Failure {
                passed: 1,
                total: 2
            })
        );
    }

    #[test]
    fn aggregate_checks_with_pending() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SUCCESS")),
            check_item(Some("IN_PROGRESS"), None),
        ];
        assert_eq!(
            aggregate_checks(&checks).0,
            Some(CheckState::Pending {
                passed: 1,
                total: 2
            })
        );
    }

    #[test]
    fn aggregate_checks_failure_takes_priority_over_pending() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SUCCESS")),
            check_item(Some("COMPLETED"), Some("FAILURE")),
            check_item(Some("IN_PROGRESS"), None),
        ];
        assert_eq!(
            aggregate_checks(&checks).0,
            Some(CheckState::Failure {
                passed: 1,
                total: 3
            })
        );
    }

    #[test]
    fn aggregate_checks_all_skipped_returns_success() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SKIPPED")),
            check_item(Some("COMPLETED"), Some("NEUTRAL")),
        ];
        assert_eq!(aggregate_checks(&checks).0, Some(CheckState::Success));
    }

    #[test]
    fn aggregate_checks_skipped_not_counted_in_total() {
        let checks = vec![
            check_item(Some("COMPLETED"), Some("SUCCESS")),
            check_item(Some("COMPLETED"), Some("SKIPPED")),
            check_item(Some("IN_PROGRESS"), None),
        ];
        assert_eq!(
            aggregate_checks(&checks).0,
            Some(CheckState::Pending {
                passed: 1,
                total: 2
            })
        );
    }

    #[test]
    fn aggregate_checks_captures_failing_name() {
        let checks = vec![
            CheckRollupItem {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
                name: Some("build".into()),
                started_at: None,
            },
            CheckRollupItem {
                status: Some("COMPLETED".into()),
                conclusion: Some("FAILURE".into()),
                name: Some("lint-check".into()),
                started_at: None,
            },
        ];
        let (state, meta) = aggregate_checks(&checks);
        assert_eq!(
            state,
            Some(CheckState::Failure {
                passed: 1,
                total: 2
            })
        );
        assert_eq!(
            meta.as_ref().and_then(|m| m.failing_name.as_deref()),
            Some("lint-check")
        );
    }

    #[test]
    fn branch_to_alias_sanitizes_special_chars() {
        assert_eq!(
            branch_to_alias(0, "my-feature-branch"),
            "br0_my_feature_branch"
        );
        assert_eq!(branch_to_alias(3, "feat/add-thing"), "br3_feat_add_thing");
    }

    #[test]
    fn branch_to_alias_index_prevents_collisions() {
        let a1 = branch_to_alias(0, "a-b");
        let a2 = branch_to_alias(1, "a_b");
        assert_ne!(a1, a2);
    }

    #[test]
    fn parse_github_timestamp_valid() {
        assert_eq!(
            parse_github_timestamp("2026-03-24T14:02:00Z"),
            Some(1774360920)
        );
        assert_eq!(parse_github_timestamp("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parse_github_timestamp_invalid() {
        assert_eq!(parse_github_timestamp(""), None);
        assert_eq!(parse_github_timestamp("not a date"), None);
        assert_eq!(parse_github_timestamp("2026-13-01T00:00:00Z"), None);
    }
}
