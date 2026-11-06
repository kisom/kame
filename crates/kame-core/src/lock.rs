//! Which kame has a document open. Two windows writing one file would each put their own head
//! and steps in it, leaving a history undo can't trust, so a document is held by the kame that
//! opens it and refused to any other.
//!
//! The lock is an advisory one on a file of its own, in a folder the caller names, never on the
//! document or beside it: SQLite holds locks of its own on the database, which on macOS can
//! conflict with these even within one process, and what's beside a document travels with it when
//! a notebook syncs. The file is named for the document's device and inode, so every spelling of
//! its path, through a symlink or in another case, comes to the same lock.

use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::session::{file_identity, FileIdentity};

/// How many times a lock is tried for whose file went as it was being taken.
const ATTEMPTS: usize = 100;

/// A document held open, until this is dropped.
#[derive(Debug)]
pub struct DocumentLock {
    /// Open for as long as the lock is held, as closing it lets the lock go.
    _file: File,
    /// Where the lock's file is, to remove it on letting go.
    path: PathBuf,
    /// Which file the document was when it was taken.
    document: FileIdentity,
}

/// Why a document's lock couldn't be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another holder has it: another kame, or another window of this one.
    Held,
    /// There was no lock to take, as when the document isn't there or the folder can't be written.
    /// Nothing else holds the document as far as anyone can tell.
    Failed(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held => write!(f, "it is open in another kame window"),
            LockError::Failed(err) => write!(f, "could not lock it — {err}"),
        }
    }
}

impl std::error::Error for LockError {}

impl DocumentLock {
    /// Takes the lock on the document at `document`, keeping its file in `folder`, which is made if
    /// it isn't there. Each holder is a file opened anew, so a second holder in the same process is
    /// refused as one in another process is.
    ///
    /// A lock file is removed when its holder lets go, so one being taken may have gone by the time
    /// it's locked. That one is let go and the lock tried again, so that whoever holds a document
    /// holds the file its lock's path names, and nobody else can take it.
    pub fn take(folder: &Path, document: &Path) -> Result<DocumentLock, LockError> {
        let identity = file_identity(document).ok_or_else(|| {
            LockError::Failed(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not there", document.display()),
            ))
        })?;
        std::fs::create_dir_all(folder).map_err(LockError::Failed)?;
        let path = folder.join(format!("{}-{}.lock", identity.device, identity.inode));
        for _ in 0..ATTEMPTS {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(LockError::Failed)?;
            #[cfg(test)]
            BEFORE_LOCK.with_borrow_mut(|hook| {
                if let Some(hook) = hook.take() {
                    hook();
                }
            });
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Err(LockError::Held),
                Err(TryLockError::Error(err)) => return Err(LockError::Failed(err)),
            }
            let locked = file.metadata().map_err(LockError::Failed)?;
            if file_identity(&path).is_some_and(|there| same_file(there, &locked)) {
                return Ok(DocumentLock {
                    _file: file,
                    path,
                    document: identity,
                });
            }
        }
        Err(LockError::Failed(io::Error::other(
            "its lock kept being let go as it was taken",
        )))
    }

    /// Whether `document` is the file this holds, however it's spelled. Once another file is put
    /// at its path, that path is no longer the one held.
    pub fn is_for(&self, document: &Path) -> bool {
        file_identity(document) == Some(self.document)
    }
}

impl Drop for DocumentLock {
    /// Removes the lock's file while it's still held, so that nobody else can be taking it, and
    /// lets the lock go as the file closes.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn same_file(identity: FileIdentity, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    identity.device == metadata.dev() && identity.inode == metadata.ino()
}

