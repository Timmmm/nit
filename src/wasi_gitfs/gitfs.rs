use std::collections::BTreeMap;

use gix::ObjectId;
use slab::Slab;

enum Directory {
    /// A directory that exists in Git but hasn't been opened yet.
    Unopened(ObjectId),
    /// An opened directory (may or may not have originally existed in Git).
    Opened(BTreeMap<String, DirectoryOrFile>),
}

/// The possible states of a file.
enum File {
    /// Unmodified file already in Git. This is its hash.
    Unmodified(ObjectId),
    /// New file that wasn't in Git. Vec<u8> is its content.
    Created(Vec<u8>),
    /// File in Git that was deleted. This is its hash.
    Deleted(ObjectId),
    /// File in Git that was modified. This is its hash and new content.
    Modified((ObjectId, Vec<u8>)),
}

type FileId = usize;
type DirectoryId = usize;

struct FileTable {
    // The files and directories "on disk". These are discovered lazily.
    // We read the entire file into memory when it is read or written for
    // the first time.
    // TODO: Add link count to File.
    file: Slab<File>, // Indexed by FileId.
    directories: Slab<Directory>, // Indexed by DirectoryId.

    // Open files and directories. A file descriptor is an index into this table.
    open_files: Vec<FileId>,
    // Open directories. File descriptors can point to these too.
    open_directories: Vec<DirectoryId>,
    // file_free_list: Vec<...>,
    // directory_free_list: Vec<...>,
}
