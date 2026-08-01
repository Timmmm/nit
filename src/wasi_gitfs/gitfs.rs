use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use gix::{ObjectId, Repository, bstr::ByteSlice, objs::tree::EntryKind};
use log::warn;
use slab::Slab;
use wasmtime_wasi::p2::{
    FsResult,
    bindings::filesystem::types::{DescriptorStat, DescriptorType, DirectoryEntry, ErrorCode},
};

use crate::wasi_gitfs::modifications::{
    FileContentsAndMetadata, FileModification, FileModifications, FileState,
};

pub type Inode = usize;
pub const ROOT_INODE: Inode = 0;

/// Maximum number of symlinks that may be followed while resolving a single
/// path, after which we give up and return `ELOOP`. Based on this value
/// https://github.com/wasix-org/wasix-libc/blob/28158c2ece7401604a9f6a409be320b47fffe78e/expected/wasm32-wasi/predefined-macros.txt#L4617
const MAX_SYMLINK_FOLLOWS: u32 = 40;

/// Sanity limit when walking up the tree to reconstruct a path. There shouldn't
/// be any loops (apart from the root pointing at itself, which we handle) but
/// let's not hang if there are.
const MAX_PATH_DEPTH: usize = 1024;

/// The contents of a file. This is shared (rather than owned by the `FileNode`)
/// because `read-via-stream` and `write-via-stream` hand out stream objects that
/// live in the WASI resource table, and those streams cannot call back into
/// `GitFs` - their `read()`/`write()` methods only get `&mut self`. Sharing the
/// buffer is the only way for stream writes to land in the filesystem.
///
/// It's `Mutex` rather than `RefCell` because everything in the resource table
/// must be `Send`.
pub type SharedContent = Arc<Mutex<Vec<u8>>>;

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
    /// Current content of the file. Filled in when the file is read or written.
    content: ObjectIdOrContent<SharedContent>,
    /// Regular, executable or symlink.
    mode: FileMode,
    /// Parent directory; needed so we can reconstruct full paths. `None` if the
    /// file has been unlinked (it stays alive because a descriptor may still be
    /// open on it, and because we never reclaim inodes).
    ///
    /// Git has no concept of hard links so there is at most one parent, and
    /// `link-at` is unsupported.
    parent: Option<Inode>,
}

/// Represents a directory on disk (after it has been lazily opened).
/// Equivalent to an inode.
pub struct DirectoryNode {
    /// Current directory entries: name -> Inode. When the directory is opened
    /// we populate this and create all the child inodes.
    entries: ObjectIdOrContent<BTreeMap<String, Inode>>,
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

impl Node {
    fn descriptor_type(&self) -> DescriptorType {
        match self {
            Node::Directory(_) => DescriptorType::Directory,
            Node::File(f) => match f.mode {
                FileMode::Regular | FileMode::Executable => DescriptorType::RegularFile,
                FileMode::Symlink => DescriptorType::SymbolicLink,
            },
        }
    }

    fn is_symlink(&self) -> bool {
        matches!(
            self,
            Node::File(FileNode {
                mode: FileMode::Symlink,
                ..
            })
        )
    }
}

/// The files and directories "on disk". These are discovered lazily.
/// We read the entire file into memory when it is read or written for
/// the first time.
struct FileSystem {
    /// Discovered file & directories, indexed by Inode.
    /// Inode 0 is always the root directory.
    ///
    /// Nodes are never removed - an unlinked file must stay readable through
    /// any descriptor that is still open on it, and a lint run is short lived
    /// so there's no point reclaiming them.
    nodes: Slab<Node>,
    // TODO (2.0): Create TiSlab that uses proper newtypes for indices.
}

impl FileSystem {
    fn new(root: ObjectId) -> Self {
        let mut nodes = Slab::new();
        // Insert root directory.
        nodes.insert(Node::Directory(DirectoryNode {
            entries: ObjectIdOrContent::ObjectId(root),
            parent: ROOT_INODE,
        }));
        Self { nodes }
    }
}

pub struct GitFs {
    // Git repository.
    repo: Repository,

    // The tree this filesystem was created from. The root node's object ID is
    // replaced by its entries as soon as the root is read, so we keep the
    // original here to diff against in `modifications()`.
    root_tree: ObjectId,

