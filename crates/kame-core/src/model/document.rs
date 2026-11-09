use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type BlockId = Uuid;
pub type AnnotationId = Uuid;

/// Where an image sits across its line. Text never runs beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl ImageAlign {
    fn is_left(&self) -> bool {
        *self == ImageAlign::Left
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Paragraph,
    Heading {
        /// 1 to 6. Read whatever whole number it is, and fitted to that ([`Block::fit_kind`]).
        #[serde(deserialize_with = "byte_from_any")]
        level: u8,
        /// Set in the middle of the line. Absent when it isn't, as in files from before it.
        #[serde(default, skip_serializing_if = "is_false")]
        centered: bool,
    },
    ListItem {
        ordered: bool,
        /// How many lists this one is nested inside, up to [`MAX_LIST_DEPTH`]. Read whatever
        /// whole number it is, and fitted to that ([`Block::fit_kind`]).
        #[serde(
            default,
            skip_serializing_if = "is_zero",
            deserialize_with = "byte_from_any"
        )]
        depth: u8,
    },
    /// An indented, italicized quotation.
    BlockQuote,
    /// An image, on a line of its own. `path` is where the file lives on disk;
    /// the block's `text` is empty (the image has no prose).
    Image {
        path: String,
        /// Where the image links to, if anywhere. Absent in files from before it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<String>,
        /// Where it sits across the line. Absent when it's the left, as in files from before it.
        #[serde(default, skip_serializing_if = "ImageAlign::is_left")]
        align: ImageAlign,
    },
    /// A horizontal rule. Its `text` is empty.
    Rule,
    /// Code, set in a monospaced face with its line breaks and spacing exact.
    Code {
        /// The language a fence named, kept for export.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Prose quoted exactly: a code block in the serif, for verse or a passage whose
    /// line breaks and spacing matter.
    Prose,
}

impl BlockKind {
    /// Whether the text is kept exactly as written, line breaks and all.
    pub fn is_verbatim(&self) -> bool {
        matches!(self, BlockKind::Code { .. } | BlockKind::Prose)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpanStyle {
    Bold,
    Italic,
    /// Inline code: set in the monospaced face, and exact on export.
    Code,
    /// A hyperlink: the span covers the anchor text and `url` is the target.
    Link {
        url: String,
    },
    /// A link to another kame document, or to a heading in this one. `target` is kept
    /// exactly as it was entered, so like a web link it can rot.
    DocumentLink {
        target: String,
    },
}

/// Bold or italic: what Ctrl-B and Ctrl-I put on words and take off them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emphasis {
    Bold,
    Italic,
}

impl Emphasis {
    /// The span style it is.
    pub fn style(self) -> SpanStyle {
        match self {
            Emphasis::Bold => SpanStyle::Bold,
            Emphasis::Italic => SpanStyle::Italic,
        }
    }
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

fn is_zero(depth: &u8) -> bool {
    *depth == 0
}

/// The nearest `u8` to any whole number, so that one out of a byte's range, as a damaged or
/// hand-made file can hold, is fitted rather than making its whole document unreadable. Anything
/// but a whole number is still an error.
fn byte_from_any<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let whole = deserializer.deserialize_any(Whole)?;
    Ok(whole.clamp(0, i128::from(u8::MAX)) as u8)
}

/// The nearest `usize` to any whole number, as [`byte_from_any`] has it, for an offset: one below
/// zero becomes 0, and one past any text is fitted to its text as any other offset past it is.
fn offset_from_any<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let whole = deserializer.deserialize_any(Whole)?;
    Ok(usize::try_from(whole.max(0)).unwrap_or(usize::MAX))
}

/// Reads any whole number, as near as an `i128` holds it.
struct Whole;

impl serde::de::Visitor<'_> for Whole {
    type Value = i128;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a whole number")
    }
    fn visit_i64<E>(self, value: i64) -> Result<i128, E> {
        Ok(i128::from(value))
    }
    fn visit_u64<E>(self, value: u64) -> Result<i128, E> {
        Ok(i128::from(value))
    }
    fn visit_i128<E>(self, value: i128) -> Result<i128, E> {
        Ok(value)
    }
    fn visit_u128<E>(self, value: u128) -> Result<i128, E> {
        Ok(i128::try_from(value).unwrap_or(i128::MAX))
    }
    /// JSON has one kind of number, and one past what a `u64` holds is read as a float.
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<i128, E> {
        if value.is_finite() && value.fract() == 0.0 {
            // Saturates, as a float cast to an integer does.
            Ok(value as i128)
        } else {
            Err(E::invalid_type(serde::de::Unexpected::Float(value), &self))
        }
    }
}

/// How deeply lists can nest.
pub const MAX_LIST_DEPTH: u8 = 5;

/// The levels a heading can have: those Markdown and HTML have.
pub const HEADING_LEVELS: std::ops::RangeInclusive<u8> = 1..=6;

/// Where a list item sits: how deep it really is, and its number if it is ordered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListPosition {
    pub depth: u8,
    pub number: Option<usize>,
}

