//! The data folder over a real temporary directory.

#[cfg(test)]
mod tests {
    use scriber_engine::DataDir;
    use scriber_engine::Files as _;
    use tempfile::TempDir;

    #[test]
    fn the_data_folder_keeps_the_ports_promises() {
        let tmp = TempDir::new().unwrap();
        let d = DataDir::new(tmp.path().to_path_buf());
        d.write("a/b/c.txt", b"x").unwrap();
        assert_eq!(d.read("a/b/c.txt").unwrap(), b"x", "written, folders made");
        d.append("a/b/c.txt", b"y").unwrap();
        assert_eq!(d.read("a/b/c.txt").unwrap(), b"xy", "appended");
        assert!(
            d.append("missing.txt", b"z").is_err(),
            "append needs the file"
        );
        d.rename("a/b/c.txt", "a/d.txt").unwrap();
        assert_eq!(d.read("a/d.txt").unwrap(), b"xy", "renamed");
        assert!(d.read("a/b/c.txt").is_err(), "and gone from where it was");
        let mut names = d.list("a").unwrap();
        names.sort();
        assert_eq!(names, ["b", "d.txt"], "listed");
        d.remove("a/d.txt").unwrap();
        assert!(d.read("a/d.txt").is_err(), "removed");
        assert_eq!(d.list("new"), Ok(vec![]), "a missing folder lists empty");
        assert!(tmp.path().join("new").is_dir(), "and is made");
    }
}
