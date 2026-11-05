//! Working out a whole site: a map from output path to contents, with nothing written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kame_core::export::{html_body, Publish, STYLE};
use minijinja::{context, Environment, Value};

use crate::config::Config;
use crate::feed;
use crate::pages::{self, Page};
use crate::refs::{note_unpublished_links, Images, SiteIndex};
use crate::source::{read_all, Source};
use crate::Problems;

/// What goes at an output path.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Bytes(Vec<u8>),
    Copy(PathBuf),
}

/// A whole site, ready to write.
#[derive(Debug, Default)]
pub struct Plan {
    pub files: BTreeMap<PathBuf, Output>,
}

/// Every template the theme has to have.
const TEMPLATES: [&str; 7] = [
    "base.html",
    "index.html",
    "archive.html",
    "tags.html",
    "tag.html",
    "document.html",
    "page.html",
];

/// A value templates print as it is: markup we rendered, or a URL, which would otherwise have
/// its slashes escaped.
fn safe(text: impl Into<String>) -> Value {
    Value::from_safe_string(text.into())
}

fn tag_url(slug: &str) -> String {
    format!("/tags/{slug}/")
}

fn document_value(source: &Source) -> Value {
    let tags: Vec<Value> = source
        .tags
        .iter()
        .map(|tag| context! { name => tag.name.clone(), url => safe(tag_url(&tag.slug)) })
        .collect();
    context! {
        title => source.title.clone(),
        date => source.date.to_string(),
        url => safe(source.url()),
        tags => tags,
    }
}

/// The path of the page at `url`: `/a/b/` is `a/b/index.html`.
fn page_path(url: &str) -> PathBuf {
    Path::new(url.trim_start_matches('/')).join("index.html")
}

/// Loads the theme's templates, naming each one that is missing or doesn't parse.
fn load_templates(
    site_dir: &Path,
    extra: &[String],
    problems: &mut Problems,
) -> Environment<'static> {
    let mut env = Environment::new();
    let root = site_dir.join("theme/templates");
    let names = TEMPLATES
        .iter()
        .map(|name| name.to_string())
        .chain(extra.iter().cloned());
    for name in names {
        if env.get_template(&name).is_ok() {
            continue;
        }
        match std::fs::read_to_string(root.join(&name)) {
            Ok(text) => {
                if let Err(err) = env.add_template_owned(name.clone(), text) {
                    problems
                        .errors
                        .push(format!("theme/templates/{name}: {err}"));
                }
            }
            Err(err) => problems
                .errors
                .push(format!("theme/templates/{name}: {err}")),
        }
    }
    env
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

impl Plan {
    /// Adds a generated file, or reports what it collides with.
    pub(crate) fn add(&mut self, path: PathBuf, output: Output, problems: &mut Problems) {
        if self.files.contains_key(&path) {
            problems
                .errors
                .push(format!("two things want to be at {}", path.display()));
            return;
        }
        self.files.insert(path, output);
    }
}

/// Renders `template`, reporting a failure rather than stopping, so that all are reported.
fn render(env: &Environment, template: &str, values: Value, problems: &mut Problems) -> Vec<u8> {
    let rendered = env
        .get_template(template)
        .and_then(|found| found.render(values));
    match rendered {
        Ok(text) => text.into_bytes(),
        Err(err) => {
            problems
                .errors
                .push(format!("theme/templates/{template}: {err}"));
            Vec::new()
        }
    }
}

/// One tag on the site, with its published documents.
struct TagEntry<'a> {
    name: String,
    documents: Vec<&'a Source>,
}

/// Groups published documents by tag slug. Spellings differing only by case share a tag, but
/// two different tags reducing to one URL are an error.
fn group_tags<'a>(
    published: &[&'a Source],
    problems: &mut Problems,
) -> BTreeMap<String, TagEntry<'a>> {
    let mut tags: BTreeMap<String, TagEntry<'a>> = BTreeMap::new();
    for source in published {
        for tag in &source.tags {
            let entry = tags.entry(tag.slug.clone()).or_insert_with(|| TagEntry {
                name: tag.name.clone(),
                documents: Vec::new(),
            });
            if entry.name.to_lowercase() != tag.name.to_lowercase() {
                problems.errors.push(format!(
                    "the tags \"{}\" and \"{}\" would share /tags/{}/",
                    entry.name, tag.name, tag.slug
                ));
            }
            if !entry
                .documents
                .iter()
                .any(|other| other.slug == source.slug)
            {
                entry.documents.push(source);
            }
        }
    }
    tags
}