    // Root directory
    fs: FileSystem,

    // Paths that may have been modified. See `modifications()`.
    maybe_changed: HashSet<PathBuf>,
}

impl GitFs {
    // Create a new GitFs instance.
    pub fn new(repo: Repository, tree: ObjectId) -> Self {
        Self {
            repo,
            root_tree: tree,
            fs: FileSystem::new(tree),
            maybe_changed: HashSet::new(),
        }
    }

    /// Get modified files. This is basically any file that has been created,
    /// moved, deleted or written to.
    ///
    /// We keep track of filenames that *might* have been modified. Afterwards
    /// we compare them with the original commit and find the real differences
    /// that way. Probably not the most efficient but simple, and it means a
    /// linter that rewrites a file with identical contents doesn't count as a
    /// modification.
    ///
    /// Note that only regular files are reported: Git can't represent an empty
    /// directory anyway, and `FileModification` has no way to describe a
    /// symlink, so those are skipped with a warning.
    pub fn modifications(&mut self) -> FileModifications {
        let mut modifications = FileModifications::new();

        // Take the paths out so we aren't borrowing `self` while resolving them.
        let maybe_changed = std::mem::take(&mut self.maybe_changed);

        for path in maybe_changed.iter() {
            let Some(original) = self.original_state(path) else {
                continue;
            };
            let Some(modified) = self.current_state(path) else {
                continue;
            };
            if original != modified {
                modifications.insert(path.clone(), FileModification { original, modified });
            }
        }

        self.maybe_changed = maybe_changed;

        modifications
    }

    /// The state of `path` in the original Git tree, or `None` if it isn't a
    /// regular file there (and so can't be represented as a `FileState`).
    fn original_state(&self, path: &Path) -> Option<FileState> {
        let tree = match self.repo.find_tree(self.root_tree) {
            Ok(tree) => tree,
            Err(e) => {
                warn!("Couldn't read the original tree: {e}");
                return None;
            }
        };

        let entry = match tree.lookup_entry_by_path(path) {
            Ok(entry) => entry,
            Err(e) => {
                warn!(
                    "Couldn't look up {} in the original tree: {e}",
                    path.display()
                );
                return None;
            }
        };

        let Some(entry) = entry else {
            return Some(FileState::NonExistent);
        };

        let executable = match entry.mode().kind() {
            EntryKind::Blob => false,
            EntryKind::BlobExecutable => true,
            EntryKind::Tree | EntryKind::Commit | EntryKind::Link => {
                warn!(
                    "{} was modified but it isn't a regular file in the original tree; ignoring it.",
                    path.display()
                );
                return None;
            }
        };

        let mut blob = match self.repo.find_blob(entry.object_id()) {
            Ok(blob) => blob,
            Err(e) => {
                warn!(
                    "Couldn't read {} from the original tree: {e}",
                    path.display()
                );
                return None;
            }
        };

        Some(FileState::Exists(FileContentsAndMetadata {
            contents: blob.take_data(),
            executable,
        }))
    }

    /// The current state of `path` in the VFS, or `None` if it isn't a regular
    /// file (and so can't be represented as a `FileState`).
    fn current_state(&mut self, path: &Path) -> Option<FileState> {
        let Some(path_str) = path.to_str() else {
            warn!("Path is not valid UTF-8: {}", path.display());
            return None;
        };

        // Anything that can't be resolved has been deleted (or never existed).
        let Ok(inode) = self.resolve_path(ROOT_INODE, path_str, false) else {
            return Some(FileState::NonExistent);
        };

        let mode = match self.fs.nodes.get(inode) {
            Some(Node::File(file)) => file.mode,
            _ => {
                warn!(
                    "{} was modified but it isn't a regular file; ignoring it.",
                    path.display()
                );
                return None;
            }
        };

        let executable = match mode {
            FileMode::Regular => false,
            FileMode::Executable => true,
            FileMode::Symlink => {
                warn!(
                    "{} was modified but it is a symlink; ignoring it.",
                    path.display()
                );
                return None;
            }
        };

        let content = match self.content(inode) {
            Ok(content) => content,
            Err(_) => {
                warn!("Couldn't read {}; ignoring it.", path.display());
                return None;
            }
        };
        let contents = content.lock().expect("content mutex poisoned").clone();

        Some(FileState::Exists(FileContentsAndMetadata {
            contents,
            executable,
        }))
    }

