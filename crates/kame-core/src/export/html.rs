//! HTML export.
//!
//! `to_html` renders a [`Document`] as one self-contained HTML file: the page
//! is what the draft publishes as. Blocks become headings, paragraphs and lists;
//! spans become `<em>`/`<strong>`; marginalia become floating `<aside>`s beside
//! the passage they annotate, and footnotes become superscript links into a
//! numbered footnote list at the foot of the document. Notes are the author's own
//! and never publish. The embedded stylesheet carries the house
//! typography — Crimson Pro for headings, EB Garamond for body text,
//! justified prose, old-style figures.
//!
//! Like the Markdown exporter, output depends only on the document contents:
//! all apparatus is numbered in reading order (block order, then anchor
//! position, then annotation id), never in storage order, so exporting the same
//! document twice — or after a serde round trip — produces identical bytes.

use crate::model::bare_link_ranges;
use crate::model::{
    is_script_url, list_positions, AnnotationKind, Block, BlockId, BlockKind, Document, SpanStyle,
};
use std::collections::HashMap;

/// The embedded stylesheet.
///
/// The fonts are named, not embedded: Crimson Pro and EB Garamond are the
/// fonts the editor ships (`fonts/`), so an exported file picks them up
/// wherever they are installed and falls back to a serif otherwise.
pub const STYLE: &str = r#"
:root {
  color-scheme: light;
  --paper: #f7f3eb;
  --ink: #201d19;
  --muted: #564f45;
  --rule: #968d7e;
  --footnote: #366084;
  --link: #3b82f6;
  --mark: rgba(255, 170, 60, 0.27);
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --paper: #1e1c19;
  --ink: #ded8cd;
  --muted: #a8a094;
  --rule: #6e6860;
  --footnote: #78a0be;
  --mark: rgba(196, 128, 40, 0.31);
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --paper: #1e1c19;
    --ink: #ded8cd;
    --muted: #a8a094;
    --rule: #6e6860;
    --footnote: #78a0be;
  --mark: rgba(196, 128, 40, 0.31);
  }
}
html {
  font-size: 100%;
}
body {
  margin: 0;
  background: var(--paper);
  color: var(--ink);
  font-family: "EB Garamond", "Iowan Old Style", Georgia, serif;
  font-size: 1.25em;
  line-height: 1.6;
  font-feature-settings: "liga" 1, "kern" 1;
}
main.document {
  box-sizing: border-box;
  max-width: 36rem;
  margin: 0 auto;
  padding: 5rem 1.5rem 6rem;
}
a {
  color: var(--link);
}
img.image {
  max-width: 100%;
  height: auto;
  display: block;
  margin: 1.2em 0;
}
h1, h2, h3, h4, h5, h6 {
  font-family: "Crimson Pro", "EB Garamond", Georgia, serif;
  font-weight: bold;
  line-height: 1.5;
  text-align: left;
  margin: 2em 0 0.35em;
}
h1 {
  font-size: 1.75em;
  margin-top: 1.3em;
}
h2 {
  font-size: 1.4em;
  margin-top: 1.4em;
}
h3 {
  font-size: 1.12em;
  margin-top: 1.33em;
}
h4, h5, h6 {
  font-size: 1em;
  margin-top: 1.49em;
}
p {
  margin: 0 0 0.875em;
  text-align: justify;
  hanging-punctuation: first allow-end last;
}
strong {
  font-weight: bold;
}
mark {
  background: var(--mark);
  color: inherit;
}
p.byline {
  color: var(--muted);
  text-align: left;
}
em {
  font-style: italic;
}
blockquote {
  margin: 1.225em 0 1.225em;
  margin-left: 1.5em;
  font-style: italic;
}
li > ul, li > ol {
  margin: 0.875em 0 0;
}
ol:not(.footnotes) {
  counter-reset: item;
}
ol:not(.footnotes) > li {
  counter-increment: item;
  list-style: none;
  position: relative;
}
ol:not(.footnotes) > li::before {
  content: counters(item, ".") ".";
  position: absolute;
  right: 100%;
  margin-right: 0.4em;
  white-space: nowrap;
}
li ol:not(.footnotes) > li::before {
  content: counters(item, ".");
}
ul, ol {
  margin: 0 0 0.875em;
  padding-left: 1.75em;
}
li {
  margin: 0.875em 0;
}
pre {
  margin: 1.225em 0 1.225em 1.5em;
  white-space: pre-wrap;
}
code {
  font-family: "IBM Plex Mono", ui-monospace, Menlo, Consolas, monospace;
  font-size: 0.84em;
  font-style: normal;
}
pre code {
  font-size: 0.75em;
}
pre.prose {
  font-family: inherit;
}
hr {
  width: 25%;
  margin: 1.5em auto;
  border: 0;
  border-top: 1px solid var(--rule);
}
aside.marginalia {
  float: right;
  clear: right;
  width: 13rem;
  margin: 0.2rem -16rem 1rem 2rem;
  font-size: 0.75em;
  line-height: 1.5;
  font-style: italic;
  color: var(--muted);
}
aside.marginalia p {
  margin: 0 0 0.6em;
  text-align: left;
}
sup.footnote-ref {
  font-size: 0.7em;
  line-height: 0;
  vertical-align: super;
}
sup.footnote-ref a {
  color: var(--footnote);
  text-decoration: none;
  padding: 0 0.15em;
}
ol.footnotes {
  margin: 3rem 0 0;
  padding: 1.2rem 0 0 1.4rem;
  border-top: 1px solid var(--rule);
  font-size: 0.88rem;
  color: var(--muted);
}
ol.footnotes li {
  margin: 0.5em 0;
}
a.footnote-backref {
  color: var(--muted);
  text-decoration: none;
  margin-left: 0.3em;
}
@media (max-width: 72rem) {
  main.document { max-width: 38rem; }
  aside.marginalia {
    float: none;
    width: auto;
    margin: 0.6rem 0 1.2rem;
    padding-left: 0.9rem;
    border-left: 2px solid var(--rule);
  }
}
@media print {
  :root, :root[data-theme="dark"], :root:not([data-theme="light"]) {
    color-scheme: light;
    --paper: #fff;
    --ink: #000;
    --muted: #333;
    --rule: #888;
    --footnote: #000;
    --link: #000;
    --mark: #eee;
  }
  body { font-size: 11pt; }
  main.document { padding: 0; }
  nav.site { display: none; }
  aside.marginalia {
    float: right;
    width: 11rem;
    margin: 0 0 1rem 1.5rem;
    padding-left: 0;
    border-left: 0;
  }
}
"#;

/// The apparatus anchored to one block, already in reading order.
#[derive(Default, Clone)]
struct BlockApparatus {
    /// Marginal bodies, in reading order.
    marginalia: Vec<String>,
    /// `(anchor end offset, footnote number)` pairs, in reading order.
    footnotes: Vec<(usize, usize)>,
}

/// Render `doc` as a self-contained HTML document.
///
/// Layout rules:
/// - headings become `<h1>`–`<h6>` (level clamped to 1–6), paragraphs `<p>`,
///   and consecutive list items of the same kind share one `<ul>`/`<ol>`,
/// - inline spans become `<em>`/`<strong>`, nesting innermost-first,
/// - marginalia become `<aside class="marginalia">` immediately before the block
///   they annotate (inside the `<li>` for list items), in reading order, since a
///   float cannot rise above where it is written and has to line up with its block; a
///   margin note whose block is gone or empty is emitted unanchored after the
///   last block, so its body survives,
/// - footnotes leave a `<sup class="footnote-ref"><a href="#fnN">N</a></sup>`
///   marker at the end of their anchored range and collect in one
///   `<ol class="footnotes">` at the foot, numbered in reading order,
/// - a note's words do not publish, though the passage it highlights does, as `<mark>`,
/// - empty blocks are skipped; a footnote or margin note that has lost
///   its anchor loses only its marker — the body is still emitted, unanchored,
/// - text is escaped (`&`, `<`, `>`); the output carries no document or
///   annotation ids, so it is stable across runs and serde round trips.
pub fn to_html(doc: &Document) -> String {
    to_html_with(doc, &crate::model::Metadata::default())
}

