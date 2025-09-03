//! Copy & paste of wasmtime-wasi's `add_to_linker_async` but without wasi-filesystem.

use wasmtime::component::{HasData, Linker};
use wasmtime_wasi::cli::{WasiCli, WasiCliView as _};
use wasmtime_wasi::clocks::{WasiClocks, WasiClocksView as _};
use wasmtime_wasi::random::WasiRandom;
use wasmtime_wasi::sockets::{WasiSockets, WasiSocketsView as _};
use wasmtime_wasi::{ResourceTable, WasiView, p2::bindings};

pub fn add_to_linker_async<T: WasiView>(linker: &mut Linker<T>) -> anyhow::Result<()> {
    let options = bindings::LinkOptions::default();
    add_to_linker_with_options_async(linker, &options)
}

/// Similar to [`add_to_linker_async`], but with the ability to enable unstable features.
pub fn add_to_linker_with_options_async<T: WasiView>(
    linker: &mut Linker<T>,
    options: &bindings::LinkOptions,
) -> anyhow::Result<()> {
    add_async_io_to_linker(linker)?;
    add_nonblocking_to_linker(linker, options)?;

    let l = linker;
    // bindings::filesystem::types::add_to_linker::<T, WasiFilesystem>(l, T::filesystem)?;
    bindings::sockets::tcp::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    bindings::sockets::udp::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    Ok(())
}

/// Shared functionality for [`add_to_linker_async`] and [`add_to_linker_sync`].
fn add_nonblocking_to_linker<'a, T: WasiView, O>(
    linker: &mut Linker<T>,
    options: &'a O,
) -> anyhow::Result<()>
where
    bindings::sockets::network::LinkOptions: From<&'a O>,
    bindings::cli::exit::LinkOptions: From<&'a O>,
{
    // use wasmtime_wasi::p2::bindings::{cli, clocks, filesystem, random, sockets};
    use wasmtime_wasi::p2::bindings::{cli, clocks, random, sockets};

    let l = linker;
    clocks::wall_clock::add_to_linker::<T, WasiClocks>(l, T::clocks)?;
    clocks::monotonic_clock::add_to_linker::<T, WasiClocks>(l, T::clocks)?;
    // filesystem::preopens::add_to_linker::<T, WasiFilesystem>(l, T::filesystem)?;
    random::random::add_to_linker::<T, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    random::insecure::add_to_linker::<T, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    random::insecure_seed::add_to_linker::<T, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    cli::exit::add_to_linker::<T, WasiCli>(l, &options.into(), T::cli)?;
    cli::environment::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::stdin::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::stdout::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::stderr::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::terminal_input::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::terminal_output::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::terminal_stdin::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::terminal_stdout::add_to_linker::<T, WasiCli>(l, T::cli)?;
    cli::terminal_stderr::add_to_linker::<T, WasiCli>(l, T::cli)?;
    sockets::tcp_create_socket::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    sockets::udp_create_socket::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    sockets::instance_network::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    sockets::network::add_to_linker::<T, WasiSockets>(l, &options.into(), T::sockets)?;
    sockets::ip_name_lookup::add_to_linker::<T, WasiSockets>(l, T::sockets)?;
    Ok(())
}

struct HasIo;

impl HasData for HasIo {
    type Data<'a> = &'a mut ResourceTable;
}

// FIXME: it's a bit unfortunate that this can't use
// `wasmtime_wasi_io::add_to_linker` and that's because `T: WasiView`, here,
// not `T: IoView`. Ideally we'd have `impl<T: WasiView> IoView for T` but
// that's not possible with these two traits in separate crates. For now this
// is some small duplication but if this gets worse over time then we'll want
// to massage this.
fn add_async_io_to_linker<T: WasiView>(l: &mut Linker<T>) -> anyhow::Result<()> {
    wasmtime_wasi_io::bindings::wasi::io::error::add_to_linker::<T, HasIo>(l, |t| t.ctx().table)?;
    wasmtime_wasi_io::bindings::wasi::io::poll::add_to_linker::<T, HasIo>(l, |t| t.ctx().table)?;
    wasmtime_wasi_io::bindings::wasi::io::streams::add_to_linker::<T, HasIo>(l, |t| t.ctx().table)?;
    Ok(())
}
