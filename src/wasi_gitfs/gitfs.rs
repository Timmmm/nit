use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::Result;
use gix::{ObjectId, Repository, objs::tree::EntryKind};
use slab::Slab;
use wasmtime_wasi::p2::{FsResult, bindings::filesystem::types::ErrorCode};

use crate::wasi_gitfs::modifications::{
    FileContentsAndMetadata, FileMode, FileModification, FileModifications,
    FileState::{self, NonExistent},
};

pub type Inode = usize;
pub const ROOT_INODE: Inode = 0;

pub enum ObjectIdOrContent<T> {
    ObjectId(ObjectId),
    Content(T),
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
    /// Current directory entries: String -> Inode. When the directory is opened
    /// we populate this and create all the file inodes.
    entries: ObjectIdOrContent<BTreeMap<String, Inode>>,
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
    /// Root tree object this filesystem was created from. Not needed by FileSystem
    /// itself but useful for callers.
    root: ObjectId,

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
        Self { root, nodes }
    }

    fn root(&self) -> ObjectId {
        self.root
    }
}

pub struct GitFs {
    // Git repository.
    repo: Repository,

    // Root directory
    fs: FileSystem,

    /// Set of file paths that may have been modified. This only includes files.
    /// When a directory is moved/created/deleted we add all of the file paths
    /// in the original/modified directory recursively.
    ///
    /// Note that WASI requires paths to be representable in UTF-8 which is
    /// why we use String and not Vec<u8> or PathBuf.
    maybe_changed: HashSet<String>,
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

    /// Get modified files. This is basically any file that has been created,
    /// moved, deleted or written to.
    ///
    /// We keep track of filenames that *might* have been modified.
    /// Afterwards we compare all the files with the original commit and
    /// find differences that way.
    ///
    /// Probably not the most efficient but simple.
    pub fn modifications(&self) -> FileModifications {
        self.maybe_changed
            .iter()
            .filter_map(|path| {
                // TODO: Don't unwrap.
                let original = self.original_state(path).unwrap();
                let modified = self.modified_state(path).unwrap();
                (original != modified)
                    .then(|| (path.clone(), FileModification { original, modified }))
            })
            .collect()
    }

    /// The state of `path` in the original Git tree. If it's a directory
    /// then it is reported as NonExistant.
    fn original_state(&self, path: &str) -> Result<FileState> {
        // TODO: Cache `tree` (but this hits the classic reference-to-sibling issue).
        let tree = self.repo.find_tree(self.fs.root())?;

        let entry = match tree.lookup_entry_by_path(path)? {
            Some(entry) => entry,
            None => return Ok(FileState::NonExistent),
        };

        let mode = match entry.mode().kind() {
            EntryKind::Blob => FileMode::Regular,
            EntryKind::BlobExecutable => FileMode::Executable,
            EntryKind::Link => FileMode::Symlink,
            // Directories don't exist as far as Git knows.
            EntryKind::Tree | EntryKind::Commit => return Ok(FileState::NonExistent),
        };

        let mut blob = self.repo.find_blob(entry.object_id())?;

        Ok(FileState::Exists(FileContentsAndMetadata {
            contents: blob.take_data(),
            mode,
        }))
    }

    /// The current state of `path` in the VFS. If it's a directory
    /// then it is reported as NonExistant.
    fn modified_state(&self, path: &str) -> Result<FileState> {
        // TODO: We need a detectable error for non-existence; it shouldn't be returns as an Err() here.
        let inode = self.resolve_path(ROOT_INODE, path, false)?;

        let mode = match &self.fs.nodes[inode] {
            Node::File(file) => file.mode,
            Node::Directory(_) => return Ok(FileState::NonExistent),
        };

        // TODO: Read inode file content.
        // let content = match self.content(inode) {
        //     Ok(content) => content,
        //     Err(_) => {
        //         warn!("Couldn't read {}; ignoring it.", path.display());
        //         return None;
        //     }
        // };
        // let contents = content.lock().expect("content mutex poisoned").clone();

        Ok(FileState::Exists(FileContentsAndMetadata {
            contents: todo!(),
            mode,
        }))
    }

    /// Record that the file at `path` may have been modified.
    fn record_modified_path(&mut self, path: String) {
        self.maybe_changed.insert(path);
    }

    /// Record that `inode` may have been modified, at its current path.
    /// If it is hard linked we record all of its paths.
    pub fn record_modified_inode(&mut self, inode: Inode) {
        for path in self.inode_paths(inode) {
            self.record_modified_path(path);
        }
    }

    /// The set of full paths for `inode`, relative to the root of the filesystem (so
    /// there is no leading `/`). Normally there will be one entry, if it has
    /// been unlinked there will be 0, if it has been hard linked it will be >1.
    // TODO: Use SmallVector<1, String> for return type.
    pub fn inode_paths(&self, inode: Inode) -> Vec<String> {
        if inode == ROOT_INODE {
            return vec![];
        }

        // There shouldn't be any path loops, but just in case we made a mistake
        // we can throw an error instead of hanging.
        const MAX_PATH_DEPTH: usize = 1024 * 1024;

        // Parent directories of the node. If it is a directory it will only have
        // one but it if is a file it can have multiple due to hard links.
        let parent_dirs: &[Inode] = match &self.fs.nodes[inode] {
            Node::File(file) => &file.parents,
            Node::Directory(dir) => &[dir.parent],
        };

        parent_dirs
            .into_iter()
            .map(|parent| {
                let mut components = Vec::new();
                let mut current = inode;

                for _ in 0..MAX_PATH_DEPTH {
                    let parent = match &self.fs.nodes[current] {
                        Node::File(_) => unreachable!("A directory cannot have a file as parent"),
                        Node::Directory(dir) => dir.parent,
                    };
                    components.push(current);
                    if parent == ROOT_INODE {
                        // We got to the root, now build up the string.
                        let mut path = String::new();
                        let mut parent = parent;
                        for child in components.into_iter().rev() {
                            let name = self
                                .name_in_directory(parent, child)
                                .expect("Logic error in inode_paths");
                            path.push('/');
                            path.push_str(&name);
                            parent = child;
                        }
                        return path;
                    }
                    current = parent;
                }

                // TODO: Hopefully nobody will have 1 million directories, but could they?
                unreachable!("Directory tree is too deep, or contains a loop");
            })
            .collect()
    }

    /// Get the name of the file `child` in the `parent` directory `parent`.
    fn name_in_directory(&self, parent: Inode, child: Inode) -> Option<String> {
        let Node::Directory(DirectoryNode {
            entries: ObjectIdOrContent::Content(entries),
            ..
        }) = &self.fs.nodes[parent]
        else {
            // This shouldn't really happen I think?
            todo!("Handle this more gracefully")
        };
        // TODO: Potentially use bimap so we don't need to search?
        entries
            .iter()
            .find(|(_, inode)| **inode == child)
            .map(|(name, _)| name.clone())
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
        &self,
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