/// How a document's outside references publish, for a tool that knows where everything
/// ends up. On its own, kame can't know, so see [`Publish::plain`].
pub struct Publish<'a> {
    /// The URL for a document link's target, or `None` to publish it as its text.
    pub document_link: &'a dyn Fn(&str) -> Option<String>,
    /// The URL for an image's path.
    pub image: &'a dyn Fn(&str) -> String,
}

impl Publish<'_> {
    pub fn plain() -> Publish<'static> {
        Publish {
            document_link: &|_| None,
            image: &|path| path.to_string(),
        }
    }
}

/// [`to_html`] for publishing: `metadata` fills the page's head. An empty title
/// falls back to the document's own, and an empty date to the created date.
pub fn to_html_with(doc: &Document, metadata: &crate::model::Metadata) -> String {
    let body = render_body(doc, &Publish::plain(), Some(metadata.author.trim()));
    let title = if metadata.title.is_empty() {
        escape(&doc.title)
    } else {
        escape(&metadata.title)
    };
    let date = if metadata.published.is_empty() {
        &metadata.created
    } else {
        &metadata.published
    };
    let tags = metadata.tags.join(", ");
    let metas: String = [
        ("author", metadata.author.as_str()),
        ("date", date.as_str()),
        ("keywords", tags.as_str()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| {
        format!(
            "<meta name=\"{name}\" content=\"{}\">\n",
            escape_attribute(value)
        )
    })
    .collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         {metas}<title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <main class=\"document\">\n{body}</main>\n</body>\n</html>\n"
    )
}

/// A document's own markup, without the page around it, for a tool that supplies its own
/// page. A whole page is this inside [`to_html_with`]'s head.
pub fn html_body(doc: &Document, publish: &Publish) -> String {
    render_body(doc, publish, None)
}

/// [`html_body`], with `author` as a visible byline if it is given: under a first heading, or
/// else leading the document.
fn render_body(doc: &Document, publish: &Publish, author: Option<&str>) -> String {
    let collected = collect_apparatus(doc);

    let byline = author
        .filter(|author| !author.is_empty())
        .map(|author| format!("<p class=\"byline\">{}</p>\n", escape(author)));
    let under_heading = doc.blocks.first().is_some_and(|first| {
        matches!(first.kind, BlockKind::Heading { .. }) && !first.text.is_empty()
    });

    let mut body = String::new();
    if let (Some(byline), false) = (&byline, under_heading) {
        body.push_str(byline);
    }
    let mut open_list: Vec<bool> = Vec::new();
    let positions = list_positions(&doc.blocks, true);
    for (index, block) in doc.blocks.iter().enumerate() {
        if block.kind == BlockKind::Rule {
            close_list(&mut body, &mut open_list);
            body.push_str("<hr>\n");
            continue;
        }
        // An image is a block with no text: emit it as an `<img>` before the
        // empty-text skip drops it.
        if let BlockKind::Image { path, link } = &block.kind {
            close_list(&mut body, &mut open_list);
            let image = format!(
                "<img class=\"image\" src=\"{}\" alt=\"\">",
                escape_attribute(&(publish.image)(path))
            );
            match link.as_deref().filter(|url| !is_script_url(url)) {
                Some(url) => body.push_str(&format!(
                    "<a href=\"{}\">{image}</a>\n",
                    escape_attribute(url)
                )),
                None => {
                    body.push_str(&image);
                    body.push('\n');
                }
            }
            continue;
        }
        // An empty block has no glyphs to carry a marker or to hang apparatus
        // on. Checked before the list bookkeeping, so an emptied item neither
        // splits a list nor consumes an ordered number.
        if block.text.is_empty() {
            continue;
        }
        let apparatus = &collected.blocks[index];
        let inner = render_inline(
            block,
            &apparatus.footnotes,
            &note_highlights(doc, block),
            publish,
        );
        // A bare newline renders as a space. A heading is one line by definition, and
        // a verbatim block is preformatted, so its newlines stand.
        let inner = inner.trim_end_matches('\n');
        let inner = match &block.kind {
            BlockKind::Heading { .. } => inner.replace('\n', " "),
            kind if kind.is_verbatim() => inner.to_string(),
            _ => inner.replace('\n', "<br>\n"),
        };
        let after = render_apparatus(apparatus);
        match block.kind {
            BlockKind::Heading { level } => {
                close_list(&mut body, &mut open_list);
                let level = level.clamp(1, 6);
                body.push_str(&after);
                body.push_str(&format!("<h{level}>{inner}</h{level}>\n"));
                if let (Some(byline), 0) = (&byline, index) {
                    body.push_str(byline);
                }
            }
            BlockKind::Paragraph => {
                close_list(&mut body, &mut open_list);
                body.push_str(&after);
                body.push_str(&format!("<p>{inner}</p>\n"));
            }
            BlockKind::BlockQuote => {
                close_list(&mut body, &mut open_list);
                body.push_str(&after);
                body.push_str(&format!("<blockquote>{inner}</blockquote>\n"));
            }
            BlockKind::ListItem { ordered, .. } => {
                let level =
                    usize::from(positions[index].expect("a list item has a position").depth);
                while open_list.len() > level + 1 {
                    close_one_list(&mut body, &mut open_list);
                }
                match open_list.get(level) {
                    Some(open) if *open == ordered => body.push_str("</li>\n"),
                    Some(_) => close_one_list(&mut body, &mut open_list),
                    // A list opening inside an item, whose <li> is still open.
                    None if level > 0 => body.push('\n'),
                    None => {}
                }
                if open_list.len() == level {
                    body.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                    open_list.push(ordered);
                }
                // Left open, since a nested list goes inside it.
                body.push_str(&format!("<li>{after}{inner}"));
            }
            BlockKind::Code { ref language } => {
                close_list(&mut body, &mut open_list);
                body.push_str(&after);
                match language {
                    Some(language) => body.push_str(&format!(
                        "<pre><code class=\"language-{}\">{inner}</code></pre>\n",
                        escape_attribute(language)
                    )),
                    None => body.push_str(&format!("<pre><code>{inner}</code></pre>\n")),
                }
            }
            BlockKind::Prose => {
                close_list(&mut body, &mut open_list);
                body.push_str(&after);
                body.push_str(&format!("<pre class=\"prose\">{inner}</pre>\n"));
            }
            BlockKind::Image { .. } | BlockKind::Rule => {
                unreachable!("these are emitted before the empty-text skip")
            }
        }
    }
    close_list(&mut body, &mut open_list);

    // Margin notes whose anchor block no longer exists: the anchor is gone, the
    // authored text is not.
    for marginal in &collected.unanchored_marginalia {
        body.push_str(&marginalia_aside(marginal));
    }

    if !collected.footnotes.is_empty() {
        body.push_str("<ol class=\"footnotes\">\n");
        for footnote in &collected.footnotes {
            let number = footnote.number;
            let text = escape(&footnote.body);
            if footnote.anchored {
                body.push_str(&format!(
                    "<li id=\"fn{number}\">{text} <a class=\"footnote-backref\" \
                     href=\"#fnref{number}\">&#8617;</a></li>\n"
                ));
            } else {
                // No marker in the text to return to.
                body.push_str(&format!("<li id=\"fn{number}\">{text}</li>\n"));
            }
        }
        body.push_str("</ol>\n");
    }
    body
}

/// Close a list still open from the previous block, if any.
fn close_list(out: &mut String, open_list: &mut Vec<bool>) {
    while !open_list.is_empty() {
        close_one_list(out, open_list);
    }
}

/// Closes the innermost open list and the item left open in it.
fn close_one_list(out: &mut String, open_list: &mut Vec<bool>) {
    match open_list.pop() {
        Some(true) => out.push_str("</li>\n</ol>\n"),
        Some(false) => out.push_str("</li>\n</ul>\n"),
        None => {}
    }
}

