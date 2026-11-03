//! Markdown pages, rendered directly so that a page can use all of Markdown.

use std::path::Path;
use std::path::PathBuf;

use kame_core::import::markdown::split_front_matter;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::Problems;

/// A page ready to publish.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub template: String,
    pub body: String,
}

/// Every `.md` file under `dir`, sorted, so that builds are repeatable.
fn markdown_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            markdown_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            found.push(path);
        }
    }
}

fn first_heading(source: &str) -> Option<String> {
    let mut title = String::new();
    let mut inside = false;
    for event in Parser::new_ext(source, options()) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => inside = true,
            Event::End(TagEnd::Heading(_)) if inside => return Some(title),
            Event::Text(text) | Event::Code(text) if inside => title.push_str(&text),
            _ => {}
        }
    }
    None
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_STRIKETHROUGH
}

/// Reads `pages/` under `site_dir`. A site doesn't have to have any.
pub fn read_all(site_dir: &Path, problems: &mut Problems) -> Vec<Page> {
    let root = site_dir.join("pages");
    let mut files = Vec::new();
    markdown_files(&root, &mut files);
    let mut pages = Vec::new();
    for file in files {
        let relative = file.strip_prefix(&root).unwrap_or(&file);
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(err) => {
                problems
                    .errors
                    .push(format!("could not read {}: {err}", relative.display()));
                continue;
            }
        };
        let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
        let front = split_front_matter(source);
        let body = front.as_ref().map_or(source, |found| found.body);
        let stem = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = front
            .as_ref()
            .map(|found| found.metadata.title.clone())
            .filter(|title| !title.is_empty())
            .or_else(|| first_heading(body))
            .unwrap_or(stem);
        let template = front
            .as_ref()
            .and_then(|found| found.template.clone())
            .unwrap_or_else(|| "page.html".to_string());
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, Parser::new_ext(body, options()));
        let url = format!(
            "/{}/",
            relative
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/")
        );
        pages.push(Page {
            url,
            title,
            template,
            body: html,
        });
    }
    pages.sort_by(|a, b| a.url.cmp(&b.url));
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;

    fn site(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = temp_dir("pages");
        for (name, text) in files {
            let path = dir.join("pages").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    fn only(dir: &Path) -> Page {
        let mut problems = Problems::default();
        let pages = read_all(dir, &mut problems);
        assert_eq!(pages.len(), 1);
        pages.into_iter().next().unwrap()
    }

    #[test]
    fn a_page_gets_a_url_from_its_path() {
        let dir = site(&[("about.md", "# About\n"), ("a/b.md", "# B\n")]);
        let mut problems = Problems::default();
        let urls: Vec<String> = read_all(&dir, &mut problems)
            .into_iter()
            .map(|p| p.url)
            .collect();
        assert_eq!(urls, ["/a/b/", "/about/"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_title_prefers_front_matter_then_the_first_heading_then_the_file() {
        let dir = site(&[("a.md", "---\ntitle: From Front\n---\n\n# Heading\n")]);
        assert_eq!(only(&dir).title, "From Front");
        std::fs::remove_dir_all(&dir).unwrap();
        let dir = site(&[("a.md", "# From Heading\n\nWords.\n")]);
        assert_eq!(only(&dir).title, "From Heading");
        std::fs::remove_dir_all(&dir).unwrap();
        let dir = site(&[("my-page.md", "Words.\n")]);
        assert_eq!(only(&dir).title, "my-page");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_page_can_ask_for_a_template() {
        let dir = site(&[(
            "a.md",
            "---\ntitle: A\ntemplate: wide.html\n---\n\nWords.\n",
        )]);
        assert_eq!(only(&dir).template, "wide.html");
        std::fs::remove_dir_all(&dir).unwrap();
        let dir = site(&[("a.md", "Words.\n")]);
        assert_eq!(only(&dir).template, "page.html");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_page_can_use_all_of_markdown() {
        let dir = site(&[("a.md", "| a |\n|---|\n| b |\n\n`code` and \"quotes\"\n")]);
        let body = only(&dir).body;
        assert!(body.contains("<table>"), "{body}");
        assert!(body.contains("<code>code</code>"), "{body}");
        assert!(body.contains("\u{201c}quotes\u{201d}"), "{body}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_pages_directory_is_fine() {
        let dir = temp_dir("nopages");
        let mut problems = Problems::default();
        assert!(read_all(&dir, &mut problems).is_empty());
        assert!(problems.errors.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn other_files_in_pages_are_ignored() {
        let dir = site(&[("a.md", "Words.\n"), ("notes.txt", "not a page")]);
        assert_eq!(only(&dir).url, "/a/");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_page_that_cant_be_read_is_an_error() {
        let dir = temp_dir("badpage");
        std::fs::create_dir_all(dir.join("pages")).unwrap();
        std::fs::write(dir.join("pages/a.md"), [0xff, 0xfe]).unwrap();
        let mut problems = Problems::default();
        assert!(read_all(&dir, &mut problems).is_empty());
        assert!(problems.errors[0].contains("a.md"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
