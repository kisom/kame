//! Where a document's references to other things end up on the site.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kame_core::links::{find_document, split_document_target};
use kame_core::model::{BlockKind, SpanStyle};

use crate::source::Source;
use crate::Problems;

/// Every document on the site, by the file it comes from.
pub struct SiteIndex {
    by_file: HashMap<PathBuf, (String, bool)>,
}

impl SiteIndex {
    pub fn new(sources: &[Source]) -> SiteIndex {
        let by_file = sources
            .iter()
            .filter_map(|source| {
                let file = source.path.canonicalize().ok()?;
                Some((file, (source.url(), source.draft)))
            })
            .collect();
        SiteIndex { by_file }
    }

    /// The site URL a document link leads to, or `None` to publish it as text. A published
    /// document never links to a draft, so publishing can't reveal one. A `#Heading` is
    /// ignored, so one with no file leads nowhere.
    pub fn link_url(&self, target: &str, from: &Source) -> Option<String> {
        let (path, _heading) = split_document_target(target);
        if path.is_empty() {
            return None;
        }
        let file = find_document(path, &from.path)?.canonicalize().ok()?;
        let (url, draft) = self.by_file.get(&file)?;
        (!*draft || from.draft).then(|| url.clone())
    }
}

/// Where each of a document's images ends up.
pub struct Images {
    copies: Vec<(String, PathBuf)>,
    named: HashMap<String, String>,
}

impl SiteIndex {
    /// Why [`SiteIndex::link_url`] found nothing, for the build to say so.
    fn why_unlinked(&self, target: &str, from: &Source) -> &'static str {
        let (path, _heading) = split_document_target(target);
        if path.is_empty() {
            return "it is to a heading, and headings aren't linked";
        }
        let Some(file) = find_document(path, &from.path) else {
            return "there is no document there";
        };
        let listed = file
            .canonicalize()
            .ok()
            .is_some_and(|file| self.by_file.contains_key(&file));
        if listed {
            "it is to a draft, which a published document doesn't reveal"
        } else {
            "that document isn't in site.yaml"
        }
    }
}

/// `text` as it can appear in a URL's path.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

impl Images {
    /// Works out where each image block's file goes, in reading order, so that two files
    /// sharing a name are told apart the same way on every build.
    pub fn resolve(source: &Source, site_dir: &Path, problems: &mut Problems) -> Images {
        let base = source.path.parent().unwrap_or(Path::new(""));
        let mut copies: Vec<(String, PathBuf)> = Vec::new();
        let mut named = HashMap::new();
        for block in &source.doc.blocks {
            let BlockKind::Image { path, .. } = &block.kind else {
                continue;
            };
            if named.contains_key(path)
                || source.placed_images.contains(path)
                || path.starts_with("http://")
                || path.starts_with("https://")
            {
                continue;
            }
            if path.starts_with('/')
                && !Path::new(path).is_file()
                && site_dir
                    .join("static")
                    .join(path.trim_start_matches('/'))
                    .is_file()
            {
                continue;
            }
            let file = base.join(path);
            let Ok(file) = file.canonicalize() else {
                problems
                    .warnings
                    .push(format!("\"{}\": no image at {path}", source.slug));
                continue;
            };
            if !file.is_file() {
                problems
                    .warnings
                    .push(format!("\"{}\": no image at {path}", source.slug));
                continue;
            }
            if let Some((name, _)) = copies.iter().find(|(_, existing)| *existing == file) {
                named.insert(path.clone(), name.clone());
                continue;
            }
            let name = unique_name(&file, &copies);
            named.insert(path.clone(), name.clone());
            copies.push((name, file));
        }
        Images { copies, named }
    }

    /// Each file to copy beside the page, as its name there and where it comes from.
    pub fn copies(&self) -> &[(String, PathBuf)] {
        &self.copies
    }

    /// The image's URL from the page beside it.
    pub fn relative(&self, path: &str) -> String {
        match self.named.get(path) {
            Some(name) => percent_encode(name),
            None => path.to_string(),
        }
    }

    /// The image's URL from anywhere, as a feed needs.
    pub fn absolute(&self, path: &str, site_url: &str, doc_url: &str) -> String {
        match self.named.get(path) {
            Some(name) => format!("{site_url}{doc_url}{}", percent_encode(name)),
            None if path.starts_with('/') => format!("{site_url}{path}"),
            None => path.to_string(),
        }
    }
}

/// `file`'s name, or with `-2`, `-3` and so on if another file has taken it.
fn unique_name(file: &Path, copies: &[(String, PathBuf)]) -> String {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let taken = |candidate: &str| copies.iter().any(|(existing, _)| existing == candidate);
    if !taken(&name) {
        return name;
    }
    let stem = file
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = file
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|number| format!("{stem}-{number}{extension}"))
        .find(|candidate| !taken(candidate))
        .expect("there is always another number")
}