    /// Record that the file at `path` may have been modified.
    fn record_path(&mut self, path: PathBuf) {
        self.maybe_changed.insert(path);
    }

    /// Record that `inode` may have been modified, at its current path.
    pub fn record_modified(&mut self, inode: Inode) {
        if let Some(path) = self.path_of(inode) {
            self.record_path(path);
        }
    }

    /// Record every file at or below `inode`, which lives at `base`. Used when
    /// a whole directory is moved or deleted, since that changes the path of
    /// everything inside it.
    fn record_subtree(&mut self, inode: Inode, base: PathBuf) -> FsResult<()> {
        match self.fs.nodes.get(inode) {
            Some(Node::File(_)) => {
                self.record_path(base);
            }
            Some(Node::Directory(_)) => {
                // The subtree may not have been read from Git yet, but its
                // paths still change, so we have to enumerate it.
                self.load_directory(inode)?;
                let children: Vec<(String, Inode)> = match self.fs.nodes.get(inode) {
                    Some(Node::Directory(dir)) => match &dir.entries {
                        ObjectIdOrContent::Content(entries) => {
                            entries.iter().map(|(k, v)| (k.clone(), *v)).collect()
                        }
                        ObjectIdOrContent::ObjectId(_) => unreachable!("just loaded"),
                    },
                    _ => unreachable!("just checked"),
                };
                for (name, child) in children {
                    self.record_subtree(child, base.join(name))?;
                }
            }
            None => return Err(ErrorCode::BadDescriptor.into()),
        }
        Ok(())
    }

    pub fn get_node(&self, inode: Inode) -> FsResult<&Node> {
        self.fs
            .nodes
            .get(inode)
            .ok_or(ErrorCode::BadDescriptor.into())
    }

    fn get_node_mut(&mut self, inode: Inode) -> FsResult<&mut Node> {
        self.fs
            .nodes
            .get_mut(inode)
            .ok_or(ErrorCode::BadDescriptor.into())
    }

    pub fn descriptor_type(&self, inode: Inode) -> FsResult<DescriptorType> {
        Ok(self.get_node(inode)?.descriptor_type())
    }

    pub fn is_directory(&self, inode: Inode) -> FsResult<bool> {
        Ok(matches!(self.get_node(inode)?, Node::Directory(_)))
    }

    pub fn is_symlink(&self, inode: Inode) -> FsResult<bool> {
        Ok(self.get_node(inode)?.is_symlink())
    }

    /// The full path of `inode`, relative to the root of the filesystem (so
    /// there is no leading `/`), or `None` if it has been unlinked.
    pub fn path_of(&self, inode: Inode) -> Option<PathBuf> {
        if inode == ROOT_INODE {
            return Some(PathBuf::new());
        }

        let mut components = Vec::new();
        let mut current = inode;

        for _ in 0..MAX_PATH_DEPTH {
            let parent = match self.fs.nodes.get(current)? {
                Node::File(file) => file.parent?,
                Node::Directory(dir) => dir.parent,
            };
            components.push(self.name_in(parent, current)?);
            if parent == ROOT_INODE {
                let mut path = PathBuf::new();
                for component in components.iter().rev() {
                    path.push(component);
                }
                return Some(path);
            }
            current = parent;
        }

        warn!("Directory tree is too deep, or contains a loop");
        None
    }

    /// The name of `child` in `parent`, if it is still linked there.
    fn name_in(&self, parent: Inode, child: Inode) -> Option<String> {
        match self.fs.nodes.get(parent)? {
            Node::Directory(dir) => match &dir.entries {
                ObjectIdOrContent::Content(entries) => entries
                    .iter()
                    .find(|(_, inode)| **inode == child)
                    .map(|(name, _)| name.clone()),
                // If it hasn't been loaded then we can't have a child of it.
                ObjectIdOrContent::ObjectId(_) => None,
            },
            Node::File(_) => None,
        }
    }

