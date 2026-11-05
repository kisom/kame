//! Export formats.

pub mod html;
pub mod markdown;

pub use html::to_html;
pub use html::{html_body, to_html_with, Publish, REFERENCES_STYLE, STYLE};
pub use markdown::markdown_unresolved;
#[cfg(test)]
pub use markdown::to_markdown;
pub use markdown::to_markdown_with;

use std::borrow::Cow;

use crate::model::{AnnotationKind, BlockKind, Document};

/// Whether `doc` opens with a centered heading repeating `title`, which a page printing the title
/// itself should honour, since [`without_repeated_title`] takes that heading away.
pub fn title_is_centered(doc: &Document, title: &str) -> bool {
    doc.blocks.first().is_some_and(|first| {
        matches!(first.kind, BlockKind::Heading { centered: true, .. })
            && same_title(&first.text, title)
    })
}

fn same_title(heading: &str, title: &str) -> bool {
    heading.trim().to_lowercase() == title.trim().to_lowercase()
}

/// `doc` without a first heading repeating `title`, for a page that prints the title itself.
/// A difference of case or surrounding space doesn't make it a different title.
/// It stays if a footnote is anchored in it, since the footnote would lose its place.
pub fn without_repeated_title<'a>(doc: &'a Document, title: &str) -> Cow<'a, Document> {
    let repeats = doc.blocks.first().is_some_and(|first| {
        matches!(first.kind, BlockKind::Heading { .. })
            && same_title(&first.text, title)
            && !doc.annotations.iter().any(|annotation| {
                annotation.kind == AnnotationKind::Footnote
                    && annotation.anchor.block_id() == first.id
            })
    });
    if !repeats {
        return Cow::Borrowed(doc);
    }
    let mut rest = doc.clone();
    rest.blocks.remove(0);
    Cow::Owned(rest)
}
