use std::{collections::BTreeMap, path::Path};

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::file_matching::MatchExpression;

#[derive(Deserialize, Debug)]
pub struct Config {
    /// Files to include. This is essentially ANDed with the linter's
    /// own match expression. There's no need for exclude since you
    /// can just use a Not expression. This must be present but can just
    /// be `{ "Bool": true }`.
    pub include: MatchExpression,

    /// Linters to run. These are run in order.
    pub linters: Vec<ConfigLinter>,
}

#[derive(Deserialize, Debug)]
pub struct RemoteLocation {
    /// URL of Wasm module to download.
    pub url: String,

    /// Hash of the Wasm binary module for integrity.
    pub hash: String,
    // Commit of the source repo. If this is specified
    // you can be guaranteed that the binary was built
    // from that source.
    // TODO (2.0): Support source hashes and attestation.
    // pub source_hash: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LinterLocation {
    /// URL of Wasm module to download.
    Remote(RemoteLocation),

    /// Path to a local Wasm module, relative to the repo root.
    Local(String),
}

#[derive(Deserialize, Debug)]
pub struct ConfigLinter {
    /// Name of the linter, for log messages.
    pub name: String,

    /// Location of the WASM module to use. It can be at a remote URL or
    /// embedded in the repo itself.
    pub location: LinterLocation,

    /// Override the default match expression provided by the linter.
    pub override_match: Option<MatchExpression>,

    /// Replace arguments from the linter config. By convention there
    /// will be an `extra` block that you can replace.
    pub override_args: Option<BTreeMap<String, Vec<String>>>,
}

/// Read JSON config. We always read in JSON5 so this works with JSONC and JSON too.
pub fn read_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)?;

    serde_json5::from_str(&content).map_err(|e| {
        anyhow!(
            "Config deserialization error ({path}): {e}",
            path = path.display()
        )
    })
}
