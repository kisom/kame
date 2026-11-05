//! The Site menu: building a notebook's site and sending it, from the app. All of the work is
//! `kame-site`'s, on a background thread, since a build reads what publishes and sending waits on
//! the network. See section 7 of `docs/spec/notebook.md`.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use eframe::egui;
use egui::{Align2, Id, Order, Vec2};
use kame_site::notebook::{build, send, Asking, GoAhead, Sent};
use kame_site::Problems;

use super::KameApp;

/// How many files to name when asking about deleting them.
const SHOWN: usize = 20;

/// What the Site menu can be asked to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SiteJob {
    Build,
    Send(GoAhead),
}

/// How a job went.
#[derive(Debug, PartialEq)]
pub(crate) struct SiteReport {
    job: SiteJob,
    /// The notebook this is about, which needn't be the one on screen by the time it's answered.
    root: PathBuf,
    /// Where the built site is, if it got that far.
    output: Option<PathBuf>,
    /// The remote and what sending would remove from it, had it gone ahead.
    would_delete: Option<(String, Vec<String>)>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl SiteReport {
    /// What to say in the status row. `None` if it takes a window, for there being something to
    /// read or to answer.
    fn in_passing(&self) -> Option<String> {
        if !self.errors.is_empty() || !self.warnings.is_empty() || self.would_delete.is_some() {
            return None;
        }
        Some(match self.job {
            SiteJob::Build => "the site is built".to_string(),
            SiteJob::Send(_) => "the site is built and sent".to_string(),
        })
    }

    fn title(&self) -> &'static str {
        if self.would_delete.is_some() {
            "Send the site?"
        } else if !self.errors.is_empty() {
            match self.job {
                SiteJob::Build => "The site didn't build",
                SiteJob::Send(_) => "The site wasn't sent",
            }
        } else {
            match self.job {
                SiteJob::Build => "The site is built",
                SiteJob::Send(_) => "The site is built and sent",
            }
        }
    }

    /// What answering yes to this report's question does: exactly what it showed, for the notebook
    /// it was about, whatever has changed on screen or on disk since.
    fn answer_yes(&self) -> Option<(SiteJob, PathBuf)> {
        let (remote, files) = self.would_delete.clone()?;
        let agreed = GoAhead::Agreed { remote, files };
        Some((SiteJob::Send(agreed), self.root.clone()))
    }
}

/// Does `job` for the notebook at `root`.
fn run(job: SiteJob, root: PathBuf) -> SiteReport {
    let mut problems = Problems::default();
    let (output, would_delete) = match &job {
        SiteJob::Build => (build(&root, &mut problems), None),
        SiteJob::Send(go_ahead) => match send(&root, go_ahead, Asking::Nobody, &mut problems) {
            Sent::Done(output) => (Some(output), None),
            Sent::WouldDelete { remote, files } => (None, Some((remote, files))),
            Sent::Failed => (None, None),
        },
    };
    SiteReport {
        job,
        root,
        output,
        would_delete,
        errors: problems.errors,
        warnings: problems.warnings,
    }
}

/// The Site menu's state.
#[derive(Default)]
pub(crate) struct Publishing {
    coming: Option<Receiver<SiteReport>>,
    /// What's in the window, once there's something to read or answer.
    pub(crate) report: Option<SiteReport>,
}

impl Publishing {
    /// Whether a job is under way, which is why the menu's entries are off.
    pub(crate) fn working(&self) -> bool {
        self.coming.is_some()
    }
}

impl KameApp {
    /// Starts `job` on the notebook on screen, if there is one.
    pub(crate) fn start_site_job(&mut self, ctx: &egui::Context, job: SiteJob) {
        if let Some(notebook) = &self.nb.notebook {
            let root = notebook.root.clone();
            self.start_site_job_in(ctx, job, root);
        }
    }

