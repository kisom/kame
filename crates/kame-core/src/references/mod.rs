//! Sources a notebook's documents cite. A citation is only the text `[key]`, found wherever it
//! is needed the way bare links are: nothing is stored in the document, so adding a reference
//! after typing its key just works.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{bare_link_ranges, AnnotationKind, Block, BlockKind, Document, Span, SpanStyle};

mod ieee;

pub use ieee::{entry, plain, Run, RunStyle};

/// The longest a key can be, in bytes.
const MAX_KEY: usize = 64;

/// What a reference is, which decides the fields it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Article,
    Conference,
    Book,
    Web,
    Freeform,
}

impl Kind {
    /// The fields other than `kind` that this kind has.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            Kind::Article => &[
                "authors",
                "title",
                "published_in",
                "volume",
                "number",
                "pages",
                "month",
                "year",
                "doi",
                "url",
            ],
            Kind::Conference => &[
                "authors",
                "title",
                "published_in",
                "location",
                "month",
                "year",
                "pages",
                "doi",
                "url",
            ],
            Kind::Book => &[
                "authors",
                "editors",
                "chapter",
                "title",
                "edition",
                "place",
                "publisher",
                "year",
                "pages",
                "url",
            ],
            Kind::Web => &["authors", "title", "site", "accessed", "url"],
            Kind::Freeform => &["text"],
        }
    }
}

/// Reads a field as the text it was written as. A format that has scalars of other types, as
/// YAML does, hands over a number's own digits, so `2.10` stays `2.10` and `year: 1981` is
/// `1981`. A format that tells a number from a string, as JSON does, refuses a number. A null,
/// which in YAML is `~`, `null` or nothing at all, is no text.
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// One flat struct, so switching kinds in the form keeps what was typed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub title: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub chapter: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub published_in: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub location: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub edition: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub place: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub publisher: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub site: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub volume: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub number: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub pages: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub month: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub year: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub doi: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub url: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub accessed: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "text"
    )]
    pub text: String,
}

impl Reference {
    /// `Err` names a field the kind doesn't have, or a missing `title` (or `text`, for a
    /// freeform one).
    pub fn check(&self) -> Result<(), String> {
        let filled = [
            ("authors", !self.authors.is_empty()),
            ("editors", !self.editors.is_empty()),
            ("title", !self.title.is_empty()),
            ("chapter", !self.chapter.is_empty()),
            ("published_in", !self.published_in.is_empty()),
            ("location", !self.location.is_empty()),
            ("edition", !self.edition.is_empty()),
            ("place", !self.place.is_empty()),
            ("publisher", !self.publisher.is_empty()),
            ("site", !self.site.is_empty()),
            ("volume", !self.volume.is_empty()),
            ("number", !self.number.is_empty()),
            ("pages", !self.pages.is_empty()),
            ("month", !self.month.is_empty()),
            ("year", !self.year.is_empty()),
            ("doi", !self.doi.is_empty()),
            ("url", !self.url.is_empty()),
            ("accessed", !self.accessed.is_empty()),
            ("text", !self.text.is_empty()),
        ];
        let allowed = self.kind.fields();
        for (name, is_filled) in filled {
            if is_filled && !allowed.contains(&name) {
                return Err(format!("{name} isn't a field of this kind"));
            }
        }
        let required = if self.kind == Kind::Freeform {
            "text"
        } else {
            "title"
        };
        let missing = if self.kind == Kind::Freeform {
            self.text.is_empty()
        } else {
            self.title.is_empty()
        };
        if missing {
            return Err(format!("{required} is missing"));
        }
        Ok(())
    }
}

/// A notebook's references by key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct References(BTreeMap<String, Reference>);

impl References {
    pub const fn new() -> References {
        References(BTreeMap::new())
    }

