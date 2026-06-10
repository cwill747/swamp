use super::types::{CheckMeta, CheckState};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct CheckRollupItem {
    #[serde(alias = "state")]
    pub(super) status: Option<String>,
    pub(super) conclusion: Option<String>,
    // `context` is the key used by StatusContext items in the REST JSON schema.
    #[serde(default, alias = "context")]
    pub(super) name: Option<String>,
    // `startedAt` is the camelCase key emitted by `gh` REST responses.
    #[serde(default, alias = "startedAt")]
    pub(super) started_at: Option<String>,
}

pub(super) fn aggregate_checks(
    checks: &[CheckRollupItem],
) -> (Option<CheckState>, Option<CheckMeta>) {
    if checks.is_empty() {
        return (None, None);
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut pending = 0u32;
    let mut skipped = 0u32;
    let mut earliest_pending_start: Option<u64> = None;
    let mut earliest_any_start: Option<u64> = None;
    let mut failing_name: Option<String> = None;

    for check in checks {
        let status = check.status.as_deref().unwrap_or("");
        let conclusion = check.conclusion.as_deref().unwrap_or("");
        let ts = check.started_at.as_deref().and_then(parse_github_timestamp);

        if let Some(t) = ts {
            earliest_any_start = Some(earliest_any_start.map_or(t, |prev: u64| prev.min(t)));
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
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
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

    /// `startedAt` (camelCase) must deserialize into the `started_at` field so
    /// that REST JSON responses produce correct timestamps.
    #[test]
    fn check_rollup_item_deserializes_started_at_camel_case() {
        let json = r#"{"status":"IN_PROGRESS","startedAt":"2026-03-24T14:02:00Z"}"#;
        let item: CheckRollupItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.started_at.as_deref(), Some("2026-03-24T14:02:00Z"));
    }

    /// `context` (REST StatusContext key) must deserialize into the `name` field.
    #[test]
    fn check_rollup_item_deserializes_context_as_name() {
        let json = r#"{"state":"SUCCESS","context":"ci/my-check"}"#;
        let item: CheckRollupItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.name.as_deref(), Some("ci/my-check"));
        assert_eq!(item.status.as_deref(), Some("SUCCESS"));
    }
}