/// Records each document link that will publish as text, so that rot is visible.
pub fn note_unpublished_links(source: &Source, index: &SiteIndex, problems: &mut Problems) {
    for block in &source.doc.blocks {
        for span in &block.spans {
            if let SpanStyle::DocumentLink { target } = &span.style {
                if index.link_url(target, source).is_none() {
                    problems.warnings.push(format!(
                        "\"{}\": the link to {target} publishes as its text: {}",
                        source.slug,
                        index.why_unlinked(target, source)
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Entry;
    use crate::source;
    use crate::testing::{block, metadata, paragraph, temp_dir, write_kame};
    use kame_core::model::{Document, Span};

    fn make(dir: &Path, slug: &str, draft: bool, document: Document) -> Source {
        let path = dir.join(format!("{slug}.kame"));
        write_kame(&path, &document, Some(&metadata(slug, "2026-11-02", &[])));
        let mut problems = Problems::default();
        source::read(
            &Entry {
                slug: slug.to_string(),
                path,
            },
            draft,
            &mut problems,
        )
        .unwrap()
    }

    fn plain() -> Document {
        let mut doc = Document::new("D");
        doc.blocks = vec![paragraph("Words.")];
        doc
    }

    fn with_images(paths: &[&str]) -> Document {
        let mut doc = Document::new("D");
        doc.blocks = paths
            .iter()
            .map(|path| {
                block(
                    BlockKind::Image {
                        path: path.to_string(),
                        link: None,
                        align: Default::default(),
                    },
                    "",
                )
            })
            .collect();
        doc
    }

    #[test]
    fn a_link_to_a_published_document_goes_to_its_page() {
        let dir = temp_dir("link");
        let a = make(&dir, "a", false, plain());
        let b = make(&dir, "b", false, plain());
        let index = SiteIndex::new(&[a.clone(), b]);
        assert_eq!(index.link_url("b.kame", &a).as_deref(), Some("/k/b/"));
        assert_eq!(
            index.link_url("b", &a).as_deref(),
            Some("/k/b/"),
            "extension left off"
        );
        let absolute = dir.join("b.kame").display().to_string();
        assert_eq!(index.link_url(&absolute, &a).as_deref(), Some("/k/b/"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_heading_in_a_link_is_ignored() {
        let dir = temp_dir("heading");
        let a = make(&dir, "a", false, plain());
        let b = make(&dir, "b", false, plain());
        let index = SiteIndex::new(&[a.clone(), b]);
        assert_eq!(index.link_url("b.kame#Intro", &a).as_deref(), Some("/k/b/"));
        assert_eq!(
            index.link_url("#Intro", &a),
            None,
            "a heading here publishes as text"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn publishing_cant_reveal_a_draft() {
        let dir = temp_dir("draftlink");
        let published = make(&dir, "pub", false, plain());
        let draft = make(&dir, "draft", true, plain());
        let other = make(&dir, "other", true, plain());
        let index = SiteIndex::new(&[published.clone(), draft.clone(), other.clone()]);
        assert_eq!(index.link_url("draft", &published), None);
        assert_eq!(
            index.link_url("draft", &other).as_deref(),
            Some("/drafts/draft/")
        );
        assert_eq!(index.link_url("pub", &draft).as_deref(), Some("/k/pub/"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_to_anything_else_publishes_as_text() {
        let dir = temp_dir("rot");
        let a = make(&dir, "a", false, plain());
        std::fs::write(dir.join("unlisted.md"), "").unwrap();
        let index = SiteIndex::new(std::slice::from_ref(&a));
        assert_eq!(index.link_url("gone.kame", &a), None);
        assert_eq!(index.link_url("unlisted", &a), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_published_as_text_are_noted() {
        let dir = temp_dir("note");
        let mut doc = plain();
        doc.blocks[0].spans = vec![Span {
            start: 0,
            end: 5,
            style: SpanStyle::DocumentLink {
                target: "gone.kame".to_string(),
            },
        }];
        let a = make(&dir, "a", false, doc);
        let index = SiteIndex::new(std::slice::from_ref(&a));
        let mut problems = Problems::default();
        note_unpublished_links(&a, &index, &mut problems);
        assert_eq!(problems.warnings.len(), 1);
        assert!(problems.warnings[0].contains("gone.kame"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_image_is_copied_beside_the_page() {
        let dir = temp_dir("image");
        std::fs::write(dir.join("a.png"), "x").unwrap();
        let source = make(&dir, "doc", false, with_images(&["a.png"]));
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        let file = dir.join("a.png").canonicalize().unwrap();
        assert_eq!(images.copies(), [("a.png".to_string(), file)]);
        assert_eq!(images.relative("a.png"), "a.png");
        assert_eq!(
            images.absolute("a.png", "https://e.com", "/k/doc/"),
            "https://e.com/k/doc/a.png"
        );
        assert!(problems.warnings.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_web_image_is_left_alone() {
        let dir = temp_dir("web");
        let url = "https://e.com/a.png";
        let source = make(&dir, "doc", false, with_images(&[url]));
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        assert!(images.copies().is_empty());
        assert_eq!(images.relative(url), url);
        assert_eq!(images.absolute(url, "https://x.com", "/k/doc/"), url);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_site_root_image_in_static_is_left_alone() {
        let dir = temp_dir("static");
        std::fs::create_dir_all(dir.join("static/files")).unwrap();
        std::fs::write(dir.join("static/files/a.png"), "x").unwrap();
        let source = make(&dir, "doc", false, with_images(&["/files/a.png"]));
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        assert!(images.copies().is_empty());
        assert_eq!(images.relative("/files/a.png"), "/files/a.png");
        assert_eq!(
            images.absolute("/files/a.png", "https://e.com", "/k/doc/"),
            "https://e.com/files/a.png"
        );
        assert!(problems.warnings.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_image_warns_and_stays_as_written() {
        let dir = temp_dir("missing");
        let source = make(&dir, "doc", false, with_images(&["gone.png"]));
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        assert!(images.copies().is_empty());
        assert_eq!(images.relative("gone.png"), "gone.png");
        assert!(problems.errors.is_empty());
        assert!(problems.warnings[0].contains("gone.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review focus: two different files with one name.
    #[test]
    fn two_images_with_one_name_stay_apart_in_reading_order() {
        let dir = temp_dir("names");
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::create_dir_all(dir.join("y")).unwrap();
        std::fs::write(dir.join("x/a.png"), "1").unwrap();
        std::fs::write(dir.join("y/a.png"), "2").unwrap();
        let source = make(
            &dir,
            "doc",
            false,
            with_images(&["x/a.png", "y/a.png", "x/a.png"]),
        );
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        assert_eq!(images.relative("x/a.png"), "a.png");
        assert_eq!(images.relative("y/a.png"), "a-2.png");
        assert_eq!(images.copies().len(), 2, "one file used twice is one copy");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review focus: awkward names must still link, and only ever land beside the page.
    #[test]
    fn awkward_image_names_link_correctly() {
        let dir = temp_dir("awkward");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/my photo été.png"), "x").unwrap();
        let source = make(
            &dir,
            "doc",
            false,
            with_images(&["sub/../sub/my photo été.png"]),
        );
        let mut problems = Problems::default();
        let images = Images::resolve(&source, &dir, &mut problems);
        assert_eq!(images.copies()[0].0, "my photo été.png");
        assert_eq!(
            images.relative("sub/../sub/my photo été.png"),
            "my%20photo%20%C3%A9t%C3%A9.png"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn linking(dir: &Path, target: &str) -> Source {
        let mut doc = plain();
        doc.blocks[0].spans = vec![Span {
            start: 0,
            end: 5,
            style: SpanStyle::DocumentLink {
                target: target.to_string(),
            },
        }];
        make(dir, "from", false, doc)
    }

    fn note(dir: &Path, target: &str, others: Vec<Source>) -> String {
        let from = linking(dir, target);
        let mut sources = others;
        sources.push(from.clone());
        let index = SiteIndex::new(&sources);
        let mut problems = Problems::default();
        note_unpublished_links(&from, &index, &mut problems);
        problems.warnings.join("\n")
    }

    #[test]
    fn a_note_says_when_a_link_is_to_a_draft() {
        let dir = temp_dir("whydraft");
        let draft = make(&dir, "d", true, plain());
        let found = note(&dir, "d.kame", vec![draft]);
        assert!(
            found.contains("d.kame") && found.contains("a draft"),
            "{found}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_says_when_a_link_is_to_a_heading() {
        let dir = temp_dir("whyheading");
        let found = note(&dir, "#Intro", Vec::new());
        assert!(
            found.contains("#Intro") && found.contains("heading"),
            "{found}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_says_when_a_document_exists_but_isnt_published() {
        let dir = temp_dir("whyunlisted");
        std::fs::write(dir.join("other.kame"), "").unwrap();
        let found = note(&dir, "other.kame", Vec::new());
        assert!(found.contains("isn't in site.yaml"), "{found}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_says_when_a_link_has_rotted() {
        let dir = temp_dir("whyrot");
        let found = note(&dir, "gone.kame", Vec::new());
        assert!(found.contains("no document there"), "{found}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
