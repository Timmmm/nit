use std::path::PathBuf;

use crate::wasi_gitfs::wasi_fs::wasi::filesystem::types::{DirectoryEntry, ErrorCode};
use wasmtime_wasi::TrappableError;

pub type FsResult<T> = Result<T, FsError>;
pub type FsError = TrappableError<ErrorCode>;

/// A descriptor is the state associated with a file descriptor. It is stored
/// in the resource table. Normally this would hold any information you need
/// to access the underlying file/directory (e.g. a POSIX file descriptor).
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Descriptor(u32);

// Type returned by `read_dir()` that allows iterating through directory entries.
pub struct ReaddirIterator {
    pub entries: Vec<DirectoryEntry>,
}

// Copy & pasted from wasmtime-wasi with everything except wasi-filesystem removed
// and using the custom types above instead of the ones from wasmtime.
wasmtime::component::bindgen!({
    path: "wasi-filesystem/wit",
    world: "wasi:filesystem/imports",
    // Adds `tracing!` call to log entries and exits into this host API.
    tracing: false,
    // Support `Result<.., error-type>` style functions.
    trappable_imports: true,
    async: {
        // TODO: Do I need to use the exact same list here?

        // Only these functions are `async` and everything else is sync
        // meaning that it basically doesn't need to block. These functions
        // are the only ones that need to block.
        //
        // Note that at this time `only_imports` works on function names
        // which in theory can be shared across interfaces, so this may
        // need fancier syntax in the future.
        only_imports: [
            "[method]descriptor.access-at",
            "[method]descriptor.advise",
            "[method]descriptor.change-directory-permissions-at",
            "[method]descriptor.change-file-permissions-at",
            "[method]descriptor.create-directory-at",
            "[method]descriptor.get-flags",
            "[method]descriptor.get-type",
            "[method]descriptor.is-same-object",
            "[method]descriptor.link-at",
            "[method]descriptor.lock-exclusive",
            "[method]descriptor.lock-shared",
            "[method]descriptor.metadata-hash",
            "[method]descriptor.metadata-hash-at",
            "[method]descriptor.open-at",
            "[method]descriptor.read",
            "[method]descriptor.read-directory",
            "[method]descriptor.readlink-at",
            "[method]descriptor.remove-directory-at",
            "[method]descriptor.rename-at",
            "[method]descriptor.set-size",
            "[method]descriptor.set-times",
            "[method]descriptor.set-times-at",
            "[method]descriptor.stat",
            "[method]descriptor.stat-at",
            "[method]descriptor.symlink-at",
            "[method]descriptor.sync",
            "[method]descriptor.sync-data",
            "[method]descriptor.try-lock-exclusive",
            "[method]descriptor.try-lock-shared",
            "[method]descriptor.unlink-file-at",
            "[method]descriptor.unlock",
            "[method]descriptor.write",
            "[method]input-stream.blocking-read",
            "[method]input-stream.blocking-skip",
            "[drop]input-stream",
            "[method]output-stream.blocking-splice",
            "[method]output-stream.blocking-flush",
            "[method]output-stream.blocking-write",
            "[method]output-stream.blocking-write-and-flush",
            "[method]output-stream.blocking-write-zeroes-and-flush",
            "[drop]output-stream",
            "[method]directory-entry-stream.read-directory-entry",
            "poll",
            "[method]pollable.block",
            "[method]pollable.ready",
            "[method]tcp-socket.start-bind",
            "[method]tcp-socket.start-connect",
            "[method]udp-socket.start-bind",
            "[method]udp-socket.stream",
            "[method]outgoing-datagram-stream.send",
        ],
    },
    // Specify the error-type for `Result<..., error-type>`.
    trappable_error_type: {
        "wasi:io/streams/stream-error" => wasmtime_wasi_io::streams::StreamError,
        "wasi:filesystem/types/error-code" => FsError,
        // "wasi:sockets/network/error-code" => wasmtime_wasi::p2::SocketError,
    },
    with: {
        // All interfaces in the wasi:io package should be aliased to
        // the wasmtime-wasi-io generated code. Note that this will also
        // map the resource types to those defined in that crate as well.
        "wasi:io/poll": wasmtime_wasi_io::bindings::wasi::io::poll,
        "wasi:io/streams": wasmtime_wasi_io::bindings::wasi::io::streams,
        "wasi:io/error": wasmtime_wasi_io::bindings::wasi::io::error,

        // Configure all other resources to be concrete types defined in
        // this crate
        // "wasi:sockets/network/network": crate::net::Network,
        // "wasi:sockets/tcp/tcp-socket": wasmtime_wasi::p2::tcp::TcpSocket,
        // "wasi:sockets/udp/udp-socket": wasmtime_wasi::p2::udp::UdpSocket,
        // "wasi:sockets/udp/incoming-datagram-stream": wasmtime_wasi::p2::udp::IncomingDatagramStream,
        // "wasi:sockets/udp/outgoing-datagram-stream": wasmtime_wasi::p2::udp::OutgoingDatagramStream,
        // "wasi:sockets/ip-name-lookup/resolve-address-stream": wasmtime_wasi::p2::ip_name_lookup::ResolveAddressStream,

        // Custom descriptor types - the ones in the wasi crate are hard-coded to read from disk.
        "wasi:filesystem/types/directory-entry-stream": ReaddirIterator,
        "wasi:filesystem/types/descriptor": Descriptor,
        // "wasi:cli/terminal-input/terminal-input": wasmtime_wasi::p2::stdio::TerminalInput,
        // "wasi:cli/terminal-output/terminal-output": wasmtime_wasi::p2::stdio::TerminalOutput,
    },
});
