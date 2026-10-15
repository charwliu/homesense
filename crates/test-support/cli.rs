//! Integration-test executable lookup; no fallback after an explicit override.
use std::path::{Path, PathBuf};

pub fn binary(compiled_path: &str) -> PathBuf {
    let path = match std::env::var_os("HOMESENSE_TEST_BIN_DIR") {
        Some(directory) => {
            let directory = PathBuf::from(directory);
            assert!(
                directory.is_dir(),
                "HOMESENSE_TEST_BIN_DIR must name an existing directory: {}",
                directory.display()
            );
            directory.join(
                Path::new(compiled_path)
                    .file_name()
                    .expect("Cargo executable path must include a file name"),
            )
        }
        None => PathBuf::from(compiled_path),
    };
    assert!(
        path.is_file(),
        "required integration-test executable is missing: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("required integration-test executable metadata")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "required integration-test executable is not executable: {}",
            path.display()
        );
    }
    path
}
