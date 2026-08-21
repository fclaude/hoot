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
//! turn after that — except under `--demo`, which shows the prompt but
//! never writes the marker, so a throwaway repo can't grant standing
//! permission over the user's real ones. See `acknowledge_at`.
//!
//! Tracked separately per backend, not one flag for "Agent" as a whole:
//! `--approve` (pi) and `--auto` (opencode) are genuinely different trust
//! grants (see the doc comments referenced above) — acknowledging one
//! backend's prompt shouldn't silently wave through the other's the first
//! time someone switches `--agent`.

use std::path::{Path, PathBuf};

use crate::agent_client::AgentBackend;

fn marker_path(home: &Path, backend: AgentBackend) -> PathBuf {
    home.join(format!(".hoot-agent-trust-ack-{}", backend.label()))
}

/// True once the user has confirmed `backend`'s trust prompt at least once
/// on this machine. A missing `$HOME` (unusual, but not impossible) is
/// treated as "not yet acknowledged" rather than erroring the whole app
/// over a one-time confirmation dialog — worst case, the prompt shows
/// again.
pub fn is_acknowledged(backend: AgentBackend) -> bool {
    let Some(home) = std::env::var_os("HOME") else { return false };
    is_acknowledged_at(Path::new(&home), backend)
}

fn is_acknowledged_at(home: &Path, backend: AgentBackend) -> bool {
    marker_path(home, backend).exists()
}

/// Records that the user has confirmed `backend`'s trust prompt, so it
/// won't show again for that backend. Best-effort: if `$HOME` isn't set or
/// the file can't be written, the prompt just shows again next launch —
/// annoying, not unsafe, so errors here are silently swallowed rather than
/// surfaced.
///
/// A `--demo` run never records anything — see `acknowledge_at`.
pub fn acknowledge(backend: AgentBackend, demo: bool) {
    let Some(home) = std::env::var_os("HOME") else { return };
    acknowledge_at(Path::new(&home), backend, demo);
}

fn acknowledge_at(home: &Path, backend: AgentBackend, demo: bool) {
    // The demo gate lives here rather than at the call site so a second
    // call site can't be added that forgets it.
    //
    // `--demo` exists to be poked at on a throwaway repo that gets deleted
    // on exit, and the marker file is the one thing a demo run could leave
    // behind that outlives it. Worse than the litter is what it means:
    // this file is a persistent, broad trust grant, and it isn't scoped to
    // the repo it was given in. Accepting the prompt while exploring a
    // temp directory would silently waive it for the user's own
    // repositories the first time they ran hoot for real — consent
    // collected under one set of stakes, spent under a much larger one.
    // The prompt still shows (and still gates the turn) in demo; only the
    // remembering is skipped, so a demo run costs at most a re-confirm.
    if demo {
        return;
    }
    let _ = std::fs::write(marker_path(home, backend), "acknowledged\n");
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
        assert!(!is_acknowledged_at(&dir, AgentBackend::OpenCode));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acknowledging_persists_across_checks() {
        let dir = scratch_dir("ack");
        assert!(!is_acknowledged_at(&dir, AgentBackend::Pi));
        acknowledge_at(&dir, AgentBackend::Pi, false);
        assert!(is_acknowledged_at(&dir, AgentBackend::Pi));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_demo_run_never_records_a_trust_grant() {
        // Confirming the prompt on a throwaway repo must not waive it for
        // the user's real ones: the marker isn't scoped to the repo it was
        // granted in, so persisting it here would spend consent collected
        // under demo stakes on everything afterwards.
        let dir = scratch_dir("demo");
        acknowledge_at(&dir, AgentBackend::Pi, true);
        assert!(!is_acknowledged_at(&dir, AgentBackend::Pi), "a demo run must leave nothing behind");

        // And the same call outside demo still does record it.
        acknowledge_at(&dir, AgentBackend::Pi, false);
        assert!(is_acknowledged_at(&dir, AgentBackend::Pi));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acknowledging_one_backend_does_not_acknowledge_the_other() {
        // pi's --approve and opencode's --auto are different trust grants
        // — confirming one shouldn't silently wave the other through the
        // first time someone switches --agent.
        let dir = scratch_dir("per-backend");
        acknowledge_at(&dir, AgentBackend::OpenCode, false);
        assert!(is_acknowledged_at(&dir, AgentBackend::OpenCode));
        assert!(!is_acknowledged_at(&dir, AgentBackend::Pi));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
