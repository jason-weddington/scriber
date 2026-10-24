//! The data folder over `std::fs`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use scriber_core::ports::{Files, PortError, fail};

/// The data folder at a root the consumer chooses; the layout inside it is
/// [`scriber_core::store`]'s.
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// The data folder at `root`, which need not exist yet.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Files for DataDir {
    fn read(&self, path: &str) -> Result<Vec<u8>, PortError> {
        std::fs::read(self.root.join(path)).map_err(fail)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        let file = self.root.join(path);
        std::fs::create_dir_all(file.parent().unwrap_or(&self.root)).map_err(fail)?;
        std::fs::write(file, bytes).map_err(fail)
    }

    fn list(&self, folder: &str) -> Result<Vec<String>, PortError> {
        // Made if missing: the port promises a missing folder lists empty.
        std::fs::create_dir_all(self.root.join(folder)).map_err(fail)?;
        let entries = std::fs::read_dir(self.root.join(folder)).map_err(fail)?;
        let names = entries.flatten().map(|e| e.file_name().into_string());
        Ok(names.flatten().collect())
    }

    fn remove(&self, path: &str) -> Result<(), PortError> {
        std::fs::remove_file(self.root.join(path)).map_err(fail)
    }

    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        let file = OpenOptions::new().append(true).open(self.root.join(path));
        file.map_err(fail)?.write_all(bytes).map_err(fail)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), PortError> {
        std::fs::rename(self.root.join(from), self.root.join(to)).map_err(fail)
    }
}