    pub fn get(&self, key: &str) -> Option<&Reference> {
        self.0.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Adds or replaces the reference for `key`, returning the one it replaced.
    pub fn insert(&mut self, key: String, reference: Reference) -> Option<Reference> {
        self.0.insert(key, reference)
    }

    pub fn remove(&mut self, key: &str) -> Option<Reference> {
        self.0.remove(key)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Every reference by key, ignoring ASCII case and then by bytes, which is the order a
    /// reader expects and doesn't depend on how the keys are stored.
    pub fn sorted(&self) -> Vec<(&str, &Reference)> {
        let mut all: Vec<(&str, &Reference)> = self
            .0
            .iter()
            .map(|(key, reference)| (key.as_str(), reference))
            .collect();
        all.sort_by(|a, b| {
            a.0.to_ascii_lowercase()
                .cmp(&b.0.to_ascii_lowercase())
                .then_with(|| a.0.cmp(b.0))
        });
        all
    }
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// Whether `key` can name a reference: ASCII letters, digits, `-` and `_`, starting with a
/// letter, at most 64 bytes.
pub fn valid_key(key: &str) -> bool {
    key.len() <= MAX_KEY
        && key.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && key.bytes().all(is_key_byte)
}

/// Byte ranges of the `[key]` in `text`, brackets included. A match has `[` and `]` around a
/// whole key, with no `[` before it and no `(` after it, so `[[x]]` and `[x](y)` never match.
pub fn citation_ranges(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    for (start, _) in text.match_indices('[') {
        if start > 0 && bytes[start - 1] == b'[' {
            continue;
        }
        let key_end = bytes[start + 1..]
            .iter()
            .position(|byte| !is_key_byte(*byte))
            .map_or(bytes.len(), |at| start + 1 + at);
        if bytes.get(key_end) != Some(&b']')
            || bytes.get(key_end + 1) == Some(&b'(')
            || !valid_key(&text[start + 1..key_end])
        {
            continue;
        }
        ranges.push((start, key_end + 1));
    }
    ranges
}

/// The citations in `block` as it's read: none in code, prose or an image, and none that
/// overlap inline code, a link's own words or a bare link.
pub fn block_citation_ranges(block: &Block) -> Vec<(usize, usize)> {
    if block.kind.is_verbatim() || matches!(block.kind, BlockKind::Image { .. }) {
        return Vec::new();
    }
    let links = bare_link_ranges(block);
    citation_ranges(&block.text)
        .into_iter()
        .filter(|(start, end)| {
            !links.iter().any(|(from, to)| from < end && start < to)
                && !block.spans.iter().any(|span| {
                    matches!(
                        span.style,
                        SpanStyle::Code | SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. }
                    ) && span.start < *end
                        && *start < span.end
                })
        })
        .collect()
}

/// Whether a reference's `url` may be written as a link: it starts, ignoring case, with `http:`,
/// `https:` or `mailto:`, and has no whitespace, control character, `<` or `>`.
pub fn linkable_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http:", "https:", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        && !url.contains(|c: char| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
}

/// The key inside a range from `citation_ranges`.
pub fn key_of(text: &str, range: (usize, usize)) -> &str {
    &text[range.0 + 1..range.1 - 1]
}

const CITATION_PREFIX: &str = "#ref-";

/// Where a citation links to in an export.
pub fn citation_url(key: &str) -> String {
    format!("{CITATION_PREFIX}{key}")
}

/// The key a `citation_url` names.
pub fn cited_key(url: &str) -> Option<&str> {
    url.strip_prefix(CITATION_PREFIX)
        .filter(|key| valid_key(key))
}

/// `block` with a link over each citation of a known key, for a place that draws or exports
/// links. The text is left alone, and the block is borrowed if nothing in it is cited.
pub fn with_citation_spans<'a>(block: &'a Block, references: &References) -> Cow<'a, Block> {
    let known: Vec<(usize, usize)> = block_citation_ranges(block)
        .into_iter()
        .filter(|range| references.contains(key_of(&block.text, *range)))
        .collect();
    if known.is_empty() {
        return Cow::Borrowed(block);
    }
    let mut cited = block.clone();
    for range in known {
        cited.spans.push(Span {
            start: range.0,
            end: range.1,
            style: SpanStyle::Link {
                url: citation_url(key_of(&block.text, range)),
            },
        });
    }
    Cow::Owned(cited)
}

