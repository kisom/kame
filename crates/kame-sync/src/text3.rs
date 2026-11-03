//! Three-way merge of a block's text, with the sentence as the unit of conflict.
//!
//! Git merges by line, since a line is what programmers edit. Prose has no lines, and two edits
//! to one sentence that merge cleanly at the word level can still make nonsense, so edits on both
//! sides of one sentence are a conflict, and edits to different sentences never are.
//! Offsets are bytes, as everywhere in kame.

use std::ops::Range;

/// What became of a stretch of a source text in the merged text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Kept as it was.
    Kept,
    /// Replaced by the other side's edit, which was the only edit there.
    Replaced,
    /// Lost a conflict, or the shared text of one. It lives on in a note.
    Lost,
}

#[derive(Clone, Debug)]
struct Segment {
    source: Range<usize>,
    merged: Range<usize>,
    fate: Fate,
}

/// Takes offsets in one source text to offsets in the merged text.
#[derive(Clone, Debug, Default)]
pub struct OffsetMap {
    segments: Vec<Segment>,
    merged_len: usize,
}

impl OffsetMap {
    fn push(&mut self, source: Range<usize>, merged: Range<usize>, fate: Fate) {
        self.merged_len = merged.end;
        if !source.is_empty() {
            self.segments.push(Segment {
                source,
                merged,
                fate,
            });
        }
    }

    /// Where something starting at `offset` starts now.
    pub fn start(&self, offset: usize) -> usize {
        for segment in &self.segments {
            if segment.source.start <= offset && offset < segment.source.end {
                return match segment.fate {
                    Fate::Kept => segment.merged.start + (offset - segment.source.start),
                    _ => segment.merged.start,
                };
            }
        }
        self.merged_len
    }

    /// Where something ending at `offset` ends now.
    pub fn end(&self, offset: usize) -> usize {
        if offset == 0 {
            return 0;
        }
        for segment in &self.segments {
            if segment.source.start < offset && offset <= segment.source.end {
                return match segment.fate {
                    Fate::Kept => segment.merged.start + (offset - segment.source.start),
                    _ => segment.merged.end,
                };
            }
        }
        self.merged_len
    }

    /// Where a span of formatting goes. `None` if it was on words that lost a conflict: they are
    /// not in the text, and it must not spread across the words that won.
    pub fn span(&self, range: Range<usize>) -> Option<Range<usize>> {
        let lost = self.segments.iter().any(|segment| {
            segment.fate == Fate::Lost
                && segment.source.start < range.end
                && range.start < segment.source.end
        });
        if lost {
            return None;
        }
        let (start, end) = (self.start(range.start), self.end(range.end));
        (start < end).then_some(start..end)
    }
}

/// A sentence edited on both sides. The first side's words are in the text at `range`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextConflict {
    pub range: Range<usize>,
    pub base: String,
    pub theirs: String,
}

#[derive(Clone, Debug)]
pub struct Merged3 {
    pub text: String,
    pub base_map: OffsetMap,
    pub ours_map: OffsetMap,
    pub theirs_map: OffsetMap,
    pub conflicts: Vec<TextConflict>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '’'
}

/// Splits text into runs of word characters, runs of whitespace, and single other characters.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = Vec::new();
    let mut last: Option<(bool, bool)> = None;
    for (index, c) in text.char_indices() {
        let class = (is_word(c), c.is_whitespace());
        let joins = last == Some(class) && (class.0 || class.1);
        match found.last_mut() {
            Some(token) if joins => token.end = index + c.len_utf8(),
            _ => found.push(index..index + c.len_utf8()),
        }
        last = Some(class);
    }
    found
}