/// Where each block sits in a list, the one place the page and the exports read this
/// from. A block that isn't a list item ends the list.
///
/// An item is never more than one level below the one before it, whatever it stores:
/// Markdown can't express that, and outdenting a parent can leave its children so. A
/// number counts its level, carries on after a nested list, and restarts after a bullet.
///
/// With `skip_empty`, as for an export, which leaves empty blocks out, those neither end
/// a list nor take a number. Rules and images have no text but do appear, so they count.
pub fn list_positions(blocks: &[Block], skip_empty: bool) -> Vec<Option<ListPosition>> {
    let mut counts: Vec<usize> = Vec::new();
    blocks
        .iter()
        .map(|block| {
            let appears = !block.text.is_empty()
                || matches!(block.kind, BlockKind::Rule | BlockKind::Image { .. });
            if skip_empty && !appears {
                return None;
            }
            let BlockKind::ListItem { ordered, depth } = block.kind else {
                counts.clear();
                return None;
            };
            let level = usize::from(depth.min(MAX_LIST_DEPTH)).min(counts.len());
            counts.truncate(level + 1);
            if counts.len() == level {
                counts.push(0);
            }
            counts[level] = if ordered { counts[level] + 1 } else { 0 };
            Some(ListPosition {
                depth: level as u8,
                number: ordered.then_some(counts[level]),
            })
        })
        .collect()
}

/// What each numbered item shows on the page: `1.`, then `1.1` and `1.1.1` as lists nest.
/// A bullet has no label, and adds nothing to the numbers below it.
pub fn list_labels(blocks: &[Block]) -> Vec<Option<String>> {
    let mut path: Vec<Option<usize>> = Vec::new();
    list_positions(blocks, false)
        .into_iter()
        .map(|position| {
            let Some(position) = position else {
                path.clear();
                return None;
            };
            path.truncate(usize::from(position.depth));
            path.push(position.number);
            position.number?;
            let numbers: Vec<String> = path.iter().flatten().map(usize::to_string).collect();
            Some(match numbers.as_slice() {
                [only] => format!("{only}."),
                _ => numbers.join("."),
            })
        })
        .collect()
}

/// The bare URLs in `block` that read as links. Code is literal, so a URL in a code
/// block or in inline code is only text.
pub fn bare_link_ranges(block: &Block) -> Vec<(usize, usize)> {
    if matches!(block.kind, BlockKind::Code { .. }) {
        return Vec::new();
    }
    link_ranges(&block.text)
        .into_iter()
        .filter(|(start, end)| {
            !block
                .spans
                .iter()
                .any(|span| span.style == SpanStyle::Code && span.start < *end && *start < span.end)
        })
        .collect()
}

/// Byte ranges of URLs in `text`, each extended to the next whitespace and
/// with trailing punctuation (`.,;:!?)]}`) trimmed. Recognises `http://`,
/// `https://` and `www.` forms, in any case, so a pasted or typed link becomes a clickable
/// range without the author marking it up.
pub fn link_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let Some(scheme) = crate::links::web_address_start(rest) else {
            i += text[i..].chars().next().map_or(0, char::len_utf8);
            continue;
        };
        let start = i;
        // The link runs to the next whitespace.
        let mut end = text.len();
        for (j, c) in text[i + scheme.len()..].char_indices() {
            if c.is_whitespace() {
                end = i + scheme.len() + j;
                break;
            }
        }
        // Trim trailing punctuation that belongs to the sentence, not the URL.
        while end > start + scheme.len() {
            let last = text[..end].chars().next_back().unwrap();
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}') {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        ranges.push((start, end));
        i = end;
    }
    ranges
}

/// A number in a kind of block that this build's model bounds, as the kind and the field are
/// written, and what the model has.
struct Bounded {
    kind: &'static str,
    field: &'static str,
    has: std::ops::RangeInclusive<u64>,
}

const BOUNDED: [Bounded; 2] = [
    Bounded {
        kind: "Heading",
        field: "level",
        has: *HEADING_LEVELS.start() as u64..=*HEADING_LEVELS.end() as u64,
    },
    Bounded {
        kind: "ListItem",
        field: "depth",
        has: 0..=MAX_LIST_DEPTH as u64,
    },
];

/// A field this build leaves out when it holds its default, with that default as JSON. Written at
/// its default by another program, it's gone once read and written again, as a field this build
/// doesn't know is, so this is what tells the two apart: the JSON alone can't, since reading drops
/// both alike.
///
/// Every `skip_serializing_if` on anything a document, a step of history or a revision holds has to
/// be here, which a test holds it to, so that one added later isn't taken for a newer version's.
struct LeftOut {
    /// The kind of block, step of history or struct the field is in, as the source names it,
    /// which the test holding this to the source reads.
    #[cfg_attr(not(test), allow(dead_code))]
    of: &'static str,
    /// The keys what holds the field is written under: a kind or a step under its own name, and a
    /// struct under the field holding it, with `[]` after where that field is a list of them. A
    /// struct written alone, as the metadata table holds a document's metadata, is named by
    /// [`has_unknown_fields_held_at`].
    held_at: &'static [&'static str],
    field: &'static str,
    default: &'static str,
}

const LEFT_OUT: &[LeftOut] = &[
    LeftOut {
        of: "Heading",
        held_at: &["Heading"],
        field: "centered",
        default: "false",
    },
    LeftOut {
        of: "ListItem",
        held_at: &["ListItem"],
        field: "depth",
        default: "0",
    },
    LeftOut {
        of: "Image",
        held_at: &["Image"],
        field: "link",
        default: "null",
    },
    LeftOut {
        of: "Image",
        held_at: &["Image"],
        field: "align",
        default: r#""Left""#,
    },
    LeftOut {
        of: "Code",
        held_at: &["Code"],
        field: "language",
        default: "null",
    },
    LeftOut {
        of: "DeleteText",
        held_at: &["DeleteText"],
        field: "old_spans",
        default: "null",
    },
    LeftOut {
        of: "DeleteText",
        held_at: &["DeleteText"],
        field: "clipped",
        default: "[]",
    },
    // A struct only the tests know, so that a struct's field is held to all this as well.
    #[cfg(test)]
    LeftOut {
        of: "Pinned",
        held_at: &["test_pinned", "test_pinned_list[]"],
        field: "pinned",
        default: "false",
    },
];

