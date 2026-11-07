//! Inputs for fuzzing, each worked out from a seed, so that a failure can be run again exactly.
//! There's no fuzzing crate: a seeded generator and some ways of damaging bytes and JSON are
//! enough to start from the shapes kame really writes and break them.
//!
//! A target is a function of a seed. [`short`] runs it over a fixed set of seeds, as part of the
//! ordinary `cargo test`. [`long`] runs it for as long as asked, from the environment:
//!
//! - `KAME_FUZZ_ITERATIONS`, how many seeds, 10,000 if it isn't set;
//! - `KAME_FUZZ_SEED`, the first, taken from the clock if it isn't set.
//!
//! Either way, a seed that fails is printed, with how to run it alone.

use serde_json::{Map, Number, Value};

/// A small, fast generator (splitmix64). Not for anything but tests.
#[derive(Clone, Debug)]
pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Random {
        Random(seed ^ 0x6A09_E667_F3BC_C908)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `limit`, or 0 when `limit` is 0.
    pub fn below(&mut self, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        (self.next_u64() % limit as u64) as usize
    }

    /// A number from `low` to `high`, both included.
    pub fn between(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    /// True one time in `n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// Some text of up to `most` characters, drawn from ASCII words, accented and other scripts,
    /// emoji, combining marks, controls and the characters that matter in JSON and Markdown.
    pub fn text(&mut self, most: usize) -> String {
        const PIECES: &[&str] = &[
            "a",
            "word",
            " ",
            " ",
            "  ",
            ".",
            ",",
            "\n",
            "\t",
            "é",
            "ü",
            "ß",
            "—",
            "“",
            "”",
            "…",
            "🙂",
            "🐢",
            "👩‍👩‍👧",
            "\u{301}",
            "\u{200b}",
            "\u{feff}",
            "\u{0}",
            "\u{1}",
            "\u{7f}",
            "日本",
            "עברית",
            "ﷺ",
            "\"",
            "\\",
            "{",
            "}",
            "[",
            "]",
            "#",
            "*",
            "_",
            "`",
            ">",
            "|",
            "<",
            "&",
            "[^1]",
            "http://x.y",
            "javascript:",
            "~/",
            "../",
            "\r\n",
            "\u{2028}",
        ];
        let count = self.below(most + 1);
        let mut text = String::new();
        for _ in 0..count {
            text.push_str(self.pick(PIECES));
        }
        text
    }
}

/// Prints the seed of a target that panics, as the panic unwinds through it.
pub struct OnFailure<'a> {
    pub target: &'a str,
    pub seed: u64,
}

impl Drop for OnFailure<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "fuzz target {} failed at seed {}. To run it alone: \
                 KAME_FUZZ_SEED={} KAME_FUZZ_ITERATIONS=1 cargo test {} -- --ignored --nocapture",
                self.target, self.seed, self.seed, self.target
            );
        }
    }
}

fn run(target: &str, seeds: impl Iterator<Item = u64>, mut each: impl FnMut(u64)) -> u64 {
    let mut count = 0;
    for seed in seeds {
        let _guard = OnFailure { target, seed };
        each(seed);
        count += 1;
    }
    count
}

/// Runs `each` over the seeds 0 to `count - 1`, for the ordinary test run.
pub fn short(target: &str, count: u64, each: impl FnMut(u64)) {
    run(target, 0..count, each);
}

/// Runs `each` over as many seeds as `KAME_FUZZ_ITERATIONS` says, from `KAME_FUZZ_SEED`, and
/// says how many it ran.
pub fn long(target: &str, each: impl FnMut(u64)) {
    let number = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().parse::<u64>().expect(name))
    };
    let iterations = number("KAME_FUZZ_ITERATIONS").unwrap_or(10_000);
    let first = number("KAME_FUZZ_SEED").unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |since| since.as_secs())
    });
    eprintln!("fuzz target {target}: {iterations} seeds from {first}");
    let started = std::time::Instant::now();
    let ran = run(target, (0..iterations).map(|n| first.wrapping_add(n)), each);
    eprintln!(
        "fuzz target {target}: ran {ran} seeds from {first} in {:.0} s, none failed",
        started.elapsed().as_secs_f64()
    );
}

