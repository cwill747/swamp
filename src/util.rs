use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn unix_to_systemtime(ts: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(ts)
}

/// Compact age string: "<1m", "5m", "1h", "2d", "1w", "1mo", "1y+".
pub fn format_compact_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        "<1m".into()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else if s < 7 * 86_400 {
        format!("{}d", s / 86_400)
    } else if s < 30 * 86_400 {
        format!("{}w", s / (7 * 86_400))
    } else if s < 365 * 86_400 {
        format!("{}mo", s / (30 * 86_400))
    } else {
        format!("{}y", s / (365 * 86_400))
    }
}

/// Stable short id for a repo path (used in socket filenames).
pub fn repo_id(path: &Path) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn ascii_mode() -> bool {
    std::env::var("SWAMP_ASCII").is_ok()
        || !std::env::var("LANG")
            .unwrap_or_default()
            .to_lowercase()
            .contains("utf")
}
