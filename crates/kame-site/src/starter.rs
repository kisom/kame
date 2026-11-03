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
    (".gitignore", b"output/\n.output-building/\n.output-old/\n"),
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
    theme_text!("static/js/theme.js"),
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

/// Whether the theme on disk is exactly the embedded one, with nothing added.
fn theme_is_current(theme: &Path) -> bool {
    fn count_files(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    if path.is_dir() {
                        count_files(&path)
                    } else {
                        1
                    }
                })
                .sum()
        })
    }
    let embedded: Vec<_> = theme_files().collect();
    count_files(theme) == embedded.len()
        && embedded.iter().all(|(path, contents)| {
            std::fs::read(theme.join(path)).is_ok_and(|found| found == *contents)
        })
}

/// The embedded theme's files, as paths under `theme/` and their contents.
fn theme_files() -> impl Iterator<Item = (&'static str, &'static [u8])> {
    FILES
        .iter()
        .filter_map(|(path, contents)| Some((path.strip_prefix("theme/")?, *contents)))
}

/// Puts the embedded theme back at `theme/`. A theme that differs is moved aside, never
/// overwritten, since edits to it are the author's. Returns where it went, or `None` if there
/// was nothing to keep, which includes a theme that is already current.
pub fn regen_theme(dir: &Path) -> Result<Option<std::path::PathBuf>, String> {
    if !dir.join("site.yaml").is_file() {
        return Err(format!(
            "{} has no site.yaml, so it isn't a site",
            dir.display()
        ));
    }
    let theme = dir.join("theme");
    if theme_is_current(&theme) {
        return Ok(None);
    }
    let failed = |err: std::io::Error| format!("could not update {}: {err}", theme.display());
    let mut kept = None;
    if theme.exists() {
        let aside = std::iter::once("theme.old".to_string())
            .chain((2..).map(|number| format!("theme.old-{number}")))
            .map(|name| dir.join(name))
            .find(|path| !path.exists())
            .expect("there is always another number");
        std::fs::rename(&theme, &aside).map_err(failed)?;
        kept = Some(aside);
    }
    for (path, contents) in theme_files() {
        let target = theme.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(failed)?;
        }
        std::fs::write(target, contents).map_err(failed)?;
    }
    Ok(kept)
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
            "theme/static/js/theme.js",
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

    #[test]
    fn git_ignores_everything_a_build_makes() {
        let parent = temp_dir("gitignore");
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        let ignored = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        for name in ["output/", ".output-building/", ".output-old/"] {
            assert!(
                ignored.lines().any(|line| line == name),
                "{name}: {ignored}"
            );
        }
        std::fs::remove_dir_all(&parent).unwrap();
    }

    fn fresh(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let parent = temp_dir(name);
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        (parent, dir)
    }

    #[test]
    fn a_current_theme_is_left_alone_with_no_copy() {
        let (parent, dir) = fresh("regensame");
        assert_eq!(regen_theme(&dir).unwrap(), None);
        assert!(!dir.join("theme.old").exists());
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn an_edited_theme_moves_aside_and_the_starting_one_returns() {
        let (parent, dir) = fresh("regenedit");
        let index = dir.join("theme/templates/index.html");
        let original = std::fs::read(&index).unwrap();
        std::fs::write(&index, "my own words").unwrap();
        assert_eq!(regen_theme(&dir).unwrap(), Some(dir.join("theme.old")));
        assert_eq!(std::fs::read(&index).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(dir.join("theme.old/templates/index.html")).unwrap(),
            "my own words"
        );
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_file_added_to_a_theme_counts_as_an_edit_and_is_kept() {
        let (parent, dir) = fresh("regenadded");
        std::fs::write(dir.join("theme/static/css/mine.css"), "mine").unwrap();
        regen_theme(&dir).unwrap();
        assert!(dir.join("theme.old/static/css/mine.css").is_file());
        assert!(!dir.join("theme/static/css/mine.css").exists());
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn an_earlier_copy_is_never_overwritten() {
        let (parent, dir) = fresh("regentwice");
        let index = dir.join("theme/templates/index.html");
        std::fs::write(&index, "first").unwrap();
        regen_theme(&dir).unwrap();
        std::fs::write(&index, "second").unwrap();
        assert_eq!(regen_theme(&dir).unwrap(), Some(dir.join("theme.old-2")));
        let read = |path: &str| std::fs::read_to_string(dir.join(path)).unwrap();
        assert_eq!(read("theme.old/templates/index.html"), "first");
        assert_eq!(read("theme.old-2/templates/index.html"), "second");
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_missing_theme_is_written() {
        let (parent, dir) = fresh("regenmissing");
        std::fs::remove_dir_all(dir.join("theme")).unwrap();
        assert_eq!(regen_theme(&dir).unwrap(), None);
        assert!(dir.join("theme/templates/index.html").is_file());
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn only_the_theme_changes() {
        let (parent, dir) = fresh("regenrest");
        std::fs::write(dir.join("site.yaml"), "mine").unwrap();
        std::fs::write(dir.join("pages/about.md"), "mine").unwrap();
        std::fs::write(dir.join("theme/templates/index.html"), "edited").unwrap();
        regen_theme(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join("site.yaml")).unwrap(), b"mine");
        assert_eq!(std::fs::read(dir.join("pages/about.md")).unwrap(), b"mine");
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn somewhere_that_isnt_a_site_is_refused() {
        let dir = temp_dir("regennosite");
        let err = regen_theme(&dir).unwrap_err();
        assert!(err.contains("site.yaml"), "{err}");
        assert!(!dir.join("theme").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
