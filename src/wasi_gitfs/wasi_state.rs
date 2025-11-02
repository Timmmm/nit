use std::collections::{BTreeMap, HashMap, hash_map};

use anyhow::Context as _;
use gix::{ObjectId, Repository, objs::tree::EntryKind};
use wasmtime::component::{HasData, Linker, Resource};
use wasmtime_wasi::{
    ResourceTable, ResourceTableError, WasiCtx, WasiCtxView, WasiView,
    p2::{
        FsError, FsResult, ReaddirIterator, StreamError, StreamResult,
        bindings::filesystem::{
            self,
            types::{
                Advice, Descriptor, DescriptorFlags, DescriptorStat, DescriptorType,
                DirectoryEntry, ErrorCode, Filesize, MetadataHashValue, NewTimestamp, OpenFlags,
                PathFlags,
            },
        },
    },
};

pub struct WasiState {
    pub wasi_ctx: WasiCtx,
    // This is basically a `Vec<any>`.
    pub resource_table: ResourceTable,
    // The git filesystem.
    pub gitfs: GitFs,
}

impl WasiView for WasiState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

// A descriptor is the state associated with a file descriptor. It is stored
// in the resource table. Normally this would hold any information you need
// to access the underlying file/directory (e.g. a POSIX file descriptor).
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct GitFsDescriptor {
    /// TODO: Need a way to refer to a specific DirectoryOrFile.
    /// We'll also need a way to get a parent of DirectoryOrFile.
    /// Could use an arena an indices I guess?
    // What kind of Git object it is (blob, tree etc.)
    pub kind: EntryKind,
    // Git commit ID.
    pub id: ObjectId,
}

// Type returned by `read_dir()` that allows iterating through directory entries.
pub struct GitFsReaddirIterator {
    pub entries: Vec<DirectoryEntry>,
}

trait ResourceTableExt {
    fn push_gitfs_descriptor(
        &mut self,
        gitfs_descriptor: GitFsDescriptor,
    ) -> anyhow::Result<Resource<Descriptor>>;
    fn get_gitfs_descriptor(
        &self,
        key: &Resource<Descriptor>,
    ) -> Result<&GitFsDescriptor, ResourceTableError>;
    fn get_mut_gitfs_descriptor(
        &mut self,
        key: &Resource<Descriptor>,
    ) -> Result<&mut GitFsDescriptor, ResourceTableError>;
    fn delete_gitfs_descriptor(
        &mut self,
        key: Resource<Descriptor>,
    ) -> std::result::Result<GitFsDescriptor, ResourceTableError>;

    fn push_gitfs_readdiriterator(
        &mut self,
        gitfs_readdiriterator: GitFsReaddirIterator,
    ) -> anyhow::Result<Resource<ReaddirIterator>>;
    fn get_gitfs_readdiriterator(
        &self,
        key: &Resource<ReaddirIterator>,
    ) -> Result<&GitFsReaddirIterator, ResourceTableError>;
    fn get_mut_gitfs_readdiriterator(
        &mut self,
        key: &Resource<ReaddirIterator>,
    ) -> Result<&mut GitFsReaddirIterator, ResourceTableError>;
    fn delete_gitfs_readdiriterator(
        &mut self,
        key: Resource<ReaddirIterator>,
    ) -> std::result::Result<GitFsReaddirIterator, ResourceTableError>;
}

