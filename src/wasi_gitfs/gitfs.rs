use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use gix::{ObjectId, Repository, progress::prodash::tree::root};
use slab::Slab;
use wasmtime_wasi::p2::{
    FsResult,
    bindings::filesystem::{preopens::Descriptor, types::ErrorCode},
};

pub type Inode = usize;
pub const ROOT_INODE: Inode = 0;

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
pub struct FileNode {
    // TODO: Represent the git_content/content as an enum of valid states?
    // Unopened(ObjectId), Opened(ObjectId, Vec<u8>), New(Vec<u8>)
    // TODO: Need a deleted state.
    /// Original content of the file in Git.
    git_content: Option<ObjectId>,
    /// Current content of the file. Filled in when the file is opened.
    content: Option<Vec<u8>>,
    /// Number of directory entries and file descriptors pointing to this file?
    link_count: u64,
    /// Parent directory; needed so we can reconstruct full paths.
    parent: Inode,

    /// Set to true when written to.
    modified: bool,
}

// TODO: Symlink?

/// Represents a file on disk (after it has been lazily opened).
/// Equivalent to an inode.
pub struct DirectoryNode {
    // TODO: Represent the content/entries as an enum of valid states?
    // Unopened(ObjectId), Opened(ObjectId, BTreeMap<Inode, String>), New(BTreeMap<Inode, String>)
    // TODO: Need a deleted state.
    git_content: Option<ObjectId>,
    /// Current directory entries: Inode -> name. When the directory is opened
    /// we populate this and create all the file inodes.
    entries: Option<BTreeMap<Inode, String>>,
    /// Number of directory entries and file descriptors pointing to this file?
    link_count: u64,
    /// Parent directory; needed so we can reconstruct full paths.
    /// Inode 0 is used for the root directory, which is its own parent.
    parent: Inode,

    /// Set to true when written to (files created/renamed/deleted).
    modified: bool,
}

pub enum Node {
    File(FileNode),
    Directory(DirectoryNode),
}

/// The files and directories "on disk". These are discovered lazily.
/// We read the entire file into memory when it is read or written for
/// the first time.
struct FileSystem {
    /// Discovered files, indexed by Inode.
    /// Inode 0 is always the root directory.
    nodes: Slab<Node>,
    // TODO: Create TiSlab that uses proper newtypes for indices.

    // TODO: Optimise finding space in `files` and `directories` with a free list.
    // (Or use an arena that already provides that.)
}

impl FileSystem {
    fn new(root: ObjectId) -> Self {
        let mut nodes = Slab::new();
        // Insert root directory.
        nodes.insert(Node::Directory(DirectoryNode {
            git_content: Some(root),
            entries: None,
            link_count: 1,
            parent: ROOT_INODE,
            modified: false,
        }));
        Self { nodes }
    }

    // fn full_path(&self, inode: Inode) -> FsResult<String> {
    //     // TODO: AI generated; review.
    //     let mut components = Vec::new();
    //     let mut current_inode = inode;
    //     loop {
    //         if current_inode == ROOT_INODE {
    //             break;
    //         }
    //         let node = self
    //             .nodes
    //             .get(current_inode)
    //             .ok_or(ErrorCode::InvalidInput)?;
    //         let parent_inode = match node {
    //             Node::File(file_node) => file_node.parent,
    //             Node::Directory(dir_node) => dir_node.parent,
    //         };
    //         let parent_node = self
    //             .nodes
    //             .get(parent_inode)
    //             .ok_or(ErrorCode::InvalidInput)?;
    //         let name = match parent_node {
    //             Node::Directory(dir_node) => {
    //                 if let Some(entries) = &dir_node.entries {
    //                     entries
    //                         .iter()
    //                         .find_map(|(inode, name)| if *inode == current_inode { Some(name) } else { None })
    //                         .ok_or(ErrorCode::InvalidInput)?
    //                         .clone()
    //                 } else {
    //                     return Err(ErrorCode::InvalidInput.into());
    //                 }
    //             }
    //             _ => return Err(ErrorCode::InvalidInput.into()),
    //         };
    //         components.push(name);
    //         current_inode = parent_inode;
    //     }
    //     components.reverse();
    //     Ok(format!("/{}", components.join("/")))
    // }

