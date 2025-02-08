use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use wasmtime::wasmparser::{Parser, Payload};

use crate::file_matching::MatchExpression;

#[derive(Debug, Deserialize)]
pub struct ArgBlock {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct NitMetadata {
    /// String to pass as argv[0] to the linter. Normally this doesn't
    /// matter and should just be a short name for the linter.
    pub argv0: String,

    /// Maximum number of filenames to pass to the linter in one go.
    /// If 0, then no filenames will be passed and the linter will only
    /// run once. There is no "unlimited" option; just use a suitably
    /// large number (e.g. 1 million).
    pub max_filenames: u64,

    /// If true, the linter will be run serially, so no
    /// more than one instance runs at a time. Otherwise
    /// it may be run in parallel.
    pub require_serial: bool,

    /// Arguments to pass. This is an ordered list of blocks of arguments.
    /// Each block can be overridden by the user, so you should leave
    /// an empty `extra` block for the user to fill in.
    pub args: Vec<ArgBlock>,

    /// Default expression to match files.
    pub default_match: MatchExpression,

    /// Repository this binary was built from. Required for
    /// commit-based integrity check.
    pub repo: String,
    // URL of attestation to verify this.
    // TODO (2.0): Support attestation.
    // pub attestation: String,
}

/// Read the `nit_metadata` section from a wasm file. This is a custom
/// section that contains a JSON file describing how to execute the module -
/// how to feed it files, etc.
///
/// You can use the wasm-custom-section tool to add these and show them
///
///     cargo install wasm-custom-section
///
pub fn read_metadata(wasm_path: &Path) -> Result<NitMetadata> {
    let module = std::fs::read(wasm_path)?;

    // Ideally we wouldn't load the entire file into memory, but
    // it's probably fine in most cases and wasmparser doesn't provide
    // a handy parse_reader() method.

    let parser = Parser::new(0);
    for payload in parser.parse_all(&module) {
        match payload? {
            Payload::CustomSection(section) if section.name() == "nit_metadata" => {
                return Ok(serde_json::from_slice::<NitMetadata>(section.data()).with_context(|| anyhow!("Reading metadata for {}", wasm_path.display()))?);
            }
            _ => {}
        }
    }
    bail!("No nit_metadata section found in the wasm file");
}

/// Return true if the file has a section called `nit_metadata`.
pub fn has_metadata(module: &[u8]) -> Result<bool> {
    let parser = Parser::new(0);
    for payload in parser.parse_all(&module) {
        match payload? {
            Payload::CustomSection(section) if section.name() == "nit_metadata" => {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}
