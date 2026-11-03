//! kame's documents, independent of any window: the model, its edit history, the `.kame`
//! file, and Markdown and HTML on the way in and out. Nothing here knows about a GUI or a
//! text engine, so a command line tool can use it as readily as the editor does.

pub mod edit;
pub mod export;
pub mod import;
pub mod links;
pub mod model;
pub mod session;
pub mod storage;
pub mod text;

/// Where tests put their files: one folder, cleared of runs over an hour old, since otherwise
/// every run leaves its files in what is often a memory-backed temporary directory.
#[cfg(test)]
pub(crate) fn test_temp_dir() -> std::path::PathBuf {
    static CLEARED: std::sync::Once = std::sync::Once::new();
    let root = std::env::temp_dir().join("kame-core-tests");
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
                let path = entry.path();
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    });
    std::fs::create_dir_all(&root).unwrap();
    root
}
