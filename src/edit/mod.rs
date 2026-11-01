//! Editing: invertible ops, the undo/redo editor, and caret commands.

mod editor;
mod ops;

pub use editor::{Cursor, EditError, Editor, Selection};
pub use ops::{apply, apply_inverse, invert, Op};
