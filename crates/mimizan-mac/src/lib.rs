//! Folder access for a sandboxed Mac build, and the App Store purchase of
//! the Mac App Store build (`store`, behind the feature of that name).
//!
//! The App Sandbox only lets the program read a folder the user picked in
//! the system dialog, and only while a security-scoped bookmark for that
//! folder is resolved. Linux and Windows have no sandbox here, so this
//! crate declines every request and the darkroom keeps its own dialog.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "macos")]
pub use apple::{adopt, pick_folder, pick_save, restore, FolderGrant};

/// The App Store purchase. Only the Mac App Store build has it.
#[cfg(all(target_os = "macos", feature = "store"))]
pub mod store;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::{adopt, pick_folder, pick_save, restore, FolderGrant};

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn other_systems_decline() {
        assert!(pick_folder().is_none());
        assert!(pick_save(None, "print.tif", &["tif", "tiff"]).is_none());
        assert!(restore(&[1, 2, 3]).is_none());
        assert!(adopt(std::path::Path::new("/tmp")).is_none());
    }
}