    /// Read the directory entries of `inode` from Git, if we haven't already.
    /// Creates a node for every child.
    fn load_directory(&mut self, inode: Inode) -> FsResult<()> {
        let tree_id = match self.get_node(inode)? {
            Node::Directory(dir) => match dir.entries {
                ObjectIdOrContent::ObjectId(tree_id) => tree_id,
                // Already loaded.
                ObjectIdOrContent::Content(_) => return Ok(()),
            },
            Node::File(_) => return Err(ErrorCode::NotDirectory.into()),
        };

        // `tree` borrows `self.repo`, so it has to be dropped before we can
        // take a mutable borrow of the whole of `self` again below. (Inserting
        // nodes is fine because that only borrows the `self.fs` field.)
        let mut entries = BTreeMap::new();
        {
            let tree = self.repo.find_tree(tree_id).map_err(|_| ErrorCode::Io)?;

            for entry in tree.iter() {
                let entry = entry.map_err(|_| ErrorCode::Io)?;
                let name = entry
                    .filename()
                    .to_str()
                    .map_err(|_| ErrorCode::IllegalByteSequence)?
                    .to_owned();

                let object_id = entry.oid().to_owned();

                let node = match entry.mode().kind() {
                    EntryKind::Tree => Node::Directory(DirectoryNode {
                        entries: ObjectIdOrContent::ObjectId(object_id),
                        parent: inode,
                    }),
                    // For simplicity, submodules are treated as empty directories.
                    EntryKind::Commit => Node::Directory(DirectoryNode {
                        entries: ObjectIdOrContent::Content(BTreeMap::new()),
                        parent: inode,
                    }),
                    EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                        Node::File(FileNode {
                            content: ObjectIdOrContent::ObjectId(object_id),
                            mode: match entry.mode().kind() {
                                EntryKind::BlobExecutable => FileMode::Executable,
                                EntryKind::Link => FileMode::Symlink,
                                _ => FileMode::Regular,
                            },
                            parent: Some(inode),
                        })
                    }
                };

                entries.insert(name, self.fs.nodes.insert(node));
            }
        }

        match self.get_node_mut(inode)? {
            Node::Directory(dir) => dir.entries = ObjectIdOrContent::Content(entries),
            Node::File(_) => unreachable!("just checked that it's a directory"),
        }

        Ok(())
    }

    /// Look up `name` in the directory `inode`. The directory is loaded first
    /// if necessary. Returns `Err(NotDirectory)` if `inode` isn't a directory.
    fn lookup(&mut self, inode: Inode, name: &str) -> FsResult<Option<Inode>> {
        self.load_directory(inode)?;
        match self.get_node(inode)? {
            Node::Directory(dir) => match &dir.entries {
                ObjectIdOrContent::Content(entries) => Ok(entries.get(name).copied()),
                ObjectIdOrContent::ObjectId(_) => unreachable!("just loaded"),
            },
            Node::File(_) => Err(ErrorCode::NotDirectory.into()),
        }
    }

    /// Insert `child` into the directory `parent` under `name`, replacing any
    /// existing entry. The directory must already be loaded.
    fn insert_entry(&mut self, parent: Inode, name: String, child: Inode) -> FsResult<()> {
        match self.get_node_mut(parent)? {
            Node::Directory(dir) => match &mut dir.entries {
                ObjectIdOrContent::Content(entries) => {
                    entries.insert(name, child);
                    Ok(())
                }
                ObjectIdOrContent::ObjectId(_) => unreachable!("must be loaded first"),
            },
            Node::File(_) => Err(ErrorCode::NotDirectory.into()),
        }
    }

    /// Remove `name` from the directory `parent`. The directory must already be
    /// loaded.
    fn remove_entry(&mut self, parent: Inode, name: &str) -> FsResult<Inode> {
        match self.get_node_mut(parent)? {
            Node::Directory(dir) => match &mut dir.entries {
                ObjectIdOrContent::Content(entries) => {
                    entries.remove(name).ok_or(ErrorCode::NoEntry.into())
                }
                ObjectIdOrContent::ObjectId(_) => unreachable!("must be loaded first"),
            },
            Node::File(_) => Err(ErrorCode::NotDirectory.into()),
        }
    }

