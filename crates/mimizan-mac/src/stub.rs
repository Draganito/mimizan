use std::path::{Path, PathBuf};

/// A folder the sandbox has been allowed to read, and the bookmark that
/// opens it again next launch.
pub struct FolderGrant {
    path: PathBuf,
    bookmark: Vec<u8>,
}

impl FolderGrant {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Empty when the system did not hand back a bookmark.
    pub fn bookmark(&self) -> Option<&[u8]> {
        if self.bookmark.is_empty() {
            None
        } else {
            Some(&self.bookmark)
        }
    }
}

pub fn pick_folder() -> Option<FolderGrant> {
    None
}

pub fn pick_save(_directory: Option<&Path>, _file_name: &str) -> Option<PathBuf> {
    None
}

pub fn restore(_bookmark: &[u8]) -> Option<FolderGrant> {
    None
}

pub fn adopt(_path: &Path) -> Option<FolderGrant> {
    None
}
