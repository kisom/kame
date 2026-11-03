//! What `kame-site new` writes. The theme lives in the repository under `theme/` and is
//! embedded here. Only `new` reads this copy: a build uses the site's own `theme/`.

use std::path::Path;

macro_rules! theme_text {
    ($path:literal) => {
        (
            concat!("theme/", $path),
            include_bytes!(concat!("../theme/", $path)) as &[u8],
        )
    };
}

macro_rules! font {
    ($name:literal) => {
        (
            concat!("theme/static/fonts/", $name),
            include_bytes!(concat!("../../kame-typeset/fonts/", $name)) as &[u8],
        )
    };
}

const SITE_YAML: &str = r#"site:
  name: My Site
  url: https://example.com
  description: Things I have been thinking about.
  author: Your Name

  # Each document gets a slug you choose, which is its URL: /k/<slug>/
  # Nothing else can change it, so a published link stays good.
  publish: {}
  #  commanding-heights: ~/data/writing/CommandingHeights.kame

  # Drafts are at /drafts/<slug>/, out of every listing and the feed. Anyone with
  # the URL can read one. `kame-site build --no-drafts` leaves them out altogether.
  drafts: {}
  #  some-essay: ~/data/writing/SomeEssay.kame
"#;

const README: &str = r#"# My Site

Built with kame-site.

    kame-site build
    python3 -m http.server -d output

Then open http://localhost:8000. Edit `theme/templates/index.html` for the home page,
and `site.yaml` for what gets published.
"#;

const ABOUT: &str = "---\ntitle: About\n---\n\nA page about this site.\n";

pub const FILES: &[(&str, &[u8])] = &[
    ("site.yaml", SITE_YAML.as_bytes()),
    ("README.md", README.as_bytes()),
    (".gitignore", b"output/\n"),
    ("pages/about.md", ABOUT.as_bytes()),
    theme_text!("templates/base.html"),
    theme_text!("templates/index.html"),
    theme_text!("templates/archive.html"),
    theme_text!("templates/tags.html"),
    theme_text!("templates/tag.html"),
    theme_text!("templates/document.html"),
    theme_text!("templates/page.html"),
    theme_text!("static/css/fonts.css"),
    theme_text!("static/css/site.css"),
    font!("EBGaramond-Regular.otf"),
    font!("EBGaramond-Bold.otf"),
    font!("EBGaramond-Italic.otf"),
    font!("EBGaramond-OFL.txt"),
    font!("CrimsonPro-Regular.ttf"),
    font!("CrimsonPro-Bold.ttf"),
    font!("CrimsonPro-Italic.ttf"),
    font!("CrimsonPro-OFL.txt"),
    font!("IBMPlexMono-Regular.ttf"),
    font!("IBMPlexMono-Bold.ttf"),
    font!("IBMPlexMono-Italic.ttf"),
    font!("IBMPlexMono-OFL.txt"),
];

const EMPTY_DIRECTORIES: &[&str] = &["static/images", "static/files", "static/css", "static/js"];

/// Writes a new site into `dir`, which can be missing or empty. Anything already there means
/// it stops, so that it can't overwrite a site.
pub fn new_site(dir: &Path) -> Result<(), String> {
    if let Ok(mut entries) = std::fs::read_dir(dir) {
        if entries.next().is_some() {
            return Err(format!("{} already has files in it", dir.display()));
        }
    }
    let write = || -> std::io::Result<()> {
        for directory in EMPTY_DIRECTORIES {
            std::fs::create_dir_all(dir.join(directory))?;
        }
        for (path, contents) in FILES {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, contents)?;
        }
        Ok(())
    };
    write().map_err(|err| format!("could not write {}: {err}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testing::temp_dir;

    #[test]
    fn a_new_site_has_everything() {
        let parent = temp_dir("new");
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        for file in [
            "site.yaml",
            "README.md",
            ".gitignore",
            "pages/about.md",
            "theme/templates/base.html",
            "theme/templates/index.html",
            "theme/templates/archive.html",
            "theme/templates/tags.html",
            "theme/templates/tag.html",
            "theme/templates/document.html",
            "theme/templates/page.html",
            "theme/static/css/fonts.css",
            "theme/static/css/site.css",
            "theme/static/fonts/EBGaramond-Regular.otf",
            "theme/static/fonts/EBGaramond-OFL.txt",
            "theme/static/fonts/CrimsonPro-Regular.ttf",
            "theme/static/fonts/IBMPlexMono-Regular.ttf",
        ] {
            assert!(dir.join(file).is_file(), "{file}");
        }
        for directory in ["static/images", "static/files", "static/css", "static/js"] {
            assert!(dir.join(directory).is_dir(), "{directory}");
        }
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_new_sites_config_reads() {
        let parent = temp_dir("newconfig");
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        let config = Config::load(&dir).unwrap();
        assert!(config.publish.is_empty());
        assert!(config.drafts.is_empty());
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn an_empty_directory_is_fine() {
        let dir = temp_dir("newempty");
        new_site(&dir).unwrap();
        assert!(dir.join("site.yaml").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_with_anything_in_it_is_refused_and_left_alone() {
        let dir = temp_dir("newfull");
        std::fs::write(dir.join("mine.txt"), "keep").unwrap();
        let err = new_site(&dir).unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert!(!dir.join("site.yaml").exists());
        assert_eq!(std::fs::read(dir.join("mine.txt")).unwrap(), b"keep");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_cant_be_written_outside_the_site() {
        for (path, _) in FILES {
            assert!(!path.contains(".."), "{path}");
            assert!(!path.starts_with('/'), "{path}");
        }
    }
}
