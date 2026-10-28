//! Opening the glossary in a text editor, against stub scripts in place of
//! macOS's `open`.

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use scriber_engine::{OPEN, TextEditor};
    use tempfile::TempDir;

    /// An executable shell script at `dir/name` running `body`.
    fn stub(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A stub that writes its arguments, one per line, to `argv.txt` in
    /// `dir`, then runs `then`.
    fn logging(dir: &TempDir, then: &str) -> (PathBuf, PathBuf) {
        let log = dir.path().join("argv.txt");
        let body = format!(
            "for a in \"$@\"; do echo \"$a\"; done > '{}'\n{then}",
            log.display()
        );
        (stub(dir, "open", &body), log)
    }

    #[test]
    fn the_arguments_are_pinned() {
        assert_eq!(OPEN, "/usr/bin/open", "the binary");
        let want: Vec<OsString> = ["-t", "/d/v.txt"].into_iter().map(OsString::from).collect();
        assert_eq!(TextEditor::args(Path::new("/d/v.txt")), want, "as text");
    }

    #[test]
    fn a_missing_file_is_created_empty_in_a_new_folder_then_opened() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("new/folder/vocabulary.txt");
        let check = format!("test -f '{}' || exit 9", file.display());
        let (binary, log) = logging(&dir, &check);
        TextEditor::new(binary).open(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"", "created empty");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("-t\n{}\n", file.display()),
            "-t <path>"
        );
    }

    #[test]
    fn an_existing_file_is_kept() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("vocabulary.txt");
        std::fs::write(&file, "keep me").unwrap();
        let (binary, _log) = logging(&dir, "");
        TextEditor::new(binary).open(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me", "kept");
    }

    #[test]
    fn a_path_with_no_folder_above_it_is_opened_as_it_is() {
        let dir = TempDir::new().unwrap();
        let (binary, log) = logging(&dir, "");
        TextEditor::new(binary).open(Path::new("/")).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "-t\n/\n", "-t /");
    }

    #[test]
    fn a_failing_editor_is_an_error_and_the_new_file_stays() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("vocabulary.txt");
        let binary = stub(&dir, "open", "echo boom >&2\nexit 3");
        let err = TextEditor::new(binary).open(&file).unwrap_err();
        assert!(err.0.contains("boom"), "{err:?}");
        assert_eq!(std::fs::read(&file).unwrap(), b"", "left in place");
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let dir = TempDir::new().unwrap();
        let editor = TextEditor::new(dir.path().join("no-such-open"));
        assert!(
            editor.open(&dir.path().join("vocabulary.txt")).is_err(),
            "missing"
        );
    }

    #[test]
    fn a_folder_that_cannot_be_made_is_an_error_and_nothing_runs() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("plain"), "a file").unwrap();
        let (binary, log) = logging(&dir, "");
        let file = dir.path().join("plain/vocabulary.txt");
        assert!(TextEditor::new(binary).open(&file).is_err(), "an error");
        assert!(!log.exists(), "the editor never ran");
    }

    #[test]
    fn a_file_that_cannot_be_made_is_an_error_and_nothing_runs() {
        let dir = TempDir::new().unwrap();
        let (binary, log) = logging(&dir, "");
        let file = dir.path().join("x".repeat(300));
        assert!(TextEditor::new(binary).open(&file).is_err(), "an error");
        assert!(!log.exists(), "the editor never ran");
    }
}