#[cfg(test)]
type BeforeLock = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    /// Run once between opening a lock's file and locking it, so that a test can let go of the lock
    /// another holder has just then.
    static BEFORE_LOCK: std::cell::RefCell<Option<BeforeLock>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for one test's locks, and a document in another.
    fn setup() -> (PathBuf, PathBuf) {
        let dir = crate::test_temp_dir().join(format!("lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        let document = dir.join("notes").join("doc.kame");
        std::fs::write(&document, b"").unwrap();
        (dir.join("open"), document)
    }

    fn locks_in(folder: &Path) -> usize {
        std::fs::read_dir(folder).map_or(0, |entries| entries.count())
    }

    #[test]
    fn a_second_holder_in_the_same_process_is_refused() {
        let (folder, document) = setup();
        let first = DocumentLock::take(&folder, &document).unwrap();
        assert!(matches!(
            DocumentLock::take(&folder, &document),
            Err(LockError::Held)
        ));
        drop(first);
        DocumentLock::take(&folder, &document).unwrap();
    }

    #[test]
    fn letting_go_removes_the_lock_file() {
        let (folder, document) = setup();
        let lock = DocumentLock::take(&folder, &document).unwrap();
        assert_eq!(locks_in(&folder), 1);
        drop(lock);
        assert_eq!(locks_in(&folder), 0);
    }

    #[test]
    fn every_spelling_of_the_document_finds_one_lock() {
        let (folder, document) = setup();
        let notes = document.parent().unwrap();
        let symlink = notes.join("link.kame");
        std::os::unix::fs::symlink(&document, &symlink).unwrap();
        let hard = notes.join("hard.kame");
        std::fs::hard_link(&document, &hard).unwrap();
        let roundabout = notes.join("..").join("notes").join("doc.kame");
        let _held = DocumentLock::take(&folder, &document).unwrap();
        for spelling in [&symlink, &hard, &roundabout] {
            assert!(
                matches!(DocumentLock::take(&folder, spelling), Err(LockError::Held)),
                "{}",
                spelling.display()
            );
        }
    }

    #[test]
    fn two_documents_are_held_apart() {
        let (folder, document) = setup();
        let other = document.with_file_name("other.kame");
        std::fs::write(&other, b"").unwrap();
        let _one = DocumentLock::take(&folder, &document).unwrap();
        let _two = DocumentLock::take(&folder, &other).unwrap();
        assert_eq!(locks_in(&folder), 2);
    }

    #[test]
    fn a_lock_file_left_behind_is_taken_over() {
        let (folder, document) = setup();
        // As a kame that was killed leaves it.
        std::fs::create_dir_all(&folder).unwrap();
        let identity = file_identity(&document).unwrap();
        std::fs::write(
            folder.join(format!("{}-{}.lock", identity.device, identity.inode)),
            b"",
        )
        .unwrap();
        let _lock = DocumentLock::take(&folder, &document).unwrap();
        assert!(matches!(
            DocumentLock::take(&folder, &document),
            Err(LockError::Held)
        ));
    }

    #[test]
    fn a_lock_let_go_as_it_is_taken_is_taken_again() {
        let (folder, document) = setup();
        let before = DocumentLock::take(&folder, &document).unwrap();
        // The next take opens this lock's file, and then it's let go, which removes that file.
        BEFORE_LOCK.set(Some(Box::new(move || drop(before))));
        let lock = DocumentLock::take(&folder, &document).unwrap();
        assert!(BEFORE_LOCK.with_borrow(Option::is_none));
        // What's held is the file at the lock's path, and not the one removed under it.
        assert!(matches!(
            DocumentLock::take(&folder, &document),
            Err(LockError::Held)
        ));
        drop(lock);
        assert_eq!(locks_in(&folder), 0);
    }

    #[test]
    fn a_lock_knows_its_document_by_the_file() {
        let (folder, document) = setup();
        let symlink = document.with_file_name("link.kame");
        std::os::unix::fs::symlink(&document, &symlink).unwrap();
        let other = document.with_file_name("other.kame");
        std::fs::write(&other, b"").unwrap();
        let lock = DocumentLock::take(&folder, &document).unwrap();
        assert!(lock.is_for(&document));
        assert!(lock.is_for(&symlink));
        assert!(!lock.is_for(&other));
        // Another file put at its path, as a sync does.
        std::fs::rename(&other, &document).unwrap();
        assert!(!lock.is_for(&document));
    }

    #[test]
    fn a_document_that_is_not_there_has_no_lock() {
        let (folder, document) = setup();
        let missing = document.with_file_name("missing.kame");
        assert!(matches!(
            DocumentLock::take(&folder, &missing),
            Err(LockError::Failed(_))
        ));
    }
}