/// A footnote's body in number order.
struct FootnoteBody {
    number: usize,
    body: String,
    /// Whether a `<sup>` marker exists in the text to link back to. A body
    /// whose anchor is gone keeps its text but has no marker to return to.
    anchored: bool,
}

/// Everything the annotation list contributes to the page: apparatus grouped by
/// block, the footnote list in number order, and marginalia whose block is gone.
struct Collected {
    blocks: Vec<BlockApparatus>,
    footnotes: Vec<FootnoteBody>,
    /// Margin-note bodies that lost their block (or whose block is empty), in
    /// reading order. They are emitted as plain asides after the last block:
    /// the anchor is gone, the authored text is not.
    unanchored_marginalia: Vec<String>,
}

/// One live annotation, placed against the block it renders beside.
struct Placed<'a> {
    /// Index of the block it hangs off, when that block still exists.
    index: Option<usize>,
    /// The anchor still has a rendered block to carry its apparatus.
    attached: bool,
    start: usize,
    end: usize,
    annotation: &'a crate::model::Annotation,
}

/// Group every live annotation by block, in reading order, and number the
/// footnotes. A note is not collected: notes never publish.
///
/// Reading order is block order, then anchor start, then anchor end, then
/// annotation id — storage order never shows through, so numbering is stable.
/// A published annotation's body is never thrown away for want of a marker: a
/// footnote or margin note whose block is missing or empty is emitted unanchored
/// (a footnote with its number but no marker or back-link, a margin note as a
/// bare aside).
///
/// Returns the per-block apparatus, the footnote bodies in number order, and
/// the unanchored marginalia.
fn collect_apparatus(doc: &Document) -> Collected {
    let block_order: HashMap<BlockId, usize> = doc
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect();

    let mut ordered: Vec<Placed> = doc
        .annotations
        .iter()
        .filter(|annotation| {
            matches!(
                annotation.kind,
                AnnotationKind::Marginal | AnnotationKind::Footnote
            )
        })
        .filter_map(|annotation| {
            let index = block_order.get(&annotation.anchor.block_id()).copied();
            let text = index.map_or("", |index| doc.blocks[index].text.as_str());
            match annotation.kind {
                // A margin note hangs off the block it annotates. If that
                // block is gone or empty there is nothing to float beside, but
                // the body still belongs in the export.
                AnnotationKind::Marginal => match index {
                    Some(index) if !text.is_empty() => Some(Placed {
                        index: Some(index),
                        attached: true,
                        start: 0,
                        end: 0,
                        annotation,
                    }),
                    _ => Some(Placed {
                        index: None,
                        attached: false,
                        start: 0,
                        end: 0,
                        annotation,
                    }),
                },
                AnnotationKind::Note => None,
                AnnotationKind::Footnote => match index {
                    Some(index) if !text.is_empty() => {
                        let (anchor_start, anchor_end) =
                            annotation.anchor.byte_range().unwrap_or((0, 0));
                        Some(Placed {
                            index: Some(index),
                            attached: true,
                            start: snap_down(text, anchor_start),
                            end: snap_down(text, anchor_end),
                            annotation,
                        })
                    }
                    _ => Some(Placed {
                        index: None,
                        attached: false,
                        start: 0,
                        end: 0,
                        annotation,
                    }),
                },
            }
        })
        .collect();
    ordered.sort_by_key(|placed| {
        (
            placed.index.unwrap_or(usize::MAX),
            placed.start,
            placed.end,
            placed.annotation.id,
        )
    });

    let mut collected = Collected {
        blocks: vec![BlockApparatus::default(); doc.blocks.len()],
        footnotes: Vec::new(),
        unanchored_marginalia: Vec::new(),
    };
    let mut footnote_number = 0usize;
    for placed in ordered {
        let annotation = placed.annotation;
        match annotation.kind {
            AnnotationKind::Marginal => {
                if placed.attached {
                    let index = placed.index.expect("an attached margin note has a block");
                    collected.blocks[index]
                        .marginalia
                        .push(annotation.body.clone());
                } else {
                    collected
                        .unanchored_marginalia
                        .push(annotation.body.clone());
                }
            }
            AnnotationKind::Note => {}
            AnnotationKind::Footnote => {
                footnote_number += 1;
                if placed.attached {
                    let index = placed.index.expect("an anchored footnote has a block");
                    collected.blocks[index]
                        .footnotes
                        .push((placed.end, footnote_number));
                }
                collected.footnotes.push(FootnoteBody {
                    number: footnote_number,
                    body: annotation.body.clone(),
                    anchored: placed.attached,
                });
            }
        }
    }
    collected
        .blocks
        .iter_mut()
        .for_each(|block| block.footnotes.sort_unstable());

    collected
}

/// Render the apparatus that follows a block: marginalia asides. Footnotes are
/// collected at the foot of the document instead.
fn render_apparatus(apparatus: &BlockApparatus) -> String {
    apparatus
        .marginalia
        .iter()
        .map(|body| marginalia_aside(body))
        .collect()
}

/// The margin note's body. The `<aside>` is where a marginal is read, whether
/// it kept its block or lost it.
fn marginalia_aside(body: &str) -> String {
    format!(
        "<aside class=\"marginalia\"><p>{}</p></aside>\n",
        escape(body).replace('\n', "<br>\n")
    )
}

/// One open inline element, compared by identity when deciding what to close.
#[derive(Clone, PartialEq)]
enum InlineTag {
    Bold,
    Italic,
    Code,
    Mark,
    Link { url: String },
}

impl InlineTag {
    fn open(&self) -> String {
        match self {
            InlineTag::Bold => "<strong>".to_string(),
            InlineTag::Italic => "<em>".to_string(),
            InlineTag::Code => "<code>".to_string(),
            InlineTag::Mark => "<mark>".to_string(),
            InlineTag::Link { url } => format!("<a href=\"{}\">", escape_attribute(url)),
        }
    }

    fn close(&self) -> &'static str {
        match self {
            InlineTag::Bold => "</strong>",
            InlineTag::Italic => "</em>",
            InlineTag::Code => "</code>",
            InlineTag::Mark => "</mark>",
            InlineTag::Link { .. } => "</a>",
        }
    }
}

/// The passages notes highlight in `block`, as byte ranges that fit its text. A note's words
/// never publish, but what it marks does.
fn note_highlights(doc: &Document, block: &Block) -> Vec<(usize, usize)> {
    doc.annotations
        .iter()
        .filter(|annotation| {
            annotation.kind == AnnotationKind::Note && annotation.anchor.block_id() == block.id
        })
        .filter_map(|annotation| {
            let (start, end) = annotation.anchor.byte_range()?;
            let start = snap_up(&block.text, start.min(block.text.len()));
            let end = snap_down(&block.text, end.min(block.text.len()));
            (start < end).then_some((start, end))
        })
        .collect()
}

