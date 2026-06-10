/// Per-branch GraphQL fallback.
///
/// When the batch GraphQL query fails (e.g. network glitch, field-level
/// GraphQL error for one alias), this module re-issues the same GraphQL
/// query but for a single branch at a time, so one bad branch does not
/// silence the others.  It reuses `graphql::list_prs_for_branches` directly
/// — one call per branch — and therefore goes through the *same* query
/// fragment builder and response parser.  The bespoke `gh pr list --json`
/// path (with its own struct definitions and schema drift) is gone.
///
/// Error policy:
/// - If every branch call fails, we return `Err` so the caller can keep the
///   previous snapshot rather than wiping it.
/// - Individual branch failures are logged at `warn` and skipped so the
///   returned map is as complete as possible.
use super::graphql;
use super::types::PrSummary;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

pub(super) fn list_prs_for_branches(
    repo_root: &Path,
    branches: &[String],
) -> Result<HashMap<String, PrSummary>> {
    let mut map: HashMap<String, PrSummary> = HashMap::new();
    let mut error_count = 0usize;

    for branch in branches {
        match graphql::list_prs_for_branches(repo_root, std::slice::from_ref(branch)) {
            Ok(partial) => map.extend(partial),
            Err(e) => {
                warn!("github:rest per-branch fallback failed for {branch:?}: {e}");
                error_count += 1;
            }
        }
    }

    if error_count == branches.len() && !branches.is_empty() {
        return Err(anyhow!(
            "all {} per-branch fallback queries failed",
            branches.len()
        ));
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    /// Smoke-test that the module compiles and the public surface is intact.
    /// End-to-end integration tests require a live `gh` binary and are left to
    /// manual / CI runs that have GitHub auth.
    #[test]
    fn module_exists() {
        // If this compiles, the rewrite did not break the module boundary.
    }
}
