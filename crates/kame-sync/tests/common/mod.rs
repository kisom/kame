//! Where these tests put their files: one folder, cleared of runs over an hour old.

use std::path::PathBuf;

pub fn test_folder(name: &str) -> PathBuf {
    static CLEARED: std::sync::Once = std::sync::Once::new();
    let root = std::env::temp_dir().join("kame-sync-tests");
    CLEARED.call_once(|| {
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|found| found.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age.as_secs() > 3600);
            if stale {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
    let folder = root.join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}
