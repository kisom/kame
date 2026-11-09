//! Editing: invertible ops, the undo/redo editor, and caret commands.

mod editor;
pub(crate) mod ops;

pub(crate) use editor::{restyled, styled_throughout};
pub use editor::{Cursor, EditError, Editor, Selection};
pub use ops::{apply_inverse, try_apply_inverse, Op};
