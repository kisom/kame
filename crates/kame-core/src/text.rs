//! Moving through text by character and by word, in byte offsets.

/// Previous character boundary before `offset`, or `None` at the start.
pub fn prev_boundary(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    text[..offset].char_indices().next_back().map(|(i, _)| i)
}

/// Next character boundary after `offset`, or `None` at the end.
pub fn next_boundary(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    text[offset..].chars().next().map(|c| offset + c.len_utf8())
}

/// Where moving right by a word from `offset` lands: the first character of the
/// next word, or with no word ahead, after the last thing in `text` that isn't
/// a space, so the end of a sentence is one stop and not two. `None` when
/// that's behind `offset` too. A word is a maximal run of [`is_word_char`]
/// characters; the current run (word or not) is skipped first, so moving right
/// lands on the next word rather than the next character.
pub fn next_word(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    let chars: Vec<(usize, char)> = text[offset..]
        .char_indices()
        .map(|(i, c)| (offset + i, c))
        .collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    // Skip the current run (word or non-word).
    let mut k = 0;
    let in_word = is_word_char(chars[0].1);
    while k < n && (is_word_char(chars[k].1) == in_word) {
        k += 1;
    }
    // Skip non-word characters to the next word's start.
    while k < n && !is_word_char(chars[k].1) {
        k += 1;
    }
    if let Some((next, _)) = chars.get(k) {
        return Some(*next);
    }
    let end = text.trim_end().len();
    (end > offset).then_some(end)
}

/// The byte index of the first character of the previous word before `offset`,
/// or `None` when no word precedes it. A caret inside (or at the end of) a word
/// moves to that word's start; a caret after whitespace/punctuation skips them
/// and moves to the previous word's start.
pub fn prev_word(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    if offset == 0 {
        return None;
    }
    let chars: Vec<(usize, char)> = text[..offset].char_indices().collect();
    let n = chars.len();
    let mut k = n;
    if is_word_char(chars[n - 1].1) {
        // Inside (or at the end of) a word: walk left to its start.
        while k > 0 && is_word_char(chars[k - 1].1) {
            k -= 1;
        }
        return Some(chars[k].0);
    }
    // After whitespace/punctuation: skip them, then find the word before.
    while k > 0 && !is_word_char(chars[k - 1].1) {
        k -= 1;
    }
    if k == 0 {
        return None;
    }
    let mut start = k - 1;
    while start > 0 && is_word_char(chars[start - 1].1) {
        start -= 1;
    }
    Some(chars[start].0)
}

/// The word around `offset`, as a byte range, for anchoring an annotation
/// placed with a bare caret.
///
/// A caret inside a word selects that word, a caret at its end selects the
/// word before it, and a caret at its start (or on punctuation and spaces)
/// selects the word after it. With no word either side the range is empty at
/// `offset`, which is still a valid zero-width anchor.
pub fn word_at(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’';

    let before = text[..offset].chars().next_back().is_some_and(is_word);
    if before {
        let start = text[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(offset, |(index, _)| index);
        let end = offset
            + text[offset..]
                .chars()
                .take_while(|c| is_word(*c))
                .map(char::len_utf8)
                .sum::<usize>();
        (start, end)
    } else {
        let forward = text[offset..]
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum::<usize>();
        if forward > 0 {
            (offset, offset + forward)
        } else {
            (offset, offset)
        }
    }
}

/// A word character for cursor movement and selection: alphanumeric plus the
/// apostrophe forms, so "don't" moves as one word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '’'
}

/// ASCII-fast, Unicode-correct case-insensitive character equality.
fn char_eq_ignore_case(a: char, b: char) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a.to_lowercase().eq(b.to_lowercase())
    }
}

/// Grows a range outward so that neither end is in the middle of a word. An end already between
/// words stays where it is, so that a range of whole words, or of punctuation, doesn't change.
pub fn widen_to_words(text: &str, start: usize, end: usize) -> (usize, usize) {
    let before = |at: usize| text[..at].chars().next_back().is_some_and(is_word_char);
    let after = |at: usize| text[at..].chars().next().is_some_and(is_word_char);
    let mut start = start.min(text.len());
    let mut end = end.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    if before(start) && after(start) {
        while before(start) {
            start = prev_boundary(text, start).unwrap_or(0);
        }
    }
    if before(end) && after(end) {
        while after(end) {
            end = next_boundary(text, end).unwrap_or(text.len());
        }
    }
    (start, end)
}

/// Case-insensitive matches of `needle` in `text`, as byte ranges of `text`. This is what finding
/// text means everywhere in kame. Overlapping matches are reported, and an empty needle matches
/// nothing.
pub fn find_in(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    // Over the text's own char boundaries, so that byte offsets stay exact even when a character
    // lowercases to a different length. `windows` yields nothing for a shorter text.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    chars
        .windows(needle.len())
        .filter(|window| {
            window
                .iter()
                .zip(&needle)
                .all(|(&(_, c), &n)| char_eq_ignore_case(c, n))
        })
        .map(|window| {
            let last = window[window.len() - 1];
            (window[0].0, last.0 + last.1.len_utf8())
        })
        .collect()
}