/// Every key cited in `doc` where it counts: blocks and footnotes, and marginals if the
/// format carries them. Notes never count.
fn cited_keys(doc: &Document, marginals: bool) -> BTreeSet<&str> {
    let mut keys = BTreeSet::new();
    for block in &doc.blocks {
        for range in block_citation_ranges(block) {
            keys.insert(key_of(&block.text, range));
        }
    }
    for annotation in &doc.annotations {
        let counts = match annotation.kind {
            AnnotationKind::Footnote => true,
            AnnotationKind::Marginal => marginals,
            AnnotationKind::Note => false,
        };
        if counts {
            for range in citation_ranges(&annotation.body) {
                keys.insert(key_of(&annotation.body, range));
            }
        }
    }
    keys
}

/// The references `doc` cites, each once, in `References::sorted` order. Keys with no
/// reference are left out.
pub fn cited<'a>(
    doc: &Document,
    references: &'a References,
    marginals: bool,
) -> Vec<(&'a str, &'a Reference)> {
    let keys = cited_keys(doc, marginals);
    references
        .sorted()
        .into_iter()
        .filter(|(key, _)| keys.contains(key))
        .collect()
}

/// Whether `key` has the shape `suggest_key` gives, in either case: letters, a four digit year and
/// perhaps a letter, as `knuth1981`, `Smith2020` or `knuth1981a`.
pub fn looks_like_suggested_key(key: &str) -> bool {
    let rest = key.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let year = rest.get(..4).unwrap_or("");
    let after = &rest[year.len()..];
    rest.len() < key.len()
        && year.len() == 4
        && year.bytes().all(|b| b.is_ascii_digit())
        && (after.is_empty() || (after.len() == 1 && after.as_bytes()[0].is_ascii_alphabetic()))
}

/// Keys cited in `doc` that have no reference and look like a suggested key, so that a
/// `[sic]` is not taken for a mistake. Sorted, each once.
pub fn unresolved(doc: &Document, references: &References, marginals: bool) -> Vec<String> {
    cited_keys(doc, marginals)
        .into_iter()
        .filter(|key| !references.contains(key) && looks_like_suggested_key(key))
        .map(str::to_string)
        .collect()
}

/// `text` with each citation of `old` made a citation of `new`.
pub fn rewrite_citation(text: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for range in citation_ranges(text) {
        if key_of(text, range) == old {
            out.push_str(&text[from..range.0]);
            out.push('[');
            out.push_str(new);
            out.push(']');
            from = range.1;
        }
    }
    out.push_str(&text[from..]);
    out
}

/// The citation-shaped range that `offset` is in or just after, whether or not its key has a
/// reference.
pub fn citation_at(text: &str, offset: usize) -> Option<(usize, usize)> {
    citation_ranges(text)
        .into_iter()
        .find(|(start, end)| *start < offset && offset <= *end)
}

/// How many digits of a year go in a suggested key, so that with a name of at most 40 letters
/// there is room left for a suffix within `MAX_KEY`.
const MAX_YEAR_DIGITS: usize = 8;

/// What can follow a name after a comma and still be part of it, as in `M. L. King, Jr.`.
pub const NAME_SUFFIXES: [&str; 7] = ["Jr.", "Jr", "Sr.", "Sr", "II", "III", "IV"];

/// A key for `reference` from its first author's last name and its year, with `a`, `b` and so
/// on added if that's taken. Characters a key can't hold are dropped. `None` if no letter is
/// left of the name.
pub fn suggest_key(reference: &Reference, references: &References) -> Option<String> {
    let author = reference.authors.first()?;
    let surname = match author.split_once(',') {
        Some((before, after)) if NAME_SUFFIXES.contains(&after.trim()) => {
            before.split_whitespace().next_back().unwrap_or("")
        }
        Some((before, _)) => before,
        None => author.split_whitespace().next_back().unwrap_or(""),
    };
    let name: String = surname
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase())
        .take(40)
        .collect();
    if name.is_empty() {
        return None;
    }
    let year: String = reference
        .year
        .chars()
        .filter(char::is_ascii_digit)
        .take(MAX_YEAR_DIGITS)
        .collect();
    let base = format!("{name}{year}");
    if !references.contains(&base) {
        return Some(base);
    }
    (1..)
        .map(|n| format!("{base}{}", suffix(n)))
        .find(|key| !references.contains(key))
}

