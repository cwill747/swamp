use anyhow::{Context, Result, bail};
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
///
/// Uses FNV-1a 64-bit over the canonicalized path bytes. Canonicalization
/// means two symlinked paths to the same repo yield the same id; FNV-1a is
/// algorithm-stable across Rust versions (unlike `DefaultHasher`/SipHash).
/// Falls back to the raw path when canonicalization fails (e.g. path doesn't
/// exist yet in tests).
pub fn repo_id(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let bytes = canonical.as_os_str().as_encoded_bytes();
    // FNV-1a 64-bit: https://www.isthe.com/chongo/tech/comp/fnv/
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let hash = bytes
        .iter()
        .fold(FNV_OFFSET, |h, &b| (h ^ b as u64).wrapping_mul(FNV_PRIME));
    format!("{:016x}", hash)
}

/// Disambiguated Zellij session name for a repo.
///
/// Returns `{container_basename}-{first 4 hex chars of repo_id}` where
/// `container` is the directory holding the bare repo / `.git` dir.
/// The suffix prevents two repos with the same basename (e.g. `~/a/myproj`
/// and `~/work/myproj`) from mapping to the same session.
pub fn session_name_for(common_dir: &Path) -> String {
    // container = the directory that holds the bare repo or .git dir
    let container = common_dir.parent().unwrap_or(common_dir);
    let basename = container
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into());
    let id = repo_id(common_dir);
    format!("{}-{}", basename, &id[..4])
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

    /// repo_id must be stable: the same input always yields the same output.
    /// This is a known-answer test that pins the FNV-1a value so a future
    /// change to the algorithm is caught immediately rather than silently
    /// orphaning running daemons.
    ///
    /// The expected value was computed over the literal bytes of
    /// `/home/user/code/myrepo/.bare` using FNV-1a 64-bit.
    #[test]
    fn repo_id_stable_known_answer() {
        // Path that does NOT exist on disk, so canonicalize falls back to the
        // raw path — making this test portable and deterministic.
        let path = std::path::Path::new("/home/user/code/myrepo/.bare");
        let id = repo_id(path);
        // 16 hex characters (64-bit hash).
        assert_eq!(id.len(), 16, "repo_id must be 16 hex chars");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "repo_id must be hex: {id}"
        );
        // Known-answer: must not change across Rust versions.
        assert_eq!(
            id, "e21f11f192ce1f63",
            "repo_id changed — this would orphan running daemons"
        );
    }

    /// Two calls with the same path must return the same id.
    #[test]
    fn repo_id_deterministic() {
        let path = std::path::Path::new("/tmp/some/repo/.bare");
        assert_eq!(repo_id(path), repo_id(path));
    }

    /// Two different paths must (in practice) return different ids.
    #[test]
    fn repo_id_differs_for_different_paths() {
        let a = std::path::Path::new("/home/alice/myrepo/.bare");
        let b = std::path::Path::new("/home/bob/myrepo/.bare");
        assert_ne!(repo_id(a), repo_id(b));
    }

    /// session_name_for must return `{basename}-{4-hex-chars}`.
    #[test]
    fn session_name_format() {
        let path = std::path::Path::new("/home/user/code/myrepo/.bare");
        let name = session_name_for(path);
        // Format: `<container_basename>-<4 hex chars>`
        // container is parent of common_dir = /home/user/code/myrepo
        // basename = myrepo
        let (prefix, suffix) = name.split_once('-').expect("session name must contain '-'");
        assert_eq!(prefix, "myrepo");
        assert_eq!(suffix.len(), 4, "suffix must be 4 hex chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix must be hex: {suffix}"
        );
    }

    /// Two repos with the same basename but different paths yield different
    /// session names.
    #[test]
    fn session_name_disambiguates_same_basename() {
        let a = std::path::Path::new("/home/alice/myrepo/.bare");
        let b = std::path::Path::new("/home/bob/myrepo/.bare");
        let na = session_name_for(a);
        let nb = session_name_for(b);
        // Both start with "myrepo-" but the suffix differs.
        assert!(na.starts_with("myrepo-"), "a: {na}");
        assert!(nb.starts_with("myrepo-"), "b: {nb}");
        assert_ne!(
            na, nb,
            "same-basename repos must have different session names"
        );
    }
}