    /// Starts `job` on the notebook at `root`, unless something else is under way.
    fn start_site_job_in(&mut self, ctx: &egui::Context, job: SiteJob, root: PathBuf) {
        if self.publishing.working() {
            return;
        }
        self.notice = Some(
            match job {
                SiteJob::Build => "building the site…",
                SiteJob::Send(_) => "building and sending the site…",
            }
            .to_string(),
        );
        let (done, coming) = channel();
        self.publishing.coming = Some(coming);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = done.send(run(job, root));
            ctx.request_repaint();
        });
    }

    /// Takes in a finished job, and shows anything it left to read or answer.
    pub(crate) fn show_site_report(&mut self, ctx: &egui::Context) {
        if let Some(coming) = &self.publishing.coming {
            match coming.try_recv() {
                Ok(report) => {
                    self.publishing.coming = None;
                    self.notice = report.in_passing();
                    if self.notice.is_none() {
                        self.publishing.report = Some(report);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.publishing.coming = None;
                    self.notice = Some("the site tool stopped unexpectedly".to_string());
                }
            }
        }
        let Some(report) = &self.publishing.report else {
            return;
        };
        let mut close = false;
        let mut send_anyway = None;
        egui::Window::new(report.title())
            .id(Id::unique("kame-site-report"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                if let Some((remote, files)) = &report.would_delete {
                    ui.label(format!(
                        "Sending {} would delete {} files from {remote}, so nothing has been sent yet.",
                        report.root.display(),
                        files.len()
                    ));
                } else if let Some(output) = &report.output {
                    ui.label(format!("It's in {}.", output.display()));
                }
                // A question is only about what would go. Warnings wait for the send they belong to.
                let files = report.would_delete.as_ref().map(|(_, files)| files);
                let lines: Vec<&String> = match files {
                    Some(files) => files.iter().take(SHOWN).collect(),
                    None => report.errors.iter().chain(&report.warnings).collect(),
                };
                let more = files.map_or(0, |files| files.len().saturating_sub(SHOWN));
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for line in lines {
                            ui.label(line);
                        }
                        if more > 0 {
                            ui.label(format!("and {more} more"));
                        }
                    });
                ui.horizontal(|ui| {
                    if report.would_delete.is_some() {
                        if ui.button("Send anyway").clicked() {
                            send_anyway = report.answer_yes();
                        }
                        close = ui.button("Don't send").clicked();
                    } else {
                        close = ui.button("Close").clicked();
                    }
                });
            });
        if close || send_anyway.is_some() {
            self.publishing.report = None;
        }
        if let Some((job, root)) = send_anyway {
            self.start_site_job_in(ctx, job, root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(job: SiteJob) -> SiteReport {
        SiteReport {
            job,
            root: PathBuf::from("/n"),
            output: Some(PathBuf::from("/n/output")),
            would_delete: None,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_job_with_nothing_to_read_only_says_so_in_passing() {
        assert!(report(SiteJob::Build).in_passing().is_some());
        let mut warned = report(SiteJob::Build);
        warned
            .warnings
            .push("a link publishes as its text".to_string());
        assert_eq!(warned.in_passing(), None);
        assert_eq!(
            warned.title(),
            "The site is built",
            "a warning isn't a failure"
        );
    }

    #[test]
    fn a_failure_and_a_question_each_take_the_window() {
        let mut failed = report(SiteJob::Send(GoAhead::Ask));
        failed.output = None;
        failed.errors.push("rsync failed".to_string());
        assert_eq!(failed.in_passing(), None);
        assert_eq!(failed.title(), "The site wasn't sent");
        let mut asking = report(SiteJob::Send(GoAhead::Ask));
        asking.output = None;
        asking.would_delete = Some(("host:/site/".to_string(), vec!["old.html".to_string()]));
        assert_eq!(asking.in_passing(), None);
        assert_eq!(asking.title(), "Send the site?");
    }

    #[test]
    fn yes_means_what_the_question_showed_and_only_a_question_can_be_answered() {
        assert_eq!(report(SiteJob::Build).answer_yes(), None);
        let mut asking = report(SiteJob::Send(GoAhead::Ask));
        asking.root = PathBuf::from("/this/notebook");
        asking.would_delete = Some(("host:/site/".to_string(), vec!["old.html".to_string()]));
        assert_eq!(
            asking.answer_yes(),
            Some((
                SiteJob::Send(GoAhead::Agreed {
                    remote: "host:/site/".to_string(),
                    files: vec!["old.html".to_string()],
                }),
                PathBuf::from("/this/notebook"),
            ))
        );
    }
}