/// Bytes damaged in one of the ways a disk, a sync tool or a program with a bug damages them:
/// bits flipped, cut short, a stretch lost or repeated, other bytes put in, or `other` spliced on.
pub fn damage_bytes(random: &mut Random, bytes: &mut Vec<u8>, other: &[u8]) {
    const BYTES: &[u8] = b"\0\xff\xc3\x80\xe2\xf0\x9f\"\\{}[],:0123456789-+.eE \n\rtrufalsn";
    let at = |random: &mut Random, len: usize| random.below(len + 1);
    match random.below(8) {
        0 => {
            for _ in 0..random.between(1, 8) {
                if !bytes.is_empty() {
                    let i = random.below(bytes.len());
                    bytes[i] ^= 1 << random.below(8);
                }
            }
        }
        1 => {
            for _ in 0..random.between(1, 4) {
                if !bytes.is_empty() {
                    let i = random.below(bytes.len());
                    bytes[i] = *random.pick(BYTES);
                }
            }
        }
        2 => {
            let cut = at(random, bytes.len());
            bytes.truncate(cut);
        }
        3 => {
            let start = at(random, bytes.len());
            let end = (start + random.below(64)).min(bytes.len());
            bytes.drain(start..end);
        }
        4 => {
            let start = at(random, bytes.len());
            let end = (start + random.below(64)).min(bytes.len());
            let copy = bytes[start..end].to_vec();
            let into = at(random, bytes.len());
            bytes.splice(into..into, copy);
        }
        5 => {
            let into = at(random, bytes.len());
            let fresh: Vec<u8> = (0..random.between(1, 16))
                .map(|_| *random.pick(BYTES))
                .collect();
            bytes.splice(into..into, fresh);
        }
        6 => {
            let keep = at(random, bytes.len());
            let from = at(random, other.len());
            bytes.truncate(keep);
            bytes.extend_from_slice(&other[from..]);
        }
        _ => *bytes = random_bytes(random, 256),
    }
}

/// Up to `most` bytes of nothing in particular.
pub fn random_bytes(random: &mut Random, most: usize) -> Vec<u8> {
    (0..random.below(most + 1))
        .map(|_| random.next_u64() as u8)
        .collect()
}