/// The longest common subsequence, as the side's token for each base token that has one.
fn align(base: &[&str], side: &[&str]) -> Vec<Option<usize>> {
    let mut matched = vec![None; base.len()];
    let mut front = 0;
    while front < base.len() && front < side.len() && base[front] == side[front] {
        matched[front] = Some(front);
        front += 1;
    }
    let (mut base_end, mut side_end) = (base.len(), side.len());
    while base_end > front && side_end > front && base[base_end - 1] == side[side_end - 1] {
        base_end -= 1;
        side_end -= 1;
        matched[base_end] = Some(side_end);
    }
    let (rows, columns) = (base_end - front, side_end - front);
    // A paragraph is far below this. Past it the middle counts as rewritten.
    if rows == 0 || columns == 0 || rows.saturating_mul(columns) > 4_000_000 {
        return matched;
    }
    let width = columns + 1;
    let mut table = vec![0u32; (rows + 1) * width];
    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            table[row * width + column] = if base[front + row] == side[front + column] {
                table[(row + 1) * width + column + 1] + 1
            } else {
                table[(row + 1) * width + column].max(table[row * width + column + 1])
            };
        }
    }
    let (mut row, mut column) = (0, 0);
    while row < rows && column < columns {
        if base[front + row] == side[front + column] {
            matched[front + row] = Some(front + column);
            row += 1;
            column += 1;
        } else if table[(row + 1) * width + column] >= table[row * width + column + 1] {
            row += 1;
        } else {
            column += 1;
        }
    }
    matched
}

/// Which sentence each base token is in. The space after a sentence belongs to it, and a line
/// ending ends one, which makes a code block merge by line.
fn sentences(text: &str, spans: &[Range<usize>]) -> Vec<usize> {
    let mut current = 0;
    let mut found = Vec::with_capacity(spans.len());
    let mut after_end = false;
    for span in spans {
        let token = &text[span.clone()];
        found.push(current);
        let space = token.chars().all(char::is_whitespace);
        if space && (after_end || token.contains('\n')) {
            current += 1;
        }
        if !space {
            after_end = matches!(token, "." | "!" | "?" | "…")
                || (after_end && matches!(token, "\"" | "”" | "’" | ")"));
        }
    }
    found
}

/// Tokens of the three texts between two anchors all three share.
#[derive(Clone, Debug)]
struct Item {
    base: Range<usize>,
    ours: Range<usize>,
    theirs: Range<usize>,
}

fn byte_range(spans: &[Range<usize>], tokens: &Range<usize>, len: usize) -> Range<usize> {
    let at = |index: usize| spans.get(index).map_or(len, |span| span.start);
    at(tokens.start)..at(tokens.end)
}