/// `a` for 1 to `z` for 26, then `aa`, `ab` and so on.
fn suffix(mut n: usize) -> String {
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Annotation;
    use crate::model::{Anchor, Span};
    use uuid::Uuid;

    fn block(kind: BlockKind, text: &str, spans: Vec<Span>) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans,
        }
    }

    fn paragraph(text: &str) -> Block {
        block(BlockKind::Paragraph, text, Vec::new())
    }

    fn known(keys: &[&str]) -> References {
        let mut references = References::new();
        for key in keys {
            references.insert(
                key.to_string(),
                Reference {
                    title: "T".to_string(),
                    ..Reference::default()
                },
            );
        }
        references
    }

    fn annotation(kind: AnnotationKind, block: &Block, body: &str) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: Anchor::block(block.id),
            body: body.to_string(),
        }
    }

    fn document(blocks: Vec<Block>, annotations: Vec<Annotation>) -> Document {
        let mut doc = Document::new("T");
        doc.blocks = blocks;
        doc.annotations = annotations;
        doc
    }

    #[test]
    fn a_key_is_ascii_starting_with_a_letter() {
        for good in ["smith2020", "a", "a-b_c", "A1"] {
            assert!(valid_key(good), "{good}");
        }
        for bad in [
            "",
            "1",
            "2020",
            "-a",
            "a b",
            "é",
            "a.b",
            "a:b",
            &"a".repeat(65),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
        assert!(valid_key(&"a".repeat(64)));
    }

    #[test]
    fn only_a_whole_key_in_single_brackets_is_a_citation() {
        assert_eq!(citation_ranges("see [a1]."), vec![(4, 8)]);
        assert_eq!(citation_ranges("é[a]é"), vec![(2, 5)]);
        assert_eq!(citation_ranges("[a][b]"), vec![(0, 3), (3, 6)]);
        for none in [
            "[[a]]", "[a](b)", "[1]", "[a b]", "[]", "[a", "a]", "\\", "[", "[é]",
        ] {
            assert_eq!(citation_ranges(none), vec![], "{none}");
        }
    }

    #[test]
    fn the_key_is_what_is_inside_the_brackets() {
        let text = "é [Smith-2] x";
        let range = citation_ranges(text)[0];
        assert_eq!(key_of(text, range), "Smith-2");
    }

    #[test]
    fn citations_do_not_count_in_code_prose_or_images() {
        for kind in [
            BlockKind::Code { language: None },
            BlockKind::Prose,
            BlockKind::Image {
                path: "p".to_string(),
                link: None,
                align: Default::default(),
            },
        ] {
            assert_eq!(
                block_citation_ranges(&block(kind, "[a]", Vec::new())),
                vec![]
            );
        }
        assert_eq!(block_citation_ranges(&paragraph("x [a]")), vec![(2, 5)]);
        let heading = block(
            BlockKind::Heading {
                level: 2,
                centered: false,
            },
            "[a]",
            Vec::new(),
        );
        assert_eq!(block_citation_ranges(&heading), vec![(0, 3)]);
    }

    #[test]
    fn citations_inside_code_and_links_are_skipped() {
        let span = |start, end, style| Span { start, end, style };
        let text = "[a] [b] [c] [d]";
        let spans = vec![
            span(0, 3, SpanStyle::Code),
            span(
                5,
                6,
                SpanStyle::Link {
                    url: "u".to_string(),
                },
            ),
            span(
                9,
                10,
                SpanStyle::DocumentLink {
                    target: "t.kame".to_string(),
                },
            ),
            span(12, 15, SpanStyle::Bold),
        ];
        let found = block_citation_ranges(&block(BlockKind::Paragraph, text, spans));
        assert_eq!(found, vec![(12, 15)]);
    }

    #[test]
    fn a_citation_url_names_its_key() {
        assert_eq!(citation_url("k1"), "#ref-k1");
        assert_eq!(cited_key("#ref-k1"), Some("k1"));
        assert_eq!(cited_key("#ref-"), None);
        assert_eq!(cited_key("#ref-1a"), None);
        assert_eq!(cited_key("#other"), None);
    }

    #[test]
    fn references_sort_ignoring_case() {
        let references = known(&["Cherry", "banana", "Apple"]);
        let keys: Vec<&str> = references.sorted().iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, ["Apple", "banana", "Cherry"]);
        assert_eq!(references.len(), 3);
        assert!(!references.is_empty());
        assert_eq!(references.keys().count(), 3);
    }

    #[test]
    fn citing_sees_blocks_and_footnotes_and_marginals_only_when_asked() {
        let first = paragraph("a [Cherry] and [Cherry] and [nope]");
        let second = block(BlockKind::Code { language: None }, "[Apple]", Vec::new());
        let annotations = vec![
            annotation(AnnotationKind::Footnote, &first, "see [banana]"),
            annotation(AnnotationKind::Marginal, &first, "[Apple]"),
            annotation(AnnotationKind::Note, &first, "[Dates]"),
        ];
        let doc = document(vec![first, second], annotations);
        let references = known(&["Apple", "banana", "Cherry", "Dates"]);
        let keys = |marginals| -> Vec<&str> {
            cited(&doc, &references, marginals)
                .iter()
                .map(|(key, _)| *key)
                .collect()
        };
        assert_eq!(keys(false), ["banana", "Cherry"]);
        assert_eq!(keys(true), ["Apple", "banana", "Cherry"]);
    }

    #[test]
    fn only_keys_that_look_suggested_are_unresolved() {
        let first = paragraph("[smith2020] [smith2020a] [Jones2019] [sic] [known2000] [smith2020]");
        let doc = document(vec![first], Vec::new());
        let references = known(&["known2000"]);
        assert_eq!(
            unresolved(&doc, &references, false),
            ["Jones2019", "smith2020", "smith2020a"]
        );
    }

    #[test]
    fn a_suggested_key_is_letters_a_year_and_perhaps_a_letter() {
        for yes in [
            "knuth1981",
            "knuth1981a",
            "a1999",
            "Knuth1981",
            "SMITH2020",
            "Smith2020A",
        ] {
            assert!(looks_like_suggested_key(yes), "{yes}");
        }
        for no in [
            "sic",
            "1981",
            "knuth",
            "knuth81",
            "knuth19811",
            "knuth1981ab",
            "Knuth1981AB",
            "k-1981",
            "",
        ] {
            assert!(!looks_like_suggested_key(no), "{no}");
        }
    }

    #[test]
    fn rewriting_changes_only_whole_citations_of_the_key() {
        assert_eq!(
            rewrite_citation("[old] [old2] [[old]]", "old", "new"),
            "[new] [old2] [[old]]"
        );
        assert_eq!(rewrite_citation("é[old]é", "old", "x"), "é[x]é");
        assert_eq!(rewrite_citation("none", "old", "new"), "none");
    }

    #[test]
    fn citation_spans_are_added_for_known_keys_only() {
        let references = known(&["yes"]);
        let plain = paragraph("[no] and nothing");
        assert!(matches!(
            with_citation_spans(&plain, &references),
            Cow::Borrowed(_)
        ));
        let mixed = paragraph("[no] and [yes]");
        let linked = with_citation_spans(&mixed, &references);
        assert_eq!(linked.text, mixed.text);
        assert_eq!(
            linked.spans,
            vec![Span {
                start: 9,
                end: 14,
                style: SpanStyle::Link {
                    url: "#ref-yes".to_string()
                }
            }]
        );
        assert!(mixed.spans.is_empty());
    }

    #[test]
    fn a_caret_in_or_just_after_a_citation_is_at_it() {
        let text = "a [b1] c";
        assert_eq!(citation_at(text, 2), None);
        for offset in 3..=6 {
            assert_eq!(citation_at(text, offset), Some((2, 6)), "{offset}");
        }
        assert_eq!(citation_at(text, 7), None);
    }

    fn author(name: &str, year: &str) -> Reference {
        Reference {
            authors: vec![name.to_string()],
            year: year.to_string(),
            ..Reference::default()
        }
    }

    #[test]
    fn a_key_is_suggested_from_the_author_and_year() {
        let none = References::new();
        let knuth = author("D. E. Knuth", "1981");
        assert_eq!(suggest_key(&knuth, &none).as_deref(), Some("knuth1981"));
        assert_eq!(
            suggest_key(&author("Knuth, Donald", "1981"), &none).as_deref(),
            Some("knuth1981")
        );
        let taken = known(&["knuth1981"]);
        assert_eq!(suggest_key(&knuth, &taken).as_deref(), Some("knuth1981a"));
        let more = known(&["knuth1981", "knuth1981a"]);
        assert_eq!(suggest_key(&knuth, &more).as_deref(), Some("knuth1981b"));
        assert_eq!(
            suggest_key(&author("Müller", "2020"), &none).as_deref(),
            Some("mller2020")
        );
        assert_eq!(suggest_key(&author("王", "2020"), &none), None);
        assert_eq!(suggest_key(&Reference::default(), &none), None);
    }

    #[test]
    fn a_suffix_after_a_comma_is_passed_over_for_the_surname() {
        let none = References::new();
        for name in ["M. L. King, Jr.", "M. L. King, Sr", "M. L. King, III"] {
            assert_eq!(
                suggest_key(&author(name, "1963"), &none).as_deref(),
                Some("king1963"),
                "{name}"
            );
        }
        assert_eq!(
            suggest_key(&author("King, M. L.", "1963"), &none).as_deref(),
            Some("king1963")
        );
    }

    #[test]
    fn a_suggested_key_is_always_a_valid_key() {
        let long = author(&"k".repeat(100), &"1".repeat(100));
        let mut taken = References::new();
        for _ in 0..30 {
            let key = suggest_key(&long, &taken).expect("a key");
            assert!(valid_key(&key), "{key}");
            taken.insert(key, Reference::default());
        }
    }

    #[test]
    fn suffixes_run_on_past_z() {
        assert_eq!(suffix(1), "a");
        assert_eq!(suffix(26), "z");
        assert_eq!(suffix(27), "aa");
        assert_eq!(suffix(28), "ab");
    }

    fn titled(kind: Kind) -> Reference {
        Reference {
            kind,
            title: "T".to_string(),
            ..Reference::default()
        }
    }

    #[test]
    fn a_reference_with_a_field_its_kind_lacks_is_refused() {
        let mut book = titled(Kind::Book);
        assert_eq!(book.check(), Ok(()));
        book.volume = "3".to_string();
        assert!(book.check().unwrap_err().contains("volume"));

        let mut web = titled(Kind::Web);
        web.doi = "10.1/x".to_string();
        assert!(web.check().unwrap_err().contains("doi"));

        let mut article = titled(Kind::Article);
        article.text = "x".to_string();
        assert!(article.check().unwrap_err().contains("text"));
    }

    #[test]
    fn a_reference_needs_a_title_or_its_text() {
        assert!(titled(Kind::Article).check().is_ok());
        assert!(Reference::default().check().unwrap_err().contains("title"));
        let mut freeform = Reference {
            kind: Kind::Freeform,
            ..Reference::default()
        };
        assert!(freeform.check().unwrap_err().contains("text"));
        freeform.text = "As typed.".to_string();
        assert_eq!(freeform.check(), Ok(()));
        freeform.title = "T".to_string();
        assert!(freeform.check().unwrap_err().contains("title"));
    }

    #[test]
    fn a_field_reads_as_a_string() {
        let json = r#"{"kind":"article","title":"T","year":"1981","pages":"1–2"}"#;
        let reference: Reference = serde_json::from_str(json).unwrap();
        assert_eq!(reference.year, "1981");
        assert_eq!(reference.pages, "1–2");
        assert!(serde_json::from_str::<Reference>(r#"{"kind":"article","bad":1}"#).is_err());
        let written = serde_json::to_string(&reference).unwrap();
        assert!(
            !written.contains("authors") && !written.contains("doi"),
            "{written}"
        );
    }

    #[test]
    fn only_a_web_or_mail_address_is_linkable() {
        for url in [
            "https://a.b/c_d",
            "http://a.b",
            "HTTPS://a.b",
            "mailto:a@b.c",
        ] {
            assert!(linkable_url(url), "{url}");
        }
        for url in [
            "script",
            "/about",
            "www.example.com/x",
            "javascript:alert(1)",
            "https://a.b/ c",
            "https://a.b/<c>",
            "https://a.b/\u{7}",
            "",
        ] {
            assert!(!linkable_url(url), "{url}");
        }
    }

    #[test]
    fn a_citation_inside_a_bare_url_is_skipped() {
        let found = block_citation_ranges(&paragraph("see https://a.b/c[knuth1981] now [x]"));
        assert_eq!(found, vec![(33, 36)]);
    }
}