/// Whether `text` starts with a UUID, written as kame writes one: lowercase and hyphenated.
fn starts_with_uuid(text: &[u8]) -> bool {
    text.len() >= 36
        && text[..36].iter().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

/// `text` with each UUID in it replaced by one that depends only on the order the UUIDs come in,
/// across every call with the same `seen`, so that what kame's commands made with random ids is
/// the same input in every run. `seen` holds each UUID met so far, the first being the first.
pub fn fixed_ids(text: &str, seen: &mut Vec<String>) -> String {
    let bytes = text.as_bytes();
    let mut fixed = String::with_capacity(text.len());
    let mut at = 0;
    let mut copied = 0;
    while at < bytes.len() {
        if starts_with_uuid(&bytes[at..]) {
            let id = &text[at..at + 36];
            let index = match seen.iter().position(|known| known == id) {
                Some(index) => index,
                None => {
                    seen.push(id.to_string());
                    seen.len() - 1
                }
            };
            fixed.push_str(&text[copied..at]);
            fixed.push_str(&format!("00000000-0000-4000-8000-{:012x}", index + 1));
            at += 36;
            copied = at;
        } else {
            at += 1;
        }
    }
    fixed.push_str(&text[copied..]);
    fixed
}

/// Where each value in `value` is, as a JSON pointer, the whole first.
pub fn pointers(value: &Value) -> Vec<String> {
    fn walk(value: &Value, at: String, found: &mut Vec<String>) {
        found.push(at.clone());
        match value {
            Value::Object(fields) => {
                for (key, field) in fields {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    walk(field, format!("{at}/{key}"), found);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, format!("{at}/{index}"), found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(value, String::new(), &mut found);
    found
}

/// Every string in `value` that reads as a UUID, so that damage can repeat one or point at one.
fn uuids(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let mut waiting = vec![value];
    while let Some(next) = waiting.pop() {
        match next {
            Value::String(text) if text.len() == 36 && text.matches('-').count() == 4 => {
                found.push(text.clone())
            }
            Value::Object(fields) => waiting.extend(fields.values()),
            Value::Array(items) => waiting.extend(items),
            _ => {}
        }
    }
    found
}

/// A value of the kind that finds the edges of what a reader takes: numbers past what a field
/// holds, kinds and styles real and made up, ids repeated, and every type.
fn edge_value(random: &mut Random, whole: &Value) -> Value {
    let number = |n: i128| -> Value {
        if let Ok(n) = u64::try_from(n) {
            Value::Number(Number::from(n))
        } else {
            Value::Number(Number::from(n as i64))
        }
    };
    match random.below(16) {
        0 => Value::Null,
        1 => Value::Bool(random.one_in(2)),
        2 => number(*random.pick(&[
            0,
            1,
            -1,
            2,
            6,
            7,
            127,
            255,
            256,
            -256,
            65_535,
            65_536,
            u32::MAX as i128,
            u32::MAX as i128 + 1,
            i64::MAX as i128,
            i64::MIN as i128,
            u64::MAX as i128,
        ])),
        3 => Value::Number(Number::from_f64(*random.pick(&[0.5, -1.5, 1e300, 3.0])).unwrap()),
        4 => number(random.below(80) as i128),
        5 => Value::String(random.text(12)),
        6 => Value::String(
            (*random.pick(&[
                "Paragraph",
                "BlockQuote",
                "Rule",
                "Prose",
                "Bold",
                "Italic",
                "Code",
                "Marginal",
                "Note",
                "Footnote",
                "Left",
                "Center",
                "Right",
                "Table",
                "Strike",
                "Bookmark",
                "",
            ]))
            .to_string(),
        ),
        7 => {
            let level = edge_number(random);
            serde_json::json!({ "Heading": { "level": level } })
        }
        8 => {
            let depth = edge_number(random);
            serde_json::json!({ "ListItem": { "ordered": random.one_in(2), "depth": depth } })
        }
        9 => serde_json::json!({ "Link": { "url": random.text(6) } }),
        10 => serde_json::json!({ "Image": { "path": random.text(6), "align": "Center" } }),
        11 => {
            let ids = uuids(whole);
            if ids.is_empty() {
                Value::String("00000000-0000-0000-0000-000000000001".to_string())
            } else {
                Value::String(random.pick(&ids).clone())
            }
        }
        12 => Value::String(format!(
            "{:08x}-0000-4000-8000-{:012x}",
            random.next_u64() as u32,
            random.next_u64() & 0xffff_ffff_ffff
        )),
        13 => Value::Array(Vec::new()),
        14 => Value::Object(Map::new()),
        // A range anchor, on a block that's there or one that isn't.
        _ => {
            let mut ids = uuids(whole);
            ids.push("x".to_string());
            let block_id = random.pick(&ids).clone();
            let (start, end) = (edge_number(random), edge_number(random));
            serde_json::json!({ "block_id": block_id, "start": start, "end": end })
        }
    }
}

/// A number from the edges: past a byte, negative, huge, or a small offset.
pub fn edge_number(random: &mut Random) -> Value {
    match random.below(6) {
        0 => Value::from(*random.pick(&[256u64, 300, 1000, 65_536, u32::MAX as u64 + 1, u64::MAX])),
        1 => Value::from(*random.pick(&[-1i64, -255, -256, i64::MIN])),
        2 => Value::from(*random.pick(&[0u64, 1, 2, 3, 5, 6, 7, 9, 255])),
        3 => Value::from(1.5),
        _ => Value::from(random.below(40) as u64),
    }
}

/// A value from `donor`, anywhere in it.
fn piece_of(random: &mut Random, donor: &Value) -> Value {
    let at = pointers(donor);
    donor
        .pointer(random.pick(&at))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Damages `value` once, somewhere: a field taken out, added or renamed, an item taken out,
/// repeated or moved, a value of another type or out of range put in, text changed, a piece of
/// `donor` put in, or the value buried deep in arrays.
pub fn damage_json(random: &mut Random, value: &mut Value, donor: &Value) {
    let whole = value.clone();
    let at = pointers(value);
    let pointer = random.pick(&at).clone();
    let Some(target) = value.pointer_mut(&pointer) else {
        return;
    };
    match random.below(12) {
        0 | 1 => *target = edge_value(random, &whole),
        2 => *target = piece_of(random, donor),
        3 => match target {
            Value::Number(_) => *target = edge_number(random),
            Value::String(text) => {
                let mut cut = random.below(text.len() + 1);
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                match random.below(3) {
                    0 => text.truncate(cut),
                    1 => text.insert_str(cut, &random.text(4)),
                    _ => *text = random.text(40),
                }
            }
            Value::Bool(flag) => *flag = !*flag,
            _ => *target = edge_value(random, &whole),
        },
        4 | 5 => match target {
            Value::Object(fields) if !fields.is_empty() => {
                let keys: Vec<String> = fields.keys().cloned().collect();
                let key = random.pick(&keys).clone();
                match random.below(3) {
                    0 => {
                        fields.remove(&key);
                    }
                    1 => {
                        let field = fields.remove(&key).unwrap();
                        fields.insert(format!("{key}_"), field);
                    }
                    _ => {
                        fields.insert(random.text(3), edge_value(random, &whole));
                    }
                }
            }
            Value::Array(items) if !items.is_empty() => {
                let i = random.below(items.len());
                match random.below(4) {
                    0 => {
                        items.remove(i);
                    }
                    1 => {
                        let copy = items[i].clone();
                        items.insert(random.below(items.len() + 1), copy);
                    }
                    2 => {
                        let j = random.below(items.len());
                        items.swap(i, j);
                    }
                    _ => items.reverse(),
                }
            }
            _ => *target = edge_value(random, &whole),
        },
        6 => {
            if let Value::Array(items) = target {
                items.insert(random.below(items.len() + 1), piece_of(random, donor));
            } else {
                *target = Value::Array(vec![target.clone()]);
            }
        }
        7 => {
            let mut deep = target.clone();
            for _ in 0..random.between(1, 200) {
                deep = if random.one_in(2) {
                    Value::Array(vec![deep])
                } else {
                    serde_json::json!({ "x": deep })
                };
            }
            *target = deep;
        }
        8 => {
            if let Value::Object(fields) = target {
                fields.clear();
            } else if let Value::Array(items) = target {
                items.clear();
            } else {
                *target = Value::Null;
            }
        }
        // Wrapped as an enum variant of a name it already holds, or taken out of one.
        9 => {
            if let Value::Object(fields) = target {
                if fields.len() == 1 {
                    let inner = fields.values().next().unwrap().clone();
                    *target = inner;
                    return;
                }
            }
            *target = serde_json::json!({ "Heading": target.clone() });
        }
        _ => {
            if let Value::Array(items) = target {
                if items.len() > 1 {
                    let i = random.below(items.len());
                    let j = random.below(items.len());
                    items.swap(i, j);
                    return;
                }
            }
            *target = edge_value(random, &whole);
        }
    }
}

/// `json` damaged `times` times as JSON, or `None` if it doesn't parse as JSON.
pub fn damaged_json(
    random: &mut Random,
    json: &str,
    donor: &Value,
    times: usize,
) -> Option<String> {
    let mut value: Value = serde_json::from_str(json).ok()?;
    for _ in 0..times {
        damage_json(random, &mut value, donor);
    }
    serde_json::to_string(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_gives_the_same_numbers_each_time() {
        let mut one = Random::new(7);
        let mut again = Random::new(7);
        let mut other = Random::new(8);
        let (a, b, c): (Vec<u64>, Vec<u64>, Vec<u64>) = (
            (0..5).map(|_| one.next_u64()).collect(),
            (0..5).map(|_| again.next_u64()).collect(),
            (0..5).map(|_| other.next_u64()).collect(),
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn damaging_json_the_same_way_gives_the_same_json() {
        let json = r#"{"a":[1,2,{"b":"c"}],"d":"00000000-0000-0000-0000-000000000001"}"#;
        let donor: Value = serde_json::from_str(json).unwrap();
        for seed in 0..200 {
            let one = damaged_json(&mut Random::new(seed), json, &donor, 3);
            let again = damaged_json(&mut Random::new(seed), json, &donor, 3);
            assert_eq!(one, again);
        }
    }

    #[test]
    fn fixed_ids_depend_only_on_their_order() {
        let mut seen = Vec::new();
        let one = fixed_ids(
            r#"["7d3c0b5e-1f2a-4b6c-9d8e-0a1b2c3d4e5f","é","f0e1d2c3-b4a5-4968-8776-655443322110","7d3c0b5e-1f2a-4b6c-9d8e-0a1b2c3d4e5f"]"#,
            &mut seen,
        );
        assert_eq!(
            one,
            r#"["00000000-0000-4000-8000-000000000001","é","00000000-0000-4000-8000-000000000002","00000000-0000-4000-8000-000000000001"]"#
        );
        let two = fixed_ids("f0e1d2c3-b4a5-4968-8776-655443322110 and more", &mut seen);
        assert_eq!(two, "00000000-0000-4000-8000-000000000002 and more");
    }

    #[test]
    fn pointers_name_every_value() {
        let value = serde_json::json!({"a/b": [1, {"~": 2}]});
        let found = pointers(&value);
        assert_eq!(found, ["", "/a~1b", "/a~1b/0", "/a~1b/1", "/a~1b/1/~0"]);
        for pointer in found {
            assert!(value.pointer(&pointer).is_some(), "{pointer}");
        }
    }
}
