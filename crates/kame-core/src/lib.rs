//! kame's documents, independent of any window: the model, its edit history, the `.kame`
//! file, and Markdown and HTML on the way in and out. Nothing here knows about a GUI or a
//! text engine, so a command line tool can use it as readily as the editor does.

pub mod edit;
pub mod export;
pub mod import;
pub mod links;
pub mod model;
pub mod session;
pub mod storage;
pub mod text;

/// Where tests put their files, gone when the test process ends.
#[cfg(test)]
pub(crate) fn test_temp_dir() -> std::path::PathBuf {
    kame_test_dir::root().to_path_buf()
}