impl ResourceTableExt for ResourceTable {
    fn push_gitfs_descriptor(
        &mut self,
        gitfs_descriptor: GitFsDescriptor,
    ) -> anyhow::Result<Resource<Descriptor>> {
        let my_resource = self.push(gitfs_descriptor)?;
        Ok(if my_resource.owned() {
            Resource::new_own(my_resource.rep())
        } else {
            Resource::new_borrow(my_resource.rep())
        })
    }
    fn get_gitfs_descriptor(
        &self,
        key: &Resource<Descriptor>,
    ) -> Result<&GitFsDescriptor, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.get(&my_key)
    }
    fn get_mut_gitfs_descriptor(
        &mut self,
        key: &Resource<Descriptor>,
    ) -> Result<&mut GitFsDescriptor, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.get_mut(&my_key)
    }
    fn delete_gitfs_descriptor(
        &mut self,
        key: Resource<Descriptor>,
    ) -> std::result::Result<GitFsDescriptor, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.delete(my_key)
    }

    fn push_gitfs_readdiriterator(
        &mut self,
        gitfs_readdiriterator: GitFsReaddirIterator,
    ) -> anyhow::Result<Resource<ReaddirIterator>> {
        let my_resource = self.push(gitfs_readdiriterator)?;
        Ok(if my_resource.owned() {
            Resource::new_own(my_resource.rep())
        } else {
            Resource::new_borrow(my_resource.rep())
        })
    }

    fn get_gitfs_readdiriterator(
        &self,
        key: &Resource<ReaddirIterator>,
    ) -> Result<&GitFsReaddirIterator, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.get(&my_key)
    }

    fn get_mut_gitfs_readdiriterator(
        &mut self,
        key: &Resource<ReaddirIterator>,
    ) -> Result<&mut GitFsReaddirIterator, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.get_mut(&my_key)
    }

    fn delete_gitfs_readdiriterator(
        &mut self,
        key: Resource<ReaddirIterator>,
    ) -> std::result::Result<GitFsReaddirIterator, ResourceTableError> {
        let my_key = if key.owned() {
            Resource::new_own(key.rep())
        } else {
            Resource::new_borrow(key.rep())
        };
        self.delete(my_key)
    }
}

pub struct GitFs {
    // Git repository.
    repo: Repository,

    // Root directory
    root: Directory,

    // Original blob content by git hash ID. When we read a blob it goes into here.
    // When we write one it goes into somewhere else...
    // There's no garbage collection currently - if you open a file, read
    // it and then close it, it will stay here. This would be relatively easy
    // to fix with a reference count.
    blob_contents: HashMap<ObjectId, Vec<u8>>,

    // Map from blob ID to its parent directory so we can implement `..` in
    // path traversal. We add to this every time we open a file.
    // There's no garbage collection currently - if you open a directory
    // and close it this will stay here. This would be relatively easy to fix
    // with a reference count, but it's probably not worth it in this case.
    parent: HashMap<ObjectId, ObjectId>,
}

impl GitFs {
    // Create a new GitFs instance.
    fn new(repo: Repository, tree: ObjectId) -> Self {
        Self {
            repo,
            root: Directory::Unopened(tree),
            blob_contents: HashMap::new(),
            parent: HashMap::new(),
        }
    }

    // Follow a path relative to an existing file or directory.
    // See https://pubs.opengroup.org/onlinepubs/9799919799/ for details about
    // POSIX's mad pathname resolution, and https://github.com/WebAssembly/wasi-filesystem/blob/main/path-resolution.md
    // for WASI specifically.
    //
    // Only relative paths are allowed. Absolute paths cause a permission error.
    // For this function the target file or directory (or symlink) must exist.
    fn resolve_path(
        &mut self,
        from: GitFsDescriptor,
        relative_path: &str,
        follow_final_symlink: bool,
    ) -> FsResult<GitFsDescriptor> {
        if relative_path.starts_with('/') {
            return Err(ErrorCode::Access.into());
        }

        let mut descriptor = from;

        // TODO: Allow a maximum of 40 symlink follows. Based on this value
        // https://github.com/wasix-org/wasix-libc/blob/28158c2ece7401604a9f6a409be320b47fffe78e/expected/wasm32-wasi/predefined-macros.txt#L4617
        let mut symlink_follow_remaining = 40;

        // So we can handle the last component separately.
        for component in relative_path.split('/') {
            match descriptor.kind {
                EntryKind::Tree => {
                    match component {
                        // Either two consecutive slashes "foo/bar//baz" or a trailing slash "foo/bar/".
                        "" => continue,
                        "." => continue,
                        ".." => {
                            // If there's no parent we're trying to .. above the root, which is not allowed by WASI.
                            descriptor.id =
                                *self.parent.get(&descriptor.id).ok_or(ErrorCode::Access)?;
                            // Parent directory must be a directory.
                            descriptor.kind = EntryKind::Tree;
                        }
                        // Named child.
                        _ => {
                            // Open the current directory and find the child component.
                            let tree = self
                                .repo
                                .find_tree(descriptor.id)
                                .map_err(|_| ErrorCode::NoEntry)?;
                            // Find the child object.
                            let entry = tree.find_entry(component).ok_or(ErrorCode::NoEntry)?;

                            descriptor.id = entry.id().detach();
                            descriptor.kind = entry.kind();
                        }
                    }
                }
                EntryKind::Blob | EntryKind::BlobExecutable => {
                    // Can't get a child of a file.
                    return Err(ErrorCode::NotDirectory.into());
                }
                EntryKind::Link => {
                    todo!("symlink support")
                }
                EntryKind::Commit => todo!(),
            }
        }

        if descriptor.kind == EntryKind::Link && follow_final_symlink {
            todo!("symlink support")
        }
        Ok(descriptor)
    }

