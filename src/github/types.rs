use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub reviews_partial: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}
