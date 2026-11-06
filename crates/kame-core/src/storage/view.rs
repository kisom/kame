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
//! Damage isn't taken for something newer: JSON that doesn't parse, a value of the wrong type or a
//! required field missing, anywhere, makes no view, as it makes no document.

use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{Annotation, Block, BlockKind, Document, Span};

/// What a block of a kind this build doesn't know says in its place.
pub const NEWER_PART: &str = "A newer version of kame wrote this part. Update kame to see it.";

/// The document in `json` as this build can show it, if it holds something only a newer version
/// knows and is otherwise sound. `None` for JSON this build reads whole, and for damage.
pub(crate) fn view_of(json: &str) -> Option<Document> {
    let mut value: Value = serde_json::from_str(json).ok()?;
    let mut newer = false;
    let document = value.as_object_mut()?;
    if let Some(Value::Array(blocks)) = document.get_mut("blocks") {
        for block in blocks.iter_mut() {
            let view = block_view(block, &mut newer)?;
            *block = serde_json::to_value(view).ok()?;
        }
    }
    if let Some(Value::Array(annotations)) = document.get_mut("annotations") {
        *annotations = known_only::<Annotation>(std::mem::take(annotations), &mut newer)?;
    }
    if !newer {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Whether `json` is a document this build can't read only because it holds something a newer
/// version knows.
pub(crate) fn is_newer(json: &str) -> bool {
    serde_json::from_str::<Document>(json).is_err() && view_of(json).is_some()
}

/// A block as this build can show it, or `None` if it's damaged.
fn block_view(block: &Value, newer: &mut bool) -> Option<Block> {
    let mut block = block.clone();
    if let Some(Value::Array(spans)) = block.get_mut("spans") {
        *spans = known_only::<Span>(std::mem::take(spans), newer)?;
    }
    match read::<Block>(&block, newer) {
        Reading::Read(read) => Some(read),
        Reading::Unknown => {
            let id: Uuid = serde_json::from_value(block.get("id")?.clone()).ok()?;
            Some(Block {
                id,
                kind: BlockKind::Paragraph,
                text: NEWER_PART.to_string(),
                spans: Vec::new(),
            })
        }
        Reading::Damaged => None,
    }
}

/// Those of `values` this build can read as a `T`, leaving out any it doesn't know. `None` if one
/// is damaged.
fn known_only<T: DeserializeOwned>(values: Vec<Value>, newer: &mut bool) -> Option<Vec<Value>> {
    let mut kept = Vec::new();
    for value in values {
        match read::<T>(&value, newer) {
            Reading::Read(_) => kept.push(value),
            Reading::Unknown => {}
            Reading::Damaged => return None,
        }
    }
    Some(kept)
}

enum Reading<T> {
    Read(T),
    /// It holds something only a newer version knows, and can't be read without it.
    Unknown,
    Damaged,
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

    #[test]
    fn damage_makes_no_view() {
        for json in [
            "{\"id\":".to_string(),
            document(&block(r#""Table""#, ""), "").replace("\"title\":\"t\",", ""),
            document(&block(r#""Table""#, ""), "").replace(&format!("\"id\":\"{ID}\","), ""),
            document(
                &block(r#""Table""#, r#"{"start":"no","end":2,"style":"Bold"}"#),
                "",
            ),
            document(&block(r#""Paragraph""#, ""), r#"{"kind":"Comment"}"#)
                .replace("\"blocks\":", "\"blocks\":7,\"x\":"),
        ] {
            assert!(view_of(&json).is_none(), "{json}");
            assert!(!is_newer(&json), "{json}");
        }
    }
}