/// Where a value is written: under the key `key`, or as an item of the list under it.
#[derive(Clone, Copy)]
struct At<'a> {
    key: Option<&'a str>,
    item: bool,
}

impl<'a> At<'a> {
    const TOP: At<'static> = At {
        key: None,
        item: false,
    };

    fn under(key: &'a str) -> At<'a> {
        At {
            key: Some(key),
            item: false,
        }
    }

    fn items(self) -> At<'a> {
        At { item: true, ..self }
    }

    /// Whether it's at `place`, as [`LeftOut::held_at`] names one.
    fn is(self, place: &str) -> bool {
        match (self.key, self.item) {
            (None, _) => false,
            (Some(key), false) => place == key,
            (Some(key), true) => place.strip_suffix("[]") == Some(key),
        }
    }
}

/// The field `key` of what's written at `at`, if this build leaves it out at its default.
fn left_out(at: At<'_>, key: &str) -> Option<&'static LeftOut> {
    LEFT_OUT
        .iter()
        .find(|left_out| key == left_out.field && left_out.held_at.iter().any(|place| at.is(place)))
}

impl BlockKind {
    /// Whether this holds a level or depth past what this build's model has, as one read from a
    /// file can, before it's fitted.
    pub fn is_past_the_model(&self) -> bool {
        match self {
            BlockKind::Heading { level, .. } => !HEADING_LEVELS.contains(level),
            BlockKind::ListItem { depth, .. } => *depth > MAX_LIST_DEPTH,
            _ => false,
        }
    }
}

/// Whether JSON as written holds a heading's level or a list item's depth that a byte holds and
/// this build's model doesn't, such as level 7: a newer kame that went further would write it, so
/// it's from a newer version, as an unknown field is. One a byte can't hold, below 0 or past 255,
/// or that isn't a whole number, is damage, and doesn't count.
pub fn holds_newer_values(written: &serde_json::Value) -> bool {
    use serde_json::Value;
    match written {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            BOUNDED.iter().any(|bounded| {
                key == bounded.kind
                    && value
                        .get(bounded.field)
                        .and_then(Value::as_u64)
                        .is_some_and(|n| n <= u64::from(u8::MAX) && !bounded.has.contains(&n))
            }) || holds_newer_values(value)
        }),
        Value::Array(items) => items.iter().any(holds_newer_values),
        _ => false,
    }
}

/// Whether JSON as written holds a field that's missing once read and written again by this
/// build, which is how something from a newer version of kame looks. A field missing from what was
/// written is only something added since, so that doesn't count.
///
/// Nor does a field this build knows and leaves out at its default ([`LEFT_OUT`]), such as a
/// heading's `centered` written as `false`. Gone once read, it was read as its default, which loses
/// nothing. A list item's `depth` below 0, which a damaged file can hold, is read as 0 too, and
/// [`fitted_on_reading`] tells of that.
pub fn has_unknown_fields(written: &serde_json::Value, known: &serde_json::Value) -> bool {
    unknown_fields_in(At::TOP, written, known)
}

/// [`has_unknown_fields`], for JSON written alone that's elsewhere written under the key `key`, as
/// a document's metadata is kept alone in its table and under `metadata` in a revision.
pub fn has_unknown_fields_held_at(
    key: &str,
    written: &serde_json::Value,
    known: &serde_json::Value,
) -> bool {
    unknown_fields_in(At::under(key), written, known)
}

/// [`has_unknown_fields`], for what's written at `at`.
fn unknown_fields_in(at: At<'_>, written: &serde_json::Value, known: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (written, known) {
        (Value::Object(written), Value::Object(known)) => {
            written.iter().any(|(key, value)| match known.get(key) {
                Some(kept) => unknown_fields_in(At::under(key), value, kept),
                None => left_out(at, key).is_none(),
            })
        }
        (Value::Array(written), Value::Array(known)) => written
            .iter()
            .zip(known)
            .any(|(value, kept)| unknown_fields_in(at.items(), value, kept)),
        _ => false,
    }
}

/// Whether JSON as written holds a number that reading it and writing it again gives as another,
/// as a level, depth or offset past what its type holds is read as the nearest one it does. A depth
/// written below 0 is read as 0 and left out, so it counts too: a field left out at its default
/// ([`LEFT_OUT`]) written as anything but that default. A field missing either way doesn't.
pub fn fitted_on_reading(written: &serde_json::Value, known: &serde_json::Value) -> bool {
    fitted_in(At::TOP, written, known)
}

fn fitted_in(at: At<'_>, written: &serde_json::Value, known: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (written, known) {
        (Value::Object(written), Value::Object(known)) => {
            written.iter().any(|(key, value)| match known.get(key) {
                Some(kept) => fitted_in(At::under(key), value, kept),
                None => left_out(at, key).is_some_and(|left_out| {
                    serde_json::from_str::<Value>(left_out.default)
                        .ok()
                        .as_ref()
                        != Some(value)
                }),
            })
        }
        (Value::Array(written), Value::Array(known)) => written
            .iter()
            .zip(known)
            .any(|(value, kept)| fitted_in(at.items(), value, kept)),
        (Value::Number(written), Value::Number(known)) => written != known,
        _ => false,
    }
}