    /// Get the list of inodes that have been modified. Note
    /// that if a program moves a file over an existing file it will
    /// end up with a different inode.
    fn modified_inodes(&self) -> Vec<Inode> {
        self.nodes
            .iter()
            .filter_map(|(inode, node)| match node {
                Node::File(file_node) if file_node.modified => Some(inode),
                Node::Directory(dir_node) if dir_node.modified => Some(inode),
                _ => None,
            })
            .collect()
    }
}

pub struct GitFs {
    // Git repository.
    repo: Repository,

    // Root directory
    fs: FileSystem,
}

pub enum FileModifications {
    /// New file with the given content.
    Created(Vec<u8>),
    /// File modified to have the given content.
    Modified(Vec<u8>),
    /// File deleted.
    Deleted,
}

impl GitFs {
    // Create a new GitFs instance.
    pub fn new(repo: Repository, tree: ObjectId) -> Self {
        Self {
            repo,
            fs: FileSystem::new(tree),
        }
    }

    /// Get modified files. Directory modifications are ignored because
    /// Git doesn't track those anyway.
    pub fn file_modifications(&self) -> BTreeMap<PathBuf, FileModifications> {
        todo!()
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

        // TODO: Allow a maximum of 40 symlink follows. Based on this value
        // https://github.com/wasix-org/wasix-libc/blob/28158c2ece7401604a9f6a409be320b47fffe78e/expected/wasm32-wasi/predefined-macros.txt#L4617
        let mut symlink_follow_remaining = 40;

        // So we can handle the last component separately.
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
                                Some(entries) => entries,
                                None => {
                                    // Need to load directory entries from Git.
                                    todo!("lazily load directory entries from Git")
                                }
                            };
                            inode = *entries
                                .iter()
                                .find(|k, v| v == child_dir)
                                .ok_or(ErrorCode::NoEntry)?
                                .0;
                        }
                    }
                }
                Node::File(file_node) => {
                    // Can't get a child of a file.
                    return Err(ErrorCode::NotDirectory.into());
                }
            }
        }

        // if node.kind == EntryKind::Link && follow_final_symlink {
        //     todo!("symlink support")
        // }
        Ok(inode)
    }

    pub fn read_file(&mut self, inode: Inode) -> FsResult<&[u8]> {
        // TODO: AI generated; review.
        todo!()
        // match &mut self.fs.nodes.get_mut(inode).ok_or(ErrorCode::InvalidInput)? {
        //     Node::File(file_node) => {
        //         if let Some(content) = &file_node.content {
        //             Ok(content)
        //         } else if let Some(git_content_id) = file_node.git_content {
        //             let blob_data = self.read_blob(git_content_id)?;
        //             file_node.content = Some(blob_data.to_vec());
        //             Ok(file_node.content.as_ref().unwrap())
        //         } else {
        //             Err(ErrorCode::NoEntry.into())
        //         }
        //     }
        //     Node::Directory(_) => Err(ErrorCode::IsDirectory.into()),
        // }
    }

    // // Read a full blob (the only API Gix gives because it may be compressed
    // // or based on diffs). It is cached.
    // fn read_blob(&mut self, id: ObjectId) -> FsResult<&[u8]> {
    //     match self.blob_contents.entry(id) {
    //         hash_map::Entry::Vacant(vacant_entry) => {
    //             let mut blob = self.repo.find_blob(id).map_err(|_| ErrorCode::NoEntry)?;
    //             let data = blob.take_data();
    //             Ok(vacant_entry.insert(data))
    //         }
    //         hash_map::Entry::Occupied(occupied_entry) => Ok(occupied_entry.into_mut()),
    //     }
    // }
}
