//! Folder access for a sandboxed Mac build, and the App Store purchase of
//! the Mac App Store build (`store`, behind the feature of that name).
//!
//! The App Sandbox only lets the program read a folder the user picked in
//! the system dialog, and only while a security-scoped bookmark for that
//! folder is resolved. The darkroom links this crate on macOS. Anywhere
//! else it keeps its own dialogs.

#![cfg(target_os = "macos")]

mod apple;
pub use apple::{adopt, pick_folder, pick_save, restore, FolderGrant, SaveGrant};

/// The App Store purchase. Only the Mac App Store build has it.
#[cfg(feature = "store")]
pub mod store;