/// Where the navigation's About link leads: the page at `/about/`, however its name is
/// capitalised, since addresses follow file names and a web server tells them apart. `None` if
/// nothing publishes there. Should there be two, the largest is all lowercase, and a build
/// always picks the same one.
fn about_url(sources: &[Source], pages: &[Page]) -> Option<String> {
    let documents = sources
        .iter()
        .filter(|source| !source.draft)
        .map(|source| source.url());
    let pages = pages.iter().map(|page| page.url.clone());
    documents
        .chain(pages)
        .filter(|url| url.eq_ignore_ascii_case("/about/"))
        .max()
}

/// Works out the whole site. `None` if anything stops it, with `problems` saying what.
pub fn build_plan(site_dir: &Path, include_drafts: bool, problems: &mut Problems) -> Option<Plan> {
    let config = match Config::load(site_dir) {
        Ok(config) => config,
        Err(errors) => {
            problems.errors.extend(errors);
            return None;
        }
    };
    let sources = read_all(&config, include_drafts, problems);
    plan_site(site_dir, &config, sources, problems)
}

/// Works out a whole site from what it's made of, with nothing written. A document at `/` is the
/// home page, in place of the usual listing, and a site with no URL has no feed, which needs one.
pub fn plan_site(
    site_dir: &Path,
    config: &Config,
    sources: Vec<Source>,
    problems: &mut Problems,
) -> Option<Plan> {
    let pages: Vec<Page> = pages::read_all(site_dir, problems);
    let extra: Vec<String> = pages.iter().map(|page| page.template.clone()).collect();
    let env = load_templates(site_dir, &extra, problems);
    if !problems.errors.is_empty() {
        return None;
    }

    let site = context! {
        name => config.name.clone(),
        url => safe(config.url.clone()),
        description => config.description.clone(),
        author => config.author.clone(),
        about => about_url(&sources, &pages).map(safe),
    };
    let kame_css = safe(STYLE);
    let index = SiteIndex::new(&sources);
    let published: Vec<&Source> = sources.iter().filter(|source| !source.draft).collect();
    let listed: Vec<Value> = published
        .iter()
        .map(|source| document_value(source))
        .collect();

    let mut plan = Plan::default();

    // Documents, with their images beside them.
    for source in &sources {
        note_unpublished_links(source, &index, problems);
        let images = Images::resolve(source, site_dir, problems);
        let document_link = |target: &str| index.link_url(target, source);
        let image = |path: &str| images.relative(path);
        let body = html_body(
            &source.doc,
            &Publish {
                document_link: &document_link,
                image: &image,
            },
        );
        let url = source.url();
        // A draft shows no tags: it is in none of their listings, so they'd lead elsewhere.
        let shown = if source.draft {
            &[][..]
        } else {
            &source.tags[..]
        };
        let tags: Vec<Value> = shown
            .iter()
            .map(|tag| context! { name => tag.name.clone(), url => safe(tag_url(&tag.slug)) })
            .collect();
        let values = context! {
            site => site.clone(),
            kame_css => kame_css.clone(),
            title => source.title.clone(),
            date => source.date.to_string(),
            author => source.author.clone(),
            tags => tags,
            body => safe(body),
            url => safe(url.clone()),
            draft => source.draft,
        };
        let page = render(&env, "document.html", values, problems);
        plan.add(page_path(&url), Output::Bytes(page), problems);
        for (name, file) in images.copies() {
            let path = Path::new(url.trim_start_matches('/')).join(name);
            plan.add(path, Output::Copy(file.clone()), problems);
        }
    }

    // Markdown pages.
    for page in &pages {
        let values = context! {
            site => site.clone(),
            kame_css => kame_css.clone(),
            title => page.title.clone(),
            body => safe(page.body.clone()),
            url => safe(page.url.clone()),
        };
        let rendered = render(&env, &page.template, values, problems);
        plan.add(page_path(&page.url), Output::Bytes(rendered), problems);
    }

    // The listings.
    if !sources.iter().any(|source| source.url() == "/") {
        let home = render(
            &env,
            "index.html",
            context! { site => site.clone(), kame_css => kame_css.clone(), documents => listed.clone() },
            problems,
        );
        plan.add(PathBuf::from("index.html"), Output::Bytes(home), problems);
    }
    let archive = render(
        &env,
        "archive.html",
        context! { site => site.clone(), kame_css => kame_css.clone(), documents => listed.clone() },
        problems,
    );
    plan.add(
        PathBuf::from("archive/index.html"),
        Output::Bytes(archive),
        problems,
    );

    let tags = group_tags(&published, problems);
    let tag_values: Vec<Value> = tags
        .iter()
        .map(|(slug, entry)| {
            context! {
                name => entry.name.clone(),
                url => safe(tag_url(slug)),
                count => entry.documents.len(),
            }
        })
        .collect();
    let list = render(
        &env,
        "tags.html",
        context! { site => site.clone(), kame_css => kame_css.clone(), tags => tag_values },
        problems,
    );
    plan.add(
        PathBuf::from("tags/index.html"),
        Output::Bytes(list),
        problems,
    );
    for (slug, entry) in &tags {
        let documents: Vec<Value> = entry
            .documents
            .iter()
            .map(|source| document_value(source))
            .collect();
        let page = render(
            &env,
            "tag.html",
            context! {
                site => site.clone(),
                kame_css => kame_css.clone(),
                tag => context! { name => entry.name.clone(), url => safe(tag_url(slug)) },
                documents => documents,
            },
            problems,
        );
        plan.add(page_path(&tag_url(slug)), Output::Bytes(page), problems);
    }

    // The feed.
    if !config.url.is_empty() {
        let xml = feed::render(config, &sources, &index, site_dir);
        plan.add(
            PathBuf::from("feed.xml"),
            Output::Bytes(xml.into_bytes()),
            problems,
        );
    }

    // Static files. The site's override the theme's on purpose, and either colliding with
    // something generated is an error.
    let mut statics: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for root in [site_dir.join("theme/static"), site_dir.join("static")] {
        let mut found = Vec::new();
        files_under(&root, &mut found);
        for file in found {
            if let Ok(relative) = file.strip_prefix(&root) {
                statics.insert(relative.to_path_buf(), file.clone());
            }
        }
    }
    for (path, file) in statics {
        plan.add(path, Output::Copy(file), problems);
    }

    problems.errors.is_empty().then_some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::starter::new_site;
    use crate::testing::{metadata, paragraph, temp_dir, write_kame};
    use kame_core::model::{Document, Metadata};

    struct Site {
        dir: PathBuf,
    }

    impl Site {
        fn new(name: &str) -> Site {
            let parent = temp_dir(name);
            let dir = parent.join("site");
            new_site(&dir).unwrap();
            Site { dir }
        }

        fn add(&self, file: &str, meta: &Metadata, text: &str) -> PathBuf {
            let path = self.dir.join("docs").join(file);
            let mut doc = Document::new("D");
            doc.blocks = vec![paragraph(text)];
            write_kame(&path, &doc, Some(meta));
            path
        }

        fn config(&self, publish: &[(&str, &Path)], drafts: &[(&str, &Path)]) {
            let list = |entries: &[(&str, &Path)]| -> String {
                entries
                    .iter()
                    .map(|(slug, path)| format!("    {slug}: {}\n", path.display()))
                    .collect()
            };
            let yaml = format!(
                "site:\n  name: Example\n  url: https://e.com\n  publish:\n{}  drafts:\n{}",
                list(publish),
                list(drafts)
            );
            let yaml = yaml.replace("publish:\n  drafts", "publish: {}\n  drafts");
            let yaml = if drafts.is_empty() {
                yaml.replace("  drafts:\n", "")
            } else {
                yaml
            };
            std::fs::write(self.dir.join("site.yaml"), yaml).unwrap();
        }

        fn plan(&self) -> Plan {
            let mut problems = Problems::default();
            let plan = build_plan(&self.dir, true, &mut problems);
            assert!(problems.errors.is_empty(), "{:?}", problems.errors);
            plan.unwrap()
        }
    }

    fn text(plan: &Plan, path: &str) -> String {
        match plan.files.get(Path::new(path)) {
            Some(Output::Bytes(bytes)) => String::from_utf8(bytes.clone()).unwrap(),
            other => panic!(
                "{path}: {other:?}, among {:?}",
                plan.files.keys().collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn a_new_site_works_out_with_nothing_published() {
        let site = Site::new("planfresh");
        let plan = site.plan();
        for path in [
            "index.html",
            "archive/index.html",
            "tags/index.html",
            "about/index.html",
            "feed.xml",
            "css/fonts.css",
        ] {
            assert!(plan.files.contains_key(Path::new(path)), "{path}");
        }
        assert!(text(&plan, "index.html").contains("A paragraph about this site."));
        assert!(text(&plan, "index.html").contains("<a href=\"/about/\">About</a>"));
    }

    #[test]
    fn a_document_gets_a_page_with_its_body() {
        let site = Site::new("plandoc");
        let a = site.add(
            "a.kame",
            &metadata("The Title", "2026-11-02", &[]),
            "Its words.",
        );
        site.config(&[("a", &a)], &[]);
        let page = text(&site.plan(), "k/a/index.html");
        assert!(page.contains("<p>Its words.</p>"), "{page}");
        assert!(page.contains("The Title"), "{page}");
        assert!(page.contains("2026-11-02"), "{page}");
        assert!(!page.contains("noindex"), "{page}");
    }

    #[test]
    fn the_home_page_keeps_to_the_latest_five() {
        let site = Site::new("planhome");
        let mut publish = Vec::new();
        let paths: Vec<(String, PathBuf)> = (1..=6)
            .map(|day| {
                let slug = format!("d{day}");
                let path = site.add(
                    &format!("{slug}.kame"),
                    &metadata(&slug, &format!("2026-01-0{day}"), &[]),
                    "x",
                );
                (slug, path)
            })
            .collect();
        for (slug, path) in &paths {
            publish.push((slug.as_str(), path.as_path()));
        }
        site.config(&publish, &[]);
        let plan = site.plan();
        let home = text(&plan, "index.html");
        assert!(home.contains("d6") && home.contains("d2"), "{home}");
        assert!(!home.contains(">d1<"), "{home}");
        assert!(text(&plan, "archive/index.html").contains(">d1<"));
        assert!(
            home.contains("href=\"/k/d6/\""),
            "URLs aren't escaped: {home}"
        );
    }

    #[test]
    fn tags_get_a_list_and_a_page_each() {
        let site = Site::new("plantags");
        let a = site.add(
            "a.kame",
            &metadata("Doc A", "2026-01-02", &["Machine Learning", "rust"]),
            "x",
        );
        let b = site.add("b.kame", &metadata("Doc B", "2026-01-01", &["Rust"]), "x");
        site.config(&[("a", &a), ("b", &b)], &[]);
        let plan = site.plan();
        let tags = text(&plan, "tags/index.html");
        assert!(tags.contains("/tags/machine-learning/"), "{tags}");
        assert!(
            tags.contains("(2)"),
            "two documents share the rust tag: {tags}"
        );
        let rust = text(&plan, "tags/rust/index.html");
        assert!(rust.contains("Doc A") && rust.contains("Doc B"), "{rust}");
        assert!(!text(&plan, "tags/machine-learning/index.html").contains("Doc B"));
    }

    #[test]
    fn a_draft_is_reachable_and_in_no_listing() {
        let site = Site::new("plandraft");
        let a = site.add("a.kame", &metadata("Public", "2026-01-01", &["t"]), "x");
        let d = site.add(
            "d.kame",
            &metadata("Secret", "2030-01-01", &["t", "secret-tag"]),
            "x",
        );
        site.config(&[("a", &a)], &[("d", &d)]);
        let plan = site.plan();
        let page = text(&plan, "drafts/d/index.html");
        assert!(page.contains("noindex"), "{page}");
        for listing in [
            "index.html",
            "archive/index.html",
            "tags/index.html",
            "tags/t/index.html",
            "feed.xml",
        ] {
            assert!(!text(&plan, listing).contains("Secret"), "{listing}");
        }
        assert!(!plan
            .files
            .contains_key(Path::new("tags/secret-tag/index.html")));
    }

    #[test]
    fn no_drafts_leaves_them_out_altogether() {
        let site = Site::new("plannodraft");
        let d = site.add("d.kame", &metadata("Secret", "2030-01-01", &[]), "x");
        site.config(&[], &[("d", &d)]);
        let mut problems = Problems::default();
        let plan = build_plan(&site.dir, false, &mut problems).unwrap();
        assert!(!plan.files.keys().any(|path| path.starts_with("drafts")));
    }

    #[test]
    fn an_image_is_copied_beside_its_page_and_linked_relatively() {
        let site = Site::new("planimage");
        std::fs::create_dir_all(site.dir.join("docs")).unwrap();
        std::fs::write(site.dir.join("docs/a.png"), "x").unwrap();
        let path = site.dir.join("docs/a.kame");
        let mut doc = Document::new("D");
        doc.blocks = vec![crate::testing::block(
            kame_core::model::BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
            },
            "",
        )];
        write_kame(&path, &doc, Some(&metadata("A", "2026-01-01", &[])));
        site.config(&[("a", &path)], &[]);
        let plan = site.plan();
        assert!(text(&plan, "k/a/index.html").contains("src=\"a.png\""));
        assert!(matches!(
            plan.files.get(Path::new("k/a/a.png")),
            Some(Output::Copy(_))
        ));
    }

    #[test]
    fn a_static_file_overrides_the_themes_own() {
        let site = Site::new("planstatic");
        std::fs::create_dir_all(site.dir.join("static/css")).unwrap();
        std::fs::write(site.dir.join("static/css/fonts.css"), "mine").unwrap();
        let plan = site.plan();
        assert_eq!(
            plan.files.get(Path::new("css/fonts.css")),
            Some(&Output::Copy(site.dir.join("static/css/fonts.css")))
        );
    }

    #[test]
    fn a_collision_is_an_error_naming_the_path() {
        let site = Site::new("plancollide");
        std::fs::create_dir_all(site.dir.join("pages/k")).unwrap();
        std::fs::write(site.dir.join("pages/k/a.md"), "x").unwrap();
        let a = site.add("a.kame", &metadata("A", "2026-01-01", &[]), "x");
        site.config(&[("a", &a)], &[]);
        let mut problems = Problems::default();
        assert!(build_plan(&site.dir, true, &mut problems).is_none());
        assert!(
            problems.errors.iter().any(|e| e.contains("k/a/index.html")),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn a_static_file_colliding_with_a_page_is_an_error() {
        let site = Site::new("planstaticcollide");
        std::fs::create_dir_all(site.dir.join("static/about")).unwrap();
        std::fs::write(site.dir.join("static/about/index.html"), "x").unwrap();
        let mut problems = Problems::default();
        assert!(build_plan(&site.dir, true, &mut problems).is_none());
        assert!(problems
            .errors
            .iter()
            .any(|e| e.contains("about/index.html")));
    }

    #[test]
    fn two_tags_reducing_to_one_with_different_names_are_an_error() {
        let site = Site::new("plantagclash");
        let a = site.add("a.kame", &metadata("A", "2026-01-01", &["C++", "c"]), "x");
        site.config(&[("a", &a)], &[]);
        let mut problems = Problems::default();
        assert!(build_plan(&site.dir, true, &mut problems).is_none());
        assert!(problems.errors[0].contains("C++"), "{:?}", problems.errors);
    }

    #[test]
    fn a_missing_template_is_an_error_naming_it() {
        let site = Site::new("plantemplate");
        std::fs::remove_file(site.dir.join("theme/templates/tag.html")).unwrap();
        let mut problems = Problems::default();
        assert!(build_plan(&site.dir, true, &mut problems).is_none());
        assert!(
            problems.errors.iter().any(|e| e.contains("tag.html")),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn every_problem_is_reported_together() {
        let site = Site::new("planall");
        site.config(
            &[
                ("a", Path::new("/no/a.kame")),
                ("b", Path::new("/no/b.kame")),
            ],
            &[],
        );
        let mut problems = Problems::default();
        assert!(build_plan(&site.dir, true, &mut problems).is_none());
        assert_eq!(problems.errors.len(), 2, "{:?}", problems.errors);
    }

    #[test]
    fn an_edited_template_is_what_publishes() {
        let site = Site::new("planedit");
        let path = site.dir.join("theme/templates/index.html");
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("A paragraph about this site.", "My own words.");
        std::fs::write(&path, edited).unwrap();
        assert!(text(&site.plan(), "index.html").contains("My own words."));
    }

    #[test]
    fn a_draft_shows_no_tags() {
        let site = Site::new("plandrafttag");
        let a = site.add(
            "a.kame",
            &metadata("Public", "2026-01-01", &["shared"]),
            "x",
        );
        let d = site.add(
            "d.kame",
            &metadata("Secret", "2030-01-01", &["shared", "only-here"]),
            "x",
        );
        site.config(&[("a", &a)], &[("d", &d)]);
        let plan = site.plan();
        let draft = text(&plan, "drafts/d/index.html");
        assert!(!draft.contains("only-here"), "{draft}");
        assert!(!draft.contains("shared"), "{draft}");
        assert!(text(&plan, "k/a/index.html").contains("/tags/shared/"));
    }

    #[test]
    fn drafts_dont_count_towards_tags() {
        let site = Site::new("plandraftcount");
        let a = site.add(
            "a.kame",
            &metadata("Public", "2026-01-01", &["shared"]),
            "x",
        );
        let d = site.add(
            "d.kame",
            &metadata("Secret", "2030-01-01", &["shared", "only-here"]),
            "x",
        );
        site.config(&[("a", &a)], &[("d", &d)]);
        let plan = site.plan();
        let tags = text(&plan, "tags/index.html");
        assert!(tags.contains("shared</a> (1)"), "{tags}");
        assert!(!tags.contains("only-here"), "{tags}");
        let shared = text(&plan, "tags/shared/index.html");
        assert!(
            shared.contains("Public") && !shared.contains("Secret"),
            "{shared}"
        );
        assert!(!plan
            .files
            .contains_key(Path::new("tags/only-here/index.html")));
    }

    #[test]
    fn a_page_has_the_theme_toggle() {
        let site = Site::new("plantoggle");
        let plan = site.plan();
        let home = text(&plan, "index.html");
        assert!(home.contains("src=\"/js/theme.js\""), "{home}");
        assert!(home.contains("id=\"theme-toggle\""), "{home}");
        assert!(plan.files.contains_key(Path::new("js/theme.js")));
    }

    #[test]
    fn a_page_and_its_feed_item_name_the_sites_author_if_it_has_none() {
        let site = Site::new("planauthor");
        let mut meta = metadata("A", "2026-01-01", &[]);
        meta.author = String::new();
        let a = site.add("a.kame", &meta, "x");
        site.config(&[("a", &a)], &[]);
        let yaml = std::fs::read_to_string(site.dir.join("site.yaml")).unwrap();
        std::fs::write(
            site.dir.join("site.yaml"),
            yaml.replace("  url:", "  author: The Site\n  url:"),
        )
        .unwrap();
        let plan = site.plan();
        assert!(text(&plan, "k/a/index.html").contains("content=\"The Site\""));
        assert!(text(&plan, "feed.xml").contains("<dc:creator>The Site</dc:creator>"));
    }

    #[test]
    fn a_document_page_shows_its_author() {
        let site = Site::new("planbyline");
        let a = site.add("a.kame", &metadata("A", "2026-01-01", &[]), "x");
        site.config(&[("a", &a)], &[]);
        let page = text(&site.plan(), "k/a/index.html");
        let meta = &page[page.find("<p class=\"meta\">").unwrap()..];
        assert!(
            meta[..meta.find("</p>").unwrap()].contains("K. Isom"),
            "{page}"
        );
    }

    #[test]
    fn a_draft_shows_its_author() {
        let site = Site::new("plandraftauthor");
        let d = site.add("d.kame", &metadata("D", "2026-01-01", &[]), "x");
        site.config(&[], &[("d", &d)]);
        let page = text(&site.plan(), "drafts/d/index.html");
        let meta = &page[page.find("<p class=\"meta\">").unwrap()..];
        assert!(
            meta[..meta.find("</p>").unwrap()].contains("K. Isom"),
            "{page}"
        );
    }

    #[test]
    fn a_draft_with_no_author_shows_the_sites() {
        let site = Site::new("plandraftsite");
        let mut meta = metadata("D", "2026-01-01", &[]);
        meta.author = String::new();
        let d = site.add("d.kame", &meta, "x");
        site.config(&[], &[("d", &d)]);
        let yaml = std::fs::read_to_string(site.dir.join("site.yaml")).unwrap();
        std::fs::write(
            site.dir.join("site.yaml"),
            yaml.replace("  url:", "  author: The Site\n  url:"),
        )
        .unwrap();
        let page = text(&site.plan(), "drafts/d/index.html");
        let meta = &page[page.find("<p class=\"meta\">").unwrap()..];
        assert!(
            meta[..meta.find("</p>").unwrap()].contains("The Site"),
            "{page}"
        );
    }
}
