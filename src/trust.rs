//! First-run consent for letting Agent spawn a real `pi`/`opencode`
//! subprocess at all.
//!
//! The subprocess runs with auto-approved tool calls (opencode's `--auto`)
//! or, for `pi`, `--approve` (real code execution from repo-local files on
//! every turn) — see the README's Agent bullet and `pi_client`'s module
//! doc comment for exactly what that means. That's a real, broad trust
//! grant, not something to hand out silently the first time someone tries
//! the Agent screen. A marker file under `$HOME` remembers that the prompt
//! has already been shown and confirmed once, so it doesn't nag on every
//! turn after that.

use std::path::{Path, PathBuf};

fn marker_path(home: &Path) -> PathBuf {
    home.join(".hoot-agent-trust-ack")
}

/// True once the user has confirmed the trust prompt at least once on this
/// machine. A missing `$HOME` (unusual, but not impossible) is treated as
/// "not yet acknowledged" rather than erroring the whole app over a
/// one-time confirmation dialog — worst case, the prompt shows again.
pub fn is_acknowledged() -> bool {
    let Some(home) = std::env::var_os("HOME") else { return false };
    is_acknowledged_at(Path::new(&home))
}

fn is_acknowledged_at(home: &Path) -> bool {
    marker_path(home).exists()
}

/// Records that the user has confirmed the trust prompt, so it won't show
/// again. Best-effort: if `$HOME` isn't set or the file can't be written,
/// the prompt just shows again next launch — annoying, not unsafe, so
/// errors here are silently swallowed rather than surfaced.
pub fn acknowledge() {
    let Some(home) = std::env::var_os("HOME") else { return };
    acknowledge_at(Path::new(&home));
}

fn acknowledge_at(home: &Path) {
    let _ = std::fs::write(marker_path(home), "acknowledged\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-trust-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unacknowledged_by_default() {
        let dir = scratch_dir("missing");
        assert!(!is_acknowledged_at(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acknowledging_persists_across_checks() {
        let dir = scratch_dir("ack");
        assert!(!is_acknowledged_at(&dir));
        acknowledge_at(&dir);
        assert!(is_acknowledged_at(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
