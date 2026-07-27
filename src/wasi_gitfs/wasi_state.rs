use std::{collections::HashSet, path::PathBuf};

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

use crate::wasi_gitfs::gitfs::{GitFs, Inode, Node, ROOT_INODE};

pub struct WasiState {
    pub wasi_ctx: WasiCtx,
    // This is basically a `Vec<any>`.
    pub resource_table: ResourceTable,
    // The git filesystem. This is a *mutable* filesystem backed by a Git repository.
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
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct GitFsDescriptor {
    pub inode: Inode,
}

/// Type returned by `read_dir()` that allows iterating through directory entries.
pub struct GitFsReaddirIterator {
    pub entries: Vec<DirectoryEntry>,
}

/// Extension trait for ResourceTable to let us store `GitFsDescriptor`s in it easily,
/// but pretending they are wasmtime's `Descriptor`s (which actually represent
/// real files on disk). Unfortunately wasmtime doesn't let us choose the
/// `Descriptor` type so we just lie to it.
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
    fn get_directories(&mut self) -> wasmtime::Result<Vec<(Resource<Descriptor>, String)>> {
        // We have one hard-coded pre-open: `/`.
        // TODO: Use the path of the repo, so that paths printed by the linters are correct.
        Ok(vec![(
            // Create a new file descriptor and add it to the resource table,
            // returning its index in the table.
            self.resource_table
                .push_gitfs_descriptor(GitFsDescriptor { inode: ROOT_INODE })
                .expect("failed to push root preopen"), // TODO: Not expect. Need to figure out what wasmtime's .context() equivalent is.
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
        let data = self.gitfs.read_file(descriptor.inode)?;
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

    // TODO (0.1): Implement all these functions.
    fn write_via_stream(
        &mut self,
        _fd: Resource<Descriptor>,
        _offset: u64,
    ) -> FsResult<Resource<Box<(dyn wasmtime_wasi::p2::OutputStream + 'static)>>> {
        todo!();
    }

    fn append_via_stream(
        &mut self,
        _fd: Resource<Descriptor>,
    ) -> FsResult<Resource<Box<(dyn wasmtime_wasi::p2::OutputStream + 'static)>>> {
        todo!();
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
        // TODO: I guess we will need to record in the descriptor how it was opened.
        Ok(DescriptorFlags::READ)
    }

    async fn get_type(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorType> {
        let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let ty = match self.gitfs.get_node(descriptor.inode)? {
            Node::File(_) => DescriptorType::RegularFile,
            Node::Directory(_) => DescriptorType::Directory,
        };
        Ok(ty)
    }

    async fn set_size(&mut self, _fd: Resource<Descriptor>, _size: Filesize) -> FsResult<()> {
        todo!()
    }

    async fn set_times(
        &mut self,
        _fd: Resource<Descriptor>,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        // TODO: Maybe we just ignore it?
        Err(ErrorCode::NotPermitted.into())
    }

    async fn read(
        &mut self,
        fd: Resource<Descriptor>,
        length: Filesize,
        offset: Filesize,
    ) -> FsResult<(Vec<u8>, bool)> {
        let descriptor = self.resource_table.get_mut_gitfs_descriptor(&fd).unwrap();
        let blob = self.gitfs.read_file(descriptor.inode)?;
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
        todo!()
    }

    async fn read_directory(
        &mut self,
        fd: Resource<Descriptor>,
    ) -> FsResult<Resource<ReaddirIterator>> {
        todo!()
        // let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // // TODO: Could use `find_tree_iter()` ideally but I don't know if the
        // // lifetime issues are easy to deal with, or if it makes any performance difference.
        // let tree = self.gitfs.repo.find_tree(descriptor.id).unwrap();
        // let mut entries: Vec<_> = tree
        //     .iter()
        //     .map(|entry| {
        //         let entry = entry.unwrap();
        //         DirectoryEntry {
        //             type_: gix_entry_kind_to_descriptor_type(entry.kind()),
        //             name: entry.filename().to_string(),
        //         }
        //     })
        //     .collect();
        // // Reverse because we pop them off the back when reading.
        // // TODO: Probably can do this more efficiently somehow.
        // entries.reverse();
        // Ok(self
        //     .resource_table
        //     .push_gitfs_readdiriterator(GitFsReaddirIterator { entries })
        //     .unwrap())
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
        todo!()
    }

    async fn stat(&mut self, fd: Resource<Descriptor>) -> FsResult<DescriptorStat> {
        todo!()
        // let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // Ok(DescriptorStat {
        //     type_: gix_entry_kind_to_descriptor_type(descriptor.kind),
        //     // Git doesn't support hard links and the normal case is 1, not 0.
        //     link_count: 1,
        //     // In posix for symlinks this is the size of the path. Does that apply here?
        //     size: match descriptor.kind {
        //         // For symlinks this should return the size of the path, which Git
        //         // conveniently stores as the blob data, so we can use the same code.
        //         EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
        //             self.gitfs.repo.find_header(descriptor.id).unwrap().size()
        //         }
        //         // Directory or submodule.
        //         EntryKind::Tree | EntryKind::Commit => 0,
        //     },
        //     // Git doesn't record this.
        //     data_access_timestamp: None,
        //     data_modification_timestamp: None,
        //     status_change_timestamp: None,
        // })
    }

    async fn stat_at(
        &mut self,
        fd: Resource<Descriptor>,
        path_flags: PathFlags,
        path: String,
    ) -> FsResult<DescriptorStat> {
        let from_descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        let follow_final_symlink: bool = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        todo!()
        // let descriptor = self
        //     .gitfs
        //     .resolve_path(*from_descriptor, &path, follow_final_symlink)?;

        // // TODO: Extract into function.
        // Ok(DescriptorStat {
        //     type_: gix_entry_kind_to_descriptor_type(descriptor.kind),
        //     // Git doesn't support hard links and the normal case is 1, not 0.
        //     link_count: 1,
        //     // In posix for symlinks this is the size of the path. Does that apply here?
        //     size: match descriptor.kind {
        //         // For symlinks this should return the size of the path, which Git
        //         // conveniently stores as the blob data, so we can use the same code.
        //         EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
        //             self.gitfs.repo.find_header(descriptor.id).unwrap().size()
        //         }
        //         // Directory or submodule.
        //         EntryKind::Tree | EntryKind::Commit => 0,
        //     },
        //     // Git doesn't record this.
        //     data_access_timestamp: None,
        //     data_modification_timestamp: None,
        //     status_change_timestamp: None,
        // })
    }

    async fn set_times_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path_flags: PathFlags,
        _path: String,
        _data_access_timestamp: NewTimestamp,
        _data_modification_timestamp: NewTimestamp,
    ) -> FsResult<()> {
        // TODO: Maybe just ignore it?
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
        // hard link.
        todo!()
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
        todo!()
        // let descriptor = self
        //     .gitfs
        //     .resolve_path(*from_descriptor, &path, follow_final_symlink)?;

        // if open_flags.contains(OpenFlags::EXCLUSIVE) {
        //     return Err(ErrorCode::Exist.into());
        // }

        // if open_flags.contains(OpenFlags::DIRECTORY) && descriptor.kind != EntryKind::Tree {
        //     return Err(ErrorCode::NotDirectory.into());
        // }

        // Ok(self
        //     .resource_table
        //     .push_gitfs_descriptor(descriptor)
        //     .unwrap())
    }

    async fn readlink_at(&mut self, fd: Resource<Descriptor>, path: String) -> FsResult<String> {
        let from_descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // let descriptor = self.gitfs.resolve_path(*from_descriptor, &path, false)?;

        // if descriptor.kind != EntryKind::Link {
        //     return Err(ErrorCode::Invalid.into());
        // }

        todo!()
        // let mut link = self
        //     .gitfs
        //     .repo
        //     .find_blob(descriptor.id)
        //     .map_err(|_| ErrorCode::NoEntry)?;
        // let link_str =
        //     String::from_utf8(link.take_data()).map_err(|_| ErrorCode::IllegalByteSequence)?;
        // Ok(link_str.to_owned())
    }

    async fn remove_directory_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _path: String,
    ) -> FsResult<()> {
        todo!()
    }

    async fn rename_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path: String,
        _new_descriptor: Resource<Descriptor>,
        _new_path: String,
    ) -> FsResult<()> {
        todo!()
    }

    async fn symlink_at(
        &mut self,
        _fd: Resource<Descriptor>,
        _old_path: String,
        _new_path: String,
    ) -> FsResult<()> {
        todo!()
    }

    async fn unlink_file_at(&mut self, _fd: Resource<Descriptor>, _path: String) -> FsResult<()> {
        todo!()
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
        todo!();
        // let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // Ok(MetadataHashValue {
        //     lower: u64::from_le_bytes(descriptor.inode.as_bytes()[0..8].try_into().unwrap()),
        //     upper: u64::from_le_bytes(descriptor.inode.as_bytes()[8..16].try_into().unwrap()),
        // })
    }

    async fn metadata_hash_at(
        &mut self,
        fd: Resource<Descriptor>,
        _path_flags: PathFlags,
        _path: String,
    ) -> FsResult<MetadataHashValue> {
        // Kind of unclear what the use case for this is if you ask me.
        // While this is read-only we can just return the object ID which is long enough.
        todo!();
        // let descriptor = self.resource_table.get_gitfs_descriptor(&fd).unwrap();
        // Ok(MetadataHashValue {
        //     lower: u64::from_le_bytes(descriptor.inode.as_bytes()[0..8].try_into().unwrap()),
        //     upper: u64::from_le_bytes(descriptor.inode.as_bytes()[8..16].try_into().unwrap()),
        // })
    }

    fn drop(&mut self, fd: Resource<Descriptor>) -> wasmtime::Result<()> {
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
        let err = self.resource_table.get(&err)?;

        // TODO: Do we need to do something here?

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
