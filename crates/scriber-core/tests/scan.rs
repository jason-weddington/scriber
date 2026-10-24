//! The scan against a fake filesystem and a fake clock.

mod common;

// Integration tests live in a #[cfg(test)] module so the workspace lints
// (tests_outside_test_module, and the clippy.toml allowances for unwrap and
// friends in tests) apply to them exactly as to unit tests.
#[cfg(test)]
mod tests {
    use std::path::Path;

    use scriber_core::model::{ExtensionSummary, ScanProgress};
    use scriber_core::ports::EntryKind;
    use scriber_core::scan::{ScanError, scan};

    use crate::common::{FakeClock, FakeHost, Recorder, file, ports};

    fn ext(extension: &str, files: u64, bytes: u64) -> ExtensionSummary {
        ExtensionSummary {
            extension: extension.to_string(),
            files,
            bytes,
        }
    }

    fn progress(files: u64, bytes: u64, folders: u64) -> ScanProgress {
        ScanProgress {
            files,
            bytes,
            folders,
        }
    }

    /// /p holds two photos and a folder holding a third photo, a note and a
    /// link back up to /p.
    fn photos() -> FakeHost {
        FakeHost::default()
            .folder(
                "/p",
                &[
                    ("a.JPG", file(300)),
                    ("b.jpg", file(200)),
                    ("raw", EntryKind::Folder),
                ],
            )
            .folder(
                "/p/raw",
                &[
                    ("c.dng", file(900)),
                    ("notes", file(5)),
                    ("loop", EntryKind::Other),
                ],
            )
    }

    #[test]
    fn counts_every_file_below_the_root_by_extension() {
        let (host, clock, sink) = (photos(), FakeClock::new(0, 0), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/p")).unwrap();
        assert_eq!(summary.root, "/p", "root as given");
        assert_eq!(summary.files, 4, "four regular files");
        assert_eq!(summary.total_bytes, 1405, "sum of sizes");
        assert_eq!(summary.unreadable_folders, 0, "every folder read");
        assert_eq!(
            summary.extensions,
            [ext("dng", 1, 900), ext("jpg", 2, 500), ext("", 1, 5)],
            "largest first, with the extension lowercased"
        );
    }

    #[test]
    fn links_and_special_entries_are_neither_counted_nor_followed() {
        // The `loop` entry would make a following walk revisit /p forever.
        let (host, clock, sink) = (photos(), FakeClock::new(0, 0), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/p")).unwrap();
        assert_eq!(
            sink.reports().last(),
            Some(&progress(4, 1405, 2)),
            "two folders read, the link skipped"
        );
        assert_eq!(summary.files, 4, "the link is not a file");
    }

    #[test]
    fn an_empty_folder_has_no_extensions() {
        let host = FakeHost::default().folder("/e", &[]);
        let (clock, sink) = (FakeClock::new(0, 0), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/e")).unwrap();
        assert_eq!(
            (summary.files, summary.total_bytes),
            (0, 0),
            "nothing counted"
        );
        assert!(summary.extensions.is_empty(), "no extensions");
        assert_eq!(sink.reports(), [progress(0, 0, 1)], "one final report");
    }

    #[test]
    fn an_unreadable_root_is_an_error() {
        let host = FakeHost::default().unreadable("/locked", "Permission denied");
        let (clock, sink) = (FakeClock::new(0, 0), Recorder::default());
        let err = scan(ports(&host, &clock, &sink), Path::new("/locked")).unwrap_err();
        assert_eq!(
            err,
            ScanError::Unreadable {
                path: "/locked".to_string(),
                reason: "Permission denied".to_string(),
            },
            "the root's own error"
        );
        assert_eq!(
            err.to_string(),
            "/locked could not be read: Permission denied",
            "one readable sentence"
        );
        assert!(
            sink.reports().is_empty(),
            "no progress for a scan that never ran"
        );
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let (host, clock, sink) = (
            FakeHost::default(),
            FakeClock::new(0, 0),
            Recorder::default(),
        );
        let result = scan(ports(&host, &clock, &sink), Path::new("/gone"));
        assert!(
            matches!(result, Err(ScanError::Unreadable { ref path, .. }) if path == "/gone"),
            "got {result:?}"
        );
    }

    #[test]
    fn an_unreadable_subfolder_is_counted_and_the_rest_is_scanned() {
        let host = FakeHost::default()
            .folder(
                "/r",
                &[
                    ("locked", EntryKind::Folder),
                    ("open", EntryKind::Folder),
                    ("x.txt", file(1)),
                ],
            )
            .unreadable("/r/locked", "Permission denied")
            .folder("/r/open", &[("y.txt", file(2))]);
        let (clock, sink) = (FakeClock::new(0, 0), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/r")).unwrap();
        assert_eq!(summary.unreadable_folders, 1, "the locked folder");
        assert_eq!(
            summary.extensions,
            [ext("txt", 2, 3)],
            "both readable files"
        );
        assert_eq!(sink.reports(), [progress(2, 3, 2)], "two folders were read");
    }

    #[test]
    fn sizes_saturate_rather_than_wrap() {
        let host =
            FakeHost::default().folder("/big", &[("a.bin", file(u64::MAX)), ("b.bin", file(1))]);
        let (clock, sink) = (FakeClock::new(0, 0), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/big")).unwrap();
        assert_eq!(summary.total_bytes, u64::MAX, "saturated total");
        assert_eq!(
            summary.extensions,
            [ext("bin", 2, u64::MAX)],
            "saturated per extension"
        );
    }

    /// Three folders in a chain, one file each.
    fn chain() -> FakeHost {
        FakeHost::default()
            .folder("/c", &[("1.a", file(1)), ("d", EntryKind::Folder)])
            .folder("/c/d", &[("2.a", file(1)), ("e", EntryKind::Folder)])
            .folder("/c/d/e", &[("3.a", file(1))])
    }

    #[test]
    fn progress_is_reported_once_the_interval_has_passed() {
        // The clock reads 100 ms later every time: each folder is a report.
        let (host, clock, sink) = (chain(), FakeClock::new(1000, 100), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/c")).unwrap();
        assert_eq!(summary.files, 3, "every folder scanned");
        assert_eq!(
            sink.reports(),
            [
                progress(1, 1, 1),
                progress(2, 2, 2),
                progress(3, 3, 3),
                progress(3, 3, 3)
            ],
            "one per folder, then the final totals"
        );
    }

    #[test]
    fn progress_waits_for_the_full_interval_since_the_last_report() {
        // 99 ms per read: the first folder is too soon, the second is 198 ms
        // in, and the third only 99 ms after that report.
        let (host, clock, sink) = (chain(), FakeClock::new(1000, 99), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/c")).unwrap();
        assert_eq!(summary.files, 3, "every folder scanned");
        assert_eq!(
            sink.reports(),
            [progress(2, 2, 2), progress(3, 3, 3)],
            "the second folder, then the final totals"
        );
    }

    #[test]
    fn elapsed_time_comes_from_the_clock() {
        // Reads: the start, one per folder, the end.
        let (host, clock, sink) = (chain(), FakeClock::new(5000, 7), Recorder::default());
        let summary = scan(ports(&host, &clock, &sink), Path::new("/c")).unwrap();
        assert_eq!(summary.elapsed_ms, 28, "four steps of 7 ms");
    }
}
