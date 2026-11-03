//! The macOS application delegate.
//!
//! winit leaves the delegate to the application, and it is the only place AppKit
//! reports files opened from Finder and asks whether quitting is all right. Both just
//! become requests in [`crate::external`], so the rest of kame handles them as it
//! handles everything else.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSURL};
use std::path::PathBuf;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KameAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                if let Some(path) = url.path() {
                    crate::external::request_open(PathBuf::from(path.to_string()));
                }
            }
        }

        // Every way of quitting comes through here: Cmd-Q and the menu, the Dock, and
        // logging out. kame closes itself once the unsaved-document prompt allows it,
        // so AppKit's own termination is always declined.
        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(
            &self,
            _application: &NSApplication,
        ) -> NSApplicationTerminateReply {
            crate::external::request_quit();
            NSApplicationTerminateReply::TerminateCancel
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Installs the delegate. Call after the event loop exists and before it runs, so a
/// file opened from Finder at launch is not missed.
pub fn install_delegate() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let delegate = AppDelegate::new(mtm);
    let app = NSApplication::sharedApplication(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // AppKit holds its delegate weakly, and this one lives as long as the app.
    std::mem::forget(delegate);
}