    /// Follow a path relative to an existing file or directory.
    /// See https://pubs.opengroup.org/onlinepubs/9799919799/ for details about
    /// POSIX's mad pathname resolution, and https://github.com/WebAssembly/wasi-filesystem/blob/main/path-resolution.md
    /// for WASI specifically.
    ///
    /// Only relative paths are allowed. Absolute paths cause a permission error.
    /// For this function the target file or directory (or symlink) must exist.
    pub fn resolve_path(
        &mut self,
        from: Inode,
        relative_path: &str,
        follow_final_symlink: bool,
    ) -> FsResult<Inode> {
        let mut symlink_follows_remaining = MAX_SYMLINK_FOLLOWS;
        self.resolve_path_impl(
            from,
            relative_path,
            follow_final_symlink,
            &mut symlink_follows_remaining,
        )
    }

    fn resolve_path_impl(
        &mut self,
        from: Inode,
        relative_path: &str,
        follow_final_symlink: bool,
        symlink_follows_remaining: &mut u32,
    ) -> FsResult<Inode> {
        if relative_path.starts_with('/') {
            return Err(ErrorCode::Access.into());
        }
        if relative_path.is_empty() {
            return Err(ErrorCode::NoEntry.into());
        }

        let mut components: Vec<&str> = relative_path.split('/').collect();

        // A trailing slash (or `/.`) means the target must be a directory, and
        // also means we follow it even if it is a symlink, because `foo/` is
        // equivalent to `foo/.`.
        let mut must_be_directory = false;
        while components.len() > 1 {
            match components[components.len() - 1] {
                "" | "." => {
                    must_be_directory = true;
                    components.pop();
                }
                _ => break,
            }
        }

        let mut inode = from;

        for (i, component) in components.iter().enumerate() {
            let is_final = i + 1 == components.len();

            match *component {
                // Either two consecutive slashes "foo//bar", or the whole path
                // was "." or "./".
                "" | "." => {
                    if !self.is_directory(inode)? {
                        return Err(ErrorCode::NotDirectory.into());
                    }
                }
                // Parent. The root's parent is itself, so this can't escape.
                ".." => match self.get_node(inode)? {
                    Node::Directory(dir) => inode = dir.parent,
                    Node::File(_) => return Err(ErrorCode::NotDirectory.into()),
                },
                // Named child.
                name => {
                    let child = self.lookup(inode, name)?.ok_or(ErrorCode::NoEntry)?;

                    let follow = !is_final || follow_final_symlink || must_be_directory;

                    inode = if follow && self.is_symlink(child)? {
                        self.follow_symlink(inode, child, symlink_follows_remaining)?
                    } else {
                        child
                    };
                }
            }
        }

        if must_be_directory && !self.is_directory(inode)? {
            return Err(ErrorCode::NotDirectory.into());
        }

        Ok(inode)
    }

    /// Resolve the symlink `link`, which lives in the directory `parent`.
    fn follow_symlink(
        &mut self,
        parent: Inode,
        link: Inode,
        symlink_follows_remaining: &mut u32,
    ) -> FsResult<Inode> {
        if *symlink_follows_remaining == 0 {
            return Err(ErrorCode::Loop.into());
        }
        *symlink_follows_remaining -= 1;

        let target = self.symlink_target(link)?;

        // An absolute symlink is resolved from the root of the VFS, which is
        // the root of the repo. Like a chroot, it can't escape.
        let (from, target) = match target.strip_prefix('/') {
            Some(target) => (ROOT_INODE, target),
            None => (parent, target.as_str()),
        };

        self.resolve_path_impl(from, target, true, symlink_follows_remaining)
    }

    /// The target path of the symlink `inode`. Git stores it as the blob data.
    pub fn symlink_target(&mut self, inode: Inode) -> FsResult<String> {
        if !self.is_symlink(inode)? {
            return Err(ErrorCode::Invalid.into());
        }
        let content = self.content(inode)?;
        let content = content.lock().expect("content mutex poisoned");
        String::from_utf8(content.clone()).map_err(|_| ErrorCode::IllegalByteSequence.into())
    }

