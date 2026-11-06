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
    file: File,
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
                    file,
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
    /// lets the lock go as the file closes. A file at its path that isn't the one it locked, as
    /// when something else removed that one, is another holder's, and stays.
    fn drop(&mut self) {
        let own = self.file.metadata().is_ok_and(|locked| {
            file_identity(&self.path).is_some_and(|there| same_file(there, &locked))
        });
        if own {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What a write into documents that aren't open here needs to know so as not to write into one
/// another kame has open: where the locks are kept, and the lock its caller holds already, on the
/// document it has open, which is never refused. With no folder, nothing is locked or refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct Locks<'a> {
    folder: Option<&'a Path>,
    own: Option<&'a DocumentLock>,
}

/// The locks taken for a write, held until this is dropped.
#[derive(Debug, Default)]
pub struct Holding {
    locks: Vec<DocumentLock>,
}

impl<'a> Locks<'a> {
    /// No locks to take, for a caller that keeps none, so that nothing is refused.
    pub const NONE: Locks<'static> = Locks {
        folder: None,
        own: None,
    };

    pub fn new(folder: Option<&'a Path>, own: Option<&'a DocumentLock>) -> Locks<'a> {
        Locks { folder, own }
    }

    /// Takes the lock on each of `documents`, to be held while they're written, so that another
    /// kame can't open one halfway through. `Err` is the first another holder has, and then none
    /// of them is held. The caller's own document needs no other lock, and nor does one named
    /// twice. One whose lock can't be taken for any other reason, as when it isn't there, counts
    /// as nobody else's, as there's nothing to tell the two apart.
    pub fn take<P: AsRef<Path>>(
        &self,
        documents: impl IntoIterator<Item = P>,
    ) -> Result<Holding, PathBuf> {
        let mut holding = Holding::default();
        let Some(folder) = self.folder else {
            return Ok(holding);
        };
        for document in documents {
            let document = document.as_ref();
            let held_here = self
                .own
                .into_iter()
                .chain(&holding.locks)
                .any(|lock| lock.is_for(document));
            if held_here {
                continue;
            }
            match DocumentLock::take(folder, document) {
                Ok(lock) => holding.locks.push(lock),
                Err(LockError::Held) => return Err(document.to_path_buf()),
                Err(LockError::Failed(_)) => {}
            }
        }
        Ok(holding)
    }
}

impl Holding {
    /// How many locks this holds.
    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
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
    fn letting_go_leaves_a_lock_file_that_is_not_its_own() {
        let (folder, document) = setup();
        let first = DocumentLock::take(&folder, &document).unwrap();
        // Something other than kame removes the lock file, and so another takes it.
        std::fs::remove_file(&first.path).unwrap();
        let second = DocumentLock::take(&folder, &document).unwrap();
        drop(first);
        assert_eq!(locks_in(&folder), 1);
        assert!(matches!(
            DocumentLock::take(&folder, &document),
            Err(LockError::Held)
        ));
        drop(second);
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

    #[test]
    fn a_write_holds_each_document_until_it_is_done() {
        let (folder, document) = setup();
        let other = document.with_file_name("other.kame");
        std::fs::write(&other, b"").unwrap();
        let holding = Locks::new(Some(&folder), None)
            .take([&document, &other])
            .unwrap();
        assert_eq!(holding.len(), 2);
        for held in [&document, &other] {
            assert!(matches!(
                DocumentLock::take(&folder, held),
                Err(LockError::Held)
            ));
        }
        drop(holding);
        assert_eq!(locks_in(&folder), 0);
        DocumentLock::take(&folder, &document).unwrap();
    }

    #[test]
    fn a_write_is_refused_a_document_another_holds_and_holds_none() {
        let (folder, document) = setup();
        let first = document.with_file_name("first.kame");
        std::fs::write(&first, b"").unwrap();
        let _theirs = DocumentLock::take(&folder, &document).unwrap();
        assert_eq!(
            Locks::new(Some(&folder), None)
                .take([&first, &document])
                .unwrap_err(),
            document
        );
        DocumentLock::take(&folder, &first).unwrap();
    }

    #[test]
    fn a_write_into_the_callers_own_document_is_not_refused() {
        let (folder, document) = setup();
        let own = DocumentLock::take(&folder, &document).unwrap();
        let symlink = document.with_file_name("link.kame");
        std::os::unix::fs::symlink(&document, &symlink).unwrap();
        let holding = Locks::new(Some(&folder), Some(&own))
            .take([&document, &symlink])
            .unwrap();
        assert!(holding.is_empty());
        drop(holding);
        // Still the caller's.
        assert!(matches!(
            DocumentLock::take(&folder, &document),
            Err(LockError::Held)
        ));
    }

    #[test]
    fn a_document_named_twice_is_held_once() {
        let (folder, document) = setup();
        let symlink = document.with_file_name("link.kame");
        std::os::unix::fs::symlink(&document, &symlink).unwrap();
        let holding = Locks::new(Some(&folder), None)
            .take([&document, &symlink, &document])
            .unwrap();
        assert_eq!(holding.len(), 1);
    }

    #[test]
    fn a_lock_that_cannot_be_taken_does_not_refuse_a_write() {
        let (folder, document) = setup();
        let missing = document.with_file_name("missing.kame");
        let holding = Locks::new(Some(&folder), None).take([&missing]).unwrap();
        assert!(holding.is_empty());
        // A folder that can't be made.
        let blocked = folder.with_file_name("blocked");
        std::fs::write(&blocked, b"").unwrap();
        let holding = Locks::new(Some(&blocked), None).take([&document]).unwrap();
        assert!(holding.is_empty());
    }

    #[test]
    fn with_no_folder_nothing_is_locked_or_refused() {
        let (folder, document) = setup();
        let _theirs = DocumentLock::take(&folder, &document).unwrap();
        assert!(Locks::NONE.take([&document]).unwrap().is_empty());
        assert!(Locks::new(None, None).take([&document]).unwrap().is_empty());
    }
}
