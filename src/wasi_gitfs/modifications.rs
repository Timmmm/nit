use std::{collections::BTreeMap, path::PathBuf};

/// The type of a file. Git only supports these three modes for blobs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FileMode {
    Regular,
    Executable,
    /// The content of the file is the target path of the symlink.
    Symlink,
}

#[derive(Debug, Eq, PartialEq)]
pub struct FileContentsAndMetadata {
    pub contents: Vec<u8>,
    pub mode: FileMode,
}

#[derive(Debug, Eq, PartialEq)]
pub enum FileState {
    /// Note that since Git doesn't track directories, directories will be
    /// reported as NonExistent.
    // TODO: Maybe rename this to `NotFile`.
    NonExistent,
    Exists(FileContentsAndMetadata),
}

pub struct FileModification {
    pub original: FileState,
    pub modified: FileState,
}

/// Map from file path to the modification.
pub type FileModifications = BTreeMap<String, FileModification>;
