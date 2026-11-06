//! Reading the documents a site publishes, without ever changing them.

use std::path::PathBuf;

use kame_core::model::{Date, Document};

use crate::config::{Config, Entry};
use crate::Problems;

/// A tag as written, and how it appears in a URL.
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub slug: String,
}

/// A document ready to publish.
#[derive(Clone, Debug)]
pub struct Source {
    pub slug: String,
    pub path: PathBuf,
    pub draft: bool,
    pub title: String,
    pub author: String,
    pub date: Date,
    pub tags: Vec<Tag>,
    pub doc: Document,
    /// Where a notebook's document publishes, as its URL follows its folders. A site's goes by slug.
    pub page: Option<String>,
    /// Image paths that are already where they publish, with nothing to copy beside the page.
    pub placed_images: Vec<String>,
    /// The document opens with its title as a centered heading, which the page prints in its place.
    pub title_centered: bool,
}

impl Source {
    /// Where this document publishes.
    pub fn url(&self) -> String {
        if let Some(page) = &self.page {
            return page.clone();
        }
        let root = if self.draft { "drafts" } else { "k" };
        format!("/{root}/{}/", self.slug)
    }
}

/// A tag as it appears in a URL: lowercase, with anything but letters and digits as a single
/// hyphen. Letters from any alphabet stay. Can be empty, which callers treat as an error.
pub fn tag_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// The day a document's file last changed, for one with no date of its own.
fn file_date(path: &std::path::Path) -> Option<Date> {
    use std::os::unix::fs::MetadataExt;
    let seconds = std::fs::metadata(path).ok()?.ctime();
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(seconds).ok()?);
    Some(Date::from_system_time(when))
}

/// Reads one document, reporting what is wrong with it. Nothing is ever written.
pub fn read(entry: &Entry, draft: bool, problems: &mut Problems) -> Option<Source> {
    let path = &entry.path;
    let slug = &entry.slug;
    if !path.is_file() {
        problems
            .errors
            .push(format!("\"{slug}\": no document at {}", path.display()));
        return None;
    }
    let loaded = kame_core::storage::Db::open_read_only(path)
        .and_then(|db| Ok((db.load_document()?, db.load_metadata()?)));
    let (doc, metadata) = match loaded {
        Ok(loaded) => loaded,
        // One from a newer kame is for that kame to publish, and opening it in this one changes
        // nothing.
        Err(_) if kame_core::storage::Db::open_read_only(path).is_ok_and(|db| db.holds_newer()) => {
            problems.errors.push(format!(
                "\"{slug}\": could not read {}: it has changes from a newer version of kame. Update kame to publish it.",
                path.display()
            ));
            return None;
        }
        Err(err) => {
            problems.errors.push(format!(
                "\"{slug}\": could not read {}: {err}. If it was last saved by an older kame, open it in kame once.",
                path.display()
            ));
            return None;
        }
    };
    from_parts(
        slug,
        path,
        draft,
        doc,
        metadata.unwrap_or_default(),
        problems,
    )
}

