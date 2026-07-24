use std::{collections::BTreeMap, path::PathBuf};

#[derive(Eq, PartialEq)]
pub struct FileContentsAndMetadata {
    pub contents: Vec<u8>,
    pub executable: bool,
}

#[derive(Eq, PartialEq)]
pub enum FileState {
    NonExistent,
    Exists(FileContentsAndMetadata),
}

pub struct FileModification {
    pub original: FileState,
    pub modified: FileState,
}

pub type FileModifications = BTreeMap<PathBuf, FileModification>;
