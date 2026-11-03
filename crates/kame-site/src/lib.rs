//! Builds a static site from `.kame` documents and Markdown pages.

pub mod config;
pub mod source;

#[cfg(test)]
pub(crate) mod testing;

/// What a build found wrong. Errors stop it, reported together; warnings don't.
#[derive(Debug, Default)]
pub struct Problems {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}
