use anyhow::{Context as _, Result, anyhow, bail};
use futures::{StreamExt as _, stream};
use log::{debug, info};
use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
};
use wasmtime::{
    Engine, Store,
    component::{Component, Linker},
};
use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, ResourceTable};

use wasmtime_wasi::p2::{
    IoView, WasiCtx, WasiCtxBuilder, WasiView, bindings::Command, pipe::MemoryOutputPipe,
};

use crate::{
    config::{ConfigLinter, LinterLocation},
    file_matching::matching_files,
    git::FileInfo,
    metadata::{ArgBlock, read_metadata},
    wasi_cache,
};

pub fn get_cache_dir() -> Option<PathBuf> {
    if let Ok(cache_dir) = env::var("NIT_CACHE_DIR") {
        Some(cache_dir.into())
    } else {
        dirs::cache_dir()
            .or_else(|| dirs::home_dir())
            .map(|d| d.join("nit"))
    }
}

/// Get the path to the .wasm file for a linter. This is either in the
/// repo for local paths (starting with /) or in the cache directory for URLs.
pub fn get_linter_path(top_level: &PathBuf, cache_dir: &Path, linter: &ConfigLinter) -> PathBuf {
    match &linter.location {
        LinterLocation::Local(path) => top_level.join(path),
        LinterLocation::Remote(remote) => get_url_linter_path(cache_dir, &remote.url),
    }
}

/// Get the path to the .wasm file for a linter with a URL location.
pub fn get_url_linter_path(cache_dir: &Path, url: &str) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(url.as_bytes());
    let hash = hasher.finalize();
    let hash_str = format!("{}.wasm", hash.to_hex());
    cache_dir.join(hash_str)
}

struct ComponentRunStates {
    wasi_ctx: WasiCtx,
    resource_table: ResourceTable,
}

impl WasiView for ComponentRunStates {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi_ctx
    }
}

impl IoView for ComponentRunStates {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.resource_table
    }
}

/// Run a single linter and return whether all executions returned EXIT_SUCCESS.
/// This does not check git diff.
pub async fn run_single_linter(
    files: &[FileInfo],
    cache_dir: &PathBuf,
    top_level: &PathBuf,
    linter: ConfigLinter,
) -> Result<bool> {
    let linter_path = get_linter_path(top_level, cache_dir, &linter);
    let metadata = read_metadata(&linter_path)?;

    log::info!("Running linter: {} ({})", linter.name, metadata.repo);

    let files = matching_files(
        files,
        if let Some(m) = &linter.override_match {
            m
        } else {
            &metadata.default_match
        },
    );

    let mut full_args: Vec<&str> = vec![metadata.argv0.as_str()];

    // Check that none of the override_args are invalid.
    if let Some(override_args) = &linter.override_args {
        let all_metadata_arg_names: BTreeSet<&str> =
            metadata.args.iter().map(|a| a.name.as_str()).collect();
        for (arg, _) in override_args {
            if !all_metadata_arg_names.contains(arg.as_str()) {
                bail!(
                    "Override arg '{}' isn't valid for linter '{}'. Valid options are {:?}.",
                    arg,
                    linter.name,
                    all_metadata_arg_names
                );
            }
        }
    }

    for ArgBlock { name, args } in metadata.args.iter() {
        let args = linter
            .override_args
            .as_ref()
            .and_then(|a| a.get(name))
            .unwrap_or(args);
        for s in args.iter() {
            full_args.push(s.as_str());
        }
    }

    info!("Loading component");

    let engine =
        Engine::new(wasmtime::Config::new().async_support(true)).context("creating WASM engine")?;

    let component = wasi_cache::load_component_cached(&engine, &linter_path).await?;

    if metadata.max_filenames == 0 {
        run_linter_command(top_level, &full_args, &engine, &component).await
    } else {
        let all_filenames = files
            .iter()
            .map(|f| {
                f.path
                    .to_str()
                    .ok_or_else(|| anyhow!("Couldn't convert path to UTF-8: {:?}", f.path))
            })
            .collect::<Result<Vec<_>>>()?;
        // Iterator of tasks to run.
        let tasks = all_filenames
            .chunks(metadata.max_filenames as usize)
            .map(|chunk| {
                let mut full_args = full_args.clone();
                full_args.extend_from_slice(&chunk);

                // We want to move full_args in and Rust doesn't have syntax to
                // only move some variables, so we convert these to references
                // and move the references in (so we don't move the actual engine/component).
                let component = &component;
                let engine = &engine;
                async move { run_linter_command(top_level, &full_args, engine, component).await }
            });

        // TODO (2.0): Add an option to explicitly set the parallelism, since
        // this doesn't always work perfectly (see the docs for available_parallelism()).
        let max_parallelism = if metadata.require_serial {
            1
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        };

        // We have to run all of the tasks even of an early one fails so they
        // can fix files and find all errors.
        let results: Vec<_> = stream::iter(tasks)
            .buffered(max_parallelism)
            .collect()
            .await;

        for result in results.into_iter() {
            if !result? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

async fn run_linter_command(
    top_level: &Path,
    args: &[&str],
    engine: &Engine,
    component: &Component,
) -> Result<bool> {
    debug!("Running linter with args: {:?}", args);

    let mut linker = Linker::new(&engine);

    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    // Allow up to 10 MB of output.
    let stdout = MemoryOutputPipe::new(10 * 1024 * 1024);
    let stderr = MemoryOutputPipe::new(10 * 1024 * 1024);

    let wasi = WasiCtxBuilder::new()
        .allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false)
        .preopened_dir(
            top_level,
            // TODO (2.0): Use `top_level` so reported paths are correct.
            ".",
            DirPerms::all(),
            FilePerms::all(),
        )?
        .stdout(stdout)
        .stderr(stderr)
        .args(args)
        // TODO (1.0): Set cwd: https://github.com/bytecodealliance/wasmtime/pull/9831
        .build();

    let state = ComponentRunStates {
        wasi_ctx: wasi,
        resource_table: ResourceTable::new(),
    };

    let mut store = Store::new(&engine, state);

    info!("Instantiating");
    let command = Command::instantiate_async(&mut store, &component, &linker).await?;

    info!("Starting call");

    let run_result = command.wasi_cli_run().call_run(&mut store).await;

    // The return type here is very weird. See
    // https://github.com/bytecodealliance/wasmtime/issues/10767
    match run_result {
        Ok(res) => res.map_err(|_| anyhow!("Unknown error running linter"))?,
        Err(error) => {
            if let Some(exit) = error.downcast_ref::<I32Exit>() {
                // Err(I32Exit(0)) is actually success.
                if exit.0 != 0 {
                    info!("Call failed with exit code {:?}", exit.0);
                    return Ok(false);
                }
            } else {
                return Err(error);
            }
        }
    };

    info!("Call finished");

    // TODO (2.0): Use WASI to check if files were modified.
    Ok(true)
}
