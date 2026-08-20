//! Runs an external editor on a temp file and reads the result back.
//!
//! This deliberately doesn't touch the terminal itself — suspending raw
//! mode/the alternate screen around the editor process is main.rs's job,
//! since it's the one holding the `Terminal` handle. Keeping that concern
//! out of here means this is fully testable without a real TTY.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Opens `$VISUAL` (falling back to `$EDITOR`, then `vi`) on a temp file
/// seeded with `initial`, waits for it to exit, and returns the file's
/// final content.
pub fn edit_text(initial: &str) -> Result<String, String> {
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".to_string());
    edit_text_with(&editor, initial)
}

fn edit_text_with(editor: &str, initial: &str) -> Result<String, String> {
    // A `NamedTempFile` (via the `tempfile` crate), not a hand-built path —
    // created with a random suffix and O_EXCL rather than a PID-based name,
    // so it can't be pre-planted as a symlink by another local user, and
    // it's 0600 from the moment it exists. Dropping it removes the file, so
    // there's no separate cleanup step even on an early error return.
    let mut file = tempfile::Builder::new().prefix("hoot-editor-").suffix(".txt").tempfile().map_err(|e| e.to_string())?;
    file.write_all(initial.as_bytes()).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;

    run_and_read(editor, file.path())
}

fn run_and_read(editor: &str, path: &Path) -> Result<String, String> {
    // $EDITOR/$VISUAL commonly carries arguments too (e.g. "code --wait"),
    // which a real shell would word-split before exec-ing — Command::new
    // doesn't do that, so a value like that would otherwise fail outright
    // (there's no binary literally named "code --wait"). This is
    // whitespace-splitting, not real shell parsing (no quoting/escaping
    // support), which matches what editor values realistically need.
    let mut parts = editor.split_whitespace();
    let Some(program) = parts.next() else {
        return Err("$EDITOR/$VISUAL is empty".to_string());
    };
    let args: Vec<&str> = parts.collect();

    // Exit status isn't a reliable signal here — real editors (vim in
    // particular) can return nonzero after a perfectly good save for all
    // sorts of benign reasons. Whatever ended up on disk is the source of
    // truth, so always read it back rather than discarding the user's edit
    // over an exit code.
    match Command::new(program).args(&args).arg(path).status() {
        Ok(_) => std::fs::read_to_string(path).map_err(|e| e.to_string()),
        Err(e) => Err(format!("couldn't launch {editor}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Writes a throwaway `#!/bin/sh` script standing in for a real editor,
    /// so tests never depend on $EDITOR or an actual interactive TTY.
    fn script(body: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("hoot-editor-test-script-{}-{suffix}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh\n{body}").unwrap();
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn returns_edited_content_on_success() {
        let editor = script(r#"printf 'edited content' > "$1""#);
        let result = edit_text_with(editor.to_str().unwrap(), "original").unwrap();
        assert_eq!(result, "edited content");
        let _ = std::fs::remove_file(&editor);
    }

    #[test]
    fn unchanged_file_returns_the_original_content() {
        // A real system binary that exits 0 without touching its args.
        let result = edit_text_with("true", "unchanged").unwrap();
        assert_eq!(result, "unchanged");
    }

    #[test]
    fn nonzero_exit_still_returns_whatever_is_on_disk() {
        // `false` never touches the file, but a nonzero exit shouldn't turn
        // that into an error — it should just report the file unchanged.
        let result = edit_text_with("false", "whatever").unwrap();
        assert_eq!(result, "whatever");
    }

    #[test]
    fn nonzero_exit_after_a_real_save_keeps_the_edit() {
        // Simulates an editor (e.g. vim) that saves successfully but still
        // exits nonzero for unrelated reasons — the save must not be lost.
        let editor = script(r#"printf 'edited content' > "$1"; exit 1"#);
        let result = edit_text_with(editor.to_str().unwrap(), "original").unwrap();
        assert_eq!(result, "edited content");
        let _ = std::fs::remove_file(&editor);
    }

    #[test]
    fn editor_value_with_arguments_is_word_split_like_a_real_shell_would() {
        // "$EDITOR=code --wait" is a common real-world value. Command::new
        // alone would treat "code --wait" as one (nonexistent) binary name
        // and fail outright — confirm the argument actually reaches the
        // program, and the file path still lands after it.
        let editor = script(r#"[ "$1" = "--flag" ] && printf 'saw the flag' > "$2""#);
        let editor_value = format!("{} --flag", editor.to_str().unwrap());
        let result = edit_text_with(&editor_value, "original").unwrap();
        assert_eq!(result, "saw the flag");
        let _ = std::fs::remove_file(&editor);
    }

    #[test]
    fn missing_editor_binary_is_an_error() {
        let err = edit_text_with("hoot-definitely-not-a-real-binary-xyz", "whatever").unwrap_err();
        assert!(err.contains("couldn't launch"), "{err}");
    }
}
