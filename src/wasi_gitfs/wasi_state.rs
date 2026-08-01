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

use crate::wasi_gitfs::gitfs::{GitFs, Inode, ROOT_INODE, SharedContent, write_at};

pub struct WasiState {
    pub wasi_ctx: WasiCtx,
    // This is basically a `Vec<any>`.
    pub resource_table: ResourceTable,
    // The git filesystem. This is a *mutable* filesystem backed by a Git repository.
    // It also tracks the paths that may have been modified; see `GitFs::modifications()`.
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
//
// In our case it *is* the file descriptor, and it contains the inode index.
#[derive(Copy, Clone)]
pub struct GitFsDescriptor {
    pub inode: Inode,
    /// The flags it was opened with. Needed by `get_flags()`, and to reject
    /// writes to a file that was opened read-only.
    pub flags: DescriptorFlags,
}

/// Type returned by `read_dir()` that allows iterating through directory entries.
pub struct GitFsReaddirIterator {
    pub entries: Vec<DirectoryEntry>,
}

/// Extension trait for ResourceTable to let us store `GitFsDescriptor`s in it easily,
/// but pretending they are wasmtime's `Descriptor`s (which actually represent
/// real files on disk). Unfortunately wasmtime doesn't let us choose the
/// `Descriptor` type so we just lie to it.
///
/// This mirrors the whole of the `ResourceTable` API even though we don't
/// currently need all of it.
#[allow(dead_code)]
trait ResourceTableExt {
    fn push_gitfs_descriptor(
        &mut self,
        gitfs_descriptor: GitFsDescriptor,
    ) -> Result<Resource<Descriptor>, ResourceTableError>;
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
    ) -> Result<Resource<ReaddirIterator>, ResourceTableError>;
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
    ) -> Result<Resource<Descriptor>, ResourceTableError> {
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
    ) -> Result<Resource<ReaddirIterator>, ResourceTableError> {
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

impl WasiState {
    /// The descriptor for `fd`. A bogus handle is a trap, not an error, but
    /// wasmtime traps for us if we return the `ResourceTableError`.
    fn descriptor(&self, fd: &Resource<Descriptor>) -> FsResult<GitFsDescriptor> {
        Ok(*self.resource_table.get_gitfs_descriptor(fd)?)
    }
}

// The preopens are the only place the filesystem is provided a Descriptor,
// from which to try open_at to get more Descriptors. If we don't provide
// anything here, none of the methods on Descriptor will ever be reachable,
// because Resources are unforgable (the runtime will trap bogus indexes).
impl filesystem::preopens::Host for WasiState {
    fn get_directories(&mut self) -> wasmtime::Result<Vec<(Resource<Descriptor>, String)>> {
        // We have one hard-coded pre-open: `/`.
        // TODO: Use the path of the repo, so that paths printed by the linters are correct.
        Ok(vec![(
            // Create a new file descriptor and add it to the resource table,
            // returning its index in the table.
            self.resource_table.push_gitfs_descriptor(GitFsDescriptor {
                inode: ROOT_INODE,
                flags: DescriptorFlags::READ | DescriptorFlags::MUTATE_DIRECTORY,
            })?,
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
    ) -> FsResult<Resource<Box<dyn wasmtime_wasi::p2::InputStream + 'static>>> {
        let descriptor = self.descriptor(&fd)?;
        if !descriptor.flags.contains(DescriptorFlags::READ) {
            return Err(ErrorCode::BadDescriptor.into());
        }

        // The stream shares the file's buffer, so it sees writes made while it
        // is open, and we don't have to copy the whole file.
        let read_stream = ReadStream {
            content: self.gitfs.content(descriptor.inode)?,
            offset: usize::try_from(offset).map_err(|_| ErrorCode::FileTooLarge)?,
        };
        let boxed_read_stream: Box<dyn wasmtime_wasi::p2::InputStream> = Box::new(read_stream);
        // TODO: Drop from the resource table at some point somehow? Might have to use push_child?
        Ok(self.resource_table.push(boxed_read_stream)?)
    }

    fn write_via_stream(
        &mut self,
        fd: Resource<Descriptor>,
        offset: u64,
    ) -> FsResult<Resource<Box<dyn wasmtime_wasi::p2::OutputStream + 'static>>> {
        self.open_write_stream(fd, Position::At(offset))
    }

    fn append_via_stream(
        &mut self,
        fd: Resource<Descriptor>,
    ) -> FsResult<Resource<Box<dyn wasmtime_wasi::p2::OutputStream + 'static>>> {
        self.open_write_stream(fd, Position::Append)
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
        // Sync not needed.
        Ok(())
    }

    async fn get_flags(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorFlags> {
        Ok(self.descriptor(&fd)?.flags)
    }

    async fn get_type(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorType> {
        let descriptor = self.descriptor(&fd)?;
        self.gitfs.descriptor_type(descriptor.inode)
    }

    async fn set_size(&mut self, fd: Resource<Descriptor>, size: Filesize) -> FsResult<()> {
        let descriptor = self.descriptor(&fd)?;
        if !descriptor.flags.contains(DescriptorFlags::WRITE) {
            return Err(ErrorCode::BadDescriptor.into());
        }
        self.gitfs.set_size(descriptor.inode, size)
    }

    async fn set_times(
        &mut self,
        _fd: Resource<Descriptor>,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        // We don't store timestamps at all (`stat()` reports none), so rather
        // than pretend to set them, say we can't.
        Err(ErrorCode::NotPermitted.into())
    }

    async fn read(
        &mut self,
        fd: Resource<Descriptor>,
        length: Filesize,
        offset: Filesize,
    ) -> FsResult<(Vec<u8>, bool)> {
        let descriptor = self.descriptor(&fd)?;
        if !descriptor.flags.contains(DescriptorFlags::READ) {
            return Err(ErrorCode::BadDescriptor.into());
        }
        self.gitfs.read_at(descriptor.inode, offset, length)
    }

    async fn write(
        &mut self,
        fd: Resource<Descriptor>,
        buffer: Vec<u8>,
        offset: Filesize,
    ) -> FsResult<Filesize> {
        let descriptor = self.descriptor(&fd)?;
        if !descriptor.flags.contains(DescriptorFlags::WRITE) {
            return Err(ErrorCode::BadDescriptor.into());
        }
        self.gitfs.write_at(descriptor.inode, offset, &buffer)
    }

    async fn read_directory(
        &mut self,
        fd: Resource<Descriptor>,
    ) -> FsResult<Resource<ReaddirIterator>> {
        let descriptor = self.descriptor(&fd)?;
        let mut entries = self.gitfs.read_directory(descriptor.inode)?;
        // Reverse because we pop them off the back when reading.
        entries.reverse();
        Ok(self
            .resource_table
            .push_gitfs_readdiriterator(GitFsReaddirIterator { entries })?)
    }

    async fn sync(&mut self, _fd: Resource<Descriptor>) -> FsResult<()> {
        // Sync not needed.
        Ok(())
    }

    async fn create_directory_at(
        &mut self,
        fd: Resource<Descriptor>,
        path: String,
    ) -> FsResult<()> {
        let descriptor = self.descriptor(&fd)?;
        let (directory, name) = self.gitfs.resolve_parent(descriptor.inode, &path)?;
        self.gitfs.create_directory(directory, &name)
    }

    async fn stat(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorStat> {
        let descriptor = self.descriptor(&fd)?;
        self.gitfs.stat(descriptor.inode)
    }

    async fn stat_at(
        &mut self,
        fd: Resource<Descriptor>,
        path_flags: PathFlags,
        path: String,
    ) -> FsResult<DescriptorStat> {
        let descriptor = self.descriptor(&fd)?;
        let follow_final_symlink = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        let inode = self
            .gitfs
            .resolve_path(descriptor.inode, &path, follow_final_symlink)?;
        self.gitfs.stat(inode)
    }

    async fn set_times_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path_flags: PathFlags,
        _path: String,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        // See `set_times()`.
        Err(ErrorCode::NotPermitted.into())
    }

    async fn link_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path_flags: PathFlags,
        _old_path: String,
        _new_descriptor: Resource<Descriptor>,
        _new_path: String,
    ) -> FsResult<()> {
        // Hard link. Git can't represent these at all.
        Err(ErrorCode::Unsupported.into())
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
        let descriptor = self.descriptor(&fd)?;
        let follow_final_symlink = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        let write = flags.contains(DescriptorFlags::WRITE);

        let inode = if open_flags.contains(OpenFlags::CREATE) {
            let (directory, name, existing) =
                self.gitfs
                    .resolve_for_create(descriptor.inode, &path, follow_final_symlink)?;
            match existing {
                Some(inode) => {
                    if open_flags.contains(OpenFlags::EXCLUSIVE) {
                        return Err(ErrorCode::Exist.into());
                    }
                    inode
                }
                None => self.gitfs.create_file(directory, &name)?,
            }
        } else {
            self.gitfs
                .resolve_path(descriptor.inode, &path, follow_final_symlink)?
        };

        // WASI says opening a symlink without `symlink-follow` is an error
        // rather than opening the link itself.
        if !follow_final_symlink && self.gitfs.is_symlink(inode)? {
            return Err(ErrorCode::Loop.into());
        }

        if self.gitfs.is_directory(inode)? {
            if write || open_flags.contains(OpenFlags::TRUNCATE) {
                return Err(ErrorCode::IsDirectory.into());
            }
        } else if open_flags.contains(OpenFlags::DIRECTORY) {
            return Err(ErrorCode::NotDirectory.into());
        }

        if open_flags.contains(OpenFlags::TRUNCATE) {
            self.gitfs.set_size(inode, 0)?;
        } else if write {
            // Conservatively assume anything opened for writing was written to.
            // `modifications()` checks whether the contents really changed.
            self.gitfs.record_modified(inode);
        }

        Ok(self
            .resource_table
            .push_gitfs_descriptor(GitFsDescriptor { inode, flags })?)
    }

    async fn readlink_at(&mut self, fd: Resource<Descriptor>, path: String) -> FsResult<String> {
        let descriptor = self.descriptor(&fd)?;
        let inode = self.gitfs.resolve_path(descriptor.inode, &path, false)?;
        self.gitfs.symlink_target(inode)
    }

    async fn remove_directory_at(
        &mut self,
        fd: Resource<Descriptor>,
        path: String,
    ) -> FsResult<()> {
        let descriptor = self.descriptor(&fd)?;
        let (directory, name) = self.gitfs.resolve_parent(descriptor.inode, &path)?;
        self.gitfs.remove_directory(directory, &name)
    }

    async fn rename_at(
        &mut self,
        fd: Resource<Descriptor>,
        old_path: String,
        new_descriptor: Resource<Descriptor>,
        new_path: String,
    ) -> FsResult<()> {
        let old_descriptor = self.descriptor(&fd)?;
        let new_descriptor = self.descriptor(&new_descriptor)?;

        let (old_directory, old_name) =
            self.gitfs.resolve_parent(old_descriptor.inode, &old_path)?;
        let (new_directory, new_name) =
            self.gitfs.resolve_parent(new_descriptor.inode, &new_path)?;

        self.gitfs
            .rename(old_directory, &old_name, new_directory, &new_name)
    }

    async fn symlink_at(
        &mut self,
        fd: Resource<Descriptor>,
        old_path: String,
        new_path: String,
    ) -> FsResult<()> {
        let descriptor = self.descriptor(&fd)?;
        // `new_path` is the symlink itself; `old_path` is its target.
        let (directory, name) = self.gitfs.resolve_parent(descriptor.inode, &new_path)?;
        self.gitfs.create_symlink(directory, &name, &old_path)
    }

    async fn unlink_file_at(&mut self, fd: Resource<Descriptor>, path: String) -> FsResult<()> {
        let descriptor = self.descriptor(&fd)?;
        let (directory, name) = self.gitfs.resolve_parent(descriptor.inode, &path)?;
        self.gitfs.unlink(directory, &name)
    }

    async fn is_same_object(
        &mut self,
        fd: Resource<Descriptor>,
        other: Resource<Descriptor>,
    ) -> wasmtime::Result<bool> {
        // Two descriptors opened separately on the same file are the same
        // object, even if they were opened with different flags.
        Ok(self.descriptor(&fd)?.inode == self.descriptor(&other)?.inode)
    }

    async fn metadata_hash(&mut self, fd: Resource<Descriptor>) -> FsResult<MetadataHashValue> {
        let descriptor = self.descriptor(&fd)?;
        Ok(metadata_hash(descriptor.inode))
    }

    async fn metadata_hash_at(
        &mut self,
        fd: Resource<Descriptor>,
        path_flags: PathFlags,
        path: String,
    ) -> FsResult<MetadataHashValue> {
        let descriptor = self.descriptor(&fd)?;
        let follow_final_symlink = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        let inode = self
            .gitfs
            .resolve_path(descriptor.inode, &path, follow_final_symlink)?;
        Ok(metadata_hash(inode))
    }

    fn drop(&mut self, fd: Resource<Descriptor>) -> wasmtime::Result<()> {
        // This will drop the `Descriptor` which should close the file.
        self.resource_table.delete_gitfs_descriptor(fd)?;
        Ok(())
    }
}

impl WasiState {
    fn open_write_stream(
        &mut self,
        fd: Resource<Descriptor>,
        position: Position,
    ) -> FsResult<Resource<Box<dyn wasmtime_wasi::p2::OutputStream + 'static>>> {
        let descriptor = self.descriptor(&fd)?;
        if !descriptor.flags.contains(DescriptorFlags::WRITE) {
            return Err(ErrorCode::BadDescriptor.into());
        }
        if self.gitfs.is_directory(descriptor.inode)? {
            return Err(ErrorCode::IsDirectory.into());
        }

        // The stream can't call back into `GitFs` (it only gets `&mut self`),
        // so it writes into the file's buffer directly. That also means we have
        // to record the possible modification now rather than when it happens.
        let write_stream = WriteStream {
            content: self.gitfs.content(descriptor.inode)?,
            position,
        };
        self.gitfs.record_modified(descriptor.inode);

        let boxed_write_stream: Box<dyn wasmtime_wasi::p2::OutputStream> = Box::new(write_stream);
        Ok(self.resource_table.push(boxed_write_stream)?)
    }
}

/// The `metadata-hash` of an inode. It's only used as an identity for the file
/// (the preview 1 adapter reports it as `st_ino`), so the inode index - which
/// is never reused - is all we need.
fn metadata_hash(inode: Inode) -> MetadataHashValue {
    MetadataHashValue {
        lower: inode as u64,
        upper: 0,
    }
}

// Allow iterating through a directory returned by `read_directory()`.
impl filesystem::types::HostDirectoryEntryStream for WasiState {
    // Get the next directory entry or None.
    async fn read_directory_entry(
        &mut self,
        stream: Resource<ReaddirIterator>,
    ) -> FsResult<Option<DirectoryEntry>> {
        let stream = self.resource_table.get_mut_gitfs_readdiriterator(&stream)?;
        Ok(stream.entries.pop())
    }

    fn drop(&mut self, stream: Resource<ReaddirIterator>) -> wasmtime::Result<()> {
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
        err: Resource<wasmtime::Error>,
    ) -> wasmtime::Result<Option<ErrorCode>> {
        // Check the handle is valid, but our streams never fail with anything
        // that can be turned into an `ErrorCode`.
        let _ = self.resource_table.get(&err)?;
        Ok(None)
    }
}

struct ReadStream {
    content: SharedContent,
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
        let content = self.content.lock().expect("content mutex poisoned");
        if self.offset >= content.len() {
            Err(StreamError::Closed)
        } else {
            let size = size.min(content.len() - self.offset);
            let bytes = bytes::Bytes::copy_from_slice(&content[self.offset..self.offset + size]);
            self.offset += size;
            Ok(bytes)
        }
    }
}

