//! `NSOpenPanel` / `NSSavePanel` and security-scoped bookmarks.
//!
//! The panel URL is what the sandbox allows. A bookmark made from that URL
//! is stored with the settings and resolved on the next launch, which calls
//! `startAccessingSecurityScopedResource` for as long as the grant is held.

use objc2::rc::autoreleasepool;
use objc2::runtime::Bool;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel, NSSavePanel};
use objc2_foundation::{
    NSArray, NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
};
use std::path::{Path, PathBuf};

pub struct FolderGrant {
    path: PathBuf,
    bookmark: Vec<u8>,
    url: objc2::rc::Retained<NSURL>,
    started: bool,
}

impl FolderGrant {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bookmark(&self) -> Option<&[u8]> {
        if self.bookmark.is_empty() {
            None
        } else {
            Some(&self.bookmark)
        }
    }
}

impl Drop for FolderGrant {
    fn drop(&mut self) {
        if self.started {
            // SAFETY: balances the successful `startAccessing` in `hold`,
            // once, on the URL it was called on.
            unsafe { self.url.stopAccessingSecurityScopedResource() }
        }
    }
}

/// The user picks one folder. Cancel returns nothing.
pub fn pick_folder() -> Option<FolderGrant> {
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    panel.setPrompt(Some(&NSString::from_str("Open")));
    panel.setMessage(Some(&NSString::from_str("Choose a folder of negatives")));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let urls = panel.URLs();
    if urls.count() == 0 {
        return None;
    }
    let url = urls.objectAtIndex(0);
    // The panel's URL is open already; a bookmark is made from it fresh.
    hold(url, false, None)
}

/// The user names the export. `extensions` are the file types the panel
/// allows (the first is appended when the user types none), so the path
/// handed back is the path that is written: the sandbox grants that one
/// file, and a name changed afterwards would be another. The returned path
/// stays writable for this launch; the save panel's grant is kept by not
/// releasing the URL.
// `setAllowedFileTypes` is the deprecated spelling of `allowedContentTypes`.
// It takes the extensions as strings, which is all that is needed here,
// and it is the one of the two whose binding is verified against the
// crate this build uses; the UTType binding is not available to check on
// the machine that wrote this.
#[allow(deprecated)]
pub fn pick_save(directory: Option<&Path>, file_name: &str, extensions: &[&str]) -> Option<PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = NSSavePanel::savePanel(mtm);
    panel.setCanCreateDirectories(true);
    panel.setPrompt(Some(&NSString::from_str("Save")));
    panel.setMessage(Some(&NSString::from_str("Save the export")));
    panel.setNameFieldStringValue(&NSString::from_str(file_name));
    if !extensions.is_empty() {
        let types: Vec<_> = extensions.iter().map(|e| NSString::from_str(e)).collect();
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
        panel.setAllowsOtherFileTypes(false);
    }
    if let Some(dir) = directory.and_then(|p| p.to_str()) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(dir));
        panel.setDirectoryURL(Some(&url));
    }
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URL()?;
    // The save URL has to stay alive or the sandbox drops the write.
    // One URL per export, released when the process ends.
    // SAFETY: plain message to a retained URL; balanced below when the
    // path cannot be read, else kept open on purpose.
    let started = unsafe { url.startAccessingSecurityScopedResource() };
    let Some(path) = url_path(&url) else {
        if started {
            // SAFETY: balances the successful `start` above, once.
            unsafe { url.stopAccessingSecurityScopedResource() }
        }
        return None;
    };
    if started {
        std::mem::forget(url);
    }
    Some(path)
}

/// The bookmark from the last launch. Nothing when it no longer resolves.
/// A stale bookmark (the folder moved or was renamed) still resolves this
/// time; it is made again from the resolved folder, so the next launch has
/// a fresh one. One that is not stale is kept as it is.
pub fn restore(bookmark: &[u8]) -> Option<FolderGrant> {
    if bookmark.is_empty() || MainThreadMarker::new().is_none() {
        return None;
    }
    let data = NSData::from_vec(bookmark.to_vec());
    let mut stale = Bool::NO;
    // SAFETY: `stale` is a live local for the whole call, which is what the
    // out-pointer must point at; the method writes a BOOL into it.
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::WithSecurityScope,
            None,
            &mut stale as *mut Bool,
        )
    }
    .ok()?;
    let keep = if stale.as_bool() { None } else { Some(bookmark) };
    let mut grant = hold(url, true, keep)?;
    // A stale bookmark the system would not write again keeps the bytes
    // that just resolved, so the next launch still has something.
    if grant.bookmark.is_empty() {
        grant.bookmark = bookmark.to_vec();
    }
    Some(grant)
}

/// A folder the user dropped on the window. Only kept when the system
/// agrees to a security-scoped bookmark; a plain path is not one.
pub fn adopt(path: &Path) -> Option<FolderGrant> {
    MainThreadMarker::new()?;
    let text = path.to_str()?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    hold(url, true, None).filter(|g| g.bookmark().is_some() && g.started)
}

/// Open the resource and keep it. `keep` is a bookmark already known to be
/// good for this URL; without one a new bookmark is made, which needs the
/// access to be started first (the system reads the folder to write it),
/// and falls back to empty when the system declines.
fn hold(url: objc2::rc::Retained<NSURL>, must_start: bool, keep: Option<&[u8]>) -> Option<FolderGrant> {
    // SAFETY: plain Objective-C message to a retained URL; no pointers of
    // ours are passed. Balanced by `stop` below or in `Drop`.
    let started = unsafe { url.startAccessingSecurityScopedResource() };
    if must_start && !started {
        return None;
    }
    let stop = |url: &NSURL| {
        if started {
            // SAFETY: balances the successful `start` above, once.
            unsafe { url.stopAccessingSecurityScopedResource() }
        }
    };
    // The path string is only meaningful once the sandbox has opened the
    // resource, and so is the bookmark.
    let Some(path) = url_path(&url) else {
        stop(&url);
        return None;
    };
    let bookmark = match keep {
        Some(bytes) => bytes.to_vec(),
        None => security_bookmark(&url),
    };
    Some(FolderGrant { path, bookmark, url, started })
}

fn security_bookmark(url: &NSURL) -> Vec<u8> {
    url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::WithSecurityScope,
        None,
        None,
    )
    .map(|data| data.to_vec())
    .unwrap_or_default()
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    let path = url.path()?;
    autoreleasepool(|pool| {
        // SAFETY: the string is retained for the whole closure and the
        // borrowed `&str` is copied into a `PathBuf` before the pool ends.
        let text = unsafe { path.to_str(pool) };
        if text.is_empty() {
            None
        } else {
            Some(PathBuf::from(text))
        }
    })
}
