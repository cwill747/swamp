use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

mod checks;
mod context;
mod graphql;
mod rest;
mod types;

#[allow(unused_imports)]
pub use types::CheckMeta;
pub use types::{CheckState, PrSummary, ReviewDecision};

pub fn list_prs_for_branches(
    repo_root: &Path,
    branches: &[String],
) -> Result<HashMap<String, PrSummary>> {
    if branches.is_empty() {
        return Ok(HashMap::new());
    }

    match graphql::list_prs_for_branches(repo_root, branches) {
        Ok(map) => Ok(map),
        Err(e) => {
            debug!("github:graphql batch failed, falling back to per-branch REST: {e}");
            rest::list_prs_for_branches(repo_root, branches)
        }
    }
}