/// The style for a link entered as `url`: a document link if it names a `.kame` file or
/// a heading, and an ordinary link otherwise.
pub fn link_style(url: String) -> SpanStyle {
    let path = url.split('#').next().unwrap_or("");
    let names_kame = path.to_lowercase().ends_with(".kame") && !path.contains("://");
    let names_heading = path.is_empty() && url.len() > 1;
    if names_kame || names_heading {
        SpanStyle::DocumentLink { target: url }
    } else {
        SpanStyle::Link { url }
    }
}

/// Whether `url` runs script when followed. Browsers ignore control characters
/// and spaces when reading a scheme, and its case, so this does too.
pub fn is_script_url(url: &str) -> bool {
    let scheme: String = url
        .chars()
        .filter(|c| *c > ' ' && *c != '\u{7f}')
        .take("javascript:".len())
        .flat_map(char::to_lowercase)
        .collect();
    scheme == "javascript:"
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// Read from any whole number, and fitted to the text ([`Block::fit_spans`]).
    #[serde(deserialize_with = "offset_from_any")]
    pub start: usize,
    #[serde(deserialize_with = "offset_from_any")]
    pub end: usize,
    pub style: SpanStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Marginal,
    Note,
    Footnote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Anchor {
    /// Anchored to a whole block — used by marginalia. Serializes as the bare
    /// block id (a string), which keeps it distinguishable from a range anchor
    /// and lets older files (which wrote range anchors as
    /// `{block_id, start, end}`) still deserialize as ranges.
    Block(BlockId),
    /// Anchored to a byte range within a block — used by notes (a highlighted
    /// passage) and footnotes (a numbered marker at a position).
    Range {
        block_id: BlockId,
        /// Read from any whole number, and fitted to the text ([`Document::fit`]).
        #[serde(deserialize_with = "offset_from_any")]
        start: usize,
        #[serde(deserialize_with = "offset_from_any")]
        end: usize,
    },
}

impl Anchor {
    pub fn block(block_id: BlockId) -> Anchor {
        Anchor::Block(block_id)
    }

    pub fn range(block_id: BlockId, start: usize, end: usize) -> Anchor {
        Anchor::Range {
            block_id,
            start,
            end,
        }
    }

    /// The block this anchor belongs to, whether block- or range-scoped.
    pub fn block_id(&self) -> BlockId {
        match self {
            Anchor::Block(block_id) | Anchor::Range { block_id, .. } => *block_id,
        }
    }

    /// The byte range for range-scoped anchors, `None` for block-scoped ones.
    pub fn byte_range(&self) -> Option<(usize, usize)> {
        match self {
            Anchor::Range { start, end, .. } => Some((*start, *end)),
            Anchor::Block(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub anchor: Anchor,
    pub body: String,
}

/// What a document says about itself, kept apart from its text: edits to it are
/// not undo steps, and restoring a snapshot leaves it alone.
///
/// Dates are free text, written `YYYY-MM-DD` by kame. An empty title means the
/// document goes by its file's name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    pub created: String,
    pub published: String,
    pub tags: Vec<String>,
    /// Words the spell checker should take as right in this document.
    pub words: Vec<String>,
    /// Marks the writer has said the checker is wrong about, in this document.
    pub accepted: Vec<AcceptedMark>,
    /// Whether a notebook's site publishes this document.
    pub publish: bool,
}

impl Metadata {
    /// Takes in the checker's exceptions from another copy of this document, keeping both.
    pub fn take_exceptions_from(&mut self, other: &Metadata) {
        for word in &other.words {
            if !self.words.contains(word) {
                self.words.push(word.clone());
            }
        }
        for mark in &other.accepted {
            if !self.accepted.contains(mark) {
                self.accepted.push(mark.clone());
            }
        }
    }
}

/// The checker's rule and the text it marked, which it shouldn't mark again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcceptedMark {
    pub rule: String,
    pub text: String,
}

#[cfg(test)]
mod exception_tests {
    use super::*;

    fn accepted(text: &str) -> AcceptedMark {
        AcceptedMark {
            rule: "Rule".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn exceptions_from_another_copy_are_added_once() {
        let mut ours = Metadata {
            words: vec!["kame".to_string(), "both".to_string()],
            accepted: vec![accepted("ours"), accepted("shared")],
            title: "Ours".to_string(),
            ..Metadata::default()
        };
        let theirs = Metadata {
            words: vec!["both".to_string(), "theirs".to_string()],
            accepted: vec![accepted("shared"), accepted("theirs")],
            title: "Theirs".to_string(),
            ..Metadata::default()
        };
        ours.take_exceptions_from(&theirs);
        assert_eq!(ours.words, vec!["kame", "both", "theirs"]);
        assert_eq!(
            ours.accepted,
            vec![accepted("ours"), accepted("shared"), accepted("theirs")]
        );
        assert_eq!(ours.title, "Ours", "only the exceptions");
    }

    #[test]
    fn a_file_from_before_these_fields_still_opens() {
        let old = r#"{"title":"T","author":"A","created":"","published":"","tags":[]}"#;
        let metadata: Metadata = serde_json::from_str(old).unwrap();
        assert!(metadata.words.is_empty());
        assert!(metadata.accepted.is_empty());
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub title: String,
    pub blocks: Vec<Block>,
    pub annotations: Vec<Annotation>,
}

impl Document {
    pub fn new(title: &str) -> Document {
        Document {
            id: Uuid::new_v4(),
            title: title.to_string(),
            blocks: vec![Block {
                id: Uuid::new_v4(),
                kind: BlockKind::Paragraph,
                text: String::new(),
                spans: Vec::new(),
            }],
            annotations: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_new_has_one_empty_paragraph() {
        let d = Document::new("t");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(d.blocks[0].text, "");
    }

    #[test]
    fn a_heading_says_its_centered_only_when_it_is() {
        let heading = |centered| BlockKind::Heading { level: 2, centered };
        let plain = serde_json::to_string(&heading(false)).unwrap();
        assert_eq!(plain, r#"{"Heading":{"level":2}}"#);
        assert_eq!(
            serde_json::from_str::<BlockKind>(&plain).unwrap(),
            heading(false)
        );
        let centered = serde_json::to_string(&heading(true)).unwrap();
        assert_eq!(
            serde_json::from_str::<BlockKind>(&centered).unwrap(),
            heading(true)
        );
    }

    #[test]
    fn an_image_says_its_alignment_only_when_it_isnt_the_left() {
        let image = |align| BlockKind::Image {
            path: "a.png".to_string(),
            link: None,
            align,
        };
        let left = serde_json::to_string(&image(ImageAlign::Left)).unwrap();
        assert_eq!(left, r#"{"Image":{"path":"a.png"}}"#);
        assert_eq!(
            serde_json::from_str::<BlockKind>(&left).unwrap(),
            image(ImageAlign::Left)
        );
        for align in [ImageAlign::Center, ImageAlign::Right] {
            let said = serde_json::to_string(&image(align)).unwrap();
            assert_eq!(
                serde_json::from_str::<BlockKind>(&said).unwrap(),
                image(align)
            );
        }
    }

    #[test]
    fn serde_round_trip_preserves_document() {
        let d = Document {
            id: Uuid::new_v4(),
            title: "Round Trip".to_string(),
            blocks: vec![
                Block {
                    id: Uuid::new_v4(),
                    kind: BlockKind::Heading {
                        level: 1,
                        centered: false,
                    },
                    text: "Title".to_string(),
                    spans: vec![Span {
                        start: 0,
                        end: 5,
                        style: SpanStyle::Bold,
                    }],
                },
                Block {
                    id: Uuid::new_v4(),
                    kind: BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    text: "First".to_string(),
                    spans: Vec::new(),
                },
            ],
            annotations: vec![
                Annotation {
                    id: Uuid::new_v4(),
                    kind: AnnotationKind::Marginal,
                    anchor: Anchor::block(Uuid::new_v4()),
                    body: "note".to_string(),
                },
                Annotation {
                    id: Uuid::new_v4(),
                    kind: AnnotationKind::Note,
                    anchor: Anchor::range(Uuid::new_v4(), 0, 3),
                    body: "range note".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&d).unwrap();
        let d2: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(d, d2);
    }

    /// Files written before block-anchored marginalia stored every anchor as
    /// `{block_id, start, end}` with no `scope` field. Those must still read
    /// as range anchors, and the newer block anchor reads as a bare id string.
    #[test]
    fn anchor_reads_legacy_range_and_new_block_forms() {
        let id = Uuid::new_v4();

        // Legacy range form (a plain struct, no discriminant).
        let legacy = format!(r#"{{"block_id":"{id}","start":1,"end":4}}"#);
        let anchor: Anchor = serde_json::from_str(&legacy).unwrap();
        assert_eq!(anchor, Anchor::range(id, 1, 4));

        // New block form serializes as the bare id string and reads back.
        let block_json = serde_json::to_string(&Anchor::block(id)).unwrap();
        assert_eq!(block_json, format!(r#""{id}""#));
        let round: Anchor = serde_json::from_str(&block_json).unwrap();
        assert_eq!(round, Anchor::block(id));
    }
}

#[cfg(test)]
mod left_out_tests {
    use super::*;

    /// A struct with a field left out at its default, as `Metadata` or `Annotation` could come to
    /// have, held as one and as a list of them. Only the tests know it, and [`LEFT_OUT`] lists it
    /// only for them.
    #[derive(Debug, Serialize, Deserialize)]
    struct Pinned {
        body: String,
        #[serde(default, skip_serializing_if = "is_false")]
        pinned: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct HoldsPinned {
        test_pinned: Pinned,
        test_pinned_list: Vec<Pinned>,
    }

    fn pinned_round_trip(json: &str) -> (serde_json::Value, serde_json::Value) {
        let written = serde_json::from_str(json).unwrap();
        let read: HoldsPinned = serde_json::from_str(json).unwrap();
        (written, serde_json::to_value(read).unwrap())
    }

    /// A struct's field left out at its default is this build's own wherever the struct is written,
    /// and only there: under the field holding it, or as an item of a list of them.
    #[test]
    fn a_struct_field_written_at_its_default_is_this_builds_own() {
        for json in [
            r#"{"test_pinned":{"body":"a","pinned":false},"test_pinned_list":[]}"#,
            r#"{"test_pinned":{"body":"a"},"test_pinned_list":[{"body":"b","pinned":false}]}"#,
        ] {
            let (written, known) = pinned_round_trip(json);
            assert!(!has_unknown_fields(&written, &known), "{json}");
            assert!(!fitted_on_reading(&written, &known), "{json}");
            let beside = json.replacen(r#""pinned":false"#, r#""pinned":false,"boxed":false"#, 1);
            let (written, known) = pinned_round_trip(&beside);
            assert!(has_unknown_fields(&written, &known), "{beside}");
        }
        let (written, known) = pinned_round_trip(
            r#"{"test_pinned":{"body":"a","pinned":true},"test_pinned_list":[]}"#,
        );
        assert_eq!(known["test_pinned"]["pinned"], true);
        assert!(!has_unknown_fields(&written, &known));

        // Written alone, as the metadata table holds a document's metadata, it's named by where
        // that's kept, and nowhere else is it known.
        let alone: serde_json::Value =
            serde_json::from_str(r#"{"body":"a","pinned":false}"#).unwrap();
        let known =
            serde_json::to_value(serde_json::from_value::<Pinned>(alone.clone()).unwrap()).unwrap();
        assert!(!has_unknown_fields_held_at("test_pinned", &alone, &known));
        assert!(
            has_unknown_fields(&alone, &known),
            "a field of whatever is at the top"
        );
        assert!(has_unknown_fields_held_at("elsewhere", &alone, &known));
    }

    /// The smallest JSON of what holds the field at `place`, with the field written as `value`,
    /// that JSON read and written again, and where in it what holds the field is.
    fn holding(
        left_out: &LeftOut,
        place: &str,
        value: &str,
    ) -> (serde_json::Value, serde_json::Value, String) {
        const ID: &str = "00000000-0000-0000-0000-000000000001";
        let field = format!(r#""{}":{value}"#, left_out.field);
        let (json, pointer) = match place {
            "Heading" => (
                format!(r#"{{"Heading":{{"level":1,{field}}}}}"#),
                "/Heading",
            ),
            "ListItem" => (
                format!(r#"{{"ListItem":{{"ordered":true,{field}}}}}"#),
                "/ListItem",
            ),
            "Image" => (
                format!(r#"{{"Image":{{"path":"a.png",{field}}}}}"#),
                "/Image",
            ),
            "Code" => (format!(r#"{{"Code":{{{field}}}}}"#), "/Code"),
            "DeleteText" => (
                format!(
                    r#"{{"DeleteText":{{"block_id":"{ID}","start":0,"end":0,"text":"","removed":[],{field}}}}}"#
                ),
                "/DeleteText",
            ),
            "test_pinned" => (
                format!(r#"{{"test_pinned":{{"body":"",{field}}},"test_pinned_list":[]}}"#),
                "/test_pinned",
            ),
            "test_pinned_list[]" => (
                format!(
                    r#"{{"test_pinned":{{"body":""}},"test_pinned_list":[{{"body":"",{field}}}]}}"#
                ),
                "/test_pinned_list/0",
            ),
            other => panic!("nothing here holds what's at {other}: add it"),
        };
        let written: serde_json::Value = serde_json::from_str(&json).unwrap();
        let known = match left_out.of {
            "DeleteText" => {
                serde_json::to_value(serde_json::from_str::<crate::edit::Op>(&json).unwrap())
            }
            "Pinned" => serde_json::to_value(serde_json::from_str::<HoldsPinned>(&json).unwrap()),
            _ => serde_json::to_value(serde_json::from_str::<BlockKind>(&json).unwrap()),
        }
        .unwrap();
        (written, known, pointer.to_string())
    }

    /// A value of each field other than its default.
    fn another_value(left_out: &LeftOut) -> &'static str {
        match (left_out.of, left_out.field) {
            ("Heading", "centered") => "true",
            ("ListItem", "depth") => "2",
            ("Image", "link") => r#""https://example.com""#,
            ("Image", "align") => r#""Center""#,
            ("Code", "language") => r#""rust""#,
            ("DeleteText", "old_spans") => "[]",
            ("DeleteText", "clipped") => {
                r#"[{"id":"00000000-0000-0000-0000-000000000002","kind":"Note","anchor":"00000000-0000-0000-0000-000000000001","body":""}]"#
            }
            ("Pinned", "pinned") => "true",
            other => panic!("no other value for {other:?}: add one"),
        }
    }

    /// Every place each field is listed at, with what holds it there.
    fn every_place() -> impl Iterator<Item = (&'static LeftOut, &'static str)> {
        LEFT_OUT.iter().flat_map(|left_out| {
            assert!(
                !left_out.held_at.is_empty(),
                "{} is nowhere",
                left_out.field
            );
            left_out.held_at.iter().map(move |place| (left_out, *place))
        })
    }

    /// Each field is left out at the default listed, and at no other value, wherever it's listed,
    /// so that it's named as it's written, and listed at the default it really has.
    #[test]
    fn each_field_left_out_is_left_out_at_its_default_and_kept_otherwise() {
        for (left_out, place) in every_place() {
            let name = format!("{}.{} at {place}", left_out.of, left_out.field);
            let (written, known, pointer) = holding(left_out, place, left_out.default);
            let fields = |value: &serde_json::Value| value.pointer(&pointer).unwrap().clone();
            assert!(
                fields(&known).get(left_out.field).is_none(),
                "{name} is kept"
            );
            assert!(!has_unknown_fields(&written, &known), "{name} is unknown");
            assert!(!fitted_on_reading(&written, &known), "{name} is fitted");

            let (written, known, _) = holding(left_out, place, another_value(left_out));
            assert_eq!(
                fields(&known),
                fields(&written),
                "{name} isn't read as written"
            );
            assert!(!has_unknown_fields(&written, &known), "{name}");
        }
    }

    /// Beside the fields left out at their defaults, a field this build doesn't know is newer still,
    /// whatever it holds.
    #[test]
    fn a_field_beside_them_that_this_build_does_not_know_is_unknown() {
        for (left_out, place) in every_place() {
            for value in [left_out.default, "false", "null", "0"] {
                let (mut written, known, pointer) = holding(left_out, place, left_out.default);
                written.pointer_mut(&pointer).unwrap()["boxed"] =
                    serde_json::from_str(value).unwrap();
                assert!(
                    has_unknown_fields(&written, &known),
                    "{} at {place}: boxed {value}",
                    left_out.of
                );
            }
        }
    }

    /// Types with fields left out at their defaults that no document, step of history or revision
    /// holds, so that what reads those needn't know them.
    const NOT_IN_DOCUMENTS: &[&str] = &[
        // A notebook's references, each in a YAML file of its own, read with unknown fields denied.
        "Reference",
    ];

    /// Every `.rs` file under `folder`, however deep.
    fn sources_in(folder: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources_in(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    /// What a line opening a type or a variant opens, if it opens one: a struct or a variant by
    /// name, or `None` for an enum, whose fields are only in its variants.
    fn opened_by(line: &str) -> Option<Option<(&str, &str)>> {
        let line = line.trim();
        let line = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        fn name(rest: &str) -> &str {
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            &rest[..end]
        }
        if let Some(rest) = line.strip_prefix("struct ") {
            return Some(Some(("struct", name(rest))));
        }
        if line.starts_with("enum ") {
            return Some(None);
        }
        let variant = line.strip_suffix(" {")?;
        (variant.starts_with(|c: char| c.is_ascii_uppercase())
            && variant.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(Some(("variant", variant)))
    }

    /// Every field left out at its default in the sources of every crate, and the app's, is listed,
    /// so that one added later can't be forgotten, and taken for a newer version's when written at
    /// its default. Each is named by the struct, kind or step it's in.
    #[test]
    fn every_field_left_out_at_its_default_is_listed() {
        let needle = concat!("skip_serializing", "_if");
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let mut files = Vec::new();
        for entry in std::fs::read_dir(crates).unwrap() {
            sources_in(&entry.unwrap().path().join("src"), &mut files);
        }
        sources_in(&crates.join("../src"), &mut files);
        files.sort();
        assert!(
            files
                .iter()
                .any(|path| path.ends_with("kame-sync/src/revlog.rs")),
            "the revision logs aren't read"
        );
        let mut found = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                if !line.contains(needle) || line.trim_start().starts_with("//") {
                    continue;
                }
                let place = format!("{}:{}", path.display(), at + 1);
                let field = lines[at + 1..]
                    .iter()
                    .find_map(|line| {
                        let line = line.trim_start();
                        let line = line.strip_prefix("pub ").unwrap_or(line);
                        let (name, _) = line.split_once(": ")?;
                        name.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                            .then_some(name)
                    })
                    .unwrap_or_else(|| panic!("{place}: no field"));
                let (_, of) = lines[..at]
                    .iter()
                    .rev()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .find_map(|line| opened_by(line))
                    .flatten()
                    .unwrap_or_else(|| panic!("{place}: {field} is in nothing this can name"));
                if !NOT_IN_DOCUMENTS.contains(&of) {
                    found.push((of.to_string(), field.to_string(), place));
                }
            }
        }
        let listed: Vec<(&str, &str)> = LEFT_OUT
            .iter()
            .map(|left_out| (left_out.of, left_out.field))
            .collect();
        for (of, field, place) in &found {
            assert!(
                listed.contains(&(of.as_str(), field.as_str())),
                "{place}: {of}.{field} is left out at its default and not in LEFT_OUT, so another \
                 program writing it at its default would make a document read-only as a newer \
                 version's. List it with the keys what holds it is written under."
            );
        }
        let mut named: Vec<(&str, &str)> = found
            .iter()
            .map(|(of, field, _)| (of.as_str(), field.as_str()))
            .collect();
        named.sort();
        named.dedup();
        let mut listed = listed;
        listed.sort();
        assert_eq!(named, listed, "found against listed");
    }
}

#[cfg(test)]
mod script_url_tests {
    use super::is_script_url;

    #[test]
    fn script_urls_are_recognised_however_they_are_disguised() {
        for url in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "  javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "\u{1}javascript:alert(1)",
        ] {
            assert!(is_script_url(url), "{url:?}");
        }
    }

    #[test]
    fn ordinary_links_are_not_script_urls() {
        for url in [
            "https://example.com/paper.pdf",
            "paper.pdf",
            "/files/paper.pdf",
            "file:///home/kyle/paper.pdf",
            "mailto:k@example.com",
            "javascript.pdf",
            "docs/javascript:notes.html",
        ] {
            assert!(!is_script_url(url), "{url:?}");
        }
    }

    use super::{link_style, SpanStyle};

    fn is_document_link(url: &str) -> bool {
        matches!(link_style(url.to_string()), SpanStyle::DocumentLink { .. })
    }

    #[test]
    fn a_link_to_a_kame_file_or_a_heading_is_a_document_link() {
        assert!(is_document_link("notes/essay.kame"));
        assert!(is_document_link("notes/essay.kame#Intro"));
        assert!(is_document_link("~/essay.KAME"));
        assert!(is_document_link("#Intro"));
    }

    #[test]
    fn any_other_link_stays_a_link() {
        assert!(!is_document_link("https://example.com/essay.kame"));
        assert!(!is_document_link("notes/paper.pdf"));
        assert!(!is_document_link("notes/essay"));
        assert!(!is_document_link("#"));
    }

    #[test]
    fn a_document_link_keeps_its_target_exactly() {
        assert_eq!(
            link_style(" a b.kame#C d".to_string()),
            SpanStyle::DocumentLink {
                target: " a b.kame#C d".to_string()
            }
        );
    }

    use super::{list_positions, Block, BlockKind, ListPosition, MAX_LIST_DEPTH};

    fn item(ordered: bool, depth: u8, text: &str) -> Block {
        Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::ListItem { ordered, depth },
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn para(text: &str) -> Block {
        Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn at(depth: u8, number: Option<usize>) -> Option<ListPosition> {
        Some(ListPosition { depth, number })
    }

    #[test]
    fn numbers_count_within_a_level_and_carry_on_after_a_nested_list() {
        let blocks = [
            item(true, 0, "a"),
            item(true, 1, "b"),
            item(true, 1, "c"),
            item(true, 0, "d"),
            item(true, 1, "e"),
        ];
        assert_eq!(
            list_positions(&blocks, false),
            vec![
                at(0, Some(1)),
                at(1, Some(1)),
                at(1, Some(2)),
                at(0, Some(2)),
                at(1, Some(1)),
            ]
        );
    }

    #[test]
    fn bullets_have_no_number_and_restart_the_count_at_their_level() {
        let blocks = [item(true, 0, "a"), item(false, 0, "b"), item(true, 0, "c")];
        assert_eq!(
            list_positions(&blocks, false),
            vec![at(0, Some(1)), at(0, None), at(0, Some(1))]
        );
    }

    #[test]
    fn anything_else_ends_the_list() {
        let blocks = [item(true, 0, "a"), para("x"), item(true, 2, "b")];
        assert_eq!(
            list_positions(&blocks, false),
            vec![at(0, Some(1)), None, at(0, Some(1))]
        );
    }

    /// Markdown can't express an item two levels below its parent, and Outdent can leave
    /// one, so it reads as just one level down.
    #[test]
    fn an_item_is_never_more_than_one_level_below_the_one_before() {
        let blocks = [
            item(false, 3, "a"),
            item(false, 3, "b"),
            item(false, 0, "c"),
            item(false, 4, "d"),
        ];
        assert_eq!(
            list_positions(&blocks, false),
            vec![at(0, None), at(1, None), at(0, None), at(1, None)]
        );
    }

    #[test]
    fn depth_stops_at_the_limit() {
        let blocks: Vec<Block> = (0..9).map(|depth| item(false, depth, "a")).collect();
        let last = list_positions(&blocks, false)[8].unwrap();
        assert_eq!(last.depth, MAX_LIST_DEPTH);
    }

    #[test]
    fn exports_see_past_empty_blocks_and_the_page_does_not() {
        let blocks = [
            item(true, 0, "a"),
            item(true, 0, ""),
            para(""),
            item(true, 0, "b"),
        ];
        assert_eq!(
            list_positions(&blocks, true),
            vec![at(0, Some(1)), None, None, at(0, Some(2))]
        );
        assert_eq!(
            list_positions(&blocks, false),
            vec![at(0, Some(1)), at(0, Some(2)), None, at(0, Some(1))]
        );
    }

    #[test]
    fn an_older_list_item_reads_as_top_level_and_a_top_level_one_writes_as_before() {
        let kind: BlockKind = serde_json::from_str(r#"{"ListItem":{"ordered":true}}"#).unwrap();
        assert_eq!(
            kind,
            BlockKind::ListItem {
                ordered: true,
                depth: 0
            }
        );
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"ListItem":{"ordered":true}}"#
        );
    }

    fn labels(blocks: &[Block]) -> Vec<Option<String>> {
        super::list_labels(blocks)
    }

    fn label(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn a_nested_number_carries_its_parents_numbers() {
        let blocks = [
            item(true, 0, "a"),
            item(true, 1, "b"),
            item(true, 2, "c"),
            item(true, 2, "d"),
            item(true, 1, "e"),
            item(true, 0, "f"),
            item(true, 1, "g"),
        ];
        assert_eq!(
            labels(&blocks),
            vec![
                label("1."),
                label("1.1"),
                label("1.1.1"),
                label("1.1.2"),
                label("1.2"),
                label("2."),
                label("2.1"),
            ]
        );
    }

    #[test]
    fn a_bullet_has_no_label_and_adds_nothing_to_the_number_below_it() {
        let blocks = [item(true, 0, "a"), item(false, 1, "b"), item(true, 2, "c")];
        assert_eq!(labels(&blocks), vec![label("1."), None, label("1.1")]);
    }

    #[test]
    fn a_number_under_a_top_level_bullet_is_a_top_level_number() {
        let blocks = [item(false, 0, "a"), item(true, 1, "b"), item(true, 1, "c")];
        assert_eq!(labels(&blocks), vec![None, label("1."), label("2.")]);
    }

    #[test]
    fn labels_start_over_after_the_list_ends() {
        let blocks = [
            item(true, 0, "a"),
            item(true, 1, "b"),
            para("x"),
            item(true, 0, "c"),
        ];
        assert_eq!(
            labels(&blocks),
            vec![label("1."), label("1.1"), None, label("1.")]
        );
    }
}
