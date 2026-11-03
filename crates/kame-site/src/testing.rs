//! What tests share: temporary sites and real `.kame` files.

use std::path::{Path, PathBuf};

use kame_core::model::{Block, BlockKind, Document, Metadata};
use kame_core::storage::Db;

/// How old a test's leftovers have to be before a later run clears them. Long enough not to
/// touch a run still going.
const STALE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Clears what earlier runs left behind. Many tests write a whole site, fonts included, and
/// the temporary directory is often held in memory, so leaving them there fills it up.
fn clear_stale(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|found| found.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

pub fn temp_dir(name: &str) -> PathBuf {
    static CLEARED: std::sync::Once = std::sync::Once::new();
    let root = std::env::temp_dir().join("kame-site-tests");
    CLEARED.call_once(|| clear_stale(&root));
    let dir = root.join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn block(kind: BlockKind, text: &str) -> Block {
    Block {
        id: uuid::Uuid::new_v4(),
        kind,
        text: text.to_string(),
        spans: Vec::new(),
    }
}

pub fn paragraph(text: &str) -> Block {
    block(BlockKind::Paragraph, text)
}

pub fn metadata(title: &str, published: &str, tags: &[&str]) -> Metadata {
    Metadata {
        title: title.to_string(),
        author: "K. Isom".to_string(),
        created: String::new(),
        published: published.to_string(),
        tags: tags.iter().map(|tag| tag.to_string()).collect(),
        ..Metadata::default()
    }
}

/// Writes a real `.kame` file, as kame would.
pub fn write_kame(path: &Path, doc: &Document, metadata: Option<&Metadata>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut db = Db::open(path).unwrap();
    db.save_document(doc).unwrap();
    if let Some(metadata) = metadata {
        db.save_metadata(metadata).unwrap();
    }
}

#[test]
fn a_written_document_reads_back() {
    let dir = temp_dir("fixture");
    let path = dir.join("a.kame");
    let mut doc = Document::new("A");
    doc.blocks = vec![paragraph("Words.")];
    write_kame(&path, &doc, Some(&metadata("A", "2026-11-02", &["x"])));
    let db = Db::open_read_only(&path).unwrap();
    assert_eq!(db.load_document().unwrap().blocks[0].text, "Words.");
    assert_eq!(db.load_metadata().unwrap().unwrap().tags, ["x"]);
    std::fs::remove_dir_all(&dir).unwrap();
}
