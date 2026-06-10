use anyhow::{Context, Result, bail};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Return a private per-user directory suitable for sockets, pid files, and
/// logs. The directory is created with mode 0700, and a pre-existing entry is
/// verified to be a real directory owned by the current user.
///
/// Resolution order (first match wins):
/// 1. `$XDG_RUNTIME_DIR/swamp`
/// 2. `$XDG_CACHE_HOME/swamp/run`
/// 3. `$HOME/.cache/swamp/run`
///
/// The shared `/tmp` fallback is intentionally omitted: it is world-writable
/// and therefore unsafe for sockets that accept destructive operations.
pub fn runtime_base_dir() -> Result<PathBuf> {
    let candidate = if let Some(v) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(v).join("swamp")
    } else if let Some(v) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(v).join("swamp").join("run")
    } else if let Some(v) = std::env::var_os("HOME") {
        PathBuf::from(v).join(".cache").join("swamp").join("run")
    } else {
        bail!(
            "cannot determine a safe runtime directory: \
             neither XDG_RUNTIME_DIR, XDG_CACHE_HOME, nor HOME is set. \
             Set at least one of these environment variables."
        );
    };

    // Create with mode 0700 if absent.
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&candidate)
        .with_context(|| format!("create runtime dir {}", candidate.display()))?;

    // Verify the final path: must be a real directory owned by us.
    let our_uid = unsafe { libc::getuid() };
    let meta = std::fs::symlink_metadata(&candidate)
        .with_context(|| format!("stat runtime dir {}", candidate.display()))?;
    if meta.is_symlink() {
        bail!(
            "runtime directory {} is a symlink; refusing to use it",
            candidate.display()
        );
    }
    if !meta.is_dir() {
        bail!(
            "runtime directory {} exists but is not a directory",
            candidate.display()
        );
    }
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != our_uid {
        bail!(
            "runtime directory {} is owned by uid {} but we are uid {}; \
             refusing to use it",
            candidate.display(),
            meta.uid(),
            our_uid
        );
    }

    // Tighten permissions to 0700 if the directory was pre-existing and
    // somehow ended up more permissive.
    let current_mode = meta.mode() & 0o777;
    if current_mode != 0o700 {
        std::fs::set_permissions(
            &candidate,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .with_context(|| format!("tighten permissions on runtime dir {}", candidate.display()))?;
    }

    Ok(candidate)
}

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
    use super::*;

    // Serializes tests that mutate process-global environment variables.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn runtime_base_dir_uses_xdg_runtime_dir() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "swamp-rbd-test-xdg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &tmp);
            std::env::remove_var("XDG_CACHE_HOME");
        }

        let result = runtime_base_dir().unwrap();
        assert_eq!(result, tmp.join("swamp"));
        assert!(result.is_dir());

        // Verify mode 0700.
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&result).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700, "directory must be mode 0700");

        let _ = std::fs::remove_dir_all(&tmp);
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

    #[test]
    fn runtime_base_dir_falls_back_to_xdg_cache_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "swamp-rbd-test-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
            std::env::set_var("XDG_CACHE_HOME", &tmp);
        }

        let result = runtime_base_dir().unwrap();
        assert_eq!(result, tmp.join("swamp").join("run"));
        assert!(result.is_dir());

        let _ = std::fs::remove_dir_all(&tmp);
        unsafe {
            std::env::remove_var("XDG_CACHE_HOME");
        }
    }

    #[test]
    fn runtime_base_dir_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "swamp-rbd-test-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
            std::env::remove_var("XDG_CACHE_HOME");
            std::env::set_var("HOME", &tmp);
        }

        let result = runtime_base_dir().unwrap();
        assert_eq!(result, tmp.join(".cache").join("swamp").join("run"));
        assert!(result.is_dir());

        let _ = std::fs::remove_dir_all(&tmp);
        // Restore HOME to something reasonable — process can't function without it.
        unsafe {
            std::env::set_var("HOME", std::env::temp_dir());
        }
    }

    #[test]
    fn runtime_base_dir_rejects_symlink() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "swamp-rbd-test-sym-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Create the real target dir and a symlink pointing to it where we
        // expect the runtime dir.
        let real_target = tmp.join("real");
        std::fs::create_dir_all(&real_target).unwrap();
        let symlink_path = tmp.join("xdg_runtime");
        std::os::unix::fs::symlink(&real_target, &symlink_path).unwrap();

        // Point XDG_RUNTIME_DIR at a path whose child "swamp" we'll pre-create as a symlink.
        let xdg_base = tmp.join("xdg_base");
        std::fs::create_dir_all(&xdg_base).unwrap();
        let swamp_sym = xdg_base.join("swamp");
        std::os::unix::fs::symlink(&real_target, &swamp_sym).unwrap();

        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &xdg_base);
            std::env::remove_var("XDG_CACHE_HOME");
        }

        let err = runtime_base_dir().unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink error, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

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
