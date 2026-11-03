//! Writing a worked-out site. The new site replaces `output/` only once all of it is written,
//! so a failure leaves the last good one alone, and nothing stale survives.

use std::path::Path;

use crate::plan::Output;
use crate::plan::Plan;

/// Where a build is assembled, beside `output/` so that the final rename stays on one disk.
const BUILDING: &str = ".output-building";
/// Where the last site waits while the new one moves in.
const OLD: &str = ".output-old";

pub fn write_site(plan: &Plan, site_dir: &Path) -> std::io::Result<()> {
    let building = site_dir.join(BUILDING);
    if building.exists() {
        std::fs::remove_dir_all(&building)?;
    }
    std::fs::create_dir_all(&building)?;
    for (path, output) in &plan.files {
        let target = building.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match output {
            Output::Bytes(bytes) => std::fs::write(&target, bytes)?,
            Output::Copy(source) => {
                std::fs::copy(source, &target)?;
            }
        }
    }
    // Moved aside before anything is deleted, so that the worst an interruption can do is
    // leave a whole site under the wrong name, for the next build to clear.
    let output = site_dir.join("output");
    let old = site_dir.join(OLD);
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    if output.exists() {
        std::fs::rename(&output, &old)?;
    }
    std::fs::rename(&building, &output)?;
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build;
    use crate::plan::Output;
    use crate::starter::new_site;
    use crate::testing::{metadata, paragraph, temp_dir, write_kame};
    use kame_core::model::Document;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, found);
                } else {
                    found.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(dir, dir, &mut found);
        found
    }

    fn site(name: &str) -> PathBuf {
        let parent = temp_dir(name);
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        let path = dir.join("docs/a.kame");
        let mut doc = Document::new("D");
        doc.blocks = vec![paragraph("Words.")];
        write_kame(&path, &doc, Some(&metadata("Doc A", "2026-11-02", &["t"])));
        let yaml = format!(
            "site:\n  name: Example\n  url: https://e.com\n  publish:\n    a: {}\n",
            path.display()
        );
        std::fs::write(dir.join("site.yaml"), yaml).unwrap();
        dir
    }

    #[test]
    fn a_build_writes_a_browsable_site() {
        let dir = site("writes");
        let problems = build(&dir, true);
        assert!(problems.errors.is_empty(), "{:?}", problems.errors);
        for file in [
            "index.html",
            "k/a/index.html",
            "archive/index.html",
            "tags/t/index.html",
            "about/index.html",
            "feed.xml",
            "fonts/EBGaramond-Regular.otf",
            "css/fonts.css",
        ] {
            assert!(dir.join("output").join(file).is_file(), "{file}");
        }
    }

    #[test]
    fn a_new_site_builds_with_no_other_steps() {
        let parent = temp_dir("fresh");
        let dir = parent.join("site");
        new_site(&dir).unwrap();
        let problems = build(&dir, true);
        assert!(problems.errors.is_empty(), "{:?}", problems.errors);
        assert!(dir.join("output/index.html").is_file());
    }

    #[test]
    fn building_twice_gives_identical_output() {
        let dir = site("twice");
        build(&dir, true);
        let first = snapshot(&dir.join("output"));
        build(&dir, true);
        assert_eq!(first, snapshot(&dir.join("output")));
    }

    #[test]
    fn a_failed_build_leaves_the_last_good_site_alone() {
        let dir = site("failed");
        build(&dir, true);
        let good = snapshot(&dir.join("output"));
        std::fs::write(
            dir.join("site.yaml"),
            "site:\n  name: E\n  url: https://e.com\n  publish:\n    a: /no/such.kame\n",
        )
        .unwrap();
        let problems = build(&dir, true);
        assert!(!problems.errors.is_empty());
        assert_eq!(good, snapshot(&dir.join("output")));
    }

    #[test]
    fn nothing_stale_survives_a_build() {
        let dir = site("stale");
        build(&dir, true);
        std::fs::write(dir.join("output/stale.html"), "old").unwrap();
        build(&dir, true);
        assert!(!dir.join("output/stale.html").exists());
    }

    #[test]
    fn a_leftover_temporary_directory_doesnt_stop_a_build() {
        let dir = site("leftover");
        std::fs::create_dir_all(dir.join(".output-building/junk")).unwrap();
        let problems = build(&dir, true);
        assert!(problems.errors.is_empty(), "{:?}", problems.errors);
        assert!(!dir.join(".output-building").exists());
        assert!(!dir.join("output/junk").exists());
    }

    #[test]
    fn building_leaves_source_documents_byte_for_byte() {
        let dir = site("sources");
        let before = std::fs::read(dir.join("docs/a.kame")).unwrap();
        build(&dir, true);
        assert_eq!(std::fs::read(dir.join("docs/a.kame")).unwrap(), before);
    }

    #[test]
    fn a_copy_comes_across_exactly() {
        let dir = site("copy");
        let source = dir.join("binary.bin");
        std::fs::write(&source, [0u8, 159, 146, 150]).unwrap();
        let mut plan = Plan::default();
        plan.files
            .insert(PathBuf::from("a/b.bin"), Output::Copy(source));
        write_site(&plan, &dir).unwrap();
        assert_eq!(
            std::fs::read(dir.join("output/a/b.bin")).unwrap(),
            [0u8, 159, 146, 150]
        );
    }

    #[test]
    fn the_command_line_says_what_it_takes() {
        assert_eq!(crate::run(&["--nonsense".to_string()]), 2);
        assert_eq!(crate::run(&[]), 2);
    }

    #[test]
    fn the_command_line_builds_and_news() {
        let parent = temp_dir("cli");
        let dir = parent.join("site");
        let shown = dir.display().to_string();
        assert_eq!(crate::run(&["new".to_string(), shown.clone()]), 0);
        assert_eq!(
            crate::run(&["new".to_string(), shown.clone()]),
            1,
            "it refuses a second time"
        );
        assert_eq!(crate::run(&["build".to_string(), shown.clone()]), 0);
        assert!(dir.join("output/index.html").is_file());
    }

    /// The old site moves aside before the new one moves in, so what is left over from an
    /// interrupted swap has to be cleared by the next build.
    #[test]
    fn a_site_left_aside_by_an_interrupted_swap_is_cleared() {
        let dir = site("aside");
        std::fs::create_dir_all(dir.join(".output-old/junk")).unwrap();
        let problems = build(&dir, true);
        assert!(problems.errors.is_empty(), "{:?}", problems.errors);
        assert!(!dir.join(".output-old").exists());
        assert!(dir.join("output/index.html").is_file());
    }

    #[test]
    fn the_old_site_isnt_left_aside_after_a_good_build() {
        let dir = site("noaside");
        build(&dir, true);
        build(&dir, true);
        assert!(!dir.join(".output-old").exists());
        assert!(!dir.join(".output-building").exists());
    }

    #[test]
    fn the_command_line_regenerates_the_theme() {
        let parent = temp_dir("cliregen");
        let dir = parent.join("site");
        let shown = dir.display().to_string();
        assert_eq!(
            crate::run(&["regen-theme".to_string(), shown.clone()]),
            1,
            "not a site yet"
        );
        assert_eq!(crate::run(&["new".to_string(), shown.clone()]), 0);
        std::fs::write(dir.join("theme/templates/index.html"), "edited").unwrap();
        assert_eq!(crate::run(&["regen-theme".to_string(), shown.clone()]), 0);
        assert!(dir.join("theme.old/templates/index.html").is_file());
        assert_eq!(
            crate::run(&["regen-theme".to_string(), shown, "extra".to_string()]),
            2
        );
    }
}