/// Render one block's text with spans and footnote markers applied.
///
/// The text is cut at every span edge, note edge and footnote offset, and the
/// active tag set is taken per segment — the same segmentation the Markdown
/// exporter uses. At each boundary the stack is cut back to the longest prefix
/// it shares with the segment's tag set, then the missing tags are opened, so
/// the tags always nest properly. Cutting back to the first tag that stopped
/// or changed position (rather than stopping at the innermost wanted one) is
/// what keeps a crossing pair honest: italic 0..10 with bold 5..15 has to close
/// both tags at 10 and reopen bold, or the italic would silently cover 10..15.
/// Notes open outside spans, so a highlighted phrase can carry emphasis without
/// producing crossed tags.
fn render_inline(
    block: &Block,
    footnotes: &[(usize, usize)],
    highlights: &[(usize, usize)],
    publish: &Publish,
) -> String {
    let text = block.text.as_str();

    let spans: Vec<(usize, usize, SpanStyle)> = block
        .spans
        .iter()
        // A script link publishes as its text, as does a link to a kame document unless
        // the hook knows where that document is.
        .filter(|span| !matches!(&span.style, SpanStyle::Link { url } if is_script_url(url)))
        .filter_map(|span| {
            let style = match &span.style {
                SpanStyle::DocumentLink { target } => SpanStyle::Link {
                    url: (publish.document_link)(target)?,
                },
                style => style.clone(),
            };
            let start = snap_up(text, span.start);
            let end = snap_down(text, span.end);
            let (start, end) = trim_whitespace_edges(text, start, end);
            (start < end).then_some((start, end, style))
        })
        .collect();
    let links = bare_link_ranges(block);

    let mut boundaries =
        Vec::with_capacity(spans.len() * 2 + links.len() * 2 + footnotes.len() + 2);
    boundaries.push(0);
    boundaries.push(text.len());
    for (start, end, _) in &spans {
        boundaries.push(*start);
        boundaries.push(*end);
    }
    for (start, end) in &links {
        boundaries.push(*start);
        boundaries.push(*end);
    }
    for (start, end) in highlights {
        boundaries.push(*start);
        boundaries.push(*end);
    }
    for (offset, _) in footnotes {
        boundaries.push(*offset);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out = String::with_capacity(text.len() + boundaries.len() * 4);
    let mut open: Vec<InlineTag> = Vec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        let mut wanted: Vec<InlineTag> = Vec::new();
        if highlights.iter().any(|(s, e)| *s <= start && end <= *e) {
            wanted.push(InlineTag::Mark);
        }
        for style in [SpanStyle::Bold, SpanStyle::Italic, SpanStyle::Code] {
            let covered = spans
                .iter()
                .any(|(s, e, covered)| *covered == style && *s <= start && *e >= end);
            if covered {
                wanted.push(match style {
                    SpanStyle::Bold => InlineTag::Bold,
                    SpanStyle::Italic => InlineTag::Italic,
                    SpanStyle::Code => InlineTag::Code,
                    SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. } => {
                        unreachable!("links are handled separately")
                    }
                });
            }
        }
        // A segment inside a URL wraps in an anchor, innermost so emphasis can
        // still apply inside the link text. Both bare URLs and stored link
        // spans take an anchor.
        for (link_start, link_end) in &links {
            if *link_start <= start && end <= *link_end {
                wanted.push(InlineTag::Link {
                    url: text[*link_start..*link_end].to_string(),
                });
            }
        }
        for (link_start, link_end, style) in &spans {
            if let SpanStyle::Link { url } = style {
                if *link_start <= start && end <= *link_end {
                    wanted.push(InlineTag::Link { url: url.clone() });
                }
            }
        }

        // Tags already open keep their nesting order; anything the segment
        // still wants that is not open yet goes outside them, in `wanted`
        // order. The stack is then cut back to the longest prefix it shares
        // with that target and the rest is reopened, so a tag can never stay
        // open past its own range just because something inside it continues.
        let mut target: Vec<InlineTag> = open
            .iter()
            .filter(|tag| wanted.contains(tag))
            .cloned()
            .collect();
        for tag in &wanted {
            if !target.contains(tag) {
                target.push(tag.clone());
            }
        }
        let keep = open
            .iter()
            .zip(target.iter())
            .take_while(|(open_tag, target_tag)| open_tag == target_tag)
            .count();
        while open.len() > keep {
            let tag = open.pop().expect("the stack is longer than its prefix");
            out.push_str(tag.close());
        }
        // A marker at a tag boundary belongs after the closing and before the
        // opening tags, where the superscript visually sits.
        for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == start) {
            out.push_str(&footnote_marker(*number));
        }
        for tag in &target[keep..] {
            out.push_str(&tag.open());
            open.push(tag.clone());
        }
        out.push_str(&escape(&text[start..end]));
    }
    while let Some(tag) = open.pop() {
        out.push_str(tag.close());
    }
    // Markers past the last glyph have no segment of their own to sit in.
    for (_, number) in footnotes.iter().filter(|(offset, _)| *offset == text.len()) {
        out.push_str(&footnote_marker(*number));
    }
    out
}

/// The inline superscript marker, linked to the footnote list.
fn footnote_marker(number: usize) -> String {
    format!(
        "<sup class=\"footnote-ref\" id=\"fnref{number}\"><a href=\"#fn{number}\">{number}</a></sup>"
    )
}

/// Escape the three characters that would otherwise be read as markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

/// [`escape`] for a value inside a quoted attribute, where a quote would end it.
/// Text content leaves quotes alone, so prose keeps the characters as typed.
fn escape_attribute(text: &str) -> String {
    escape(text).replace('"', "&quot;").replace('\'', "&#39;")
}

/// Pull a snapped `(start, end)` range in past leading and trailing whitespace.
fn trim_whitespace_edges(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end {
        match text[start..end].chars().next() {
            Some(character) if character.is_whitespace() => start += character.len_utf8(),
            _ => break,
        }
    }
    while end > start {
        match text[start..end].chars().next_back() {
            Some(character) if character.is_whitespace() => end -= character.len_utf8(),
            _ => break,
        }
    }
    (start, end)
}

