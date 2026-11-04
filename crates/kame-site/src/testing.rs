//! What tests share: temporary sites and real `.kame` files.

use std::path::{Path, PathBuf};

use kame_core::model::{Block, BlockKind, Document, Metadata};
use kame_core::storage::Db;

pub fn temp_dir(name: &str) -> PathBuf {
    kame_test_dir::folder(name)
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
