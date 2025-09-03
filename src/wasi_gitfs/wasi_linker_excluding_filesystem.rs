//! Copy & pased of wasmtime-wasi's `add_to_linker_async` but without wasi-filesystem.

use wasmtime::component::{HasData, Linker};
use wasmtime_wasi::p2::{WasiImpl, WasiView, bindings};
use wasmtime_wasi_io::IoImpl;

struct HasWasi<T>(T);

impl<T: 'static> HasData for HasWasi<T> {
    type Data<'a> = WasiImpl<&'a mut T>;
}

pub fn add_to_linker_async<T: WasiView + 'static>(linker: &mut Linker<T>) -> anyhow::Result<()> {
    let options = bindings::LinkOptions::default();
    add_to_linker_with_options_async(linker, &options)
}

/// Similar to [`add_to_linker_async`], but with the ability to enable unstable features.
pub fn add_to_linker_with_options_async<T: WasiView + 'static>(
    linker: &mut Linker<T>,
    options: &bindings::LinkOptions,
) -> anyhow::Result<()> {
    wasmtime_wasi_io::add_to_linker_async(linker)?;
    add_nonblocking_to_linker(linker, options)?;

    let l = linker;
    let f: fn(&mut T) -> WasiImpl<&mut T> = |t| WasiImpl(IoImpl(t));
    // bindings::filesystem::types::add_to_linker::<T, HasWasi<T>>(l, f)?;
    bindings::sockets::tcp::add_to_linker::<T, HasWasi<T>>(l, f)?;
    bindings::sockets::udp::add_to_linker::<T, HasWasi<T>>(l, f)?;
    Ok(())
}

/// Shared functionality for [`add_to_linker_async`] and [`add_to_linker_sync`].
fn add_nonblocking_to_linker<'a, T: WasiView + 'static, O>(
    linker: &mut Linker<T>,
    options: &'a O,
) -> anyhow::Result<()>
where
    bindings::sockets::network::LinkOptions: From<&'a O>,
    bindings::cli::exit::LinkOptions: From<&'a O>,
{
    use wasmtime_wasi::p2::bindings::{cli, clocks, /* filesystem, */ random, sockets};

    let l = linker;
    let f: fn(&mut T) -> WasiImpl<&mut T> = |t| WasiImpl(IoImpl(t));
    clocks::wall_clock::add_to_linker::<T, HasWasi<T>>(l, f)?;
    clocks::monotonic_clock::add_to_linker::<T, HasWasi<T>>(l, f)?;
    // filesystem::preopens::add_to_linker::<T, HasWasi<T>>(l, f)?;
    random::random::add_to_linker::<T, HasWasi<T>>(l, f)?;
    random::insecure::add_to_linker::<T, HasWasi<T>>(l, f)?;
    random::insecure_seed::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::exit::add_to_linker::<T, HasWasi<T>>(l, &options.into(), f)?;
    cli::environment::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::stdin::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::stdout::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::stderr::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::terminal_input::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::terminal_output::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::terminal_stdin::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::terminal_stdout::add_to_linker::<T, HasWasi<T>>(l, f)?;
    cli::terminal_stderr::add_to_linker::<T, HasWasi<T>>(l, f)?;
    sockets::tcp_create_socket::add_to_linker::<T, HasWasi<T>>(l, f)?;
    sockets::udp_create_socket::add_to_linker::<T, HasWasi<T>>(l, f)?;
    sockets::instance_network::add_to_linker::<T, HasWasi<T>>(l, f)?;
    sockets::network::add_to_linker::<T, HasWasi<T>>(l, &options.into(), f)?;
    sockets::ip_name_lookup::add_to_linker::<T, HasWasi<T>>(l, f)?;
    Ok(())
}
