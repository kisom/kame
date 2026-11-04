//! Where these tests put their files, gone when the test process ends.

use std::path::PathBuf;

pub fn test_folder(name: &str) -> PathBuf {
    kame_test_dir::folder(name)
}
