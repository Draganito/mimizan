//! Folder access for a sandboxed Mac build.
//!
//! The App Sandbox only lets the program read a folder the user picked in
//! the system dialog, and only while a security-scoped bookmark for that
//! folder is resolved. Linux and Windows have no sandbox here, so this
//! crate declines every request and the darkroom keeps its own dialog.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "macos")]
pub use apple::{adopt, pick_folder, pick_save, restore, FolderGrant};

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
        assert!(pick_save(None, "print.tif").is_none());
        assert!(restore(&[1, 2, 3]).is_none());
        assert!(adopt(std::path::Path::new("/tmp")).is_none());
    }
}
