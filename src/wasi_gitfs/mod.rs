//! Module for WASI Git Filesystem integration.
//!
//! We expose a virtual filesystem backed by a Git repository. This filesystem
//! is *mutable* and records changes made to it. When finished with it you
//! can query it and get a list of file paths that have been modified.
//!
//! The naive way to implement a VFS is to have a map from path to file contents.
//! However this fails in two ways:
//!
//! 1. Symlinks allow accessing files through multiple paths.
//! 2. Files can be kept open while they are renamed or deleted.
//!
//! Therefore we need to implement a traditional filesystem structure with inodes
//! and directory entries. Each inode represents either a file or a directory.
//! Directories contain directory entries mapping names to inodes. Each inode
//! also has a link count representing how many directory entries point to it.
//! When a file is opened we keep it alive even if its link count drops to zero
//! (i.e. it is deleted or renamed).
//!
//! At the end we still need to map the virtual files back to files on disk
//! so we can propagate the changes. In order to do that we need to be
//! able to find the full path of each file. We do this by keeping track
//! of the parent directory of each directory and file. This allows us to
//! reconstruct the full path by walking up the tree.
//!
//! Created/deleted files are tracked because their parent directory is modified.

pub mod gitfs;
pub mod modifications;
pub mod wasi_linker_excluding_filesystem;
pub mod wasi_state;