/// Where the next write to a `WriteStream` goes.
enum Position {
    /// At a fixed offset that advances as we write.
    At(u64),
    /// Always at the end of the file.
    Append,
}

struct WriteStream {
    content: SharedContent,
    position: Position,
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::Pollable for WriteStream {
    async fn ready(&mut self) {
        // It's always ready.
    }
}

/// How many bytes we accept in one `write()`. There's no real limit because we
/// just write into a `Vec`, but `check-write` has to return something.
const WRITE_CHUNK_SIZE: usize = 1024 * 1024;

impl wasmtime_wasi::p2::OutputStream for WriteStream {
    fn write(&mut self, bytes: bytes::Bytes) -> StreamResult<()> {
        let mut content = self.content.lock().expect("content mutex poisoned");

        let offset = match self.position {
            Position::At(offset) => offset,
            Position::Append => content.len() as u64,
        };

        write_at(&mut content, offset, &bytes).map_err(|_| {
            StreamError::LastOperationFailed(wasmtime::Error::msg("file is too large"))
        })?;

        if let Position::At(offset) = &mut self.position {
            *offset += bytes.len() as u64;
        }

        Ok(())
    }

    fn flush(&mut self) -> StreamResult<()> {
        // Nothing is buffered; writes go straight into the file's contents.
        Ok(())
    }

    fn check_write(&mut self) -> StreamResult<usize> {
        Ok(WRITE_CHUNK_SIZE)
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
