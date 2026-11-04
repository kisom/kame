//! Builds a static site from `.kame` documents and Markdown pages.

pub mod config;
pub mod feed;
pub mod notebook;
pub mod pages;
pub mod plan;
pub mod refs;
pub mod source;
pub mod starter;
pub mod write;

#[cfg(test)]
pub(crate) mod testing;

/// What a build found wrong. Errors stop it, reported together; warnings don't.
#[derive(Debug, Default)]
pub struct Problems {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Builds the site at `site_dir`, replacing `output/` only if all of it succeeds.
pub fn build(site_dir: &std::path::Path, include_drafts: bool) -> Problems {
    let mut problems = Problems::default();
    if let Some(plan) = plan::build_plan(site_dir, include_drafts, &mut problems) {
        if let Err(err) = write::write_site(&plan, site_dir) {
            problems
                .errors
                .push(format!("could not write the site: {err}"));
        }
    }
    problems
}

const USAGE: &str =
    "usage:\n  kame-site new DIR\n  kame-site build [--no-drafts] [DIR]\n  kame-site sync [DIR]\n  kame-site regen-theme [DIR]";

/// The command line, without its program name. Returns the exit code: 1 when the work
/// failed, and 2 when the command wasn't understood.
pub fn run(args: &[String]) -> i32 {
    let mut args = args.iter().map(String::as_str);
    match args.next() {
        Some("new") => {
            let (Some(dir), None) = (args.next(), args.next()) else {
                eprintln!("{USAGE}");
                return 2;
            };
            match starter::new_site(std::path::Path::new(dir)) {
                Ok(()) => {
                    println!("Created a site in {dir}. Edit site.yaml, then run kame-site build.");
                    0
                }
                Err(err) => {
                    eprintln!("kame-site: {err}");
                    1
                }
            }
        }
        Some("regen-theme") => {
            let dir = args.next().unwrap_or(".");
            if args.next().is_some() {
                eprintln!("{USAGE}");
                return 2;
            }
            match starter::regen_theme(std::path::Path::new(dir)) {
                Ok(Some(kept)) => {
                    println!("Updated the theme. Your old one is in {}.", kept.display());
                    0
                }
                Ok(None) => {
                    println!("The theme is up to date.");
                    0
                }
                Err(err) => {
                    eprintln!("kame-site: {err}");
                    1
                }
            }
        }
        Some("build") => {
            let mut include_drafts = true;
            let mut dir = None;
            for arg in args {
                match arg {
                    "--no-drafts" => include_drafts = false,
                    other if other.starts_with('-') || dir.is_some() => {
                        eprintln!("{USAGE}");
                        return 2;
                    }
                    other => dir = Some(other),
                }
            }
            let dir = std::path::Path::new(dir.unwrap_or("."));
            if notebook::is_notebook(dir) {
                let mut problems = Problems::default();
                let output = notebook::build(dir, &mut problems);
                return finish_notebook(output, problems);
            }
            let problems = build(dir, include_drafts);
            for warning in &problems.warnings {
                eprintln!("warning: {warning}");
            }
            for error in &problems.errors {
                eprintln!("error: {error}");
            }
            i32::from(!problems.errors.is_empty())
        }
        Some("sync") => {
            let dir = args.next().unwrap_or(".");
            if args.next().is_some() {
                eprintln!("{USAGE}");
                return 2;
            }
            let dir = std::path::Path::new(dir);
            if !notebook::is_notebook(dir) {
                eprintln!(
                    "kame-site: sync is for a notebook, and {} has no notebook.yaml",
                    dir.display()
                );
                return 1;
            }
            let mut problems = Problems::default();
            let output = notebook::sync(dir, &mut problems);
            finish_notebook(output, problems)
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

/// Says how a notebook's build went, and what its exit code is.
fn finish_notebook(output: Option<std::path::PathBuf>, problems: Problems) -> i32 {
    for warning in &problems.warnings {
        eprintln!("warning: {warning}");
    }
    for error in &problems.errors {
        eprintln!("error: {error}");
    }
    if let Some(output) = &output {
        println!("Built in {}", output.display());
    }
    i32::from(output.is_none() || !problems.errors.is_empty())
}
