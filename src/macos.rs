//! macOS application events.
//!
//! AppKit reports files opened from Finder, and asks whether quitting is all right,
//! through the application delegate. winit installs its own delegate and panics if it
//! finds another, despite what its documentation says, so kame adds the two methods it
//! needs to winit's class instead. winit defines neither, so nothing is overridden.
//! Both only leave a request in [`crate::external`], so the rest of kame handles them
//! as it handles everything else.

use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;
use objc2_app_kit::NSApplicationTerminateReply;
use objc2_foundation::{NSArray, NSURL};
use std::path::PathBuf;

/// `application:openURLs:`. Must not panic: it is called from AppKit.
unsafe extern "C-unwind" fn open_urls(
    _delegate: *mut AnyObject,
    _cmd: Sel,
    _application: *mut AnyObject,
    urls: &NSArray<NSURL>,
) {
    for url in urls.iter() {
        if let Some(path) = url.path() {
            crate::external::request_open(PathBuf::from(path.to_string()));
        }
    }
}

/// `applicationShouldTerminate:`. Every way of quitting comes through here: Cmd-Q and
/// the menu, the Dock, and logging out. kame closes itself once the unsaved-document
/// prompt allows it, so AppKit's own termination is always declined. Must not panic.
unsafe extern "C-unwind" fn should_terminate(
    _delegate: *mut AnyObject,
    _cmd: Sel,
    _application: *mut AnyObject,
) -> NSApplicationTerminateReply {
    crate::external::request_quit();
    NSApplicationTerminateReply::TerminateCancel
}

/// Adds kame's methods to winit's delegate class. Call after the event loop exists,
/// which registers that class, and before it runs, so a file opened from Finder at
/// launch is not missed. If a later winit renames the class this quietly does nothing:
/// Finder opens stop working, but kame still runs.
pub fn install_app_events() {
    let Some(class) = AnyClass::get(c"WinitApplicationDelegate") else {
        return;
    };
    let class = class as *const AnyClass as *mut AnyClass;
    // SAFETY: each signature matches its Objective-C type encoding: `v` void and `Q`
    // an unsigned long for the return, `@` an object, `:` the selector.
    unsafe {
        let open: unsafe extern "C-unwind" fn(_, _, _, &NSArray<NSURL>) = open_urls;
        class_addMethod(
            class,
            sel!(application:openURLs:),
            std::mem::transmute::<*const (), Imp>(open as *const ()),
            c"v@:@@".as_ptr(),
        );
        let quit: unsafe extern "C-unwind" fn(_, _, _) -> _ = should_terminate;
        class_addMethod(
            class,
            sel!(applicationShouldTerminate:),
            std::mem::transmute::<*const (), Imp>(quit as *const ()),
            c"Q@:@".as_ptr(),
        );
    }
}
