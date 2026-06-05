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

/// Open a URL in the user's default browser. Best-effort: spawns the platform
/// opener detached and ignores failures (we're inside a TUI; there's nowhere
/// useful to surface the error).
pub fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Copy `text` to the clipboard via an OSC 52 escape sequence. Because this
/// rides the terminal protocol rather than a local opener, it reaches the
/// user's own clipboard even across SSH and multiplexers (as long as they pass
/// OSC 52 through). Best-effort: a terminal that ignores OSC 52 is a no-op.
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = b0 << 16 | b1 << 8 | b2;
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            base64_encode(b"https://github.com/cwill747/swamp/pull/52"),
            "aHR0cHM6Ly9naXRodWIuY29tL2N3aWxsNzQ3L3N3YW1wL3B1bGwvNTI="
        );
    }
}
