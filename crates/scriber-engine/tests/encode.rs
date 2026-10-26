//! The afconvert encoder, against stub scripts in place of the binary.

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use scriber_engine::{AFCONVERT, Afconvert, Encoder as _};
    use tempfile::TempDir;

    /// An executable shell script at `dir/name` running `body`.
    fn stub(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_arguments_are_pinned() {
        assert_eq!(AFCONVERT, "/usr/bin/afconvert", "the binary");
        let want: Vec<OsString> = [
            "-f",
            "m4af",
            "-d",
            "aac",
            "-b",
            "32000",
            "/in/audio.wav",
            "/out/audio.m4a",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            Afconvert::args(Path::new("/in/audio.wav"), Path::new("/out/audio.m4a")),
            want,
            "32 kbit/s AAC in M4A"
        );
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let dir = TempDir::new().unwrap();
        let encoder = Afconvert::new(dir.path().join("no-such-afconvert"), dir.path().into());
        assert!(encoder.to_m4a("a.wav", "a.m4a").is_err(), "missing");
    }

    #[test]
    fn a_successful_run_passes_paths_under_the_root() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("argv.txt");
        let binary = stub(
            &dir,
            "afconvert",
            &format!(
                "for a in \"$@\"; do echo \"$a\"; done > '{}'",
                log.display()
            ),
        );
        let root = dir.path().join("data");
        let encoder = Afconvert::new(binary, root.clone());
        encoder
            .to_m4a("library/x/audio.wav", "library/x/audio.m4a")
            .unwrap();
        let argv: Vec<OsString> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(OsString::from)
            .collect();
        assert_eq!(
            argv,
            Afconvert::args(
                &root.join("library/x/audio.wav"),
                &root.join("library/x/audio.m4a")
            ),
            "the arguments"
        );
    }

    #[test]
    fn a_failing_run_is_an_error_with_its_stderr() {
        let dir = TempDir::new().unwrap();
        let binary = stub(&dir, "afconvert", "echo '  bad input  ' >&2; exit 3");
        let encoder = Afconvert::new(binary, dir.path().into());
        let error = encoder.to_m4a("a.wav", "a.m4a").unwrap_err();
        assert!(error.0.ends_with(": bad input"), "{error}");
    }
}