/// Round `offset` up to the next UTF-8 char boundary, clamped to `text.len()`.
fn snap_up(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Round `offset` down to the previous UTF-8 char boundary, clamped to
/// `text.len()`.
fn snap_down(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Anchor, Annotation, AnnotationKind, Block, BlockId, BlockKind, Document, Span, SpanStyle,
    };
    use uuid::Uuid;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn paragraph(text: &str) -> Block {
        block(BlockKind::Paragraph, text)
    }

    fn annotation(
        kind: AnnotationKind,
        block_id: BlockId,
        start: usize,
        end: usize,
        body: &str,
    ) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: Anchor::range(block_id, start, end),
            body: body.to_string(),
        }
    }

    fn document(blocks: Vec<Block>, annotations: Vec<Annotation>) -> Document {
        Document {
            id: Uuid::new_v4(),
            title: "Doc".to_string(),
            blocks,
            annotations,
        }
    }

    fn rich_document() -> Document {
        let mut styled = paragraph("Hello world");
        styled.spans = vec![
            Span {
                start: 0,
                end: 5,
                style: SpanStyle::Bold,
            },
            Span {
                start: 6,
                end: 11,
                style: SpanStyle::Italic,
            },
        ];
        let annotated = paragraph("Anchored text");
        let annotated_id = annotated.id;
        let bullet = block(
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
            "Bullet",
        );
        let ordered = block(
            BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
            "First",
        );
        document(
            vec![
                block(BlockKind::Heading { level: 1 }, "A Title"),
                styled,
                annotated,
                bullet,
                ordered,
            ],
            vec![
                annotation(AnnotationKind::Marginal, annotated_id, 0, 8, "margin"),
                annotation(AnnotationKind::Note, annotated_id, 9, 13, "note body"),
                annotation(
                    AnnotationKind::Footnote,
                    annotated_id,
                    0,
                    8,
                    "Footnote body.",
                ),
            ],
        )
    }

    #[test]
    fn marginalia_becomes_aside() {
        let d = rich_document();
        let html = to_html(&d);
        assert!(html.contains("<aside class=\"marginalia\">"));
    }

    #[test]
    fn footnotes_have_anchor_links() {
        let d = rich_document();
        let html = to_html(&d);
        assert!(html.contains("<a href=\"#fn1\">"));
    }

    #[test]
    fn html_is_deterministic() {
        let d = rich_document();
        assert_eq!(to_html(&d), to_html(&d));
    }

    #[test]
    fn empty_document_exports_valid_shell() {
        let html = to_html(&Document::new(""));
        assert!(!html.is_empty());
        assert!(html.starts_with("<!DOCTYPE") || html.starts_with("<html"));
    }

    #[test]
    fn headings_paragraphs_and_lists_export() {
        let html = to_html(&rich_document());
        assert!(html.contains("<h1>A Title</h1>"));
        assert!(html.contains("<p><strong>Hello</strong> <em>world</em></p>"));
        assert!(html.contains("<ul>"));
        assert!(html.contains("<ol>"));
        assert!(html.contains("<li>Bullet</li>"));
        assert!(html.contains("<li>First</li>"));
    }

    #[test]
    fn block_quotes_export_as_blockquote() {
        let doc = document(
            vec![
                paragraph("Before."),
                block(BlockKind::BlockQuote, "Quoted words"),
            ],
            Vec::new(),
        );
        let html = to_html(&doc);
        assert!(html.contains("<blockquote>Quoted words</blockquote>"));
    }

    #[test]
    fn urls_export_as_anchors() {
        let doc = document(vec![paragraph("see https://example.com now")], Vec::new());
        let html = to_html(&doc);
        assert!(
            html.contains("<a href=\"https://example.com\">https://example.com</a>"),
            "a URL becomes a clickable anchor: {html}"
        );
    }

    #[test]
    fn stored_links_export_as_anchored_links() {
        let mut p = paragraph("click here");
        p.spans = vec![Span {
            start: 0,
            end: 10,
            style: SpanStyle::Link {
                url: "http://x.com".into(),
            },
        }];
        let html = to_html(&document(vec![p], Vec::new()));
        assert!(
            html.contains("<a href=\"http://x.com\">click here</a>"),
            "a stored link exports its anchor and url: {html}"
        );
    }

    #[test]
    fn images_export_as_img_tags() {
        let doc = document(
            vec![block(
                BlockKind::Image {
                    path: "/img/a.png".into(),
                    link: None,
                },
                "",
            )],
            Vec::new(),
        );
        let html = to_html(&doc);
        assert!(
            html.contains("<img class=\"image\" src=\"/img/a.png\" alt=\"\">"),
            "an image becomes an <img>: {html}"
        );
    }

    #[test]
    fn consecutive_list_items_share_one_list() {
        let doc = document(
            vec![
                block(
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                    "One",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: false,
                        depth: 0,
                    },
                    "Two",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "First",
                ),
                block(
                    BlockKind::ListItem {
                        ordered: true,
                        depth: 0,
                    },
                    "Second",
                ),
            ],
            Vec::new(),
        );
        let html = to_html(&doc);
        assert_eq!(html.matches("<ul>").count(), 1);
        assert_eq!(html.matches("</ul>").count(), 1);
        assert_eq!(html.matches("<ol>").count(), 1);
        assert_eq!(html.matches("</ol>").count(), 1);
        assert!(html.contains("<ul>\n<li>One</li>\n<li>Two</li>\n</ul>"));
        assert!(html.contains("<ol>\n<li>First</li>\n<li>Second</li>\n</ol>"));
    }

    #[test]
    fn marginalia_sits_before_the_annotated_block() {
        let html = to_html(&rich_document());
        let aside = html.find("<aside class=\"marginalia\">").expect("aside");
        let after_aside = &html[aside..];
        let close = after_aside.find("</aside>").expect("aside closes");
        assert!(
            after_aside[close..].contains("<p>"),
            "its block follows: {html}"
        );
        assert!(html.contains("<aside class=\"marginalia\"><p>margin</p></aside>"));
    }

    #[test]
    fn footnotes_collect_in_a_list_at_the_foot() {
        let html = to_html(&rich_document());
        assert!(html.contains("<ol class=\"footnotes\">"));
        assert!(html.contains("<li id=\"fn1\">Footnote body."));
        assert!(html.contains("href=\"#fnref1\""));
    }

    #[test]
    fn footnote_numbers_follow_document_order_not_storage_order() {
        let alpha = paragraph("Alpha");
        let beta = paragraph("Beta");
        let (alpha_id, beta_id) = (alpha.id, beta.id);
        let doc = document(
            vec![alpha, beta],
            vec![
                annotation(AnnotationKind::Footnote, beta_id, 0, 5, "second"),
                annotation(AnnotationKind::Footnote, alpha_id, 0, 5, "first"),
            ],
        );
        let html = to_html(&doc);
        assert!(html.contains(
            "Alpha<sup class=\"footnote-ref\" id=\"fnref1\"><a href=\"#fn1\">1</a></sup>"
        ));
        assert!(html.contains(
            "Beta<sup class=\"footnote-ref\" id=\"fnref2\"><a href=\"#fn2\">2</a></sup>"
        ));
        assert!(html.contains("<li id=\"fn1\">first"));
        assert!(html.contains("<li id=\"fn2\">second"));
    }

    #[test]
    fn text_and_title_are_escaped() {
        let doc = Document {
            id: Uuid::new_v4(),
            title: "A & B <draft>".to_string(),
            blocks: vec![paragraph("a < b & c > d")],
            annotations: Vec::new(),
        };
        let html = to_html(&doc);
        assert!(html.contains("a &lt; b &amp; c &gt; d"));
        assert!(!html.contains("a < b"));
        assert!(html.contains("<title>A &amp; B &lt;draft&gt;</title>"));
    }

    #[test]
    fn stylesheet_carries_the_house_typography() {
        let html = to_html(&Document::new("Title"));
        assert!(html.contains("<style>"));
        assert!(html.contains("Crimson Pro"));
        assert!(html.contains("EB Garamond"));
        assert!(html.contains("text-align: justify"));
    }

    #[test]
    fn heading_levels_are_clamped() {
        let low = to_html(&document(
            vec![block(BlockKind::Heading { level: 0 }, "Low")],
            Vec::new(),
        ));
        let high = to_html(&document(
            vec![block(BlockKind::Heading { level: 9 }, "High")],
            Vec::new(),
        ));
        assert!(low.contains("<h1>Low</h1>"));
        assert!(high.contains("<h6>High</h6>"));
    }

    #[test]
    fn overlapping_spans_stay_nested() {
        let mut p = paragraph("abcdefghijklmnop");
        p.spans = vec![
            Span {
                start: 0,
                end: 10,
                style: SpanStyle::Italic,
            },
            Span {
                start: 5,
                end: 15,
                style: SpanStyle::Bold,
            },
        ];
        // Italic ends where bold is still running, so both tags close at 10 and
        // bold reopens: "klmno" is bold, not italic. Letting the outer tag stay
        // open would italicise it.
        assert!(to_html(&document(vec![p], Vec::new()))
            .contains("<p><em>abcde<strong>fghij</strong></em><strong>klmno</strong>p</p>"));
    }

    #[test]
    fn span_edges_are_trimmed_past_whitespace() {
        let mut trailing = paragraph("Hello world");
        trailing.spans = vec![Span {
            start: 0,
            end: 6,
            style: SpanStyle::Bold,
        }];
        let html = to_html(&document(vec![trailing], Vec::new()));
        assert!(html.contains("<p><strong>Hello</strong> world</p>"));
    }

    #[test]
    fn annotations_without_a_block_lose_only_their_marker() {
        let body = paragraph("Body");
        let empty = paragraph("");
        let empty_id = empty.id;
        let doc = document(
            vec![body, empty],
            vec![
                annotation(AnnotationKind::Marginal, Uuid::new_v4(), 0, 4, "orphan"),
                annotation(AnnotationKind::Footnote, empty_id, 0, 0, "nowhere"),
            ],
        );
        let html = to_html(&doc);
        // A margin note with no block to float beside keeps its body: it is
        // emitted as an aside after the last block, like an unanchored note.
        assert!(html.contains("<aside class=\"marginalia\"><p>orphan</p></aside>"));
        // The footnote lost its marker, not its body: it is listed unanchored,
        // with no back-reference to a `<sup>` that was never written.
        assert!(!html.contains("fnref1"));
        assert!(html.contains("<ol class=\"footnotes\">"));
        assert!(html.contains("<li id=\"fn1\">nowhere</li>"));
    }

    #[test]
    fn marginalia_on_a_emptied_block_keep_their_body() {
        // Deleting all the text under a marginal collapses its anchor to 0..0
        // and leaves an empty block: the aside must survive, after the prose.
        let emptied = paragraph("");
        let emptied_id = emptied.id;
        let doc = document(
            vec![paragraph("Body"), emptied],
            vec![annotation(
                AnnotationKind::Marginal,
                emptied_id,
                0,
                0,
                "the body survives",
            )],
        );
        let html = to_html(&doc);
        let prose_end = html.find("<p>Body</p>").expect("the paragraph exports");
        let aside = html
            .find("<aside class=\"marginalia\"><p>the body survives</p></aside>")
            .expect("an unanchored marginal is still exported");
        assert!(aside > prose_end, "the aside follows the last block");
    }

    #[test]
    fn emoji_and_multibyte_anchors_do_not_panic() {
        let mut p = paragraph("Ünïcode 😀 text");
        p.spans = vec![Span {
            start: 0,
            end: 999,
            style: SpanStyle::Italic,
        }];
        let id = p.id;
        let doc = document(
            vec![p],
            vec![
                annotation(AnnotationKind::Footnote, id, 0, 4, "snapped"),
                annotation(AnnotationKind::Note, id, 0, 999, "wide"),
            ],
        );
        let html = to_html(&doc);
        assert!(!html.contains("wide"), "{html}");
        assert!(html.contains("<li id=\"fn1\">snapped"));
    }

    #[test]
    fn export_is_stable_across_round_trip() {
        let d = rich_document();
        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(to_html(&d), to_html(&back));
    }

    #[test]
    fn a_line_break_in_a_block_is_a_br() {
        let mut doc = Document::new("t");
        doc.blocks[0].text = "first\nsecond".to_string();
        assert!(
            to_html(&doc).contains("<p>first<br>\nsecond</p>"),
            "{}",
            to_html(&doc)
        );
    }

    #[test]
    fn a_line_break_in_a_heading_is_a_space() {
        let mut doc = Document::new("t");
        doc.blocks[0].kind = BlockKind::Heading { level: 2 };
        doc.blocks[0].text = "first\nsecond".to_string();
        assert!(
            to_html(&doc).contains("<h2>first second</h2>"),
            "{}",
            to_html(&doc)
        );
    }

    #[test]
    fn publishing_fills_the_page_head() {
        let mut doc = Document::new("stored");
        doc.blocks = vec![block(BlockKind::Paragraph, "Body.")];
        let meta = crate::model::Metadata {
            title: "On <Burnout>".to_string(),
            author: "K. Isom".to_string(),
            created: "2019-01-01".to_string(),
            published: "2019-01-02".to_string(),
            tags: vec!["life".to_string(), "work".to_string()],
        };
        let html = to_html_with(&doc, &meta);
        assert!(html.contains("<title>On &lt;Burnout&gt;</title>"), "{html}");
        assert!(
            html.contains(r#"<meta name="author" content="K. Isom">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta name="date" content="2019-01-02">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta name="keywords" content="life, work">"#),
            "{html}"
        );
        assert!(!html.contains("stored"), "{html}");
    }

    #[test]
    fn empty_metadata_writes_no_meta_tags() {
        let mut doc = Document::new("stored");
        doc.blocks = vec![block(BlockKind::Paragraph, "Body.")];
        let html = to_html_with(&doc, &crate::model::Metadata::default());
        assert!(!html.contains(r#"name="author""#), "{html}");
        assert!(!html.contains(r#"name="keywords""#), "{html}");
        assert!(!html.contains(r#"name="date""#), "{html}");
        assert!(html.contains("<title>stored</title>"), "{html}");
    }

    #[test]
    fn a_quote_in_metadata_cannot_leave_its_attribute() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Paragraph, "Body.")];
        let meta = crate::model::Metadata {
            author: r#"K" onload="x"#.to_string(),
            tags: vec!["it's".to_string()],
            ..Default::default()
        };
        let html = to_html_with(&doc, &meta);
        assert!(
            html.contains(r#"content="K&quot; onload=&quot;x""#),
            "{html}"
        );
        assert!(html.contains(r#"content="it&#39;s""#), "{html}");
    }

    #[test]
    fn a_quote_in_a_link_or_image_cannot_leave_its_attribute() {
        let mut doc = Document::new("t");
        let mut linked = block(BlockKind::Paragraph, "Body.");
        linked.spans = vec![crate::model::Span {
            start: 0,
            end: 4,
            style: crate::model::SpanStyle::Link {
                url: r#"http://x/" onclick="y"#.to_string(),
            },
        }];
        let image = block(
            BlockKind::Image {
                path: r#"a" onerror="z.png"#.to_string(),
                link: None,
            },
            "",
        );
        doc.blocks = vec![linked, image];
        let html = to_html(&doc);
        assert!(!html.contains(r#"" onclick=""#), "{html}");
        assert!(!html.contains(r#"" onerror=""#), "{html}");
    }

    #[test]
    fn quotes_in_prose_stay_as_typed() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Paragraph, r#"She said "hello"."#)];
        assert!(to_html(&doc).contains(r#"<p>She said "hello".</p>"#));
    }

    fn linked(url: &str) -> Block {
        let mut linked = block(BlockKind::Paragraph, "the demo");
        linked.spans = vec![crate::model::Span {
            start: 0,
            end: 8,
            style: crate::model::SpanStyle::Link {
                url: url.to_string(),
            },
        }];
        linked
    }

    #[test]
    fn a_script_link_exports_as_its_text() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("javascript:alert(1)")];
        let html = to_html(&doc);
        assert!(html.contains("<p>the demo</p>"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
    }

    #[test]
    fn a_pdf_link_still_exports() {
        let mut doc = Document::new("t");
        doc.blocks = vec![linked("paper.pdf")];
        assert!(to_html(&doc).contains(r#"<a href="paper.pdf">the demo</a>"#));
    }

    fn with_annotations(kind: AnnotationKind) -> Document {
        let mut doc = Document::new("t");
        let prose = block(BlockKind::Paragraph, "Some prose here.");
        doc.annotations = vec![annotation(kind, prose.id, 5, 10, "SECRET BODY")];
        doc.blocks = vec![prose];
        doc
    }

    /// A note is the author's own: neither its words nor any sign of it publishes.
    #[test]
    fn a_notes_words_never_reach_the_page() {
        let html = to_html(&with_annotations(AnnotationKind::Note));
        assert!(!html.contains("SECRET BODY"), "{html}");
        assert!(!html.contains("<details"), "{html}");
        assert!(html.contains("<mark>"), "its highlight does: {html}");
    }

    #[test]
    fn a_note_whose_block_is_gone_leaves_no_trace_either() {
        let mut doc = with_annotations(AnnotationKind::Note);
        doc.annotations[0].anchor = Anchor::range(Uuid::new_v4(), 0, 1);
        assert!(!to_html(&doc).contains("SECRET BODY"));
    }

    #[test]
    fn a_note_does_not_disturb_footnote_numbers() {
        let mut doc = Document::new("t");
        let prose = block(BlockKind::Paragraph, "Some prose here.");
        doc.annotations = vec![
            annotation(AnnotationKind::Note, prose.id, 0, 4, "private"),
            annotation(AnnotationKind::Footnote, prose.id, 5, 10, "public"),
        ];
        doc.blocks = vec![prose];
        let html = to_html(&doc);
        assert!(html.contains(r#"id="fn1""#), "{html}");
        assert!(html.contains("public"), "{html}");
    }

    #[test]
    fn marginals_and_footnotes_still_publish() {
        assert!(to_html(&with_annotations(AnnotationKind::Marginal)).contains("SECRET BODY"));
        assert!(to_html(&with_annotations(AnnotationKind::Footnote)).contains("SECRET BODY"));
    }

    /// The aside floats into the margin, and a float cannot rise above where it is
    /// written, so it has to come first to line up with its block.
    #[test]
    fn a_marginal_comes_before_its_block_of_every_kind() {
        for kind in [
            BlockKind::Paragraph,
            BlockKind::Heading { level: 2 },
            BlockKind::BlockQuote,
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
        ] {
            let mut doc = Document::new("t");
            let target = block(kind.clone(), "Words");
            doc.annotations = vec![annotation(
                AnnotationKind::Marginal,
                target.id,
                0,
                0,
                "SIDE",
            )];
            doc.blocks = vec![target];
            let html = to_html(&doc);
            let side = html.find("SIDE").expect("the marginal publishes");
            let words = html.find("Words").expect("the block publishes");
            assert!(side < words, "{kind:?}: {html}");
        }
    }

    fn image(link: Option<&str>) -> Block {
        block(
            BlockKind::Image {
                path: "a.png".to_string(),
                link: link.map(str::to_string),
            },
            "",
        )
    }

    #[test]
    fn a_linked_image_is_wrapped_in_its_link() {
        let mut doc = Document::new("t");
        doc.blocks = vec![image(Some("b\".png"))];
        let html = to_html(&doc);
        assert!(
            html.contains(r#"<a href="b&quot;.png"><img class="image" src="a.png" alt=""></a>"#),
            "{html}"
        );
    }

    #[test]
    fn an_image_with_a_script_link_publishes_without_it() {
        let mut doc = Document::new("t");
        doc.blocks = vec![image(Some("javascript:alert(1)"))];
        let html = to_html(&doc);
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains(r#"<img class="image" src="a.png""#), "{html}");
    }

    #[test]
    fn a_rule_is_an_hr() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Rule, "")];
        assert!(to_html(&doc).contains("<hr>\n"));
    }

    #[test]
    fn code_is_preformatted_with_its_language() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Code {
                language: Some("rust".to_string()),
            },
            "a < b\n    c",
        )];
        let html = to_html(&doc);
        assert!(
            html.contains("<pre><code class=\"language-rust\">a &lt; b\n    c</code></pre>\n"),
            "{html}"
        );
    }

    #[test]
    fn a_code_block_is_set_smaller_than_inline_code() {
        let style = STYLE;
        assert!(rule_for("code", style).contains("font-size: 0.84em"));
        assert!(rule_for("pre code", style).contains("font-size: 0.75em"));
    }

    #[test]
    fn prose_is_preformatted_in_the_serif() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Prose, "one\n  two")];
        let html = to_html(&doc);
        assert!(
            html.contains("<pre class=\"prose\">one\n  two</pre>\n"),
            "{html}"
        );
        assert!(
            html.contains("pre.prose"),
            "the stylesheet sets its face: {html}"
        );
    }

    #[test]
    fn emphasis_in_prose_exports_inside_the_pre() {
        let mut doc = Document::new("t");
        let mut prose = block(BlockKind::Prose, "one\n  two");
        prose.spans = vec![Span {
            start: 6,
            end: 9,
            style: SpanStyle::Bold,
        }];
        doc.blocks = vec![prose];
        let html = to_html(&doc);
        assert!(
            html.contains("<pre class=\"prose\">one\n  <strong>two</strong></pre>"),
            "{html}"
        );
    }

    #[test]
    fn a_document_link_publishes_as_its_text() {
        let mut doc = Document::new("t");
        let mut linked = paragraph("See the intro now.");
        linked.spans = vec![Span {
            start: 4,
            end: 13,
            style: SpanStyle::DocumentLink {
                target: "notes/a.kame#Intro".to_string(),
            },
        }];
        doc.blocks = vec![linked];
        let html = to_html(&doc);
        assert!(html.contains("<p>See the intro now.</p>"), "{html}");
        assert!(!html.contains("a.kame"), "{html}");
    }

    #[test]
    fn inline_code_exports_as_code_and_is_escaped() {
        let mut doc = Document::new("t");
        let mut block = paragraph("use a<b now");
        block.spans = vec![Span {
            start: 4,
            end: 7,
            style: SpanStyle::Code,
        }];
        doc.blocks = vec![block];
        let html = to_html(&doc);
        assert!(
            html.contains("<p>use <code>a&lt;b</code> now</p>"),
            "{html}"
        );
    }

    #[test]
    fn a_url_in_inline_code_is_not_a_link() {
        let mut doc = Document::new("t");
        let text = "curl https://a.b now";
        let mut block = paragraph(text);
        block.spans = vec![Span {
            start: 0,
            end: 16,
            style: SpanStyle::Code,
        }];
        doc.blocks = vec![block];
        let html = to_html(&doc);
        assert!(!html.contains("<a href"), "{html}");
    }

    fn list(items: &[(bool, u8, &str)]) -> Document {
        let mut doc = Document::new("t");
        doc.blocks = items
            .iter()
            .map(|(ordered, depth, text)| {
                block(
                    BlockKind::ListItem {
                        ordered: *ordered,
                        depth: *depth,
                    },
                    text,
                )
            })
            .collect();
        doc
    }

    #[test]
    fn a_nested_list_sits_inside_its_parent_item() {
        let html = to_html(&list(&[
            (false, 0, "a"),
            (false, 1, "b"),
            (false, 1, "c"),
            (false, 0, "d"),
        ]));
        assert!(
            html.contains(
                "<ul>\n<li>a\n<ul>\n<li>b</li>\n<li>c</li>\n</ul>\n</li>\n<li>d</li>\n</ul>\n"
            ),
            "{html}"
        );
    }

    #[test]
    fn a_list_ending_several_levels_deep_closes_them_all() {
        let html = to_html(&list(&[(true, 0, "a"), (false, 1, "b"), (true, 2, "c")]));
        assert!(
            html.contains(
                "<ol>\n<li>a\n<ul>\n<li>b\n<ol>\n<li>c</li>\n</ol>\n</li>\n</ul>\n</li>\n</ol>\n"
            ),
            "{html}"
        );
    }

    #[test]
    fn a_change_of_list_kind_at_one_level_stays_inside_the_parent() {
        let html = to_html(&list(&[(false, 0, "a"), (false, 1, "b"), (true, 1, "c")]));
        assert!(
            html.contains(
                "<ul>\n<li>a\n<ul>\n<li>b</li>\n</ul>\n<ol>\n<li>c</li>\n</ol>\n</li>\n</ul>\n"
            ),
            "{html}"
        );
    }

    #[test]
    fn nested_numbers_are_numbered_by_the_stylesheet() {
        let html = to_html(&list(&[(true, 0, "a"), (true, 1, "b")]));
        assert!(html.contains("counters(item, \".\")"), "{html}");
    }

    #[test]
    fn footnotes_keep_their_own_numbers() {
        let html = to_html(&list(&[(true, 0, "a")]));
        assert!(html.contains("ol:not(.footnotes)"), "{html}");
    }

    fn linking_to(target: &str) -> Document {
        let mut doc = Document::new("t");
        let mut block = paragraph("See the intro now.");
        block.spans = vec![Span {
            start: 4,
            end: 13,
            style: SpanStyle::DocumentLink {
                target: target.to_string(),
            },
        }];
        doc.blocks = vec![block];
        doc
    }

    #[test]
    fn a_body_is_only_the_documents_own_markup() {
        let html = html_body(&linking_to("a.kame"), &Publish::plain());
        assert_eq!(html, "<p>See the intro now.</p>\n");
    }

    #[test]
    fn a_body_keeps_footnotes() {
        let mut doc = Document::new("t");
        let block = paragraph("Words.");
        let id = block.id;
        doc.blocks = vec![block];
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Footnote,
            anchor: Anchor::Range {
                block_id: id,
                start: 0,
                end: 5,
            },
            body: "A note.".to_string(),
        });
        let html = html_body(&doc, &Publish::plain());
        assert!(html.contains("<ol class=\"footnotes\">"), "{html}");
    }

    #[test]
    fn a_whole_page_wraps_exactly_the_body() {
        let doc = linking_to("a.kame");
        let page = to_html(&doc);
        let body = html_body(&doc, &Publish::plain());
        assert!(
            page.contains(&format!("<main class=\"document\">\n{body}</main>")),
            "{page}"
        );
    }

    #[test]
    fn a_document_link_publishes_where_the_hook_says() {
        let publish = Publish {
            document_link: &|target| (target == "a.kame#Intro").then(|| "/k/a/".to_string()),
            image: &|path| path.to_string(),
        };
        assert_eq!(
            html_body(&linking_to("a.kame#Intro"), &publish),
            "<p>See <a href=\"/k/a/\">the intro</a> now.</p>\n"
        );
    }

    #[test]
    fn a_document_link_the_hook_doesnt_know_publishes_as_its_text() {
        let publish = Publish {
            document_link: &|_| None,
            image: &|path| path.to_string(),
        };
        assert_eq!(
            html_body(&linking_to("gone.kame"), &publish),
            "<p>See the intro now.</p>\n"
        );
    }

    #[test]
    fn an_image_publishes_where_the_hook_says_and_is_escaped() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(
            BlockKind::Image {
                path: "/home/me/a.png".to_string(),
                link: None,
            },
            "",
        )];
        let publish = Publish {
            document_link: &|_| None,
            image: &|path| format!("/k/s/{}\"", path.rsplit('/').next().unwrap()),
        };
        assert!(
            html_body(&doc, &publish).contains("src=\"/k/s/a.png&quot;\""),
            "{}",
            html_body(&doc, &publish)
        );
    }

    fn rule_for<'a>(selector: &str, style: &'a str) -> &'a str {
        let at = style
            .find(&format!("\n{selector} {{"))
            .unwrap_or_else(|| panic!("no rule for {selector}"));
        let body = &style[at + 1..];
        &body[..body.find('}').unwrap()]
    }

    #[test]
    fn the_stylesheet_sets_type_as_kame_does() {
        let body = rule_for("body", STYLE);
        assert!(body.contains("line-height: 1.6;"), "{body}");
        assert!(!STYLE.contains("oldstyle-nums"), "kame uses lining figures");
        assert!(!STYLE.contains("hyphens: auto"), "kame doesn't hyphenate");
        assert!(rule_for("h1", STYLE).contains("font-size: 1.75em"));
        assert!(rule_for("h2", STYLE).contains("font-size: 1.4em"));
        assert!(rule_for("h3", STYLE).contains("font-size: 1.12em"));
    }

    #[test]
    fn only_paragraphs_are_justified_as_in_kame() {
        assert!(!rule_for("body", STYLE).contains("text-align: justify"));
        assert!(rule_for("p", STYLE).contains("text-align: justify"));
    }

    #[test]
    fn a_quote_is_set_in_and_italic_as_in_kame() {
        let quote = rule_for("blockquote", STYLE);
        assert!(quote.contains("font-style: italic"), "{quote}");
        assert!(quote.contains("margin-left: 1.5em"), "{quote}");
    }

    #[test]
    fn the_stylesheet_follows_the_system_and_an_explicit_choice() {
        assert!(STYLE.contains("@media (prefers-color-scheme: dark)"));
        assert!(STYLE.contains(":root:not([data-theme=\"light\"])"));
        assert!(STYLE.contains(":root[data-theme=\"dark\"]"));
        assert!(STYLE.contains("color-scheme: dark"));
    }

    #[test]
    fn printing_is_always_dark_on_white() {
        let print = &STYLE[STYLE.find("@media print").unwrap()..];
        assert!(print.contains("--paper: #fff"), "{print}");
        assert!(print.contains("--ink: #000"), "{print}");
    }

    fn authored(author: &str) -> crate::model::Metadata {
        crate::model::Metadata {
            author: author.to_string(),
            ..crate::model::Metadata::default()
        }
    }

    #[test]
    fn the_author_shows_under_a_first_heading() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(BlockKind::Heading { level: 1 }, "Title"),
            paragraph("Words."),
        ];
        let html = to_html_with(&doc, &authored("K. Isom"));
        assert!(
            html.contains("<h1>Title</h1>\n<p class=\"byline\">K. Isom</p>\n<p>Words.</p>"),
            "{html}"
        );
    }

    #[test]
    fn the_author_leads_a_document_with_no_first_heading() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Words.")];
        let html = to_html_with(&doc, &authored("K. Isom"));
        assert!(
            html.contains("<p class=\"byline\">K. Isom</p>\n<p>Words.</p>"),
            "{html}"
        );
    }

    #[test]
    fn the_author_shows_once_and_is_escaped() {
        let mut doc = Document::new("t");
        doc.blocks = vec![
            block(BlockKind::Heading { level: 1 }, "One"),
            block(BlockKind::Heading { level: 2 }, "Two"),
        ];
        let html = to_html_with(&doc, &authored("A & <B>"));
        assert_eq!(html.matches("class=\"byline\"").count(), 1);
        assert!(html.contains(">A &amp; &lt;B&gt;<"), "{html}");
    }

    #[test]
    fn no_author_means_no_byline() {
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("Words.")];
        assert!(!to_html_with(&doc, &authored("")).contains("<p class=\"byline\""));
    }

    #[test]
    fn a_body_has_no_byline_since_a_theme_prints_its_own() {
        let mut doc = Document::new("t");
        doc.blocks = vec![block(BlockKind::Heading { level: 1 }, "Title")];
        assert!(!html_body(&doc, &Publish::plain()).contains("byline"));
    }

    fn noted(text: &str, start: usize, end: usize, kind: AnnotationKind) -> Document {
        let mut doc = Document::new("t");
        let para = paragraph(text);
        let id = para.id;
        doc.blocks = vec![para];
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: Anchor::Range {
                block_id: id,
                start,
                end,
            },
            body: "SECRET NOTE".to_string(),
        });
        doc
    }

    #[test]
    fn a_notes_highlight_publishes_without_its_words() {
        let html = html_body(
            &noted("See this bit now.", 4, 12, AnnotationKind::Note),
            &Publish::plain(),
        );
        assert_eq!(html, "<p>See <mark>this bit</mark> now.</p>\n");
    }

    #[test]
    fn a_highlight_nests_with_other_styles() {
        let mut doc = noted("one two three", 4, 13, AnnotationKind::Note);
        doc.blocks[0].spans = vec![Span {
            start: 0,
            end: 7,
            style: SpanStyle::Bold,
        }];
        let html = html_body(&doc, &Publish::plain());
        assert!(html.contains("<mark>"), "{html}");
        assert_eq!(
            html.matches("<mark>").count(),
            html.matches("</mark>").count(),
            "{html}"
        );
        assert_eq!(
            html.matches("<strong>").count(),
            html.matches("</strong>").count(),
            "{html}"
        );
        let open = html.find("<strong>").unwrap();
        assert!(open < html.find("two").unwrap(), "{html}");
    }

    #[test]
    fn a_highlight_counts_bytes_and_survives_a_bad_range() {
        let html = html_body(
            &noted("été ok", 2, 5, AnnotationKind::Note),
            &Publish::plain(),
        );
        assert_eq!(html.matches("<mark>").count(), 1, "{html}");
        let html = html_body(
            &noted("short", 3, 400, AnnotationKind::Note),
            &Publish::plain(),
        );
        assert!(html.contains("<mark>rt</mark>"), "{html}");
        let html = html_body(
            &noted("short", 300, 400, AnnotationKind::Note),
            &Publish::plain(),
        );
        assert!(!html.contains("<mark>"), "{html}");
    }

    #[test]
    fn two_overlapping_highlights_are_one_run() {
        let mut doc = noted("one two three", 0, 7, AnnotationKind::Note);
        let id = doc.blocks[0].id;
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::Range {
                block_id: id,
                start: 4,
                end: 13,
            },
            body: String::new(),
        });
        let html = html_body(&doc, &Publish::plain());
        assert_eq!(html, "<p><mark>one two three</mark></p>\n");
    }

    #[test]
    fn only_notes_highlight() {
        for kind in [AnnotationKind::Marginal, AnnotationKind::Footnote] {
            let html = html_body(&noted("See this bit.", 4, 12, kind), &Publish::plain());
            assert!(!html.contains("<mark>"), "{html}");
        }
    }

    #[test]
    fn a_highlight_in_code_stays_exact() {
        let mut doc = noted("a < b\n  c", 0, 5, AnnotationKind::Note);
        doc.blocks[0].kind = BlockKind::Code { language: None };
        let html = html_body(&doc, &Publish::plain());
        assert!(html.contains("<mark>a &lt; b</mark>\n  c"), "{html}");
    }

    #[test]
    fn the_stylesheet_styles_highlights_in_both_modes() {
        assert!(STYLE.contains("mark {"));
        assert!(
            STYLE.matches("--mark:").count() >= 3,
            "light, dark and system dark"
        );
    }

    #[test]
    fn body_type_is_twenty_pixels_at_a_browsers_default_size() {
        assert!(rule_for("body", STYLE).contains("font-size: 1.25em;"));
    }
}
