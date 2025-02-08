use std::path::Path;

use anyhow::{Context, Result};
use tokio::fs;
use wasmtime::{component::Component, Engine};

use crate::{hash_adapter, unique_filename::unique_filename};

pub async fn load_component_cached(engine: &Engine, wasi_path: &Path) -> Result<Component> {
    let wasi = fs::read(wasi_path).await.context("reading WASI module")?;

    let compatibility_hash = engine.precompile_compatibility_hash();

    let mut digest = blake3::Hasher::new();
    digest.update(&wasi);
    let compatibility_digest = hash_adapter::hash_digest(compatibility_hash, digest);

    // TODO: Use with_added_extension() when stable.
    let mut filename = wasi_path
        .file_name()
        .expect("wasi file must have filename")
        .to_owned();
    filename.push(format!(".{}.cache", compatibility_digest.to_hex()));
    let cache_path = wasi_path.with_file_name(filename);

    if !cache_path.exists() {
        let compiled = engine
            .precompile_component(&wasi)
            .context("precompiling WASI module")?;

        let tmpfile = wasi_path.with_file_name(unique_filename("tmp-", ".cache"));
        fs::write(&tmpfile, compiled).await?;
        // Check again in case another process just wrote the file.
        if !cache_path.exists() {
            fs::rename(tmpfile, &cache_path).await?;
        }
    }

    // SAFETY: The file must be trusted (it can cause arbitrary code execution)
    // and it is mmapped so it must be valid for the lifetime of the Component.
    // There is a small window between !cache_path.exists() and fs::rename()
    // where we might end up overwriting it, but it should be with an atomic
    // rename and the contents should remain the same (assuming WASM compilation
    // is deterministic).
    unsafe { Component::deserialize_file(&engine, cache_path) }
}
