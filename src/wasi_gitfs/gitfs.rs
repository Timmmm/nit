use std::collections::BTreeMap;

use gix::ObjectId;
use slab::Slab;

enum DirectoryOrFile {
    Directory(?),
    File(?),
}

enum Directory {
    /// A directory that exists in Git but hasn't been opened ever.
    /// When it is opened we discover all of its children.
    Unopened(ObjectId),
    /// An opened directory (may or may not have originally existed in Git).
    Opened(BTreeMap<String, DirectoryOrFile>),
    /// Modified directory (files added or deleted).
    Modified(BTreeMap<String, DirectoryOrFile>),
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

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
struct FileNode {
    file: File,
    link_count: u64,
}

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
struct DirectoryNode {
    directory: Directory,
    link_count: u64,
}

/// The files and directories "on disk". These are discovered lazily.
/// We read the entire file into memory when it is read or written for
/// the first time.
 struct FileSystem {
    // TODO: Create TiSlab that uses proper newtypes for indices.
    /// Discovered files. Indexed by FileId.
    file: Slab<FileNode>,
    /// Discovered directories. Indexed by DirectoryId.
    directories: Slab<DirectoryNode>,

    // TODO: Optimise finding space in `files` and `directories` with a free list.
    // We could probably wrap this generic functionality in a struct.
    // file_free_list: Vec<...>,
    // directory_free_list: Vec<...>,
}

/// File table associated with the process (i.e. the entire WASI instance).
///
/// Unlike on UNIX a file descriptor is opaque rather than an integer so
/// we can separate files and directories instead of mixing them.
struct ProcessFileTable {
    // Open files and directories. A file descriptor is an index into this table.
    open_files: Vec<FileId>,
    // Open directories. File descriptors can point to these too.
    open_directories: Vec<DirectoryId>,
}
