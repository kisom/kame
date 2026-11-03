//! Requests from outside the window: the OS asking kame to open a file or to quit.
//!
//! They arrive between frames, so they wait here until the app's next `logic` pass,
//! and waking the app is what gets that pass to run. Keeping this apart from the
//! platform code means only the part that receives them is specific to macOS.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// What the OS has asked for since the last look.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Requests {
    pub open: Vec<PathBuf>,
    pub quit: bool,
}

static PENDING: Mutex<Requests> = Mutex::new(Requests {
    open: Vec::new(),
    quit: false,
});
static WAKER: OnceLock<eframe::egui::Context> = OnceLock::new();

/// Gives the requests a way to wake the app. Until then they simply wait, which is
/// how a file opened from Finder survives a launch still in progress.
pub fn set_waker(ctx: eframe::egui::Context) {
    let _ = WAKER.set(ctx);
}

fn wake() {
    if let Some(ctx) = WAKER.get() {
        ctx.request_repaint();
    }
}

pub fn request_open(path: PathBuf) {
    with_pending(|pending| pending.open.push(path));
    wake();
}

pub fn request_quit() {
    with_pending(|pending| pending.quit = true);
    wake();
}

/// Everything asked for since the last call.
pub fn take() -> Requests {
    with_pending(std::mem::take)
}

fn with_pending<R>(f: impl FnOnce(&mut Requests) -> R) -> R {
    let mut pending = PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut pending)
}
