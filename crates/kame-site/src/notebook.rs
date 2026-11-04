//! Building a notebook as its own site. Only documents marked to publish are read for the site's
//! content, URLs follow the folders, and everything under `static/` is public. All of the rendering
//! is what any other site uses. See section 9 of
//! `docs/superpowers/specs/2026-11-03-notebook-design.md`.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use kame_core::model::{BlockKind, SpanStyle};
use kame_notebook::links::Follow;
use kame_notebook::{read_document, Notebook, INDEX};

use crate::config::Config;
use crate::plan::{plan_site, Output, Plan};
use crate::source::{from_parts, Source};
use crate::starter::FILES;
use crate::Problems;

/// Whether `dir` is a notebook rather than a `site.yaml` site.
pub fn is_notebook(dir: &Path) -> bool {
    dir.join(kame_notebook::SETTINGS).is_file() && !dir.join("site.yaml").is_file()
}

/// `text` as it can appear in a URL's path, keeping its slashes.
fn url_path(text: &str) -> String {
    text.split('/')
        .map(crate::refs::percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a document publishes: its folders and name, with an index as its folder's page.
fn document_url(relative: &str) -> String {
    let path = relative
        .strip_suffix(INDEX)
        .map(|folder| folder.to_string())
        .unwrap_or_else(|| {
            let stem = relative.rsplit_once('.').map_or(relative, |(stem, _)| stem);
            format!("{stem}/")
        });
    format!("/{}", url_path(&path))
}

/// The path from the notebook's root.
fn relative_to(notebook: &Notebook, path: &Path) -> Option<String> {
    let inside = path.strip_prefix(&notebook.root).ok()?;
    Some(
        inside
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Where a link in the document at `from` leads on the site, or `None` to publish it as its text.
/// A file that isn't there publishes as written, for the web server to answer.
fn published_link(
    notebook: &Notebook,
    published: &[PathBuf],
    target: &str,
    from: &Path,
    slug: &str,
    problems: &mut Problems,
) -> Option<String> {
    let (path, _) = kame_core::links::split_document_target(target);
    let as_written = |notebook: &Notebook| {
        let named = kame_core::links::resolve_open_path(path, from);
        let parts: Vec<String> = named
            .components()
            .fold(PathBuf::new(), |mut tidy, part| {
                match part {
                    std::path::Component::ParentDir => {
                        tidy.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => tidy.push(other),
                }
                tidy
            })
            .strip_prefix(&notebook.root)
            .ok()?
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let trailing = if path.ends_with('/') { "/" } else { "" };
        Some(format!("/{}{trailing}", url_path(&parts.join("/"))))
    };
    match notebook.follow(target, from) {
        Follow::Open(file) => {
            let file = file.canonicalize().unwrap_or(file);
            if published.contains(&file) {
                return relative_to(notebook, &file).map(|relative| document_url(&relative));
            }
            problems.warnings.push(format!(
                "\"{slug}\": the link to {target} publishes as its text: that document isn't marked to publish"
            ));
            None
        }
        Follow::View(file) => {
            let relative = relative_to(notebook, &file)?;
            if relative.starts_with("static/") {
                return Some(format!("/{}", url_path(&relative)));
            }
            problems.warnings.push(format!(
                "\"{slug}\": the link to {target} publishes as its text: only static/ is public"
            ));
            None
        }
        Follow::Create(file) => {
            // A folder with no index simply has no page, and that's for the web server to say.
            if file.ends_with(INDEX) {
                let folder = relative_to(notebook, file.parent()?)?;
                return Some(format!("/{}/", url_path(&folder)));
            }
            problems.warnings.push(format!(
                "\"{slug}\": the link to {target} publishes as its text: there is no document there"
            ));
            None
        }
        Follow::Rotted => {
            let url = as_written(notebook);
            if url.is_some() {
                problems.warnings.push(format!(
                    "\"{slug}\": nothing is at {target}, so its link is left to the web server"
                ));
            } else {
                problems.warnings.push(format!(
                    "\"{slug}\": the link to {target} publishes as its text: it leads outside the notebook"
                ));
            }
            url
        }
    }
}

/// Puts a document's relative links in terms of the site, since a page is further from everything
/// than its document is. Web addresses stay as they are, and a link that can't lead anywhere public
/// is left as its words.
fn rewrite_links(
    notebook: &Notebook,
    published: &[PathBuf],
    source: &mut Source,
    problems: &mut Problems,
) {
    let (from, slug) = (source.path.clone(), source.slug.clone());
    for block in &mut source.doc.blocks {
        let mut spans = std::mem::take(&mut block.spans);
        spans.retain_mut(|span| {
            let target = match &span.style {
                SpanStyle::DocumentLink { target } => target.clone(),
                SpanStyle::Link { url } if !url.contains(':') && !url.starts_with('#') => {
                    url.clone()
                }
                _ => return true,
            };
            if target.starts_with('#') {
                return false;
            }
            match published_link(notebook, published, &target, &from, &slug, problems) {
                Some(url) => {
                    span.style = SpanStyle::Link { url };
                    true
                }
                None => false,
            }
        });
        block.spans = spans;
        // An image's link goes the same way as any other.
        if let BlockKind::Image { link, .. } = &mut block.kind {
            if let Some(url) = link.clone().filter(|url| !url.contains(':')) {
                *link = published_link(notebook, published, &url, &from, &slug, problems);
            }
        }
    }
}

/// The documents that publish: the contents page always, and anything marked.
fn read_sources(notebook: &Notebook, problems: &mut Problems) -> Vec<Source> {
    let home = notebook.root.join(INDEX);
    let mut sources: Vec<Source> = Vec::new();
    for entry in notebook.documents() {
        let Some((doc, metadata)) = read_document(&entry.path) else {
            // An unreadable document that isn't marked can't be told from one that is.
            problems.warnings.push(format!(
                "could not read {}, so it isn't published",
                entry.relative
            ));
            continue;
        };
        if !metadata.publish && entry.path != home {
            continue;
        }
        if let Some(mut source) =
            from_parts(&entry.relative, &entry.path, false, doc, metadata, problems)
        {
            source.page = Some(document_url(&entry.relative));
            sources.push(source);
        }
    }
    sources.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.slug.cmp(&b.slug)));
    sources
}

/// Everything under `from`, as paths relative to it.
fn files_under(from: &Path, inside: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(from.join(inside)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        // Syncthing leaves conflict copies in any folder, and what's hidden is how something works.
        if text.starts_with('.') || text.contains(".sync-conflict-") {
            continue;
        }
        let relative = inside.join(&name);
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files_under(from, &relative, found),
            Ok(kind) if kind.is_file() => found.push(relative),
            _ => {}
        }
    }
}

/// Lays out the theme a notebook's build reads: the standard one, with whatever the notebook's
/// `theme/` folder holds taking its place file by file.
fn lay_out_theme(notebook: &Notebook, site_dir: &Path) -> std::io::Result<()> {
    let theme = site_dir.join("theme");
    if theme.exists() {
        std::fs::remove_dir_all(&theme)?;
    }
    for (path, contents) in FILES {
        if path.starts_with("theme/") {
            let target = site_dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap_or(site_dir))?;
            std::fs::write(target, contents)?;
        }
    }
    let mut own = Vec::new();
    files_under(&notebook.root.join("theme"), Path::new(""), &mut own);
    for relative in own {
        let target = theme.join(&relative);
        std::fs::create_dir_all(target.parent().unwrap_or(&theme))?;
        std::fs::copy(notebook.root.join("theme").join(&relative), target)?;
    }
    Ok(())
}

/// Works out a notebook's site, with nothing written but its theme, in `site_dir`.
pub fn plan_notebook(
    notebook: &Notebook,
    site_dir: &Path,
    problems: &mut Problems,
) -> Option<Plan> {
    if let Err(err) =
        std::fs::create_dir_all(site_dir).and_then(|()| lay_out_theme(notebook, site_dir))
    {
        problems
            .errors
            .push(format!("could not lay out the theme: {err}"));
        return None;
    }
    let mut sources = read_sources(notebook, problems);
    let published: Vec<PathBuf> = sources
        .iter()
        .filter_map(|source| source.path.canonicalize().ok())
        .collect();
    for source in &mut sources {
        rewrite_links(notebook, &published, source, problems);
    }
    let config = Config {
        name: notebook.title(),
        url: notebook
            .settings
            .url
            .clone()
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string(),
        description: String::new(),
        author: notebook.settings.author.clone().unwrap_or_default(),
        publish: Vec::new(),
        drafts: Vec::new(),
    };
    for source in &mut sources {
        if source.author.is_empty() {
            source.author = config.author.clone();
        }
    }
    let mut plan = plan_site(site_dir, &config, sources, problems)?;
    let mut statics = Vec::new();
    files_under(&notebook.root.join("static"), Path::new(""), &mut statics);
    for relative in statics {
        plan.add(
            Path::new("static").join(&relative),
            Output::Copy(notebook.root.join("static").join(&relative)),
            problems,
        );
    }
    problems.errors.is_empty().then_some(plan)
}

/// Where a notebook's site is built: under the user's cache folder, so that a sync tool never
/// carries it, and named for the notebook's path, so that notebooks don't share one.
pub fn site_dir(notebook: &Notebook) -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|found| !found.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Caches")
            } else {
                home.join(".cache")
            })
        })?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    notebook.root.hash(&mut hasher);
    let name = notebook
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(
        cache
            .join("kame-site")
            .join(format!("{name}-{:016x}", hasher.finish())),
    )
}

