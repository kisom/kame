//! Building a notebook as its own site. Only documents marked to publish are read for the site's
//! content, URLs follow the folders, and everything under `static/` is public. All of the rendering
//! is what any other site uses. See section 9 of
//! `docs/superpowers/specs/2026-11-03-notebook-design.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kame_core::model::{BlockKind, SpanStyle};
use kame_notebook::images::IMAGES;
use kame_notebook::links::{is_web_link, Follow};
use kame_notebook::{read_document, Notebook, INDEX};

use crate::config::Config;
use crate::plan::{plan_site, Output, Plan};
use crate::refs::percent_encode;
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
    let path = if relative == INDEX || relative.ends_with(&format!("/{INDEX}")) {
        relative[..relative.len() - INDEX.len()].to_string()
    } else {
        let stem = relative.rsplit_once('.').map_or(relative, |(stem, _)| stem);
        format!("{stem}/")
    };
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

/// Takes out `.` and `..` by what they say, since what's named may not exist.
fn tidied_path(path: &Path) -> PathBuf {
    let mut tidy = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                tidy.pop();
            }
            std::path::Component::CurDir => {}
            other => tidy.push(other),
        }
    }
    tidy
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
        let named = tidied_path(&kame_core::links::resolve_open_path(path, from));
        let parts: Vec<String> = named
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
            // Only what would be public if it were there is left to the web server, since a page
            // naming anything else says that it exists.
            let url = as_written(notebook).filter(|url| url.starts_with("/static/"));
            let named = kame_core::links::resolve_open_path(path, from);
            let hidden = url
                .as_deref()
                .is_some_and(|url| url.split('/').any(|part| part.starts_with('.')));
            match url.filter(|_| !hidden && notebook.holds(&tidied_path(&named))) {
                Some(url) => {
                    problems.warnings.push(format!(
                        "\"{slug}\": nothing is at {target}, so its link is left to the web server"
                    ));
                    Some(url)
                }
                None => {
                    problems.warnings.push(format!(
                        "\"{slug}\": the link to {target} publishes as its text: it doesn't lead anywhere public"
                    ));
                    None
                }
            }
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
    images: &mut BTreeMap<PathBuf, PathBuf>,
    problems: &mut Problems,
) {
    let (from, slug) = (source.path.clone(), source.slug.clone());
    let kept = notebook.root.join(IMAGES);
    let mut placed = Vec::new();
    for block in &mut source.doc.blocks {
        let mut spans = std::mem::take(&mut block.spans);
        spans.retain_mut(|span| {
            let target = match &span.style {
                SpanStyle::DocumentLink { target } => target.clone(),
                SpanStyle::Link { url } if !is_web_link(url) && !url.starts_with('#') => {
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
        // An image in a published document publishes too, so it has to be the notebook's to publish.
        if let BlockKind::Image { path, .. } = &mut block.kind {
            let file = from.parent().unwrap_or(Path::new("")).join(&*path);
            let real = file.canonicalize();
            // One of the notebook's kept images publishes under `static/images/`, because this
            // document shows it, and what's kept stays private until one does.
            let in_kept = real.as_ref().ok().and_then(|real| {
                let inside = real.strip_prefix(&kept).ok()?;
                Some((inside.to_path_buf(), real.clone()))
            });
            if let Some((inside, real)) = in_kept {
                *path = published_image_url(&inside);
                placed.push(path.clone());
                images.insert(inside, real);
            } else if real.is_ok_and(|real| !real.starts_with(&notebook.root)) {
                problems.warnings.push(format!(
                    "\"{slug}\": the image {path} isn't published: it's outside the notebook"
                ));
                block.kind = BlockKind::Paragraph;
            }
        }
        // An image's link goes the same way as any other.
        if let BlockKind::Image { link, .. } = &mut block.kind {
            if let Some(url) = link.clone().filter(|url| !is_web_link(url)) {
                *link = published_link(notebook, published, &url, &from, &slug, problems);
            }
        }
    }
    source.placed_images = placed;
}

/// The URL of an image kept at `inside` under the notebook's `images/`.
fn published_image_url(inside: &Path) -> String {
    let parts = inside
        .components()
        .map(|part| percent_encode(&part.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>()
        .join("/");
    format!("/static/images/{parts}")
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
    let mut images = BTreeMap::new();
    for source in &mut sources {
        rewrite_links(notebook, &published, source, &mut images, problems);
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
    for (inside, file) in images {
        plan.add(
            Path::new("static/images").join(inside),
            Output::Copy(file),
            problems,
        );
    }
    problems.errors.is_empty().then_some(plan)
}

/// A file a build leaves in `output/`, by which the next one knows that folder is its own to
/// replace. It isn't sent anywhere.
const MARKER: &str = ".kame-site-output";
/// How many files a publish can remove from the remote without asking.
const TOO_MANY_DELETIONS: usize = 5;

/// A folder to lay out the theme in while building, out of the way of the notebook, where a folder
/// named `pages` would be taken for the site's own. Its templates get rendered, so in a temporary
/// folder others can write to, it has to be one that nobody could have prepared: it's newly made
/// under a random name, which fails if something is there, and only this user can open it.
fn new_work_dir(parent: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let work = parent.join(format!("kame-site-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new().mode(0o700).create(&work)?;
    Ok(work)
}

/// Builds the notebook at `dir` into its `output/` folder, which is replaced only if all of it
/// succeeds, returning where that is. A folder there that a build didn't make is left alone.
pub fn build(dir: &Path, problems: &mut Problems) -> Option<PathBuf> {
    let notebook = match Notebook::open(dir) {
        Ok(notebook) => notebook,
        Err(err) => {
            problems.errors.push(err);
            return None;
        }
    };
    let output = notebook.root.join("output");
    let is_empty = std::fs::read_dir(&output).is_ok_and(|mut found| found.next().is_none());
    if output.exists() && !is_empty && !output.join(MARKER).exists() {
        problems.errors.push(format!(
            "{} wasn't made by kame-site, so a build won't replace it",
            output.display()
        ));
        return None;
    }
    let work = match new_work_dir(&std::env::temp_dir()) {
        Ok(work) => work,
        Err(err) => {
            problems
                .errors
                .push(format!("could not make a folder to build in: {err}"));
            return None;
        }
    };
    let built = plan_notebook(&notebook, &work, problems).and_then(|mut plan| {
        plan.add(PathBuf::from(MARKER), Output::Bytes(Vec::new()), problems);
        if let Err(err) = crate::write::write_site(&plan, &notebook.root) {
            problems
                .errors
                .push(format!("could not write the site: {err}"));
            return None;
        }
        Some(output)
    });
    let _ = std::fs::remove_dir_all(&work);
    built
}

/// Whether a remote could be read as anything but where to send a site: it has an option at its
/// start or as its host, or space or control characters to split it up.
fn bad_remote(remote: &str) -> bool {
    let host = remote
        .split_once(':')
        .filter(|(before, _)| !before.contains('/'))
        .map_or("", |(before, _)| before);
    let host = host.rsplit('@').next().unwrap_or(host);
    remote.starts_with('-')
        || host.starts_with('-')
        || remote.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The files rsync says it would delete, from what `--itemize-changes` prints.
fn deletions(said: &str) -> Vec<&str> {
    said.lines()
        .filter_map(|line| line.strip_prefix("*deleting"))
        .map(str::trim)
        .collect()
}

/// rsync with what every run of it needs. The source and destination follow a `--`.
fn rsync() -> std::process::Command {
    let mut command = std::process::Command::new("rsync");
    command
        .arg(format!("--exclude={MARKER}"))
        .args(["-az", "--delete"]);
    command
}

/// Builds the notebook at `dir` and sends it to its `remote`, as `make sync` does for a site. A
/// dry run comes first, since `--delete` empties whatever `remote` names of everything else. If
/// that would remove more than a few files they're listed, and nothing happens without `go_ahead`.
pub fn sync(dir: &Path, go_ahead: bool, problems: &mut Problems) -> Option<PathBuf> {
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
    // This file syncs from other machines, and rsync or ssh would obey an option found in it.
    if bad_remote(&remote) {
        problems
            .errors
            .push(format!("notebook.yaml has an odd remote, \"{remote}\""));
        return None;
    }
    let output = build(dir, problems)?;
    let source = format!("{}/", output.display());
    if !go_ahead {
        let dry = rsync()
            .args(["--dry-run", "--itemize-changes", "--"])
            .arg(&source)
            .arg(&remote)
            .output();
        let dry = match dry {
            Ok(dry) if dry.status.success() => dry,
            Ok(dry) => {
                problems.errors.push(format!(
                    "rsync's dry run failed: {}",
                    String::from_utf8_lossy(&dry.stderr).trim()
                ));
                return None;
            }
            Err(err) => {
                problems.errors.push(format!("could not run rsync: {err}"));
                return None;
            }
        };
        let said = String::from_utf8_lossy(&dry.stdout);
        let doomed = deletions(&said);
        if doomed.len() > TOO_MANY_DELETIONS {
            const SHOWN: usize = 20;
            let mut message = format!(
                "this would delete {} files from {remote}, so nothing was sent. If that's right, run sync with --yes:",
                doomed.len()
            );
            for file in doomed.iter().take(SHOWN) {
                message.push_str("\n  ");
                message.push_str(file);
            }
            if doomed.len() > SHOWN {
                message.push_str(&format!("\n  and {} more", doomed.len() - SHOWN));
            }
            problems.errors.push(message);
            return None;
        }
    }
    // The trailing slash means what's in the folder, rather than the folder.
    let status = rsync()
        .args(["-v", "--"])
        .arg(&source)
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

    /// What the navigation's About link leads to, if there is one.
    fn about_link(plan: &Plan) -> Option<String> {
        let home = page(plan, "index.html");
        let (before, _) = home.split_once(">About</a>")?;
        let (_, href) = before.rsplit_once("href=\"")?;
        Some(href.trim_end_matches('"').to_string())
    }

    #[test]
    fn the_about_link_leads_to_an_about_page_named_with_a_capital() {
        let nb = made("nbaboutupper", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("About.kame", true, "About words.");
        let (plan, _) = nb.plan();
        assert!(plan.files.contains_key(Path::new("About/index.html")));
        assert_eq!(about_link(&plan).as_deref(), Some("/About/"));
    }

    #[test]
    fn the_about_link_leads_to_an_about_page_named_in_lowercase() {
        let nb = made("nbaboutlower", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("about.kame", true, "About words.");
        let (plan, _) = nb.plan();
        assert!(plan.files.contains_key(Path::new("about/index.html")));
        assert_eq!(about_link(&plan).as_deref(), Some("/about/"));
    }

    #[test]
    fn the_lowercase_about_page_wins_when_a_notebook_publishes_both() {
        let nb = made("nbaboutboth", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("About.kame", true, "Capital.");
        nb.file(
            "about.md",
            "---\ntitle: About\ndate: 2026-11-03\npublish: true\n---\n\nLower.\n",
        );
        let (plan, _) = nb.plan();
        assert_eq!(about_link(&plan).as_deref(), Some("/about/"));
    }

    #[test]
    fn there_is_no_about_link_without_a_published_about_page() {
        let nb = made("nbaboutnone", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("About.kame", false, "Not marked to publish.");
        nb.words("Elsewhere/About.kame", true, "Not the site's about page.");
        let (plan, _) = nb.plan();
        assert_eq!(about_link(&plan), None);
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
    fn planning_writes_nothing_inside_the_notebook_and_comes_out_the_same_twice() {
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

    fn image(path: &str) -> Document {
        let mut doc = Document::new("D");
        let mut block = paragraph("");
        block.kind = BlockKind::Image {
            path: path.to_string(),
            link: None,
            align: Default::default(),
        };
        doc.blocks = vec![paragraph("Words."), block];
        doc
    }

    #[test]
    fn an_image_from_outside_the_notebook_is_not_published() {
        let nb = made("nbimage", "");
        nb.words("Notebook.kame", false, "Contents.");
        let outside = nb.root.parent().unwrap().join("private.png");
        std::fs::write(&outside, "secret picture").unwrap();
        std::os::unix::fs::symlink(&outside, nb.root.join("linked.png")).unwrap();
        nb.file("papers/figure.png", "a figure");
        nb.document(
            "absolute.kame",
            true,
            &image(&outside.display().to_string()),
        );
        nb.document("climbing.kame", true, &image("../private.png"));
        nb.document("linked.kame", true, &image("linked.png"));
        nb.document("inside.kame", true, &image("papers/figure.png"));
        let (plan, problems) = nb.plan();
        assert_eq!(page(&plan, "inside/figure.png"), "a figure");
        for (path, output) in &plan.files {
            if let Output::Copy(from) = output {
                let text = std::fs::read_to_string(from).unwrap_or_default();
                assert!(!text.contains("secret picture"), "{} leaks", path.display());
            }
        }
        for slug in ["absolute.kame", "climbing.kame", "linked.kame"] {
            assert!(
                problems
                    .warnings
                    .iter()
                    .any(|said| said.contains(slug) && said.contains("outside")),
                "{slug}: {:?}",
                problems.warnings
            );
            let url = document_url(slug);
            let page_text = page(&plan, &format!("{}index.html", &url[1..]));
            assert!(page_text.contains("Words."));
            assert!(!page_text.contains("private.png"), "{slug}");
            assert!(!page_text.contains("linked.png"), "{slug}");
        }
    }

    #[test]
    fn an_image_climbing_to_static_from_a_folder_is_published() {
        let nb = made("nbclimb", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("static/plate.png", "a plate");
        nb.document("maritain/essay.kame", true, &image("../static/plate.png"));
        let (plan, problems) = nb.plan();
        assert_eq!(page(&plan, "maritain/essay/plate.png"), "a plate");
        assert!(
            !problems.warnings.iter().any(|said| said.contains("image")),
            "{:?}",
            problems.warnings
        );
    }

    #[test]
    fn a_kept_image_publishes_only_when_a_published_document_shows_it() {
        let nb = made("nbkept", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("images/maritain/essay/shown here.png", "shown");
        nb.file("images/maritain/essay/never.png", "never shown");
        nb.file("images/quiet/private.png", "a private plate");
        nb.document(
            "maritain/essay.kame",
            true,
            &image("../images/maritain/essay/shown here.png"),
        );
        nb.document(
            "second.kame",
            true,
            &image("images/maritain/essay/shown here.png"),
        );
        nb.document("quiet.kame", false, &image("images/quiet/private.png"));
        nb.words("images/not-a-document.kame", true, "Not listed.");
        let (plan, problems) = nb.plan();
        assert_eq!(
            page(&plan, "static/images/maritain/essay/shown here.png"),
            "shown"
        );
        let page_text = page(&plan, "maritain/essay/index.html");
        assert!(
            page_text.contains("/static/images/maritain/essay/shown%20here.png"),
            "{page_text}"
        );
        for (path, output) in &plan.files {
            let text = match output {
                Output::Copy(from) => std::fs::read_to_string(from).unwrap_or_default(),
                _ => String::new(),
            };
            assert!(!text.contains("private plate"), "{} leaks", path.display());
            assert!(!text.contains("never shown"), "{} leaks", path.display());
        }
        assert!(!plan.files.keys().any(|path| path.starts_with("images")));
        assert!(
            !problems.warnings.iter().any(|said| said.contains("image")),
            "{:?}",
            problems.warnings
        );
    }

    #[test]
    fn a_kept_image_cannot_carry_a_link_to_an_unmarked_document() {
        let nb = made("nbkeptlinked", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.words("private-plans.kame", false, "Private.");
        nb.file("images/plate.png", "a plate");
        let mut doc = image("images/plate.png");
        for block in &mut doc.blocks {
            if let BlockKind::Image { link, .. } = &mut block.kind {
                *link = Some("private-plans.kame".to_string());
            }
        }
        nb.document("essay.kame", true, &doc);
        let (plan, _) = nb.plan();
        let page_text = page(&plan, "essay/index.html");
        assert!(
            page_text.contains("/static/images/plate.png"),
            "{page_text}"
        );
        assert!(!page_text.contains("private-plans"), "{page_text}");
    }

    #[test]
    fn a_kept_image_linking_out_of_the_notebook_is_not_published() {
        let nb = made("nbkeptlink", "");
        nb.words("Notebook.kame", false, "Contents.");
        let outside = nb.root.parent().unwrap().join("kept-private.png");
        std::fs::write(&outside, "secret picture").unwrap();
        nb.file("images/placeholder.png", "");
        std::os::unix::fs::symlink(&outside, nb.root.join("images/linked.png")).unwrap();
        nb.document("essay.kame", true, &image("images/linked.png"));
        let (plan, problems) = nb.plan();
        assert!(!plan.files.keys().any(|path| path.ends_with("linked.png")));
        assert!(problems
            .warnings
            .iter()
            .any(|said| said.contains("outside")));
    }

    #[test]
    fn a_colon_in_a_path_does_not_make_it_a_web_address() {
        let nb = made("nbcolon", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("papers/a:b.pdf", "private");
        nb.file("static/c:d.pdf", "public");
        for (name, url) in [
            ("private", "papers/a:b.pdf"),
            ("public", "static/c:d.pdf"),
            ("web", "//example.com/x"),
            ("mail", "mailto:a@example.com"),
        ] {
            nb.document(
                &format!("{name}.kame"),
                true,
                &linking(
                    "The words.",
                    SpanStyle::Link {
                        url: url.to_string(),
                    },
                ),
            );
        }
        let (plan, _) = nb.plan();
        assert!(
            !page(&plan, "private/index.html").contains("a:b"),
            "the private path stays out"
        );
        assert!(page(&plan, "public/index.html").contains("/static/c%3Ad.pdf"));
        assert!(page(&plan, "web/index.html").contains("//example.com/x"));
        assert!(page(&plan, "mail/index.html").contains("mailto:a@example.com"));
    }

    /// Every page's text, for checking that a name appears on none of them.
    fn everything(plan: &Plan) -> String {
        plan.files
            .iter()
            .filter_map(|(path, output)| match output {
                Output::Bytes(bytes) if !path.ends_with("fonts") => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_missing_file_only_publishes_where_it_would_be_public() {
        let nb = made("nbmissing", "");
        nb.words("Notebook.kame", false, "Contents.");
        std::os::unix::fs::symlink(nb.root.parent().unwrap(), nb.root.join("away")).unwrap();
        std::os::unix::fs::symlink(nb.root.parent().unwrap(), {
            std::fs::create_dir_all(nb.root.join("static")).unwrap();
            nb.root.join("static/away")
        })
        .unwrap();
        for (name, url) in [
            ("a", "papers/secret-plan.pdf"),
            ("b", "away/secret-plan.pdf"),
            ("c", ".hidden/secret-plan.pdf"),
            ("d", "theme/secret-plan.pdf"),
            ("e", "static/away/secret-plan.pdf"),
            ("f", "static/.hidden/secret-plan.pdf"),
            ("g", "Q3:secret-plan.pdf"),
            ("h", "zotero:secret-plan"),
        ] {
            nb.document(
                &format!("{name}.kame"),
                true,
                &linking(
                    "The words.",
                    SpanStyle::Link {
                        url: url.to_string(),
                    },
                ),
            );
        }
        nb.document(
            "kept.kame",
            true,
            &linking(
                "The words.",
                SpanStyle::Link {
                    url: "static/gone.pdf".to_string(),
                },
            ),
        );
        let (plan, problems) = nb.plan();
        let all = everything(&plan);
        assert!(!all.contains("secret-plan"), "a private name leaked");
        assert!(page(&plan, "kept/index.html").contains("href=\"/static/gone.pdf\""));
        for name in [
            "a.kame", "b.kame", "c.kame", "d.kame", "e.kame", "f.kame", "g.kame", "h.kame",
        ] {
            assert!(
                problems
                    .warnings
                    .iter()
                    .any(|said| said.contains(name) && said.contains("as its text")),
                "{name}: {:?}",
                problems.warnings
            );
        }
    }

    #[test]
    fn only_a_remote_that_can_only_be_a_destination_is_used() {
        for remote in [
            "--rsh=x",
            "\" --rsh=x\"",
            "x@-oProxyCommand=y:/p",
            "-oProxyCommand=y:/p",
            "host -o y:/p",
            "\"a\\nb:/p\"a\nb:/p",
        ] {
            assert!(bad_remote(remote.trim_matches('"')), "{remote}");
        }
        for remote in [
            "kyle@host:/srv/philosophy/",
            "host:/srv/x",
            "/srv/local/",
            "../up/",
            "user@host.example.net:site",
        ] {
            assert!(!bad_remote(remote), "{remote}");
        }
    }

    #[test]
    fn only_a_file_named_notebook_kame_is_an_index() {
        assert_eq!(
            document_url("maritain/MyNotebook.kame"),
            "/maritain/MyNotebook/"
        );
        assert_eq!(document_url("MyNotebook.kame"), "/MyNotebook/");
    }

    #[test]
    fn a_remote_can_not_be_taken_for_an_option() {
        let nb = made("nbdash", "remote: --rsh=touch /tmp/kame-pwned\n");
        nb.words("Notebook.kame", false, "Contents.");
        let mut problems = Problems::default();
        assert_eq!(sync(&nb.root, false, &mut problems), None);
        assert!(
            problems.errors[0].contains("remote"),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn publish_alone_is_enough_for_front_matter() {
        let nb = made("nbpublishonly", "");
        nb.file("about.md", "---\npublish: true\n---\n\nAbout words.\n");
        let (plan, _) = nb.plan();
        let text = page(&plan, "about/index.html");
        assert!(text.contains("About words."));
        assert!(!text.contains("publish: true"));
    }

    fn tree(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        files_under(root, Path::new(""), &mut found);
        let mut names: Vec<String> = found
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_folder_a_build_works_in_is_private_and_new() {
        use std::os::unix::fs::PermissionsExt;
        let parent = temp_dir("nbwork");
        let one = new_work_dir(&parent).unwrap();
        let other = new_work_dir(&parent).unwrap();
        assert_ne!(one, other);
        let mode = std::fs::metadata(&one).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn a_build_goes_in_the_notebook_and_is_all_there() {
        let nb = made("nbbuild", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("static/paper.pdf", "a paper");
        let mut problems = Problems::default();
        let output = build(&nb.root, &mut problems).unwrap();
        assert!(problems.errors.is_empty(), "{:?}", problems.errors);
        assert_eq!(output, nb.root.join("output"));
        assert!(output.join("index.html").is_file());
        assert!(output.join("static/paper.pdf").is_file());
        // Nothing else appears in the notebook, and the build leaves no working folders.
        let names: Vec<String> = std::fs::read_dir(&nb.root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        let mut names = names;
        names.sort();
        assert_eq!(
            names,
            ["Notebook.kame", "notebook.yaml", "output", "static"]
        );
        // And it can build again over its own output.
        let mut problems = Problems::default();
        assert!(
            build(&nb.root, &mut problems).is_some(),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn a_build_will_not_replace_an_output_folder_it_did_not_make() {
        let nb = made("nbowned", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("output/mine.txt", "my own");
        let mut problems = Problems::default();
        assert_eq!(build(&nb.root, &mut problems), None);
        assert!(
            problems.errors[0].contains("output"),
            "{:?}",
            problems.errors
        );
        assert_eq!(
            std::fs::read_to_string(nb.root.join("output/mine.txt")).unwrap(),
            "my own"
        );
    }

    #[test]
    fn a_pages_folder_in_a_notebook_is_not_a_sites_pages() {
        let nb = made("nbpages", "");
        nb.words("Notebook.kame", false, "Contents.");
        nb.file("pages/private.md", "# Private\n\nsecret page.\n");
        let mut problems = Problems::default();
        let output = build(&nb.root, &mut problems).unwrap();
        assert!(!output.join("private").exists());
        assert!(!tree(&output).iter().any(|name| name.contains("private")));
    }

    #[test]
    fn deletions_are_read_from_what_rsync_would_do() {
        let said = ">f+++++++++ index.html\n*deleting   old/page.html\ncd+++++++++ new/\n*deleting   gone with space.txt\n";
        assert_eq!(deletions(said), ["old/page.html", "gone with space.txt"]);
        assert!(deletions("").is_empty());
    }

    fn rsync_is_here() -> bool {
        std::process::Command::new("rsync")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn sending_stops_before_deleting_much_unless_told_to_go_ahead() {
        if !rsync_is_here() {
            return;
        }
        let nb = made("nbsend", "");
        let remote = nb.root.parent().unwrap().join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        for at in 0..(TOO_MANY_DELETIONS + 3) {
            std::fs::write(remote.join(format!("precious{at}.txt")), "keep").unwrap();
        }
        std::fs::write(
            nb.root.join("notebook.yaml"),
            format!("remote: {}/\n", remote.display()),
        )
        .unwrap();
        nb.words("Notebook.kame", false, "Contents.");

        let mut problems = Problems::default();
        assert_eq!(sync(&nb.root, false, &mut problems), None);
        assert!(
            problems.errors[0].contains("--yes"),
            "{:?}",
            problems.errors
        );
        assert!(remote.join("precious0.txt").exists(), "nothing was touched");
        assert!(!remote.join("index.html").exists(), "nothing was sent");

        let mut problems = Problems::default();
        assert!(
            sync(&nb.root, true, &mut problems).is_some(),
            "{:?}",
            problems.errors
        );
        assert!(!remote.join("precious0.txt").exists());
        assert!(remote.join("index.html").exists());
        assert!(
            !remote.join(MARKER).exists(),
            "the marker isn't for publishing"
        );

        // An everyday publish, removing a page or two, goes straight through.
        nb.words("a.kame", true, "A page.");
        let mut problems = Problems::default();
        assert!(
            sync(&nb.root, false, &mut problems).is_some(),
            "{:?}",
            problems.errors
        );
        assert!(remote.join("a/index.html").exists());
        std::fs::remove_file(nb.root.join("a.kame")).unwrap();
        let mut problems = Problems::default();
        assert!(
            sync(&nb.root, false, &mut problems).is_some(),
            "{:?}",
            problems.errors
        );
        assert!(!remote.join("a/index.html").exists());
    }

    #[test]
    fn sending_a_notebook_needs_a_remote() {
        let nb = made("nbremote", "");
        let mut problems = Problems::default();
        assert_eq!(sync(&nb.root, false, &mut problems), None);
        assert!(
            problems.errors[0].contains("remote"),
            "{:?}",
            problems.errors
        );
    }

    #[test]
    fn the_command_line_starts_a_notebook_and_says_so() {
        let dir = temp_dir("nbnew");
        let root = dir.join("Fresh");
        let args = ["new-notebook".to_string(), root.display().to_string()];
        assert_eq!(crate::run(&args), 0);
        assert!(root.join("notebook.yaml").is_file());
        assert!(root.join("Notebook.kame").is_file());
        assert_eq!(crate::run(&args), 1, "not twice");
        assert_eq!(crate::run(&["new-notebook".to_string()]), 2);
    }

    #[test]
    fn a_notebook_is_told_from_a_site_by_its_files() {
        let nb = made("nbkind", "");
        assert!(is_notebook(&nb.root));
        std::fs::write(nb.root.join("site.yaml"), "").unwrap();
        assert!(!is_notebook(&nb.root));
    }
}