/// Merges `ours` and `theirs` against `base`. A conflict's text comes from `ours`, so a caller
/// wanting the same result either way round must pass the same side first.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Merged3 {
    let (base_spans, ours_spans, theirs_spans) = (tokens(base), tokens(ours), tokens(theirs));
    let b: Vec<&str> = base_spans.iter().map(|span| &base[span.clone()]).collect();
    let o: Vec<&str> = ours_spans.iter().map(|span| &ours[span.clone()]).collect();
    let t: Vec<&str> = theirs_spans
        .iter()
        .map(|span| &theirs[span.clone()])
        .collect();
    let ours_match = align(&b, &o);
    let theirs_match = align(&b, &t);

    // Between tokens all three share lie the stretches where something changed.
    let mut items: Vec<Item> = Vec::new();
    let (mut base_at, mut ours_at, mut theirs_at) = (0, 0, 0);
    for index in 0..b.len() {
        if let (Some(our), Some(their)) = (ours_match[index], theirs_match[index]) {
            if index > base_at || our > ours_at || their > theirs_at {
                items.push(Item {
                    base: base_at..index,
                    ours: ours_at..our,
                    theirs: theirs_at..their,
                });
            }
            items.push(Item {
                base: index..index + 1,
                ours: our..our + 1,
                theirs: their..their + 1,
            });
            base_at = index + 1;
            ours_at = our + 1;
            theirs_at = their + 1;
        }
    }
    if base_at < b.len() || ours_at < o.len() || theirs_at < t.len() {
        items.push(Item {
            base: base_at..b.len(),
            ours: ours_at..o.len(),
            theirs: theirs_at..t.len(),
        });
    }

    let of_base = |range: &Range<usize>| &base[byte_range(&base_spans, range, base.len())];
    let of_ours = |range: &Range<usize>| &ours[byte_range(&ours_spans, range, ours.len())];
    let of_theirs = |range: &Range<usize>| &theirs[byte_range(&theirs_spans, range, theirs.len())];

    // Which sentences each side changed. An edit both made is no disagreement.
    let sentence_of = sentences(base, &base_spans);
    let sentence_at = |token: usize| {
        sentence_of
            .get(token)
            .or(sentence_of.last())
            .copied()
            .unwrap_or(0)
    };
    let count = sentence_of.last().map_or(1, |last| last + 1);
    let (mut ours_changed, mut theirs_changed) = (vec![false; count], vec![false; count]);
    for item in &items {
        let (was, mine, other) = (
            of_base(&item.base),
            of_ours(&item.ours),
            of_theirs(&item.theirs),
        );
        if mine == other {
            continue;
        }
        let sentence = sentence_at(item.base.start);
        ours_changed[sentence] |= mine != was;
        theirs_changed[sentence] |= other != was;
    }
    let conflicted = |sentence: usize| ours_changed[sentence] && theirs_changed[sentence];

    let mut text = String::new();
    let (mut base_map, mut ours_map, mut theirs_map) = (
        OffsetMap::default(),
        OffsetMap::default(),
        OffsetMap::default(),
    );
    let mut conflicts = Vec::new();
    let mut index = 0;
    while index < items.len() {
        let sentence = sentence_at(items[index].base.start);
        let mut last = index;
        if conflicted(sentence) {
            while last + 1 < items.len() && sentence_at(items[last + 1].base.start) == sentence {
                last += 1;
            }
        }
        let item = Item {
            base: items[index].base.start..items[last].base.end,
            ours: items[index].ours.start..items[last].ours.end,
            theirs: items[index].theirs.start..items[last].theirs.end,
        };
        index = last + 1;
        let (base_bytes, ours_bytes, theirs_bytes) = (
            byte_range(&base_spans, &item.base, base.len()),
            byte_range(&ours_spans, &item.ours, ours.len()),
            byte_range(&theirs_spans, &item.theirs, theirs.len()),
        );
        let (was, mine, other) = (
            &base[base_bytes.clone()],
            &ours[ours_bytes.clone()],
            &theirs[theirs_bytes.clone()],
        );
        let start = text.len();
        if mine == other {
            text.push_str(mine);
            let merged = start..text.len();
            let fate = if was == mine {
                Fate::Kept
            } else {
                Fate::Replaced
            };
            base_map.push(base_bytes, merged.clone(), fate);
            ours_map.push(ours_bytes, merged.clone(), Fate::Kept);
            theirs_map.push(theirs_bytes, merged, Fate::Kept);
        } else if conflicted(sentence) {
            text.push_str(mine);
            let merged = start..text.len();
            conflicts.push(TextConflict {
                range: merged.clone(),
                base: was.to_string(),
                theirs: other.to_string(),
            });
            base_map.push(base_bytes, merged.clone(), Fate::Lost);
            ours_map.push(ours_bytes, merged.clone(), Fate::Kept);
            theirs_map.push(theirs_bytes, merged, Fate::Lost);
        } else if mine != was {
            text.push_str(mine);
            let merged = start..text.len();
            base_map.push(base_bytes, merged.clone(), Fate::Replaced);
            ours_map.push(ours_bytes, merged.clone(), Fate::Kept);
            theirs_map.push(theirs_bytes, merged, Fate::Replaced);
        } else {
            text.push_str(other);
            let merged = start..text.len();
            base_map.push(base_bytes, merged.clone(), Fate::Replaced);
            ours_map.push(ours_bytes, merged.clone(), Fate::Replaced);
            theirs_map.push(theirs_bytes, merged, Fate::Kept);
        }
    }
    Merged3 {
        text,
        base_map,
        ours_map,
        theirs_map,
        conflicts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "One fish swims. Two birds fly. Three cats sleep.";

    #[test]
    fn edits_to_different_sentences_both_land() {
        let merged = merge3(
            BASE,
            "One red fish swims. Two birds fly. Three cats sleep.",
            "One fish swims. Two birds fly. Three cats sleep soundly.",
        );
        assert_eq!(
            merged.text,
            "One red fish swims. Two birds fly. Three cats sleep soundly."
        );
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn edits_to_one_sentence_conflict_though_their_words_differ() {
        let merged = merge3(
            BASE,
            "One fish swims. Two old birds fly. Three cats sleep.",
            "One fish swims. Two birds fly south. Three cats sleep.",
        );
        assert_eq!(
            merged.text,
            "One fish swims. Two old birds fly. Three cats sleep."
        );
        assert_eq!(merged.conflicts.len(), 1);
        let conflict = &merged.conflicts[0];
        assert_eq!(&merged.text[conflict.range.clone()], "Two old birds fly. ");
        assert_eq!(conflict.theirs, "Two birds fly south. ");
        assert_eq!(conflict.base, "Two birds fly. ");
    }

    #[test]
    fn the_same_edit_on_both_sides_is_no_conflict() {
        let same = "One fish swims. Two birds soar. Three cats sleep.";
        let merged = merge3(BASE, same, same);
        assert_eq!(merged.text, same);
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn deleting_a_sentence_the_other_side_edited_conflicts() {
        let merged = merge3(
            BASE,
            "One fish swims. Three cats sleep.",
            "One fish swims. Two birds fly away. Three cats sleep.",
        );
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].theirs.contains("fly away"));
    }

    #[test]
    fn multibyte_text_stays_on_character_boundaries() {
        let merged = merge3(
            "Un été ici. Déjà là.",
            "Un bel été ici. Déjà là.",
            "Un été ici. Déjà là — enfin.",
        );
        assert_eq!(merged.text, "Un bel été ici. Déjà là — enfin.");
        let at = merged.ours_map.start("Un bel é".len());
        assert!(merged.text.is_char_boundary(at));
    }

    #[test]
    fn offsets_follow_the_text() {
        let merged = merge3(
            BASE,
            "A few words first. One fish swims. Two birds fly. Three cats sleep.",
            "One fish swims. Two birds fly. Three cats sleep soundly.",
        );
        let cats = BASE.find("cats").unwrap();
        let at = merged.base_map.start(cats);
        assert_eq!(&merged.text[at..at + 4], "cats");
        let theirs_cats = merged.theirs_map.span(cats..cats + 4).unwrap();
        assert_eq!(&merged.text[theirs_cats], "cats");
    }

    #[test]
    fn formatting_on_lost_words_does_not_spread() {
        let merged = merge3(
            BASE,
            "One fish swims. Two old birds fly. Three cats sleep.",
            "One fish swims. Two birds fly south. Three cats sleep.",
        );
        let theirs = "One fish swims. Two birds fly south. Three cats sleep.";
        let south = theirs.find("south").unwrap();
        assert_eq!(merged.theirs_map.span(south..south + 5), None);
    }

    #[test]
    fn a_code_block_merges_by_line() {
        let merged = merge3(
            "a = 1\nb = 2\nc = 3\n",
            "a = 10\nb = 2\nc = 3\n",
            "a = 1\nb = 2\nc = 30\n",
        );
        assert_eq!(merged.text, "a = 10\nb = 2\nc = 30\n");
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn no_shared_text_is_one_conflict() {
        let merged = merge3("", "Mine.", "Theirs.");
        assert_eq!(merged.text, "Mine.");
        assert_eq!(merged.conflicts[0].theirs, "Theirs.");
    }
}