/// Builds the notebook at `dir`, returning where the site went, if it did.
pub fn build(dir: &Path, problems: &mut Problems) -> Option<PathBuf> {
    let notebook = match Notebook::open(dir) {
        Ok(notebook) => notebook,
        Err(err) => {
            problems.errors.push(err);
            return None;
        }
    };
    let Some(site_dir) = site_dir(&notebook) else {
        problems
            .errors
            .push("could not find a cache folder to build in".to_string());
        return None;
    };
    let plan = plan_notebook(&notebook, &site_dir, problems)?;
    if let Err(err) = crate::write::write_site(&plan, &site_dir) {
        problems
            .errors
            .push(format!("could not write the site: {err}"));
        return None;
    }
    Some(site_dir.join("output"))
}

/// Builds the notebook at `dir` and sends it to its `remote`, as `make sync` does for a site.
pub fn sync(dir: &Path, problems: &mut Problems) -> Option<PathBuf> {
    let remote = match Notebook::open(dir) {
        Ok(notebook) => notebook.settings.remote,
        Err(err) => {
            problems.errors.push(err);
            return None;
        }
    };
    let Some(remote) = remote.filter(|remote| !remote.trim().is_empty()) else {
        problems
            .errors
            .push("notebook.yaml has no remote to send this to".to_string());
        return None;
    };
    let output = build(dir, problems)?;
    // The trailing slash means what's in the folder, rather than the folder.
    let status = std::process::Command::new("rsync")
        .args(["-avz", "--delete"])
        .arg(format!("{}/", output.display()))
        .arg(&remote)
        .status();
    match status {
        Ok(status) if status.success() => Some(output),
        Ok(status) => {
            problems.errors.push(format!("rsync failed: {status}"));
            None
        }
        Err(err) => {
            problems.errors.push(format!("could not run rsync: {err}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{paragraph, temp_dir, write_kame};
    use kame_core::model::{Block, Document, Metadata, Span};

    struct Made {
        root: PathBuf,
        build: PathBuf,
    }

    fn made(name: &str, yaml: &str) -> Made {
        let dir = temp_dir(name);
        let root = dir.join("nb");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notebook.yaml"), yaml).unwrap();
        Made {
            root: root.canonicalize().unwrap(),
            build: dir.join("build"),
        }
    }

    fn marked(publish: bool) -> Metadata {
        Metadata {
            created: "2026-11-03".to_string(),
            publish,
            ..Metadata::default()
        }
    }

    impl Made {
        fn document(&self, relative: &str, publish: bool, doc: &Document) {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_kame(&path, doc, Some(&marked(publish)));
        }

        fn words(&self, relative: &str, publish: bool, words: &str) {
            let mut doc = Document::new("D");
            doc.blocks = vec![paragraph(words)];
            self.document(relative, publish, &doc);
        }

        fn file(&self, relative: &str, contents: &str) {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn plan(&self) -> (Plan, Problems) {
            let notebook = Notebook::open(&self.root).unwrap();
            let mut problems = Problems::default();
            let plan = plan_notebook(&notebook, &self.build, &mut problems);
            assert!(problems.errors.is_empty(), "{:?}", problems.errors);
            (plan.unwrap(), problems)
        }
    }

    fn page(plan: &Plan, path: &str) -> String {
        match plan.files.get(Path::new(path)) {
            Some(Output::Bytes(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
            Some(Output::Copy(from)) => std::fs::read_to_string(from).unwrap_or_default(),
            None => panic!(
                "nothing at {path}: {:?}",
                plan.files.keys().collect::<Vec<_>>()
            ),
        }
    }

    fn linking(words: &str, style: SpanStyle) -> Document {
        let mut doc = Document::new("D");
        let mut block: Block = paragraph(words);
        block.spans = vec![Span {
            start: 0,
            end: words.len(),
            style,
        }];
        doc.blocks = vec![block];
        doc
    }

    #[test]
    fn urls_follow_the_folders_and_an_index_is_its_folders_page() {
        assert_eq!(document_url("Notebook.kame"), "/");
        assert_eq!(document_url("maritain/Notebook.kame"), "/maritain/");
        assert_eq!(
            document_url("maritain/materialism.kame"),
            "/maritain/materialism/"
        );
        assert_eq!(document_url("about.md"), "/about/");
        assert_eq!(document_url("a b/c d.kame"), "/a%20b/c%20d/");
    }

    #[test]
    fn only_marked_documents_and_the_contents_page_publish() {
        let nb = made("nbmarked", "title: Philosophy\n");
        nb.words("Notebook.kame", false, "The contents.");
        nb.words("maritain/materialism.kame", true, "Marked words.");
        nb.words("maritain/Notebook.kame", true, "The folder's index.");
        nb.words("private/research.kame", false, "Unmarked secret.");
        nb.words("maritain/private.kame", false, "Another secret.");
        let (plan, _) = nb.plan();
        assert!(page(&plan, "index.html").contains("The contents."));
        assert!(page(&plan, "maritain/materialism/index.html").contains("Marked words."));
        assert!(
            page(&plan, "maritain/index.html").contains("The folder&#39;s index.")
                || page(&plan, "maritain/index.html").contains("The folder's index.")
        );
        for path in plan.files.keys() {
            if path
                .extension()
                .is_some_and(|ext| ext == "html" || ext == "xml")
            {
                let text = page(&plan, &path.display().to_string());
                assert!(!text.contains("secret"), "{} leaks", path.display());
            }
        }
        assert!(!plan
            .files
            .contains_key(Path::new("private/research/index.html")));
    }

    #[test]
    fn with_no_contents_page_the_home_page_is_the_usual_listing() {
        let nb = made("nbhome", "title: Philosophy\n");
        nb.words("a.kame", true, "Words.");
        let (plan, _) = nb.plan();
        assert!(page(&plan, "index.html").contains("/a/"));
    }

    #[test]
    fn all_of_static_is_public_but_nothing_else_is() {
        let nb = made("nbstatic", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("static/paper.pdf", "a paper");
        nb.file("static/deep/image.png", "an image");
        nb.file(
            "static/paper.sync-conflict-20260101-120000-ABC.pdf",
            "conflict",
        );
        nb.file("static/.hidden", "hidden");
        nb.file("papers/private.pdf", "private");
        let (plan, _) = nb.plan();
        assert_eq!(page(&plan, "static/paper.pdf"), "a paper");
        assert_eq!(page(&plan, "static/deep/image.png"), "an image");
        let paths: Vec<String> = plan.files.keys().map(|p| p.display().to_string()).collect();
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("conflict") || p.contains("hidden")),
            "{paths:?}"
        );
        assert!(!paths.iter().any(|p| p.contains("private")), "{paths:?}");
    }

    #[test]
    fn links_are_put_in_terms_of_the_site() {
        let nb = made("nblinks", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("deep/open.kame", true, "Open.");
        nb.words("deep/shut.kame", false, "Shut.");
        nb.file("static/paper.pdf", "a paper");
        nb.file("papers/private.pdf", "private");
        let cases = [
            (
                "to-open",
                SpanStyle::DocumentLink {
                    target: "open".to_string(),
                },
                "href=\"/deep/open/\"",
            ),
            (
                "to-paper",
                SpanStyle::Link {
                    url: "../static/paper.pdf".to_string(),
                },
                "href=\"/static/paper.pdf\"",
            ),
            (
                "to-gone",
                SpanStyle::Link {
                    url: "../static/gone.pdf".to_string(),
                },
                "href=\"/static/gone.pdf\"",
            ),
            (
                "to-folder",
                SpanStyle::Link {
                    url: "../other/".to_string(),
                },
                "href=\"/other/\"",
            ),
            (
                "to-web",
                SpanStyle::Link {
                    url: "https://example.com/x".to_string(),
                },
                "href=\"https://example.com/x\"",
            ),
        ];
        for (name, style, _) in &cases {
            nb.document(
                &format!("deep/{name}.kame"),
                true,
                &linking("The words.", style.clone()),
            );
        }
        nb.document(
            "deep/to-shut.kame",
            true,
            &linking(
                "The words.",
                SpanStyle::DocumentLink {
                    target: "shut".to_string(),
                },
            ),
        );
        nb.document(
            "deep/to-private.kame",
            true,
            &linking(
                "The words.",
                SpanStyle::Link {
                    url: "../papers/private.pdf".to_string(),
                },
            ),
        );
        nb.document(
            "deep/to-outside.kame",
            true,
            &linking(
                "The words.",
                SpanStyle::Link {
                    url: "../../../outside.pdf".to_string(),
                },
            ),
        );
        let (plan, problems) = nb.plan();
        for (name, _, want) in &cases {
            let text = page(&plan, &format!("deep/{name}/index.html"));
            assert!(text.contains(want), "{name} should have {want}: {text}");
        }
        for name in ["to-shut", "to-private", "to-outside"] {
            let text = page(&plan, &format!("deep/{name}/index.html"));
            assert!(text.contains("The words."), "{name}");
            assert!(!text.contains("<a href=\"/deep/shut"), "{name}");
            assert!(!text.contains("private.pdf"), "{name}");
            assert!(!text.contains("outside.pdf"), "{name}");
        }
        for word in ["shut", "private.pdf", "outside.pdf", "gone.pdf"] {
            assert!(
                problems.warnings.iter().any(|said| said.contains(word)),
                "{word}: {:?}",
                problems.warnings
            );
        }
    }

    #[test]
    fn markdown_publishes_when_its_front_matter_says_so() {
        let nb = made("nbmd", "");
        nb.file(
            "about.md",
            "---\ntitle: About\ndate: 2026-11-03\npublish: true\n---\n\nAbout words.\n",
        );
        nb.file(
            "notes.md",
            "---\ntitle: Notes\ndate: 2026-11-03\n---\n\nsecret notes.\n",
        );
        let (plan, _) = nb.plan();
        assert!(page(&plan, "about/index.html").contains("About words."));
        assert!(!plan.files.contains_key(Path::new("notes/index.html")));
    }

    #[test]
    fn a_notebooks_theme_overrides_the_standard_one_file_by_file() {
        let nb = made("nbtheme", "");
        nb.words("Notebook.kame", false, "Contents.");
        let (plan, _) = nb.plan();
        let standard = page(&plan, "index.html");
        assert!(!standard.contains("MY OWN THEME"));
        nb.file("theme/templates/document.html", "MY OWN THEME {{ body }}");
        let (plan, _) = nb.plan();
        assert!(page(&plan, "index.html").contains("MY OWN THEME"));
        assert!(
            plan.files.contains_key(Path::new("archive/index.html")),
            "the rest is still there"
        );
    }

    #[test]
    fn with_no_url_a_site_still_builds_with_no_feed() {
        let nb = made("nbnourl", "");
        nb.words("Notebook.kame", false, "Contents.");
        let (plan, _) = nb.plan();
        assert!(!plan.files.contains_key(Path::new("feed.xml")));
        let nb = made("nburl", "url: https://philosophy.example.net/\n");
        nb.words("Notebook.kame", false, "Contents.");
        let (plan, _) = nb.plan();
        assert!(page(&plan, "feed.xml").contains("https://philosophy.example.net/"));
    }

    #[test]
    fn a_build_writes_nothing_inside_the_notebook_and_comes_out_the_same_twice() {
        let nb = made("nbdet", "url: https://x.example/\n");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("a.kame", true, "Words.");
        let before: Vec<PathBuf> = {
            let mut found = Vec::new();
            files_under(&nb.root, Path::new(""), &mut found);
            found.sort();
            found
        };
        let (first, _) = nb.plan();
        let (second, _) = nb.plan();
        for path in first.files.keys() {
            let path = path.display().to_string();
            if path.ends_with(".html") || path.ends_with(".xml") {
                assert_eq!(page(&first, &path), page(&second, &path), "{path}");
            }
        }
        let mut after = Vec::new();
        files_under(&nb.root, Path::new(""), &mut after);
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn sending_a_notebook_needs_a_remote() {
        let nb = made("nbremote", "");
        let mut problems = Problems::default();
        assert_eq!(sync(&nb.root, &mut problems), None);
        assert!(
            problems.errors[0].contains("remote"),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn a_notebook_is_told_from_a_site_by_its_files() {
        let nb = made("nbkind", "");
        assert!(is_notebook(&nb.root));
        std::fs::write(nb.root.join("site.yaml"), "").unwrap();
        assert!(!is_notebook(&nb.root));
    }
}
