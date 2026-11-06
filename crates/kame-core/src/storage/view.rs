//! What this build can show of a document holding something only a newer kame knows: a kind of
//! block, a style, a kind of note, or another value of one of the model's enums. It's read as JSON
//! and made into a document this build can hold, which leaves those out, so it's for looking at and
//! never for writing back.
//!
//! - A block of a kind this build doesn't know becomes a paragraph saying a newer version wrote it,
//!   with the block's id.
//! - A span with a style it doesn't know is left out, and so is a note of a kind it doesn't know.
//! - Any other value it doesn't know falls back to its field's default, where the field has one,
//!   and otherwise its block is one this build doesn't know.
//!
//! What reads a closed document and never writes it, such as a site build or a notebook's search,
//! takes it with those blocks left out instead, and the notes on them, so that nothing stands in
//! their place.
//!
//! A block's kind, a span's style and a note's kind are read first, on their own, so that one this
//! build doesn't know makes its piece unknown whatever else the piece holds: a new kind can bring
//! fields of new shapes. Otherwise damage isn't taken for something newer: JSON that doesn't parse,
//! a value of the wrong type or a required field missing, anywhere, makes no view, as it makes no
//! document.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{Annotation, AnnotationKind, Block, BlockKind, Document, Span, SpanStyle};

/// What a block of a kind this build doesn't know says in its place.
pub const NEWER_PART: &str = "A newer version of kame wrote this part. Update kame to see it.";

/// The document in `json` as this build can show it, if it holds something only a newer version
/// knows and is otherwise sound. `None` for JSON this build reads whole, and for damage.
pub(crate) fn view_of(json: &str) -> Option<Document> {
    view_from(serde_json::from_str(json).ok()?)
}

/// [`view_of`], for a document already read as JSON.
pub(crate) fn view_from(value: Value) -> Option<Document> {
    shown(value).map(|(document, _)| document)
}

/// The document in `json` for what reads it and never writes it, such as a site build or a
/// notebook's search: [`view_of`], with each block of a kind this build doesn't know left out, and
/// the notes on it, rather than a paragraph standing in its place. `None` as for [`view_of`].
pub(crate) fn readable_of(json: &str) -> Option<Document> {
    let (mut document, unknown) = shown(serde_json::from_str(json).ok()?)?;
    document.blocks.retain(|block| !unknown.contains(&block.id));
    document
        .annotations
        .retain(|annotation| !unknown.contains(&annotation.anchor.block_id()));
    Some(document)
}

/// [`view_from`], with the ids of the blocks of a kind this build doesn't know.
fn shown(mut value: Value) -> Option<(Document, Vec<Uuid>)> {
    let mut newer = false;
    let mut unknown = Vec::new();
    let document = value.as_object_mut()?;
    if let Some(Value::Array(blocks)) = document.get_mut("blocks") {
        for block in blocks.iter_mut() {
            let (view, known) = block_view(block, &mut newer)?;
            if !known {
                unknown.push(view.id);
            }
            *block = serde_json::to_value(view).ok()?;
        }
    }
    if let Some(Value::Array(annotations)) = document.get_mut("annotations") {
        let mut kept = Vec::new();
        for mut annotation in std::mem::take(annotations) {
            match known::<AnnotationKind, Annotation>(&mut annotation, &mut newer) {
                Reading::Read(_) => kept.push(annotation),
                Reading::Unknown => {}
                Reading::Damaged => return None,
            }
        }
        *annotations = kept;
    }
    if !newer {
        return None;
    }
    Some((serde_json::from_value(value).ok()?, unknown))
}

/// Whether `json` holds something from a newer version of kame: a field this build would drop by
/// writing it again, or a kind, style or other value it doesn't know. JSON that's damaged, or isn't
/// a document at all, holds nothing newer. It's read once, which matters when opening a long one.
pub(crate) fn is_newer(json: &str) -> bool {
    let Ok(written) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    match Document::deserialize(&written) {
        Ok(read) => serde_json::to_value(read)
            .is_ok_and(|known| crate::model::has_unknown_fields(&written, &known)),
        Err(_) => view_from(written).is_some(),
    }
}

