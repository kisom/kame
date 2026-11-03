//! What the checker marks in a document, rule by rule, so that noisy rules can be found:
//!
//! ```sh
//! cargo run --release -p kame-check --example check -- ESSAY.kame
//! ```
use std::collections::BTreeMap;
use std::path::Path;

use kame_check::Checker;
use kame_core::storage::Db;

fn main() {
    let path = std::env::args().nth(1).expect("usage: check DOC.kame");
    let db = Db::open_read_only(Path::new(&path)).expect("a kame document");
    let doc = db.load_document().expect("a kame document");
    let mut checker = Checker::new();
    let mut by_rule: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut words = 0;
    for block in &doc.blocks {
        words += block.text.split_whitespace().count();
        for finding in checker.check_block(block) {
            by_rule
                .entry(finding.rule)
                .or_default()
                .push(block.text[finding.start..finding.end].to_string());
        }
    }
    let total: usize = by_rule.values().map(Vec::len).sum();
    println!("{total} marks in {words} words");
    for (rule, marked) in by_rule {
        println!("  {rule} ({}): {}", marked.len(), marked.join(", "));
    }
}
