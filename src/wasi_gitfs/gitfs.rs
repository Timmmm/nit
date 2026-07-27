use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use gix::{ObjectId, Repository};
use slab::Slab;
use wasmtime_wasi::p2::{FsResult, bindings::filesystem::types::ErrorCode};

use crate::wasi_gitfs::modifications::FileModifications;

pub type Inode = usize;
pub const ROOT_INODE: Inode = 0;

pub enum ObjectIdOrContent<T> {
    ObjectId(ObjectId),
    Content(T),
}

/// The type of a file. Git only supports these three modes for blobs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FileMode {
    Regular,
    Executable,
    /// The content of the file is the target path of the symlink.
    Symlink,
}

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
pub struct FileNode {
    /// Current content of the file. Filled in when the file is opened.
    content: ObjectIdOrContent<Vec<u8>>,
    /// Mode of the file. This can be changed in some cases (e.g. marking the
    /// file as executable).
    mode: FileMode,
    /// Number of file descriptors pointing to this file.
    open_count: u64,
    /// Parent director(ies); needed so we can reconstruct full paths.
    /// There may be none for an unlinked file. There will be multiple
    /// for hard-linked files.
    parents: Vec<Inode>,
}

// TODO: Symlink?

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
pub struct DirectoryNode {
    /// Current directory entries: Inode -> name. When the directory is opened
    /// we populate this and create all the file inodes.
    entries: ObjectIdOrContent<BTreeMap<Inode, String>>,
    /// Number of file descriptors pointing to this directory. When you rmdir()
    /// a directory that is open, you can still call readdir() on it succesfully;
    /// it will just return no entries (not even "." or "..").
    open_count: u64,
    /// Parent directory; needed so we can reconstruct full paths.
    /// Unlike files directories cannot be hard linked, so there is only one parent.
    /// The root directory's parent is itself. This is the only loop in the
    /// directory tree.
    parent: Inode,
}

pub enum Node {
    File(FileNode),
    Directory(DirectoryNode),
}

/// The files and directories "on disk". These are discovered lazily.
/// We read the entire file into memory when it is read or written for
/// the first time.
struct FileSystem {
    /// Discovered file & directories, indexed by Inode.
    /// Inode 0 is always the root directory.
    nodes: Slab<Node>,
    // TODO (2.0): Create TiSlab that uses proper newtypes for indices.

    // TODO (2.0): Optimise finding space in `files` and `directories` with a free list.
    // (Or use an arena that already provides that.)
}

impl FileSystem {
    fn new(root: ObjectId) -> Self {
        let mut nodes = Slab::new();
        // Insert root directory.
        nodes.insert(Node::Directory(DirectoryNode {
            entries: ObjectIdOrContent::ObjectId(root),
            open_count: 1,
            parent: ROOT_INODE,
        }));
        Self { nodes }
    }
}

pub struct GitFs {
    // Git repository.
    repo: Repository,

    // Root directory
    fs: FileSystem,

    // Set of paths that may have been modified.
    maybe_changed: HashSet<PathBuf>,
}

impl GitFs {
    // Create a new GitFs instance.
    pub fn new(repo: Repository, tree: ObjectId) -> Self {
        Self {
            repo,
            fs: FileSystem::new(tree),
            maybe_changed: Default::default(),
        }
    }

    /// Get modified files and directories. This is basically any
    /// file/directory that has been created, moved, deleted or written to.
    ///
    /// We keep track of filenames and directories that *might* have been modified.
    /// Afterwards we compare all the files and directories (and all contents
    /// of the directories) with the original commit and find differences
    /// that way. Probably not the most efficient but simple.
    pub fn modifications(&self) -> FileModifications {
        todo!(
            "return a list of modified files and directories (after checking potential modifications vs the original commit)"
        )
    }

    pub fn get_node(&self, inode: Inode) -> FsResult<&Node> {
        self.fs
            .nodes
            .get(inode)
            .ok_or(ErrorCode::BadDescriptor.into())
    }

    /// Follow a path relative to an existing file or directory.
    /// See https://pubs.opengroup.org/onlinepubs/9799919799/ for details about
    /// POSIX's mad pathname resolution, and https://github.com/WebAssembly/wasi-filesystem/blob/main/path-resolution.md
    /// for WASI specifically.
    ///
    /// Only relative paths are allowed. Absolute paths cause a permission error.
    /// For this function the target file or directory (or symlink) must exist.
    fn resolve_path(
        &mut self,
        from: Inode,
        relative_path: &str,
        follow_final_symlink: bool,
    ) -> FsResult<Inode> {
        if relative_path.starts_with('/') {
            return Err(ErrorCode::Access.into());
        }

        let mut inode = from;

        // Allow a maximum of 40 symlink follows. Based on this value
        // https://github.com/wasix-org/wasix-libc/blob/28158c2ece7401604a9f6a409be320b47fffe78e/expected/wasm32-wasi/predefined-macros.txt#L4617
        let mut symlink_follow_remaining = 40;

        // So we can handle the last component separately.
        // TODO: What does that comment mean ^ ? Do we handle the last component differently?
        for component in relative_path.split('/') {
            match self.fs.nodes.get(inode).ok_or(ErrorCode::BadDescriptor)? {
                Node::Directory(dir_node) => {
                    match component {
                        // Either two consecutive slashes "foo/bar//baz" or a trailing slash "foo/bar/".
                        "" => {}
                        // Same directory.
                        "." => {}
                        // Parent.
                        ".." => {
                            inode = dir_node.parent;
                        }
                        // Named child.
                        child_dir => {
                            let entries = match &dir_node.entries {
                                ObjectIdOrContent::ObjectId(object_id) => {
                                    // TODO (0.1): Need to load directory entries from Git.
                                    todo!("lazily load directory entries from Git")
                                }
                                ObjectIdOrContent::Content(content) => content,
                            };
                            inode = *entries
                                .iter()
                                .find(|(_, v)| *v == child_dir)
                                .ok_or(ErrorCode::NoEntry)?
                                .0;
                        }
                    }
                }
                Node::File(_) => {
                    // Can't get a child of a file.
                    return Err(ErrorCode::NotDirectory.into());
                }
            }
        }

        // TODO: Handle symlinks and check `follow_final_symlink`.
        Ok(inode)
    }

    pub fn read_file(&mut self, inode: Inode) -> FsResult<&[u8]> {
        let node = self
            .fs
            .nodes
            .get_mut(inode)
            .ok_or(ErrorCode::BadDescriptor)?;

        match node {
            Node::File(file_node) => {
                match file_node.content {
                    // Lazily load content from git.
                    ObjectIdOrContent::ObjectId(blob_id) => {
                        let mut blob = self
                            .repo
                            .find_blob(blob_id)
                            .map_err(|_| ErrorCode::NoEntry)?;
                        file_node.content = ObjectIdOrContent::Content(blob.take_data());

                        // This sucks but there doesn't seem to be a better way.
                        let content = match &file_node.content {
                            ObjectIdOrContent::Content(content) => content,
                            _ => unreachable!(),
                        };
                        Ok(content)
                    }
                    // Content already available.
                    ObjectIdOrContent::Content(ref content) => Ok(content),
                }
            }
            Node::Directory(_) => Err(ErrorCode::IsDirectory.into()),
        }
    }
}