    // Read a full blob (the only API Gix gives because it may be compressed
    // or based on diffs). It is cached.
    fn read_blob(&mut self, id: ObjectId) -> FsResult<&[u8]> {
        match self.blob_contents.entry(id) {
            hash_map::Entry::Vacant(vacant_entry) => {
                let mut blob = self.repo.find_blob(id).map_err(|_| ErrorCode::NoEntry)?;
                let data = blob.take_data();
                Ok(vacant_entry.insert(data))
            }
            hash_map::Entry::Occupied(occupied_entry) => Ok(occupied_entry.into_mut()),
        }
    }
}

fn gix_entry_kind_to_descriptor_type(kind: EntryKind) -> DescriptorType {
    match kind {
        EntryKind::Tree => DescriptorType::Directory,
        EntryKind::Blob | EntryKind::BlobExecutable => DescriptorType::RegularFile,
        EntryKind::Link => DescriptorType::SymbolicLink,
        // For simplicity, submodules are treated as empty directories.
        EntryKind::Commit => DescriptorType::Directory,
    }
}

// The preopens are the only place the filesystem is provided a Descriptor,
// from which to try open_at to get more Descriptors. If we don't provide
// anything here, none of the methods on Descriptor will ever be reachable,
// because Resources are unforgable (the runtime will trap bogus indexes).
impl filesystem::preopens::Host for WasiState {
    fn get_directories(&mut self) -> anyhow::Result<Vec<(Resource<Descriptor>, String)>> {
        // We have one hard-coded pre-open: `/`.
        Ok(vec![(
            // Create a new file descriptor and add it to the resource table,
            // returning its index in the table.
            self.resource_table
                .push_gitfs_descriptor(GitFsDescriptor {
                    kind: EntryKind::Tree,
                    id: self.gitfs.tree,
                })
                .with_context(|| format!("failed to push root preopen"))?,
            // Path
            "/".to_string(),
        )])
    }
}

// Allow performing all the usual filesystem operations on a file descriptor.
impl filesystem::types::HostDescriptor for WasiState {
    fn read_via_stream(
        &mut self,
        fd: Resource<Descriptor>,
        offset: u64,
    ) -> FsResult<Resource<Box<(dyn wasmtime_wasi::p2::InputStream + 'static)>>> {
        let descriptor = self.resource_table.get_mut_gitfs_descriptor(&fd).unwrap();
        let data = self.gitfs.read_blob(descriptor.id)?;
        // TODO: Don't copy all the data.
        // TODO: Handle usize=32 bit. In fact, we probably can't actually read files
        // stored in Git that are more than 4 GB?
        let read_stream = ReadStream {
            data: bytes::Bytes::copy_from_slice(data),
            offset: offset as usize,
        };
        let boxed_read_stream: Box<dyn wasmtime_wasi::p2::InputStream> = Box::new(read_stream);
        // TODO: Drop from the resource table at some point somehow? Might have to use push_child?
        Ok(self.resource_table.push(boxed_read_stream).unwrap())
    }

    fn write_via_stream(
        &mut self,
        _fd: Resource<Descriptor>,
        _offset: u64,
    ) -> FsResult<Resource<Box<(dyn wasmtime_wasi::p2::OutputStream + 'static)>>> {
        Err(ErrorCode::ReadOnly.into())
    }