/// A document ready to publish, from what's been read of it.
pub fn from_parts(
    slug: &str,
    path: &std::path::Path,
    draft: bool,
    doc: Document,
    metadata: kame_core::model::Metadata,
    problems: &mut Problems,
) -> Option<Source> {
    let slug = &slug.to_string();
    let stem = || {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| slug.clone())
    };
    let title = [&metadata.title, &doc.title]
        .into_iter()
        .find(|title| !title.is_empty() && title.as_str() != "Untitled")
        .cloned()
        .unwrap_or_else(stem);

    let written = [&metadata.published, &metadata.created]
        .into_iter()
        .find(|date| !date.trim().is_empty());
    let date = match written {
        Some(text) => match Date::parse(text) {
            Some(date) => date,
            None => {
                problems.errors.push(format!(
                    "\"{slug}\": \"{text}\" isn't a date: use YYYY-MM-DD"
                ));
                return None;
            }
        },
        None => {
            problems.warnings.push(format!(
                "\"{slug}\" has no date of its own, so it is using its file's"
            ));
            // One with only logs has no file yet, and they are as recent.
            let logs = kame_notebook::log_folder(path);
            match file_date(path).or_else(|| file_date(&logs)) {
                Some(date) => date,
                None => {
                    problems.errors.push(format!(
                        "\"{slug}\": could not read a date for {}",
                        path.display()
                    ));
                    return None;
                }
            }
        }
    };

    let mut tags = Vec::new();
    for name in &metadata.tags {
        let tag_url = tag_slug(name);
        if tag_url.is_empty() {
            problems.errors.push(format!(
                "\"{slug}\": the tag \"{name}\" has no letters or digits"
            ));
            return None;
        }
        tags.push(Tag {
            name: name.clone(),
            slug: tag_url,
        });
    }

    let title_centered = kame_core::export::title_is_centered(&doc, &title);
    let doc = kame_core::export::without_repeated_title(&doc, &title).into_owned();
    Some(Source {
        slug: slug.clone(),
        path: path.to_path_buf(),
        draft,
        title,
        author: metadata.author,
        date,
        tags,
        doc,
        page: None,
        placed_images: Vec::new(),
        title_centered,
    })
}

