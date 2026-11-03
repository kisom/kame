//! Export formats.

pub mod html;
pub mod markdown;

pub use html::to_html;
pub use html::{html_body, to_html_with, Publish, STYLE};
#[cfg(test)]
pub use markdown::to_markdown;
pub use markdown::to_markdown_with;

use std::borrow::Cow;

use crate::model::{AnnotationKind, BlockKind, Document};

/// `doc` without a first heading repeating `title`, for a page that prints the title itself.
/// It stays if a footnote is anchored in it, since the footnote would lose its place.
pub fn without_repeated_title<'a>(doc: &'a Document, title: &str) -> Cow<'a, Document> {
    let repeats = doc.blocks.first().is_some_and(|first| {
        matches!(first.kind, BlockKind::Heading { .. })
            && first.text == title
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