    fn append_via_stream(
        &mut self,
        _fd: Resource<Descriptor>,
    ) -> FsResult<Resource<Box<(dyn wasmtime_wasi::p2::OutputStream + 'static)>>> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn advise(
        &mut self,
        _fd: Resource<Descriptor>,
        _offset: Filesize,
        _length: Filesize,
        _advice: Advice,
    ) -> FsResult<()> {
        // Not used.
        Ok(())
    }

    async fn sync_data(&mut self, _fd: Resource<Descriptor>) -> FsResult<()> {
        //  Sync not needed.
        Ok(())
    }

    async fn get_flags(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorFlags> {
        // TODO: I guess we will need to record in the descriptor how it was opened.
        Ok(DescriptorFlags::READ)
    }

    async fn get_type(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorType> {
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        Ok(gix_entry_kind_to_descriptor_type(descriptor.kind))
    }

    async fn set_size(&mut self, _fd: Resource<Descriptor>, _size: Filesize) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn set_times(
        &mut self,
        _fd: Resource<Descriptor>,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn read(
        &mut self,
        fd: Resource<Descriptor>,
        length: Filesize,
        offset: Filesize,
    ) -> FsResult<(Vec<u8>, bool)> {
        let descriptor = self.resource_table.get_mut_gitfs_descriptor(&fd).unwrap();
        let blob = self.gitfs.read_blob(descriptor.id)?;
        // TODO: Handle usize properly.
        let length = length as usize;
        let offset = offset as usize;
        if offset >= blob.len() {
            // TODO: Should this be an error?
            Ok((Vec::new(), true))
        } else {
            let length = length.min(blob.len() - offset);
            let eof = offset + length >= blob.len();
            Ok((blob[offset..(offset + length)].to_owned(), eof))
        }
    }

    async fn write(
        &mut self,
        _fd: Resource<Descriptor>,
        _buffer: Vec<u8>,
        _offset: Filesize,
    ) -> FsResult<Filesize> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn read_directory(
        &mut self,
        fd: Resource<Descriptor>,
    ) -> FsResult<Resource<ReaddirIterator>> {
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // TODO: Could use `find_tree_iter()` ideally but I don't know if the
        // lifetime issues are easy to deal with, or if it makes any performance difference.
        let tree = self.gitfs.repo.find_tree(descriptor.id).unwrap();
        let mut entries: Vec<_> = tree
            .iter()
            .map(|entry| {
                let entry = entry.unwrap();
                DirectoryEntry {
                    type_: gix_entry_kind_to_descriptor_type(entry.kind()),
                    name: entry.filename().to_string(),
                }
            })
            .collect();
        // Reverse because we pop them off the back when reading.
        // TODO: Probably can do this more efficiently somehow.
        entries.reverse();
        Ok(self
            .resource_table
            .push_gitfs_readdiriterator(GitFsReaddirIterator { entries })
            .unwrap())
    }

    async fn sync(&mut self, _fd: Resource<Descriptor>) -> FsResult<()> {
        // Sync not needed.
        Ok(())
    }

    async fn create_directory_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path: String,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn stat(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorStat> {
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        Ok(DescriptorStat {
            type_: gix_entry_kind_to_descriptor_type(descriptor.kind),
            // Git doesn't support hard links and the normal case is 1, not 0.
            link_count: 1,
            // In posix for symlinks this is the size of the path. Does that apply here?
            size: match descriptor.kind {
                // For symlinks this should return the size of the path, which Git
                // conveniently stores as the blob data, so we can use the same code.
                EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                    self.gitfs.repo.find_header(descriptor.id).unwrap().size()
                }
                // Directory or submodule.
                EntryKind::Tree | EntryKind::Commit => 0,
            },
            // Git doesn't record this.
            data_access_timestamp: None,
            data_modification_timestamp: None,
            status_change_timestamp: None,
        })
    }

    async fn stat_at(
        &mut self,
        fd: Resource<Descriptor>,
        path_flags: PathFlags,
        path: String,
    ) -> FsResult<DescriptorStat> {
        let from_descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let follow_final_symlink: bool = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        let descriptor = self
            .gitfs
            .resolve_path(*from_descriptor, &path, follow_final_symlink)?;

        // TODO: Extract into function.
        Ok(DescriptorStat {
            type_: gix_entry_kind_to_descriptor_type(descriptor.kind),
            // Git doesn't support hard links and the normal case is 1, not 0.
            link_count: 1,
            // In posix for symlinks this is the size of the path. Does that apply here?
            size: match descriptor.kind {
                // For symlinks this should return the size of the path, which Git
                // conveniently stores as the blob data, so we can use the same code.
                EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                    self.gitfs.repo.find_header(descriptor.id).unwrap().size()
                }
                // Directory or submodule.
                EntryKind::Tree | EntryKind::Commit => 0,
            },
            // Git doesn't record this.
            data_access_timestamp: None,
            data_modification_timestamp: None,
            status_change_timestamp: None,
        })
    }

    async fn set_times_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path_flags: PathFlags,
        _path: String,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn link_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path_flags: PathFlags,
        _old_path: String,
        _new_descriptor: Resource<Descriptor>,
        _new_path: String,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    // Open the relative path `path`, relative to the directory `fd`. Unlike
    // POSIX `openat` path must be relative.
    async fn open_at(
        &mut self,
        fd: Resource<Descriptor>,
        path_flags: PathFlags,
        path: String,
        open_flags: OpenFlags,
        flags: DescriptorFlags,
    ) -> FsResult<Resource<Descriptor>> {
        if open_flags.contains(OpenFlags::CREATE)
            || open_flags.contains(OpenFlags::TRUNCATE)
            || flags.contains(DescriptorFlags::WRITE)
        {
            return Err(ErrorCode::ReadOnly.into());
        }

        // TODO: Handle other DescriptorFlags maybe.

        let from_descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let follow_final_symlink: bool = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        let descriptor = self
            .gitfs
            .resolve_path(*from_descriptor, &path, follow_final_symlink)?;

        if open_flags.contains(OpenFlags::EXCLUSIVE) {
            return Err(ErrorCode::Exist.into());
        }

        if open_flags.contains(OpenFlags::DIRECTORY) && descriptor.kind != EntryKind::Tree {
            return Err(ErrorCode::NotDirectory.into());
        }

        Ok(self
            .resource_table
            .push_gitfs_descriptor(descriptor)
            .unwrap())
    }

    async fn readlink_at(&mut self, fd: Resource<Descriptor>, path: String) -> FsResult<String> {
        let from_descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let descriptor = self.gitfs.resolve_path(*from_descriptor, &path, false)?;

        if descriptor.kind != EntryKind::Link {
            return Err(ErrorCode::Invalid.into());
        }

        let mut link = self
            .gitfs
            .repo
            .find_blob(descriptor.id)
            .map_err(|_| ErrorCode::NoEntry)?;
        let link_str =
            String::from_utf8(link.take_data()).map_err(|_| ErrorCode::IllegalByteSequence)?;
        Ok(link_str.to_owned())
    }

    async fn remove_directory_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path: String,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn rename_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path: String,
        _new_descriptor: Resource<Descriptor>,
        _new_path: String,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn symlink_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path: String,
        _new_path: String,
    ) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn unlink_file_at(&mut self, _fd: Resource<Descriptor>, _path: String) -> FsResult<()> {
        Err(ErrorCode::ReadOnly.into())
    }

    async fn is_same_object(
        &mut self,
        fd: Resource<Descriptor>,
        other: Resource<Descriptor>,
    ) -> wasmtime::Result<bool> {
        let fd = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let other = self.resource_table.get_gitfs_descriptor(&other).unwrap();
        Ok(fd == other)
    }

    async fn metadata_hash(&mut self, fd: Resource<Descriptor>) -> FsResult<MetadataHashValue> {
        // Kind of unclear what the use case for this is if you ask me.
        // While this is read-only we can just return the object ID which is long enough.
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        Ok(MetadataHashValue {
            lower: u64::from_le_bytes(descriptor.id.as_bytes()[0..8].try_into().unwrap()),
            upper: u64::from_le_bytes(descriptor.id.as_bytes()[8..16].try_into().unwrap()),
        })
    }

    async fn metadata_hash_at(
        &mut self,
        fd: Resource<Descriptor>,
        _path_flags: PathFlags,
        _path: String,
    ) -> FsResult<MetadataHashValue> {
        // Kind of unclear what the use case for this is if you ask me.
        // While this is read-only we can just return the object ID which is long enough.
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        Ok(MetadataHashValue {
            lower: u64::from_le_bytes(descriptor.id.as_bytes()[0..8].try_into().unwrap()),
            upper: u64::from_le_bytes(descriptor.id.as_bytes()[8..16].try_into().unwrap()),
        })
    }

    fn drop(&mut self, fd: Resource<Descriptor>) -> anyhow::Result<()> {
        // This will drop the `Descriptor` which should close the file.
        self.resource_table.delete_gitfs_descriptor(fd)?;
        Ok(())
    }
}

// Allow iterating through a directory returned by `read_directory()`.
impl filesystem::types::HostDirectoryEntryStream for WasiState {
    // Get the next directory entry or None.
    async fn read_directory_entry(
        &mut self,
        stream: Resource<ReaddirIterator>,
    ) -> FsResult<Option<DirectoryEntry>> {
        let stream = self
            .resource_table
            .get_mut_gitfs_readdiriterator(&stream)
            .unwrap();
        Ok(stream.entries.pop())
    }