/// A block as this build can show it, and whether it's of a kind this build knows, or `None` if
/// it's damaged. Its kind is read first, on its own, since a kind this build doesn't know may bring
/// text or spans of a shape it doesn't know.
fn block_view(block: &Value, newer: &mut bool) -> Option<(Block, bool)> {
    let mut block = block.clone();
    let reading = match read_tag::<BlockKind>(&mut block, newer) {
        Reading::Read(()) => {
            if let Some(Value::Array(spans)) = block.get_mut("spans") {
                let mut kept = Vec::new();
                for mut span in std::mem::take(spans) {
                    match known::<SpanStyle, Span>(&mut span, newer) {
                        Reading::Read(_) => kept.push(span),
                        Reading::Unknown => {}
                        Reading::Damaged => return None,
                    }
                }
                *spans = kept;
            }
            read::<Block>(&block, newer)
        }
        other => other.unread(),
    };
    match reading {
        Reading::Read(read) => Some((read, true)),
        Reading::Unknown => {
            let id: Uuid = serde_json::from_value(block.get("id")?.clone()).ok()?;
            let placeholder = Block {
                id,
                kind: BlockKind::Paragraph,
                text: NEWER_PART.to_string(),
                spans: Vec::new(),
            };
            Some((placeholder, false))
        }
        Reading::Damaged => None,
    }
}

/// Reads a span or an annotation, `T`, whose tag, a `K`, is read first and on its own: one of a
/// kind this build doesn't know is unknown, whatever else it holds and in whatever order.
fn known<K, T>(piece: &mut Value, newer: &mut bool) -> Reading<T>
where
    K: DeserializeOwned + Serialize + Tag,
    T: DeserializeOwned,
{
    match read_tag::<K>(piece, newer) {
        Reading::Read(()) => read::<T>(piece, newer),
        other => other.unread(),
    }
}

/// Which field of its piece a tag is.
trait Tag {
    const FIELD: &'static str;
}

impl Tag for BlockKind {
    const FIELD: &'static str = "kind";
}

impl Tag for SpanStyle {
    const FIELD: &'static str = "style";
}

impl Tag for AnnotationKind {
    const FIELD: &'static str = "kind";
}

/// Reads the tag of `piece` as a `K`, and puts it back as this build writes it, with any value in
/// it that this build doesn't know taking its default. A piece with no tag is damaged.
fn read_tag<K: DeserializeOwned + Serialize + Tag>(
    piece: &mut Value,
    newer: &mut bool,
) -> Reading<()> {
    let Some(tag) = piece.get_mut(K::FIELD) else {
        return Reading::Damaged;
    };
    match read::<K>(tag, newer) {
        Reading::Read(known) => match serde_json::to_value(known) {
            Ok(known) => {
                *tag = known;
                Reading::Read(())
            }
            Err(_) => Reading::Damaged,
        },
        other => other.unread(),
    }
}

enum Reading<T> {
    Read(T),
    /// It holds something only a newer version knows, and can't be read without it.
    Unknown,
    Damaged,
}

impl<T> Reading<T> {
    /// A reading that wasn't [`Reading::Read`], as one of another type.
    fn unread<U>(self) -> Reading<U> {
        match self {
            Reading::Read(_) | Reading::Damaged => Reading::Damaged,
            Reading::Unknown => Reading::Unknown,
        }
    }
}

/// Reads `value` as a `T`, letting any field holding a value this build doesn't know fall back to
/// its default. `newer` is set if there was one.
fn read<T: DeserializeOwned>(value: &Value, newer: &mut bool) -> Reading<T> {
    let mut value = value.clone();
    let mut unknown = false;
    loop {
        let err = match T::deserialize(&value) {
            Ok(read) => return Reading::Read(read),
            Err(err) => err,
        };
        let Some(name) = unknown_variant(&err.to_string()) else {
            // Once something newer was found, what's left can only be read with it.
            return if unknown {
                Reading::Unknown
            } else {
                Reading::Damaged
            };
        };
        unknown = true;
        *newer = true;
        if !remove_field_holding(&mut value, &name) {
            return Reading::Unknown;
        }
    }
}

/// The name in serde's message for a variant it doesn't know.
fn unknown_variant(message: &str) -> Option<String> {
    let rest = message.split_once("unknown variant `")?.1;
    Some(rest.split_once('`')?.0.to_string())
}