/// Reads a site's documents: published ones newest first, then by slug, then drafts in the
/// same order.
pub fn read_all(config: &Config, include_drafts: bool, problems: &mut Problems) -> Vec<Source> {
    let order = |a: &Source, b: &Source| b.date.cmp(&a.date).then_with(|| a.slug.cmp(&b.slug));
    let with_author = |mut source: Source| {
        if source.author.is_empty() {
            source.author = config.author.clone();
        }
        source
    };
    let mut published: Vec<Source> = config
        .publish
        .iter()
        .filter_map(|entry| read(entry, false, problems))
        .map(with_author)
        .collect();
    published.sort_by(order);
    if include_drafts {
        let mut drafts: Vec<Source> = config
            .drafts
            .iter()
            .filter_map(|entry| read(entry, true, problems))
            .map(with_author)
            .collect();
        drafts.sort_by(order);
        published.extend(drafts);
    }
    published
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{block, metadata, paragraph, temp_dir, write_kame};
    use kame_core::model::BlockKind;
    use kame_core::storage::Db;

    fn entry(slug: &str, path: PathBuf) -> Entry {
        Entry {
            slug: slug.to_string(),
            path,
        }
    }

    fn doc(title: &str) -> Document {
        let mut doc = Document::new(title);
        doc.blocks = vec![paragraph("Words.")];
        doc
    }

    #[test]
    fn the_title_prefers_metadata_then_the_document_then_the_file() {
        let dir = temp_dir("titles");
        let cases = [
            ("a.kame", "Doc", Some("Meta"), "Meta"),
            ("b.kame", "Doc", None, "Doc"),
            ("my-file.kame", "Untitled", None, "my-file"),
        ];
        for (file, doc_title, meta_title, expected) in cases {
            let path = dir.join(file);
            let meta = meta_title.map(|title| metadata(title, "2026-11-02", &[]));
            write_kame(&path, &doc(doc_title), meta.as_ref());
            let mut problems = Problems::default();
            let source = read(&entry("s", path), false, &mut problems).unwrap();
            assert_eq!(source.title, expected);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_date_is_published_then_created() {
        let dir = temp_dir("dates");
        let path = dir.join("a.kame");
        let mut meta = metadata("A", "2026-11-02", &[]);
        meta.created = "2020-01-01".to_string();
        write_kame(&path, &doc("A"), Some(&meta));
        let mut problems = Problems::default();
        let found = read(&entry("a", path.clone()), false, &mut problems).unwrap();
        assert_eq!(found.date.to_string(), "2026-11-02");

        meta.published = String::new();
        write_kame(&path, &doc("A"), Some(&meta));
        let found = read(&entry("a", path), false, &mut problems).unwrap();
        assert_eq!(found.date.to_string(), "2020-01-01");
        assert!(problems.warnings.is_empty(), "{:?}", problems.warnings);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn with_no_date_the_files_ctime_is_used_with_a_warning() {
        let dir = temp_dir("ctime");
        let path = dir.join("a.kame");
        // Either side of the write, so that a build running across midnight UTC still passes.
        let before = Date::from_system_time(std::time::SystemTime::now());
        write_kame(&path, &doc("A"), Some(&metadata("A", "", &[])));
        let mut problems = Problems::default();
        let found = read(&entry("a", path), false, &mut problems).unwrap();
        let after = Date::from_system_time(std::time::SystemTime::now());
        assert!(
            found.date >= before && found.date <= after,
            "{}",
            found.date
        );
        assert_eq!(problems.warnings.len(), 1);
        assert!(problems.warnings[0].contains("\"a\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_date_that_wont_parse_is_an_error_naming_the_document() {
        let dir = temp_dir("baddate");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "next week", &[])));
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_none());
        assert!(problems.errors[0].contains("next week"));
        assert!(problems.errors[0].contains("\"a\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_created_doesnt_matter_when_published_is_good() {
        let dir = temp_dir("goodpub");
        let path = dir.join("a.kame");
        let mut meta = metadata("A", "2026-11-02", &[]);
        meta.created = "whenever".to_string();
        write_kame(&path, &doc("A"), Some(&meta));
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_some());
        assert!(problems.errors.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repeated_title_leaves_the_body() {
        let dir = temp_dir("repeat");
        let path = dir.join("a.kame");
        let mut document = Document::new("A");
        document.blocks = vec![
            block(
                BlockKind::Heading {
                    level: 1,
                    centered: false,
                },
                "The Title",
            ),
            paragraph("Words."),
        ];
        write_kame(
            &path,
            &document,
            Some(&metadata("The Title", "2026-11-02", &[])),
        );
        let mut problems = Problems::default();
        let source = read(&entry("a", path), false, &mut problems).unwrap();
        assert_eq!(source.doc.blocks.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let mut problems = Problems::default();
        let found = read(
            &entry("gone", PathBuf::from("/no/such.kame")),
            false,
            &mut problems,
        );
        assert!(found.is_none());
        assert!(problems.errors[0].contains("/no/such.kame"));
        assert!(!std::path::Path::new("/no/such.kame").exists());
    }

    #[test]
    fn something_that_isnt_a_document_says_to_open_it_in_kame() {
        let dir = temp_dir("notkame");
        let path = dir.join("a.kame");
        std::fs::write(&path, "not a database").unwrap();
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_none());
        assert!(
            problems.errors[0].contains("open it in kame"),
            "{:?}",
            problems.errors
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reading_leaves_the_file_byte_for_byte() {
        let dir = temp_dir("readonly");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "2026-11-02", &["x"])));
        let before = std::fs::read(&path).unwrap();
        let mut problems = Problems::default();
        read(&entry("a", path.clone()), false, &mut problems).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file from before kame kept metadata would otherwise publish under the wrong title
    /// and date, with no sign of why.
    #[test]
    fn a_file_with_no_metadata_table_says_to_open_it_in_kame() {
        let dir = temp_dir("oldfile");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "2026-11-02", &["x"])));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("DROP TABLE metadata", []).unwrap();
        drop(conn);
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_none());
        assert!(
            problems.errors[0].contains("open it in kame"),
            "{:?}",
            problems.errors
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Opening it in this kame changes nothing, so that isn't what to say.
    #[test]
    fn a_document_from_a_newer_kame_says_to_update_kame() {
        let dir = temp_dir("newerkind");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "2026-11-02", &[])));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE documents SET json = replace(json, '\"kind\":\"Paragraph\"', '\"kind\":\"Table\"')",
            [],
        )
        .unwrap();
        drop(conn);
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_none());
        let said = &problems.errors[0];
        assert!(said.contains("a newer version of kame"), "{said}");
        assert!(said.contains("Update kame"), "{said}");
        assert!(!said.contains("open it in kame"), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review focus: someone will build with the essay still open in kame.
    #[test]
    fn a_document_open_for_writing_can_still_be_read() {
        let dir = temp_dir("open");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "2026-11-02", &[])));
        let mut writer = Db::open(&path).unwrap();
        let mut newer = doc("A");
        newer.blocks = vec![paragraph("Newer.")];
        writer.save_document(&newer).unwrap();
        let mut problems = Problems::default();
        let source = read(&entry("a", path), false, &mut problems).unwrap();
        assert_eq!(source.doc.blocks[0].text, "Newer.");
        drop(writer);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tags_normalize_for_their_urls() {
        assert_eq!(tag_slug("Machine Learning"), "machine-learning");
        assert_eq!(tag_slug("C++ & Rust!"), "c-rust");
        assert_eq!(tag_slug("  spaced  "), "spaced");
        // Review focus: letters from other alphabets stay.
        assert_eq!(tag_slug("Économie"), "économie");
    }

    /// Review focus: a tag that reduces to nothing mustn't make `/tags//`.
    #[test]
    fn a_tag_with_no_letters_is_an_error() {
        let dir = temp_dir("emptytag");
        let path = dir.join("a.kame");
        write_kame(
            &path,
            &doc("A"),
            Some(&metadata("A", "2026-11-02", &["!!!"])),
        );
        let mut problems = Problems::default();
        assert!(read(&entry("a", path), false, &mut problems).is_none());
        assert!(problems.errors[0].contains("!!!"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn documents_sort_newest_first_then_by_slug_with_drafts_last() {
        let dir = temp_dir("order");
        let write = |name: &str, date: &str| {
            let path = dir.join(format!("{name}.kame"));
            write_kame(&path, &doc(name), Some(&metadata(name, date, &[])));
            entry(name, path)
        };
        let config = Config {
            name: "S".to_string(),
            url: "https://e.com".to_string(),
            description: String::new(),
            author: String::new(),
            publish: vec![
                write("old", "2020-01-01"),
                write("b", "2026-01-01"),
                write("a", "2026-01-01"),
            ],
            drafts: vec![write("draft", "2030-01-01")],
        };
        let mut problems = Problems::default();
        let slugs =
            |sources: Vec<Source>| -> Vec<String> { sources.into_iter().map(|s| s.slug).collect() };
        assert_eq!(
            slugs(read_all(&config, true, &mut problems)),
            ["a", "b", "old", "draft"]
        );
        assert_eq!(
            slugs(read_all(&config, false, &mut problems)),
            ["a", "b", "old"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn urls_depend_on_being_a_draft() {
        let dir = temp_dir("urls");
        let path = dir.join("a.kame");
        write_kame(&path, &doc("A"), Some(&metadata("A", "2026-11-02", &[])));
        let mut problems = Problems::default();
        assert_eq!(
            read(&entry("a", path.clone()), false, &mut problems)
                .unwrap()
                .url(),
            "/k/a/"
        );
        assert_eq!(
            read(&entry("a", path), true, &mut problems).unwrap().url(),
            "/drafts/a/"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_document_with_no_author_takes_the_sites() {
        let dir = temp_dir("siteauthor");
        let mut anonymous = metadata("A", "2026-01-02", &[]);
        anonymous.author = String::new();
        let a = dir.join("a.kame");
        write_kame(&a, &doc("A"), Some(&anonymous));
        let mut named = metadata("B", "2026-01-01", &[]);
        named.author = "Someone Else".to_string();
        let b = dir.join("b.kame");
        write_kame(&b, &doc("B"), Some(&named));
        let config = Config {
            name: "S".to_string(),
            url: "https://e.com".to_string(),
            description: String::new(),
            author: "The Site".to_string(),
            publish: vec![entry("a", a), entry("b", b)],
            drafts: Vec::new(),
        };
        let mut problems = Problems::default();
        let found = read_all(&config, true, &mut problems);
        assert_eq!(found[0].author, "The Site");
        assert_eq!(found[1].author, "Someone Else");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
