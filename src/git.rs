use std::{
    io::BufRead as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, bail, Context as _, Result};
use itertools::Itertools as _;
use serde::Deserialize;

pub fn git_top_level() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(&["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse --show-toplevel")?;
    let path = std::str::from_utf8(&output.stdout)
        .with_context(|| anyhow!("Path is not UTF-8: {:?}", output.stdout))?;
    Ok(PathBuf::from(path.trim()))
}

pub fn git_hooks_dir() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(&["rev-parse", "--git-path", "hooks"])
        .output()
        .context("Failed to run git rev-parse --git-path hooks")?;
    let path = std::str::from_utf8(&output.stdout)
        .with_context(|| anyhow!("Path is not UTF-8: {:?}", output.stdout))?;
    Ok(PathBuf::from(path.trim()))
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub enum FileType {
    Symlink,
    /// Marked as executable in Git. This is possible on Windows too.
    ExecutableFile,
    /// Not marked as executable in Git.
    File,
}

pub struct FileInfo {
    pub path: PathBuf,
    pub ty: FileType,
    pub shebang: Option<String>,
}

/// Get info on all of the files in a tree (i.e. a commit). This doesn't work
/// for the index or working directory.
pub fn git_tree_files(top_level: &Path, treeish: &str) -> Result<Vec<FileInfo>> {
    // pre-commit uses git ls-files to get the list of all files.
    // It uses git diff --names-only for changed files but I'm not sure exactly how it gets the from/to refs if you don't specify them.

    let command = Command::new("git")
        .arg("ls-tree")
        // Recursive.
        .arg("-r")
        // Null terminated lines.
        .arg("-z")
        // Show all files (not just in the CWD), and show paths relative to
        // the top level (instead of the CWD). Doesn't really matter since
        // we set the CWD to the top level, but belt an braces.
        .arg("--full-tree")
        .arg("--format=%(objectmode)%x00%(objectname)%x00%(objectsize)%x00%(path)")
        .arg(treeish)
        // Set the working directory to the root anyway just in case.
        .current_dir(top_level)
        .output()
        .context("Failed to run git ls-tree")?;

    if !command.status.success() {
        bail!("git ls-tree command failed");
    }

    command
        .stdout
        .split(|&b| b == 0)
        .tuples()
        .map(|(mode, _hash, _size, path)| {
            // mode:   octal permission bits, e.g. 100644.
            // _hash:  object hash
            // _size:  size in bytes
            // path:   file path

            let path = Path::new(
                std::str::from_utf8(path).with_context(|| anyhow!("Failed to parse path"))?,
            );
            let ty = match mode {
                b"120000" => FileType::Symlink,
                b"100755" => FileType::ExecutableFile,
                _ => FileType::File,
            };

            let shebang = (ty == FileType::ExecutableFile)
                .then(|| {
                    let contents = std::fs::File::open(&path)
                        .with_context(|| anyhow!("Failed to read {:?}", path))
                        .ok()?;
                    let reader = std::io::BufReader::new(contents);
                    reader.lines().next().and_then(|maybe_first_line| {
                        maybe_first_line.ok().and_then(|first_line| {
                            first_line.strip_prefix("#!").map(ToOwned::to_owned)
                        })
                    })
                })
                .flatten();

            Ok(FileInfo {
                path: path.to_owned(),
                ty,
                shebang: shebang,
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Get info on all of the staged files.
pub fn git_staged_files(top_level: &Path) -> Result<Vec<FileInfo>> {
    let command = Command::new("git")
        .arg("ls-files")
        // Show staged files (technically the default option but let's be explicit).
        .arg("--cached")
        // Null terminated lines.
        .arg("-z")
        // Show paths relative to top level.
        .arg("--full-name")
        .arg("--format=%(objectmode)%x00%(objectname)%x00%(objectsize)%x00%(path)")
        // Set the working directory to the root anyway just in case.
        .current_dir(top_level)
        .output()
        .context("Failed to run git ls-files")?;

    if !command.status.success() {
        bail!("git ls-files command failed");
    }

    // TODO (1.0): DRY.
    command
        .stdout
        .split(|&b| b == 0)
        .tuples()
        .map(|(mode, _hash, _size, path)| {
            // mode:   octal permission bits, e.g. 100644.
            // _hash:  object hash
            // size:   size in bytes
            // path:   file path

            let path = Path::new(
                std::str::from_utf8(path).with_context(|| anyhow!("Failed to parse path"))?,
            );
            let ty = match mode {
                b"120000" => FileType::Symlink,
                b"100755" => FileType::ExecutableFile,
                _ => FileType::File,
            };

            let shebang = (ty == FileType::ExecutableFile)
                .then(|| {
                    let contents = std::fs::File::open(&path)
                        .with_context(|| anyhow!("Failed to read {:?}", path))
                        .ok()?;
                    let reader = std::io::BufReader::new(contents);
                    reader.lines().next().and_then(|maybe_first_line| {
                        maybe_first_line.ok().and_then(|first_line| {
                            first_line.strip_prefix("#!").map(ToOwned::to_owned)
                        })
                    })
                })
                .flatten();

            Ok(FileInfo {
                path: path.to_owned(),
                ty,
                shebang: shebang,
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

/// List of files changed in the working directory (not staged).
pub fn git_diff_unstaged(top_level: &Path) -> Result<Vec<u8>> {
    let output = std::process::Command::new("git")
        .args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules",
        ])
        .current_dir(top_level)
        .output()?;
    if !output.status.success() {
        bail!("git diff command failed");
    }
    Ok(output.stdout)
}
