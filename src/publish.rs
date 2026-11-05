//! The Site menu: building a notebook's site and sending it, from the app. All of the work is
//! `kame-site`'s, on a background thread, since a build reads what publishes and sending waits on
//! the network. See section 7 of `docs/spec/notebook.md`.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use eframe::egui;
use egui::{Align2, Id, Order, Vec2};
use kame_site::notebook::{build, send, Asking, Sent};
use kame_site::Problems;

use super::KameApp;

/// How many files to name when asking about deleting them.
const SHOWN: usize = 20;

/// What the Site menu can be asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SiteJob {
    Build,
    Send { go_ahead: bool },
}

/// How a job went.
#[derive(Debug, PartialEq)]
pub(crate) struct SiteReport {
    job: SiteJob,
    /// Where the built site is, if it got that far.
    output: Option<PathBuf>,
    /// What sending would have removed, had it gone ahead.
    would_delete: Vec<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl SiteReport {
    /// What to say in the status row. `None` if it takes a window, for there being something to
    /// read or to answer.
    fn in_passing(&self) -> Option<String> {
        if !self.errors.is_empty() || !self.warnings.is_empty() || !self.would_delete.is_empty() {
            return None;
        }
        Some(match self.job {
            SiteJob::Build => "the site is built".to_string(),
            SiteJob::Send { .. } => "the site is built and sent".to_string(),
        })
    }

    fn title(&self) -> &'static str {
        if !self.would_delete.is_empty() {
            "Send the site?"
        } else if !self.errors.is_empty() {
            match self.job {
                SiteJob::Build => "The site didn't build",
                SiteJob::Send { .. } => "The site wasn't sent",
            }
        } else {
            match self.job {
                SiteJob::Build => "The site is built",
                SiteJob::Send { .. } => "The site is built and sent",
            }
        }
    }
}

/// Does `job` for the notebook at `root`.
fn run(job: SiteJob, root: &std::path::Path) -> SiteReport {
    let mut problems = Problems::default();
    let (output, would_delete) = match job {
        SiteJob::Build => (build(root, &mut problems), Vec::new()),
        SiteJob::Send { go_ahead } => match send(root, go_ahead, Asking::Nobody, &mut problems) {
            Sent::Done(output) => (Some(output), Vec::new()),
            Sent::WouldDelete(doomed) => (None, doomed),
            Sent::Failed => (None, Vec::new()),
        },
    };
    SiteReport {
        job,
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
    /// Starts `job`, if there's a notebook and nothing else is under way.
    pub(crate) fn start_site_job(&mut self, ctx: &egui::Context, job: SiteJob) {
        let Some(root) = self
            .nb
            .notebook
            .as_ref()
            .map(|notebook| notebook.root.clone())
        else {
            return;
        };
        if self.publishing.working() {
            return;
        }
        self.notice = Some(
            match job {
                SiteJob::Build => "building the site…",
                SiteJob::Send { .. } => "building and sending the site…",
            }
            .to_string(),
        );
        let (done, coming) = channel();
        self.publishing.coming = Some(coming);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = done.send(run(job, &root));
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
        let mut send_anyway = false;
        egui::Window::new(report.title())
            .id(Id::unique("kame-site-report"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                let remote = self
                    .nb
                    .notebook
                    .as_ref()
                    .and_then(|notebook| notebook.settings.remote.clone())
                    .unwrap_or_default();
                if !report.would_delete.is_empty() {
                    ui.label(format!(
                        "This would delete {} files from {remote}, so nothing has been sent yet.",
                        report.would_delete.len()
                    ));
                } else if let Some(output) = &report.output {
                    ui.label(format!("It's in {}.", output.display()));
                }
                // A question is only about what would go. Warnings wait for the send they belong to.
                let lines: Vec<&String> = if report.would_delete.is_empty() {
                    report.errors.iter().chain(&report.warnings).collect()
                } else {
                    report.would_delete.iter().take(SHOWN).collect()
                };
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for line in lines {
                            ui.label(line);
                        }
                        if report.would_delete.len() > SHOWN {
                            ui.label(format!("and {} more", report.would_delete.len() - SHOWN));
                        }
                    });
                ui.horizontal(|ui| {
                    if report.would_delete.is_empty() {
                        close = ui.button("Close").clicked();
                    } else {
                        send_anyway = ui.button("Send anyway").clicked();
                        close = ui.button("Don't send").clicked();
                    }
                });
            });
        if close || send_anyway {
            self.publishing.report = None;
        }
        if send_anyway {
            self.start_site_job(ctx, SiteJob::Send { go_ahead: true });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(job: SiteJob) -> SiteReport {
        SiteReport {
            job,
            output: Some(PathBuf::from("/n/output")),
            would_delete: Vec::new(),
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
        let mut failed = report(SiteJob::Send { go_ahead: false });
        failed.output = None;
        failed.errors.push("rsync failed".to_string());
        assert_eq!(failed.in_passing(), None);
        assert_eq!(failed.title(), "The site wasn't sent");
        let mut asking = report(SiteJob::Send { go_ahead: false });
        asking.output = None;
        asking.would_delete.push("old.html".to_string());
        assert_eq!(asking.in_passing(), None);
        assert_eq!(asking.title(), "Send the site?");
    }
}
