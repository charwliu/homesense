//! Materialize compiled fixture bytes in an owned private temporary directory.
use std::path::{Path, PathBuf};

pub struct Directory(PathBuf);
impl Directory {
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("fixture directory clock")
                .as_nanos()
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).expect("create private fixture directory");
        Self(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path().join(name);
        std::fs::write(&path, bytes).expect("write compiled fixture bytes");
        path
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
