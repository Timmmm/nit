use indicatif::ProgressBar;
use log::info;
use reqwest::Url;
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    sync::{Arc, atomic::AtomicU64},
};

use anyhow::{Context, Result, anyhow, bail};
use futures::{Stream, StreamExt as _, TryStreamExt as _, stream};
use tokio::{
    fs::{self, File},
    io::{AsyncBufRead, AsyncReadExt as _},
};

use crate::{
    config::{ConfigLinter, LinterLocation},
    engine::get_url_linter_path,
    unique_filename::unique_filename,
};

/// Calculate the SHA3 hash of a file.
pub async fn file_binary_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).await?;
    let mut hasher = blake3::Hasher::default();
    let mut buffer = [0; 4096];

    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.write_all(&buffer[..bytes_read])?;
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub async fn download(url: Url, save_to: &Path, progress_bar: ProgressBar) -> Result<()> {
    let response = reqwest::get(url.clone())
        .await
        .with_context(|| anyhow!("GET '{url}'"))?;

    let content_length = response.content_length();

    match content_length {
        Some(length) => progress_bar.set_length(length),
        None => progress_bar.unset_length(),
    };

    let downloaded_bytes: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let downloaded_bytes_copy = downloaded_bytes.clone();

    let bytes_stream = response.bytes_stream().inspect_ok(move |bytes| {
        downloaded_bytes_copy.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::AcqRel);
        progress_bar.inc(bytes.len() as u64);
    });

    let mut stream_reader = to_async_read(bytes_stream);

    let mut file = tokio::fs::File::create(&save_to)
        .await
        .with_context(|| anyhow!("Creating destination file '{}'", save_to.display()))?;
    tokio::io::copy(&mut stream_reader, &mut file)
        .await
        .with_context(|| anyhow!("Writing to destination file: '{}'", save_to.display()))?;

    let downloaded_bytes = downloaded_bytes.load(std::sync::atomic::Ordering::Acquire);

    if let Some(content_length) = content_length {
        if downloaded_bytes != content_length {
            bail!(
                "Content length from server was {content_length} but we downloaded {downloaded_bytes} bytes"
            );
        }
    }

    Ok(())
}

fn to_async_read(
    stream: impl Stream<Item = std::result::Result<tokio_util::bytes::Bytes, reqwest::Error>>,
) -> impl AsyncBufRead {
    // Map Arc<reqwest::Error> back to io::Error, and wrap with StreamReader.
    tokio_util::io::StreamReader::new(stream.map_err(|ae| std::io::Error::other(ae)))
}

pub async fn fetch_linters(linters: &[ConfigLinter], cache_dir: &Path) -> Result<()> {
    info!("Fetching linters...");

    // 1. Collect all the URL/binary hash pairs.
    // 2. Deduplicate URLs. Throw an error if different binary hashes
    //    were given for the same URL.
    // 3. Check which ones are already downloaded.
    // 4. Download the missing ones atomically.

    let mut url_to_hash = BTreeMap::new();
    for linter in linters {
        // Don't need to download local linters.
        match &linter.location {
            LinterLocation::Local(_) => {}
            LinterLocation::Remote(remote) => {
                if let Some(hash) = url_to_hash.get(&remote.url) {
                    if hash != &remote.hash {
                        bail!("Different binary hashes for the same URL: {}", remote.url);
                    }
                } else {
                    url_to_hash.insert(remote.url.clone(), remote.hash.clone());
                }
            }
        }
    }

    let task_info_stream = stream::iter(url_to_hash.iter());

    // Set up a new multi-progress bar.
    // The bar is stored in an `Arc` to facilitate sharing between threads.
    let multibar = std::sync::Arc::new(indicatif::MultiProgress::new());

    // Add an overall progress indicator to the multibar.
    // It has 10 steps and will increment on completion of each task.
    let main_pb = std::sync::Arc::new(
        multibar
            .clone()
            .add(indicatif::ProgressBar::new(url_to_hash.len() as u64)),
    );
    main_pb.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{msg} {bar:10} {pos}/{len}")
            .unwrap(),
    );
    main_pb.set_message("total  ");

    // Make the main progress bar render immediately rather than waiting for the
    // first task to finish.
    main_pb.tick();

    let max_concurrent_downloads = 4;

    std::fs::create_dir_all(cache_dir)?;

    // Set up a future to iterate over tasks and run up to 3 at a time.
    task_info_stream
        .enumerate()
        // Weirdly try_for_each_concurrent needs its *input* to be fallible.
        .map(Ok)
        .try_for_each_concurrent(max_concurrent_downloads, |(i, (url, hash))| {
            // Clone multibar and main_pb.  We will move the clones into each task.
            let multibar = multibar.clone();
            let main_pb = main_pb.clone();
            async move {
                // Add a new progress indicator to the multibar.
                let task_pb = multibar.add(indicatif::ProgressBar::no_length());
                // task_pb.set_style(ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} [{eta_precise}] {decimal_bytes:>10} / {decimal_total_bytes} {decimal_bytes_per_sec:>10} {msg}").expect("invalid format string"));
                task_pb.set_style(
                    indicatif::ProgressStyle::default_bar()
                        .template("task {msg} {bar:10} {pos}/{len}")
                        .unwrap(),
                );
                task_pb.set_message(format!("{}: {}", i + 1, url));

                let binary_path = get_url_linter_path(cache_dir, url);

                // Check if it already exists.
                let maybe_hash = file_binary_hash(&binary_path).await;
                if !matches!(maybe_hash, Ok(h) if h == *hash) {
                    let url = url.parse()?;

                    info!("Downloading {url}");

                    let tmpfile = binary_path.with_file_name(unique_filename("tmp-", ".wasm"));

                    download(url, &tmpfile, task_pb.clone()).await?;
                    fs::rename(tmpfile, &binary_path).await?;
                }

                let read_hash = file_binary_hash(&binary_path).await?;
                if read_hash != *hash {
                    bail!(
                        "Hash mismatch for {url} after download: expected {hash}, got {read_hash}"
                    );
                }

                // Increment the overall progress indicator.
                main_pb.inc(1);

                // Clear this tasks's progress indicator.
                task_pb.finish_and_clear();
                Ok(())
            }
        })
        .await?;

    // Change the message on the overall progress indicator.
    main_pb.finish_and_clear();

    info!("Linters fetched");

    Ok(())
}
