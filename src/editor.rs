//! Runs an external editor on a temp file and reads the result back.
//!
//! This deliberately doesn't touch the terminal itself — suspending raw
//! mode/the alternate screen around the editor process is main.rs's job,
//! since it's the one holding the `Terminal` handle. Keeping that concern
//! out of here means this is fully testable without a real TTY.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Opens `$VISUAL` (falling back to `$EDITOR`, then `vi`) on a temp file
/// seeded with `initial`, waits for it to exit, and returns the file's
/// final content.
pub fn edit_text(initial: &str) -> Result<String, String> {
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".to_string());
    edit_text_with(&editor, initial)
}

fn edit_text_with(editor: &str, initial: &str) -> Result<String, String> {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("steer-editor-{}-{suffix}.txt", std::process::id()));
    std::fs::write(&path, initial).map_err(|e| e.to_string())?;

    let result = run_and_read(editor, &path);

    let _ = std::fs::remove_file(&path);
    result
}

fn run_and_read(editor: &str, path: &Path) -> Result<String, String> {
    let status = Command::new(editor).arg(path).status();
    match status {
        Ok(s) if s.success() => std::fs::read_to_string(path).map_err(|e| e.to_string()),
        Ok(s) => Err(format!("{editor} exited with {s}")),
        Err(e) => Err(format!("couldn't launch {editor}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// Writes a throwaway `#!/bin/sh` script standing in for a real editor,
    /// so tests never depend on $EDITOR or an actual interactive TTY.
    fn script(body: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("steer-editor-test-script-{}-{suffix}", std::process::id()));
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
    fn nonzero_exit_is_an_error() {
        let err = edit_text_with("false", "whatever").unwrap_err();
        assert!(err.contains("exited"), "{err}");
    }

    #[test]
    fn missing_editor_binary_is_an_error() {
        let err = edit_text_with("steer-definitely-not-a-real-binary-xyz", "whatever").unwrap_err();
        assert!(err.contains("couldn't launch"), "{err}");
    }

}
