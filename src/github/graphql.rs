use super::checks::{CheckRollupItem, aggregate_checks};
use super::context::get_repo_context;
use super::types::{PrSummary, ReviewDecision};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

pub(super) fn list_prs_for_branches(
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

    if let Some(errors) = &response.errors
        && !errors.is_empty()
    {
        let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(anyhow!("GraphQL errors: {}", msgs.join("; ")));
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
