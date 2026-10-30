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
    NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
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
            // Balances the successful `startAccessing` in `hold`.
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
    hold(url, false)
}

/// The user names the export. The returned path stays readable for this
/// launch; the save panel's grant is kept by not releasing the URL.
pub fn pick_save(directory: Option<&Path>, file_name: &str) -> Option<PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = NSSavePanel::savePanel(mtm);
    panel.setCanCreateDirectories(true);
    panel.setPrompt(Some(&NSString::from_str("Save")));
    panel.setMessage(Some(&NSString::from_str("Save the export")));
    panel.setNameFieldStringValue(&NSString::from_str(file_name));
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
    let started = unsafe { url.startAccessingSecurityScopedResource() };
    let Some(path) = url_path(&url) else {
        if started {
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
pub fn restore(bookmark: &[u8]) -> Option<FolderGrant> {
    if bookmark.is_empty() || MainThreadMarker::new().is_none() {
        return None;
    }
    let data = NSData::from_vec(bookmark.to_vec());
    // `is_stale` is null: a stale bookmark still resolves for this launch,
    // and the next Open replaces it.
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::WithSecurityScope,
            None,
            std::ptr::null_mut::<Bool>(),
        )
    }
    .ok()?;
    let mut grant = hold(url, true)?;
    // A resolved bookmark that cannot be written again keeps the bytes that
    // just worked, so the next launch still has them.
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
    hold(url, true).filter(|g| g.bookmark().is_some() && g.started)
}

fn hold(url: objc2::rc::Retained<NSURL>, must_start: bool) -> Option<FolderGrant> {
    let bookmark = security_bookmark(&url);
    // Start before reading the path: the path string is only meaningful
    // once the sandbox has opened the resource.
    let started = unsafe { url.startAccessingSecurityScopedResource() };
    if must_start && !started {
        return None;
    }
    let Some(path) = url_path(&url) else {
        if started {
            unsafe { url.stopAccessingSecurityScopedResource() }
        }
        return None;
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
        // Copied into a `PathBuf` before the pool ends.
        let text = unsafe { path.to_str(pool) };
        if text.is_empty() {
            None
        } else {
            Some(PathBuf::from(text))
        }
    })
}