    /// Resolve everything up to the final component of `path`, returning the
    /// containing directory and the final component's name. Used by operations
    /// that create or remove entries, where the final component need not exist.
    pub fn resolve_parent(&mut self, from: Inode, path: &str) -> FsResult<(Inode, String)> {
        if path.starts_with('/') {
            return Err(ErrorCode::Access.into());
        }

        // Trailing slashes are irrelevant here; "foo/bar/" names "bar" too.
        let path = path.trim_end_matches('/');

        let (directory, name) = match path.rsplit_once('/') {
            Some((directory, name)) => (self.resolve_path(from, directory, true)?, name),
            None => (from, path),
        };

        // These don't name something that can be created or removed.
        if name.is_empty() || name == "." || name == ".." {
            return Err(ErrorCode::Invalid.into());
        }

        if !self.is_directory(directory)? {
            return Err(ErrorCode::NotDirectory.into());
        }

        Ok((directory, name.to_owned()))
    }

    /// Like `resolve_parent()`, but also looks up the final component, which
    /// may not exist. Used by `open-at` with the `CREATE` flag.
    pub fn resolve_for_create(
        &mut self,
        from: Inode,
        path: &str,
        follow_final_symlink: bool,
    ) -> FsResult<(Inode, String, Option<Inode>)> {
        let (directory, name) = self.resolve_parent(from, path)?;

        let child = match self.lookup(directory, &name)? {
            Some(child) => {
                if follow_final_symlink && self.is_symlink(child)? {
                    let mut remaining = MAX_SYMLINK_FOLLOWS;
                    // The symlink may point at something that doesn't exist,
                    // in which case we're creating that.
                    self.follow_symlink(directory, child, &mut remaining).ok()
                } else {
                    Some(child)
                }
            }
            None => None,
        };

        Ok((directory, name, child))
    }

    /// The contents of a file, loading it from Git if necessary. The buffer is
    /// shared with any streams that are open on the file.
    pub fn content(&mut self, inode: Inode) -> FsResult<SharedContent> {
        let repo = &self.repo;
        match self
            .fs
            .nodes
            .get_mut(inode)
            .ok_or(ErrorCode::BadDescriptor)?
        {
            Node::File(file) => match &file.content {
                // Content already available.
                ObjectIdOrContent::Content(content) => Ok(content.clone()),
                // Lazily load content from git.
                ObjectIdOrContent::ObjectId(blob_id) => {
                    let mut blob = repo.find_blob(*blob_id).map_err(|_| ErrorCode::NoEntry)?;
                    let content: SharedContent = Arc::new(Mutex::new(blob.take_data()));
                    file.content = ObjectIdOrContent::Content(content.clone());
                    Ok(content)
                }
            },
            Node::Directory(_) => Err(ErrorCode::IsDirectory.into()),
        }
    }

    /// The size of a file in bytes. This avoids loading the whole blob if the
    /// file hasn't been read yet.
    fn size(&self, inode: Inode) -> FsResult<u64> {
        match self.get_node(inode)? {
            Node::File(file) => match &file.content {
                ObjectIdOrContent::Content(content) => {
                    Ok(content.lock().expect("content mutex poisoned").len() as u64)
                }
                ObjectIdOrContent::ObjectId(blob_id) => Ok(self
                    .repo
                    .find_header(*blob_id)
                    .map_err(|_| ErrorCode::NoEntry)?
                    .size()),
            },
            Node::Directory(_) => Ok(0),
        }
    }

    pub fn stat(&self, inode: Inode) -> FsResult<DescriptorStat> {
        let node = self.get_node(inode)?;
        Ok(DescriptorStat {
            type_: node.descriptor_type(),
            // Git doesn't support hard links and the normal case is 1, not 0.
            link_count: 1,
            // In POSIX the size of a symlink is the length of its target, which
            // is exactly what Git stores as the blob data, so this works for
            // symlinks too.
            size: self.size(inode)?,
            // Git doesn't record these.
            data_access_timestamp: None,
            data_modification_timestamp: None,
            status_change_timestamp: None,
        })
    }

    /// The entries of a directory, not including "." or "..", which the WASI
    /// spec says must not be returned.
    pub fn read_directory(&mut self, inode: Inode) -> FsResult<Vec<DirectoryEntry>> {
        self.load_directory(inode)?;

        let children: Vec<(String, Inode)> = match self.get_node(inode)? {
            Node::Directory(dir) => match &dir.entries {
                ObjectIdOrContent::Content(entries) => {
                    entries.iter().map(|(k, v)| (k.clone(), *v)).collect()
                }
                ObjectIdOrContent::ObjectId(_) => unreachable!("just loaded"),
            },
            Node::File(_) => return Err(ErrorCode::NotDirectory.into()),
        };

        children
            .into_iter()
            .map(|(name, child)| {
                Ok(DirectoryEntry {
                    type_: self.descriptor_type(child)?,
                    name,
                })
            })
            .collect()
    }

