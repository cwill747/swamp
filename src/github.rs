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

#[derive(Default)]
pub struct PrFetch {
    pub prs: HashMap<String, PrSummary>,
    pub failed: Vec<String>,
}

pub fn list_prs_for_branches(repo_root: &Path, branches: &[String]) -> Result<PrFetch> {
    if branches.is_empty() {
        return Ok(PrFetch::default());
    }

    // Deduplicate branches before building queries (safe here; daemon/mod.rs
    // owns the branch list but dedup at the call site is cheap and correct).
    let mut deduped: Vec<String> = Vec::with_capacity(branches.len());
    for b in branches {
        if !deduped.contains(b) {
            deduped.push(b.clone());
        }
    }
    let branches = &deduped;

    match graphql::list_prs_for_branches(repo_root, branches) {
        Ok(prs) => Ok(PrFetch {
            prs,
            failed: Vec::new(),
        }),
        Err(e) if is_preflight_error(&e) => Err(e),
        Err(e) => {
            debug!("github:graphql batch failed, falling back to per-branch GraphQL: {e}");
            rest::list_prs_for_branches(repo_root, branches)
        }
    }
}

fn is_preflight_error(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("failed to spawn gh")
        || msg.contains("failed to run gh")
        || msg.contains("gh repo view failed")
        || msg.contains("gh api graphql failed")
            && (msg.contains("authentication")
                || msg.contains("authenticate")
                || msg.contains("not logged")
                || msg.contains("login")
                || msg.contains("unauthorized")
                || msg.contains("forbidden"))
}
