//! A command line for trying the merge on real documents and the logs on real machines.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kame_core::model::{Anchor, Block, BlockKind, Document};
use kame_core::storage::Db;
use kame_sync::files::{merge_versions, shared_ops, Version};
use kame_sync::revlog::{Replica, Store};

const USAGE: &str = "usage:
  kame-sync merge ONE.kame OTHER.kame OUT.kame   merge two files into a new one
  kame-sync study ONE.kame                       what revisions of a real document would cost
  kame-sync trial ONE.kame [ROUNDS]              replay real edits as if on two machines
  kame-sync step FOLDER DEVICE STATE [WORDS]     add WORDS as a sentence, sync, print the head
  kame-sync enable FOLDER                        turn on revision logs for a folder
  kame-sync sync FILE DEVICE [BLOCK:WORDS]       sync a document with its logs, as the app does";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["merge", one, other, out] => merge_files(Path::new(one), Path::new(other), Path::new(out)),
        ["study", file] => study(Path::new(file)),
        ["trial", file] => trial(Path::new(file), 500),
        ["trial", file, rounds] => trial(Path::new(file), rounds.parse().unwrap_or(500)),
        ["step", folder, device, state] => step(folder, device, state, None),
        ["step", folder, device, state, words] => step(folder, device, state, Some(words)),
        ["enable", folder] => enable(Path::new(folder)),
        ["sync", file, device] => sync_file(Path::new(file), device, None),
        ["sync", file, device, words] => sync_file(Path::new(file), device, Some(words)),
        _ => Err(USAGE.into()),
    };
    match run {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn label(path: &Path) -> String {
    path.file_stem().map_or_else(
        || "the other copy".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

fn merge_files(one: &Path, other: &Path, out: &Path) -> Result<()> {
    if out.exists() {
        return Err(format!("{} already exists", out.display()).into());
    }
    // What this build doesn't know of either would be left out of the merge, or written fitted, and
    // the base comes from their own histories, which that covers too.
    for file in [one, other] {
        if Db::open_read_only(file)?.holds_newer() {
            return Err(format!(
                "{} was written by a newer version of kame; update kame to merge it",
                file.display()
            )
            .into());
        }
    }
    let (first, second) = (Version::read(one)?, Version::read(other)?);
    if first.doc.id != second.doc.id {
        return Err("these are not two versions of one document".into());
    }
    println!(
        "history: {} and {} ops, {} shared",
        first.ops.len(),
        second.ops.len(),
        shared_ops(&first.ops, &second.ops)
    );
    let merged = merge_versions(&first, &label(one), &second, &label(other));
    let mut db = Db::open(out)?;
    db.save_state(&merged.doc, 0)?;
    if let Some(metadata) = Db::open_read_only(one)?.load_metadata()? {
        db.save_metadata(&metadata)?;
    }
    println!(
        "{} blocks, {} notes to look at",
        merged.doc.blocks.len(),
        merged.notes.len()
    );
    for id in &merged.notes {
        if let Some(note) = merged.doc.annotations.iter().find(|note| note.id == *id) {
            println!("  {}", note.body.chars().take(100).collect::<String>());
        }
    }
    Ok(())
}

/// Replays a document's history, writing a revision every `every` ops, to see what the logs
/// would weigh against the database.
fn study(file: &Path) -> Result<()> {
    let version = Version::read(file)?;
    let on_disk = std::fs::metadata(file)?.len();
    let words: usize = version
        .doc
        .blocks
        .iter()
        .map(|block| block.text.split_whitespace().count())
        .sum();
    println!(
        "{}: {} blocks, {words} words, {} notes, {} ops, {} bytes on disk",
        file.display(),
        version.doc.blocks.len(),
        version.doc.annotations.len(),
        version.ops.len(),
        on_disk
    );
    let mut states: Vec<Document> = Vec::with_capacity(version.ops.len() + 1);
    let mut doc = version.doc.clone();
    states.push(doc.clone());
    for op in version.ops.iter().rev() {
        kame_core::edit::apply_inverse(&mut doc, op);
        states.push(doc.clone());
    }
    states.reverse();
    let scratch = std::env::temp_dir().join(format!("kame-sync-study-{}", uuid::Uuid::new_v4()));
    for every in [1usize, 10, 50, 200] {
        let folder = scratch.join(every.to_string());
        let replica = Replica {
            folder: folder.clone(),
            device: "study".to_string(),
        };
        let mut last: Option<String> = None;
        let mut count = 0;
        let started = std::time::Instant::now();
        for (at, state) in states.iter().enumerate() {
            if at % every == 0 || at + 1 == states.len() {
                let synced = replica.sync(state, last.as_deref(), at as i64)?;
                last = Some(synced.head);
                count += 1;
            }
            if every == 1 && count >= 400 {
                break;
            }
        }
        let weight = std::fs::metadata(folder.join("study.log"))?.len();
        println!(
            "  a revision every {every:>3} ops: {count:>5} revisions, {weight:>9} bytes, {:>6} per revision, {:?} in all",
            weight / count.max(1) as u64,
            started.elapsed()
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

fn states_of(version: &Version) -> Vec<Document> {
    let mut states = Vec::with_capacity(version.ops.len() + 1);
    let mut doc = version.doc.clone();
    states.push(doc.clone());
    for op in version.ops.iter().rev() {
        kame_core::edit::apply_inverse(&mut doc, op);
        states.push(doc.clone());
    }
    states.reverse();
    states
}

/// A real stretch of a document's history, with each changed block's edits dealt to one of two
/// imaginary machines. Merging them has a right answer, because the document really did end up
/// a certain way. Prints counts only, so it is safe to run on private writing.
fn trial(file: &Path, rounds: usize) -> Result<()> {
    use std::collections::HashMap;
    let version = Version::read(file)?;
    let states = states_of(&version);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut below = |limit: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % limit.max(1) as u64) as usize
    };
    let (mut exact, mut text_exact, mut noted, mut skipped) = (0, 0, 0, 0);
    let (mut changed_total, mut notes_total) = (0usize, 0usize);
    let (
        mut order_only,
        mut content_differs,
        mut extra_blocks,
        mut missing_blocks,
        mut different_text,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for _ in 0..rounds {
        let from = below(states.len());
        let to = (from + 1 + below(800)).min(states.len() - 1);
        let (base, end) = (&states[from], &states[to]);
        let base_at: HashMap<_, _> = base.blocks.iter().map(|block| (block.id, block)).collect();
        let end_at: HashMap<_, _> = end.blocks.iter().map(|block| (block.id, block)).collect();
        let mut sides: HashMap<uuid::Uuid, bool> = HashMap::new();
        let mut ids: Vec<_> = base
            .blocks
            .iter()
            .chain(&end.blocks)
            .map(|block| block.id)
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            if base_at.get(&id) != end_at.get(&id) {
                sides.insert(id, below(2) == 0);
            }
        }
        if sides.len() < 2 {
            skipped += 1;
            continue;
        }
        changed_total += sides.len();
        let view = |side: bool| -> Document {
            let takes = |id: &uuid::Uuid| sides.get(id) == Some(&side);
            let mut blocks: Vec<Block> = end
                .blocks
                .iter()
                .filter_map(|block| {
                    if takes(&block.id) {
                        Some(block.clone())
                    } else {
                        base_at.get(&block.id).map(|was| (*was).clone())
                    }
                })
                .collect();
            for (at, block) in base.blocks.iter().enumerate() {
                if !end_at.contains_key(&block.id) && !takes(&block.id) {
                    let after = base.blocks[..at]
                        .iter()
                        .rev()
                        .find_map(|before| blocks.iter().position(|found| found.id == before.id));
                    blocks.insert(after.map_or(0, |at| at + 1), block.clone());
                }
            }
            let block_of = |note: &kame_core::model::Annotation| match note.anchor {
                Anchor::Block(id) => id,
                Anchor::Range { block_id, .. } => block_id,
            };
            let mut annotations = Vec::new();
            let notes = base.annotations.iter().chain(&end.annotations);
            let mut seen = std::collections::HashSet::new();
            for note in notes {
                if !seen.insert(note.id) {
                    continue;
                }
                let home = block_of(note);
                let from_end = end.annotations.iter().find(|found| found.id == note.id);
                let from_base = base.annotations.iter().find(|found| found.id == note.id);
                let chosen = match sides.get(&home) {
                    Some(owner) if *owner == side => from_end,
                    Some(_) => from_base,
                    None => from_end
                        .or(from_base)
                        .filter(|_| from_end == from_base)
                        .or(from_end),
                };
                if let Some(chosen) = chosen {
                    annotations.push(chosen.clone());
                }
            }
            Document {
                id: end.id,
                title: end.title.clone(),
                blocks,
                annotations,
            }
        };
        let (one, other) = (view(true), view(false));
        let merged = kame_sync::merge::merge(
            Some(base),
            kame_sync::merge::Side {
                doc: &one,
                label: "one",
                when: 2,
            },
            kame_sync::merge::Side {
                doc: &other,
                label: "other",
                when: 1,
            },
        );
        notes_total += merged.notes.len();
        if !merged.notes.is_empty() {
            noted += 1;
        }
        let texts = |doc: &Document| {
            doc.blocks
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<_>>()
        };
        let same_text = texts(&merged.doc) == texts(end);
        text_exact += usize::from(same_text);
        let sorted = |doc: &Document| {
            let mut found = doc
                .blocks
                .iter()
                .map(|block| (block.id, block.text.clone()))
                .collect::<Vec<_>>();
            found.sort();
            found
        };
        let same_blocks = sorted(&merged.doc) == sorted(end);
        order_only += usize::from(same_blocks && !same_text);
        if !same_blocks {
            let (got, want) = (sorted(&merged.doc), sorted(end));
            let got_ids: std::collections::HashSet<_> = got.iter().map(|found| found.0).collect();
            let want_ids: std::collections::HashSet<_> = want.iter().map(|found| found.0).collect();
            extra_blocks += got_ids.difference(&want_ids).count();
            missing_blocks += want_ids.difference(&got_ids).count();
            different_text += got
                .iter()
                .filter(|found| {
                    want.iter()
                        .any(|other| other.0 == found.0 && other.1 != found.1)
                })
                .count();
            content_differs += 1;
        }
        let mut got = merged.doc.annotations.clone();
        let mut want = end.annotations.clone();
        got.sort_by_key(|note| note.id);
        want.sort_by_key(|note| note.id);
        exact += usize::from(merged.doc.blocks == end.blocks && got == want);
    }
    let ran = rounds - skipped;
    println!(
        "{}: {} ops, {ran} rounds ({skipped} too small), {:.1} changed blocks a round",
        file.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        version.ops.len(),
        changed_total as f64 / ran.max(1) as f64
    );
    println!("  merged text identical to what really happened: {text_exact}/{ran}");
    println!("  same blocks and words in a different order: {order_only}/{ran}");
    println!(
        "  content differs: {content_differs}/{ran} ({extra_blocks} extra blocks, {missing_blocks} missing, {different_text} with different text)"
    );
    println!("  whole document identical, notes and formatting too: {exact}/{ran}");
    println!("  rounds with a note for the author: {noted}/{ran}, {notes_total} notes in all");
    Ok(())
}

/// One machine in a real test: read its working copy, maybe add a sentence to the first block,
/// sync with the folder, and keep the result. The state file is outside the synced folder.
fn step(folder: &str, device: &str, state: &str, words: Option<&str>) -> Result<()> {
    let state_path = PathBuf::from(state);
    let (mut doc, last): (Document, Option<String>) = match std::fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(_) => (shared_start(), None),
    };
    if let Some(words) = words {
        // BLOCK:WORDS picks a paragraph, and plain WORDS means the first.
        let (at, words) = match words.split_once(':') {
            Some((at, rest)) if at.parse::<usize>().is_ok() => (at.parse::<usize>()?, rest),
            _ => (0, words),
        };
        let block = doc.blocks.get_mut(at).ok_or("no such paragraph")?;
        if !block.text.is_empty() {
            block.text.push(' ');
        }
        block.text.push_str(words);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let replica = Replica {
        folder: PathBuf::from(folder),
        device: device.to_string(),
    };
    let synced = replica.sync(&doc, last.as_deref(), now)?;
    std::fs::write(
        &state_path,
        serde_json::to_vec(&(&synced.doc, &synced.head))?,
    )?;
    let store = Store::load(Path::new(folder))?;
    println!(
        "{device}: head {} heads {} revisions {} written {} notes {} digest {}",
        &synced.head[..12],
        store.heads().len(),
        store.revision_count(),
        synced.written,
        synced.doc.annotations.len(),
        &kame_sync::merge::document_digest(&synced.doc)[..12],
    );
    for block in &synced.doc.blocks {
        println!("  | {}", block.text);
    }
    for note in &synced.doc.annotations {
        println!("  note: {}", note.body);
    }
    Ok(())
}

fn enable(folder: &Path) -> Result<()> {
    match kame_sync::logs::enable(folder)? {
        Some(lines) => println!(
            "logs are on for {}. Add these to .stignore on every machine, if they aren't there:\n{}",
            folder.display(),
            lines.join("\n")
        ),
        None => println!(
            "logs are on for {}. Stop its .kame, .kame-wal and .kame-shm files syncing on every machine.",
            folder.display()
        ),
    }
    Ok(())
}

/// Syncs one document, optionally after adding WORDS to paragraph BLOCK. Prints counts only, unless
/// asked to add words, since a document may be private.
fn sync_file(file: &Path, device: &str, words: Option<&str>) -> Result<()> {
    kame_sync::logs::build_from_logs(file)?;
    let mut session = kame_core::session::Session::open(file).map_err(|err| err.to_string())?;
    if let Some((block, words)) = words.and_then(|words| words.split_once(':')) {
        let target = session
            .document()
            .blocks
            .get(block.parse::<usize>()?)
            .ok_or("no such paragraph")?;
        let cursor = kame_core::edit::Cursor {
            block_id: target.id,
            offset: target.text.len(),
        };
        session.set_caret(cursor);
        session.insert(words);
    }
    let mut logged = kame_sync::logs::Logged::open(&session, device)
        .ok_or("this document isn't in a folder with logs turned on")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let outcome = logged.sync(&mut session, now)?;
    println!(
        "{device}: written {} merged {} notes {} from {} digest {}",
        outcome.written,
        outcome.merged,
        outcome.notes,
        outcome.from.as_deref().unwrap_or("-"),
        &kame_sync::merge::document_digest(session.document())[..12],
    );
    Ok(())
}

/// Every machine in a test starts from this, so they share their blocks' ids.
fn shared_start() -> Document {
    Document {
        id: uuid::Uuid::nil(),
        title: "Sync test".to_string(),
        blocks: (1..=3)
            .map(|id| Block {
                id: uuid::Uuid::from_u128(id),
                kind: BlockKind::Paragraph,
                text: String::new(),
                spans: Vec::new(),
            })
            .collect(),
        annotations: Vec::new(),
    }
}
