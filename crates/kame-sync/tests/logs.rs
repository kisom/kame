//! Revisions moving between machines in logs, with the sync tool played by copying files.

use std::path::{Path, PathBuf};

use kame_core::model::{Block, BlockKind, Document};
use kame_sync::merge::document_digest;
use kame_sync::revlog::{Replica, Store};
use uuid::Uuid;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("kame-sync-tests")
        .join(format!("{name}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn start() -> Document {
    Document {
        id: Uuid::from_u128(99),
        title: "Test".to_string(),
        blocks: vec![
            Block {
                id: Uuid::from_u128(1),
                kind: BlockKind::Paragraph,
                text: "First paragraph.".to_string(),
                spans: Vec::new(),
            },
            Block {
                id: Uuid::from_u128(2),
                kind: BlockKind::Paragraph,
                text: "Second paragraph.".to_string(),
                spans: Vec::new(),
            },
        ],
        annotations: Vec::new(),
    }
}

/// A machine: its own folder, as a sync tool would keep it, and what it has open.
struct Machine {
    replica: Replica,
    doc: Document,
    last: Option<String>,
    clock: i64,
}

impl Machine {
    fn new(root: &Path, name: &str) -> Machine {
        Machine {
            replica: Replica {
                folder: root.join(name),
                device: name.to_string(),
            },
            doc: start(),
            last: None,
            clock: 0,
        }
    }

    fn sync(&mut self) -> String {
        self.clock += 1;
        let synced = self
            .replica
            .sync(&self.doc, self.last.as_deref(), self.clock)
            .unwrap();
        self.doc = synced.doc;
        self.last = Some(synced.head.clone());
        synced.head
    }

    fn append(&mut self, block: usize, words: &str) {
        self.doc.blocks[block].text.push_str(words);
    }

    /// The sync tool: other machines' logs arrive whole.
    fn receive_from(&self, other: &Machine) {
        std::fs::create_dir_all(&self.replica.folder).unwrap();
        for entry in std::fs::read_dir(&other.replica.folder).unwrap().flatten() {
            let name = entry.file_name();
            if name.to_string_lossy() == format!("{}.log", other.replica.device) {
                std::fs::copy(entry.path(), self.replica.folder.join(name)).unwrap();
            }
        }
    }
}

fn exchange(machines: &[&Machine]) {
    for (at, machine) in machines.iter().enumerate() {
        for (other_at, other) in machines.iter().enumerate() {
            if at != other_at {
                machine.receive_from(other);
            }
        }
    }
}

#[test]
fn two_machines_merge_separately_and_write_the_same_revision() {
    let root = folder("two");
    let (mut talos, mut titan) = (Machine::new(&root, "talos"), Machine::new(&root, "titan"));
    let first = talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    // Each root is the same content by different hands, so they merge first.
    let (one, two) = (talos.sync(), titan.sync());
    assert_eq!(one, two, "the first agreement");
    exchange(&[&talos, &titan]);
    talos.sync();
    titan.sync();
    let _ = first;

    talos.append(0, " Written on talos.");
    titan.append(1, " Written on titan.");
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    let (one, two) = (talos.sync(), titan.sync());
    assert_eq!(one, two, "separately, the same id");
    assert_eq!(talos.doc, titan.doc);
    assert_eq!(
        talos.doc.blocks[0].text,
        "First paragraph. Written on talos."
    );
    assert_eq!(
        talos.doc.blocks[1].text,
        "Second paragraph. Written on titan."
    );
    let log = std::fs::read_to_string(titan.replica.folder.join("titan.log")).unwrap();
    let merges_in_titans_log = log
        .lines()
        .filter(|line| line.contains(&format!("\"id\":\"{two}\"")))
        .count();
    assert_eq!(merges_in_titans_log, 1);
}

#[test]
fn a_conflict_converges_with_one_note_on_both() {
    let root = folder("conflict");
    let (mut talos, mut titan) = (Machine::new(&root, "talos"), Machine::new(&root, "titan"));
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    talos.sync();
    titan.sync();
    talos.doc.blocks[0].text = "First good paragraph.".to_string();
    titan.doc.blocks[0].text = "First paragraph here.".to_string();
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    assert_eq!(talos.sync(), titan.sync());
    assert_eq!(talos.doc.annotations.len(), 1);
    assert_eq!(document_digest(&talos.doc), document_digest(&titan.doc));
}

#[test]
fn undoing_a_merge_is_just_the_next_revision_and_sticks() {
    let root = folder("undo");
    let (mut talos, mut titan) = (Machine::new(&root, "talos"), Machine::new(&root, "titan"));
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    talos.sync();
    titan.sync();
    let before = talos.doc.clone();
    titan.append(1, " From titan.");
    titan.sync();
    exchange(&[&talos, &titan]);
    talos.sync();
    assert!(talos.doc.blocks[1].text.contains("From titan"));
    // The author undoes the merge.
    talos.doc = before;
    talos.sync();
    exchange(&[&talos, &titan]);
    titan.sync();
    talos.sync();
    assert!(
        !talos.doc.blocks[1].text.contains("From titan"),
        "it does not come back"
    );
    assert_eq!(talos.doc, titan.doc);
}

#[test]
fn a_line_cut_off_in_transit_is_ignored_until_the_rest_arrives() {
    let root = folder("torn");
    let (mut talos, mut titan) = (Machine::new(&root, "talos"), Machine::new(&root, "titan"));
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    talos.sync();
    titan.sync();
    exchange(&[&talos, &titan]);
    titan.append(0, " Arriving.");
    titan.sync();
    let log = std::fs::read(titan.replica.folder.join("titan.log")).unwrap();
    std::fs::write(
        talos.replica.folder.join("titan.log"),
        &log[..log.len() - 20],
    )
    .unwrap();
    talos.sync();
    assert!(!talos.doc.blocks[0].text.contains("Arriving"));
    std::fs::write(talos.replica.folder.join("titan.log"), &log).unwrap();
    talos.sync();
    assert!(talos.doc.blocks[0].text.contains("Arriving"));
}

#[test]
fn a_revision_waits_for_the_log_holding_what_it_builds_on() {
    let root = folder("order");
    let (mut talos, mut titan, mut straylight) = (
        Machine::new(&root, "talos"),
        Machine::new(&root, "titan"),
        Machine::new(&root, "straylight"),
    );
    talos.sync();
    titan.receive_from(&talos);
    titan.sync();
    titan.append(0, " Built on talos.");
    titan.sync();
    // Titan's log reaches straylight without talos's.
    straylight.receive_from(&titan);
    let store = Store::load(&straylight.replica.folder).unwrap();
    assert!(store.heads().is_empty(), "nothing usable yet");
    straylight.receive_from(&talos);
    straylight.sync();
    assert!(straylight.doc.blocks[0].text.contains("Built on talos"));
}

struct Random(u64);

impl Random {
    fn below(&mut self, limit: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % limit as u64) as usize
    }
}

