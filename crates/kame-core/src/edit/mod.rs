//! Editing: invertible ops, the undo/redo editor, and caret commands.

mod editor;
pub(crate) mod ops;

pub use editor::{Cursor, EditError, Editor, Selection};
pub use ops::{apply_inverse, Op};