    /// Create an empty regular file called `name` in the directory `directory`.
    pub fn create_file(&mut self, directory: Inode, name: &str) -> FsResult<Inode> {
        self.create_node(
            directory,
            name,
            Node::File(FileNode {
                content: ObjectIdOrContent::Content(Arc::new(Mutex::new(Vec::new()))),
                mode: FileMode::Regular,
                parent: Some(directory),
            }),
        )
    }

    /// Create a symlink called `name` in `directory`, pointing at `target`.
    pub fn create_symlink(&mut self, directory: Inode, name: &str, target: &str) -> FsResult<()> {
        self.create_node(
            directory,
            name,
            Node::File(FileNode {
                content: ObjectIdOrContent::Content(Arc::new(Mutex::new(
                    target.as_bytes().to_owned(),
                ))),
                mode: FileMode::Symlink,
                parent: Some(directory),
            }),
        )?;
        Ok(())
    }

    /// Create an empty directory called `name` in `directory`.
    pub fn create_directory(&mut self, directory: Inode, name: &str) -> FsResult<()> {
        self.create_node(
            directory,
            name,
            Node::Directory(DirectoryNode {
                entries: ObjectIdOrContent::Content(BTreeMap::new()),
                parent: directory,
            }),
        )?;
        Ok(())
    }

    fn create_node(&mut self, directory: Inode, name: &str, node: Node) -> FsResult<Inode> {
        self.load_directory(directory)?;
        if self.lookup(directory, name)?.is_some() {
            return Err(ErrorCode::Exist.into());
        }

        let inode = self.fs.nodes.insert(node);
        self.insert_entry(directory, name.to_owned(), inode)?;
        self.record_modified(inode);
        Ok(inode)
    }

    /// Remove the file (or symlink) `name` from `directory`.
    pub fn unlink(&mut self, directory: Inode, name: &str) -> FsResult<()> {
        self.load_directory(directory)?;
        let inode = self.lookup(directory, name)?.ok_or(ErrorCode::NoEntry)?;

        if self.is_directory(inode)? {
            return Err(ErrorCode::IsDirectory.into());
        }

        // Record the path while it still has one.
        self.record_modified(inode);

        self.remove_entry(directory, name)?;
        if let Node::File(file) = self.get_node_mut(inode)? {
            file.parent = None;
        }

        Ok(())
    }

    /// Remove the empty directory `name` from `directory`.
    pub fn remove_directory(&mut self, directory: Inode, name: &str) -> FsResult<()> {
        self.load_directory(directory)?;
        let inode = self.lookup(directory, name)?.ok_or(ErrorCode::NoEntry)?;

        if !self.is_directory(inode)? {
            return Err(ErrorCode::NotDirectory.into());
        }
        if !self.read_directory(inode)?.is_empty() {
            return Err(ErrorCode::NotEmpty.into());
        }

        self.remove_entry(directory, name)?;

        // An empty directory doesn't exist as far as Git is concerned, so
        // there's nothing to record.

        Ok(())
    }