    fn drop(&mut self, stream: Resource<ReaddirIterator>) -> anyhow::Result<()> {
        self.resource_table.delete_gitfs_readdiriterator(stream)?;
        Ok(())
    }
}

impl filesystem::types::Host for WasiState {
    fn convert_error_code(&mut self, err: FsError) -> wasmtime::Result<ErrorCode> {
        err.downcast()
    }

    fn filesystem_error_code(
        &mut self,
        err: Resource<anyhow::Error>,
    ) -> anyhow::Result<Option<ErrorCode>> {
        let err = self.resource_table.get(&err)?;

        // TODO: Do something here?

        Ok(None)
    }
}

struct ReadStream {
    data: bytes::Bytes,
    offset: usize,
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::Pollable for ReadStream {
    /// An asynchronous function which resolves when this object's readiness
    /// operation is ready.
    ///
    /// This function is invoked as part of `poll` in `wasi:io/poll`. The
    /// meaning of when this function Returns depends on what object this
    /// [`Pollable`] is attached to. When the returned future resolves then the
    /// corresponding call to `wasi:io/poll` will return.
    ///
    /// Note that this method does not return an error. Returning an error
    /// should be done through accessors on the object that this `pollable` is
    /// connected to. The call to `wasi:io/poll` itself does not return errors,
    /// only a list of ready objects.
    async fn ready(&mut self) {
        // It's always ready.
    }
}

impl wasmtime_wasi::p2::InputStream for ReadStream {
    /// Reads up to `size` bytes, returning a buffer holding these bytes on
    /// success.
    ///
    /// This function does not block the current thread and is the equivalent of
    /// a non-blocking read. On success all bytes read are returned through
    /// `Bytes`, which is no larger than the `size` provided. If the returned
    /// list of `Bytes` is empty then no data is ready to be read at this time.
    ///
    /// # Errors
    ///
    /// The [`StreamError`] return value communicates when this stream is
    /// closed, when a read fails, or when a trap should be generated.
    fn read(&mut self, size: usize) -> StreamResult<bytes::Bytes> {
        if self.offset >= self.data.len() {
            Err(StreamError::Closed)
        } else {
            let size = size.min(self.data.len() - self.offset);
            let offset = self.offset;
            self.offset += size;
            Ok(self.data.slice(offset..offset + size))
        }
    }
}

struct HasWasiFs;

impl HasData for HasWasiFs {
    type Data<'a> = &'a mut WasiState;
}

pub fn add_to_linker_async(linker: &mut Linker<WasiState>) -> anyhow::Result<()> {
    filesystem::types::add_to_linker::<WasiState, HasWasiFs>(linker, |t| t)?;
    filesystem::preopens::add_to_linker::<WasiState, HasWasiFs>(linker, |t| t)?;
    Ok(())
}
