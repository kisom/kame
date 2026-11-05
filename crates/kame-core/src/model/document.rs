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
        level: u8,
    },
    ListItem {
        ordered: bool,
        /// How many lists this one is nested inside.
        #[serde(default, skip_serializing_if = "is_zero")]
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

fn is_zero(depth: &u8) -> bool {
    *depth == 0
}

/// How deeply lists can nest.
pub const MAX_LIST_DEPTH: u8 = 5;

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
/// `https://` and `www.` forms, so a pasted or typed link becomes a clickable
/// range without the author marking it up.
pub fn link_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let scheme = ["https://", "http://", "www."]
            .into_iter()
            .find(|prefix| rest.starts_with(prefix));
        let Some(scheme) = scheme else {
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
    pub start: usize,
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
        start: usize,
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
                    kind: BlockKind::Heading { level: 1 },
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