/// Takes out the first field, at any depth, holding the variant `name`, written bare or with what
/// it holds. Returns whether there was one. `value` itself isn't a field, so a variant there stays.
fn remove_field_holding(value: &mut Value, name: &str) -> bool {
    let holds = |field: &Value| match field {
        Value::String(variant) => variant == name,
        Value::Object(tagged) => tagged.len() == 1 && tagged.contains_key(name),
        _ => false,
    };
    match value {
        Value::Object(fields) => {
            if let Some(key) = fields
                .iter()
                .find(|(_, field)| holds(field))
                .map(|(key, _)| key.clone())
            {
                fields.remove(&key);
                return true;
            }
            fields
                .values_mut()
                .any(|field| remove_field_holding(field, name))
        }
        Value::Array(items) => items
            .iter_mut()
            .any(|item| remove_field_holding(item, name)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ImageAlign, SpanStyle};

    const ID: &str = "00000000-0000-0000-0000-000000000010";

    fn document(blocks: &str, annotations: &str) -> String {
        format!(
            r#"{{"id":"00000000-0000-0000-0000-000000000001","title":"t","blocks":[{blocks}],"annotations":[{annotations}]}}"#
        )
    }

    fn block(kind: &str, spans: &str) -> String {
        format!(r#"{{"id":"{ID}","kind":{kind},"text":"Words.","spans":[{spans}]}}"#)
    }

    #[test]
    fn a_document_this_build_reads_whole_has_no_view() {
        let json = document(&block(r#""Paragraph""#, ""), "");
        assert!(serde_json::from_str::<Document>(&json).is_ok());
        assert!(view_of(&json).is_none());
        assert!(!is_newer(&json));
    }

    #[test]
    fn a_kind_this_build_does_not_know_with_or_without_contents_is_a_placeholder() {
        for kind in [r#""Table""#, r#"{"Table":{"rows":2}}"#] {
            let json = document(&block(kind, ""), "");
            assert!(is_newer(&json), "{kind}");
            let view = view_of(&json).unwrap();
            assert_eq!(view.blocks[0].id.to_string(), ID);
            assert_eq!(view.blocks[0].kind, BlockKind::Paragraph);
            assert_eq!(view.blocks[0].text, NEWER_PART);
        }
    }

    #[test]
    fn a_style_inside_a_known_one_is_left_out_with_its_span() {
        let spans = r#"{"start":0,"end":2,"style":"Strike"},{"start":0,"end":2,"style":"Bold"}"#;
        let view = view_of(&document(&block(r#""Paragraph""#, spans), "")).unwrap();
        assert_eq!(view.blocks[0].text, "Words.");
        assert_eq!(view.blocks[0].spans.len(), 1);
        assert_eq!(view.blocks[0].spans[0].style, SpanStyle::Bold);
    }

    #[test]
    fn a_value_with_a_default_falls_back_to_it() {
        let kind = r#"{"Image":{"path":"a.png","align":{"Tilted":5}}}"#;
        let view = view_of(&document(&block(kind, ""), "")).unwrap();
        assert_eq!(
            view.blocks[0].kind,
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: ImageAlign::Left,
            }
        );
    }

    /// What a kind this build doesn't know holds can be of any shape: it's the newer version's.
    #[test]
    fn a_kind_this_build_does_not_know_is_unknown_whatever_else_its_piece_holds() {
        let spans = r#"{"start":"no","end":2,"style":"Bold"}"#;
        let view = view_of(&document(&block(r#""Table""#, spans), "")).unwrap();
        assert_eq!(view.blocks[0].text, NEWER_PART);
        let note = r#"{"anchor":{"at":3},"id":7,"kind":"Bookmark"}"#;
        let view = view_of(&document(&block(r#""Paragraph""#, ""), note)).unwrap();
        assert!(view.annotations.is_empty());
        let span = r#"{"start":[0,1],"end":2,"style":"Strike"}"#;
        let view = view_of(&document(&block(r#""Paragraph""#, span), "")).unwrap();
        assert!(view.blocks[0].spans.is_empty());
    }

    #[test]
    fn damage_makes_no_view() {
        for json in [
            "{\"id\":".to_string(),
            document(&block(r#""Table""#, ""), "").replace("\"title\":\"t\",", ""),
            document(&block(r#""Table""#, ""), "").replace(&format!("\"id\":\"{ID}\","), ""),
            document(
                &block(r#""Paragraph""#, r#"{"start":"no","end":2,"style":"Bold"}"#),
                "",
            ),
            document(&block(r#""Table""#, ""), r#"{"kind":7}"#),
            document(&block(r#""Paragraph""#, ""), r#"{"kind":"Comment"}"#)
                .replace("\"blocks\":", "\"blocks\":7,\"x\":"),
        ] {
            assert!(view_of(&json).is_none(), "{json}");
            assert!(!is_newer(&json), "{json}");
        }
    }
}
