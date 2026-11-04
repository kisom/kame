//! Folders for tests. The temporary directory is often held in memory, and a test can write a
//! whole site into it, so what a run writes has to go when the run does. `cargo test` has no hook
//! for the end of a run, so each test process gets a folder of its own, removed as it exits, whether
//! its tests passed or not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const PREFIX: &str = "kame-tests-";
/// How old a folder has to be before it's taken for one left by a run that was killed.
const STALE: Duration = Duration::from_secs(3600);

static ROOT: OnceLock<PathBuf> = OnceLock::new();

extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::os::raw::c_int;
}

extern "C" fn remove_root() {
    if let Some(root) = ROOT.get() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A run that's killed can't clean up after itself, so the next one does.
fn clear_stale(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry.file_name().to_string_lossy().starts_with(PREFIX);
        let stale = || {
            entry
                .metadata()
                .and_then(|found| found.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE)
        };
        if ours && stale() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// This test process's folder.
pub fn root() -> &'static Path {
    ROOT.get_or_init(|| {
        let temp = std::env::temp_dir();
        clear_stale(&temp);
        let started = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = temp.join(format!("{PREFIX}{}-{started}", std::process::id()));
        std::fs::create_dir_all(&root).expect("could not make a folder for tests");
        // SAFETY: `atexit` only keeps the pointer, to a function that lives as long as the process.
        unsafe { atexit(remove_root) };
        root
    })
}

/// A new, empty folder for one test.
pub fn folder(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let folder = root().join(format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&folder).expect("could not make a folder for a test");
    folder
}
