use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct RepositoryOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RepoContext {
    name: String,
    owner: RepositoryOwner,
    url: String,
}

pub(super) fn get_repo_context(repo_root: &Path) -> Result<(String, String, String)> {
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