/// Machines edit, sync and receive in random orders. When everything has arrived everywhere they
/// must agree, however crossed their merges were, with every sentence accounted for.
#[test]
fn random_delivery_converges_and_keeps_every_sentence() {
    for seed in 1..=60u64 {
        let root = folder("random");
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut machines = vec![
            Machine::new(&root, "talos"),
            Machine::new(&root, "titan"),
            Machine::new(&root, "straylight"),
        ];
        for machine in &mut machines {
            machine.sync();
        }
        let mut written = Vec::new();
        for step in 0..40 {
            let who = random.below(3);
            match random.below(3) {
                0 => {
                    let block = random.below(machines[who].doc.blocks.len());
                    let sentence = format!(" Zq{who}x{step}q.");
                    machines[who].append(block, &sentence);
                    written.push(format!("Zq{who}x{step}q"));
                }
                1 => {
                    machines[who].sync();
                }
                _ => {
                    let from = (who + 1 + random.below(2)) % 3;
                    if machines[from].replica.folder.exists() {
                        machines[who].receive_from(&machines[from]);
                    }
                }
            }
        }
        // Everything arrives, and everyone syncs until nothing new is written.
        for _ in 0..6 {
            for machine in &mut machines {
                machine.sync();
            }
            let all: Vec<&Machine> = machines.iter().collect();
            exchange(&all);
        }
        for machine in &mut machines {
            machine.sync();
        }
        let (a, b, c) = (
            machines[0].last.clone(),
            machines[1].last.clone(),
            machines[2].last.clone(),
        );
        assert_eq!(a, b, "seed {seed}");
        assert_eq!(b, c, "seed {seed}");
        assert_eq!(machines[0].doc, machines[2].doc, "seed {seed}");
        let doc = &machines[0].doc;
        let mut words = String::new();
        for block in &doc.blocks {
            words.push_str(&block.text);
        }
        for note in &doc.annotations {
            words.push_str(&note.body);
        }
        for sentence in &written {
            assert!(
                words.contains(sentence.as_str()),
                "seed {seed} lost {sentence}: {words}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