    /// Rename `old_name` in `old_directory` to `new_name` in `new_directory`.
    pub fn rename(
        &mut self,
        old_directory: Inode,
        old_name: &str,
        new_directory: Inode,
        new_name: &str,
    ) -> FsResult<()> {
        self.load_directory(old_directory)?;
        self.load_directory(new_directory)?;

        let inode = self
            .lookup(old_directory, old_name)?
            .ok_or(ErrorCode::NoEntry)?;

        if old_directory == new_directory && old_name == new_name {
            return Ok(());
        }

        let is_directory = self.is_directory(inode)?;

        // Moving a directory into itself would create a loop that we could
        // never reconstruct a path from.
        if is_directory && self.is_ancestor_of(inode, new_directory)? {
            return Err(ErrorCode::Invalid.into());
        }

        // POSIX allows renaming onto an existing entry of the same type.
        if let Some(existing) = self.lookup(new_directory, new_name)? {
            match (is_directory, self.is_directory(existing)?) {
                (true, true) => {
                    if !self.read_directory(existing)?.is_empty() {
                        return Err(ErrorCode::NotEmpty.into());
                    }
                    self.remove_entry(new_directory, new_name)?;
                }
                (false, false) => {
                    self.unlink(new_directory, new_name)?;
                }
                (true, false) => return Err(ErrorCode::NotDirectory.into()),
                (false, true) => return Err(ErrorCode::IsDirectory.into()),
            }
        }

        // Record every path that is going away...
        let old_path = self.path_of(inode);
        if let Some(old_path) = old_path {
            self.record_subtree(inode, old_path)?;
        }

        self.remove_entry(old_directory, old_name)?;
        self.insert_entry(new_directory, new_name.to_owned(), inode)?;
        match self.get_node_mut(inode)? {
            Node::File(file) => file.parent = Some(new_directory),
            Node::Directory(dir) => dir.parent = new_directory,
        }

        // ...and every path that is appearing.
        if let Some(new_path) = self.path_of(inode) {
            self.record_subtree(inode, new_path)?;
        }

        Ok(())
    }

    /// Whether `ancestor` is `inode` or one of its parent directories.
    fn is_ancestor_of(&self, ancestor: Inode, inode: Inode) -> FsResult<bool> {
        let mut current = inode;
        for _ in 0..MAX_PATH_DEPTH {
            if current == ancestor {
                return Ok(true);
            }
            if current == ROOT_INODE {
                return Ok(false);
            }
            current = match self.get_node(current)? {
                Node::Directory(dir) => dir.parent,
                Node::File(file) => match file.parent {
                    Some(parent) => parent,
                    None => return Ok(false),
                },
            };
        }
        Err(ErrorCode::Loop.into())
    }

    /// Truncate or zero-extend a file to `size` bytes.
    pub fn set_size(&mut self, inode: Inode, size: u64) -> FsResult<()> {
        if self.is_directory(inode)? {
            return Err(ErrorCode::IsDirectory.into());
        }
        let size = usize::try_from(size).map_err(|_| ErrorCode::FileTooLarge)?;
        let content = self.content(inode)?;
        content
            .lock()
            .expect("content mutex poisoned")
            .resize(size, 0);
        self.record_modified(inode);
        Ok(())
    }

    /// Write `data` at `offset`, zero-filling any gap. Returns the number of
    /// bytes written, which is always all of them.
    pub fn write_at(&mut self, inode: Inode, offset: u64, data: &[u8]) -> FsResult<u64> {
        if self.is_directory(inode)? {
            return Err(ErrorCode::IsDirectory.into());
        }
        let content = self.content(inode)?;
        write_at(
            &mut content.lock().expect("content mutex poisoned"),
            offset,
            data,
        )?;
        self.record_modified(inode);
        Ok(data.len() as u64)
    }

    /// Read up to `length` bytes at `offset`. Also returns whether the end of
    /// the file was reached.
    pub fn read_at(&mut self, inode: Inode, offset: u64, length: u64) -> FsResult<(Vec<u8>, bool)> {
        if self.is_directory(inode)? {
            return Err(ErrorCode::IsDirectory.into());
        }
        let content = self.content(inode)?;
        let content = content.lock().expect("content mutex poisoned");

        let Ok(offset) = usize::try_from(offset) else {
            return Ok((Vec::new(), true));
        };
        if offset >= content.len() {
            return Ok((Vec::new(), true));
        }

        let length = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(content.len() - offset);
        let eof = offset + length >= content.len();
        Ok((content[offset..offset + length].to_owned(), eof))
    }
}

/// Write `data` into `content` at `offset`, growing it (zero-filled) as needed.
pub fn write_at(content: &mut Vec<u8>, offset: u64, data: &[u8]) -> FsResult<()> {
    let offset = usize::try_from(offset).map_err(|_| ErrorCode::FileTooLarge)?;
    let end = offset
        .checked_add(data.len())
        .ok_or(ErrorCode::FileTooLarge)?;

    if end > content.len() {
        content.resize(end, 0);
    }
    content[offset..end].copy_from_slice(data);
    Ok(())
}
