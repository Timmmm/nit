use std::{
    io::BufRead as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, anyhow, bail};
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

#[derive(Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Symlink,
    /// Text file marked as executable in Git. This is possible on Windows too.
    ExecutableText,
    /// Binary file marked as executable in Git.
    ExecutableBinary,
    /// Text file not marked as executable in Git.
    Text,
    /// Binary file not marked as executable in Git.
    Binary,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
pub struct FileInfo {
    pub path: PathBuf,
    pub ty: FileType,
    pub shebang: Option<String>,
}

#[derive(Eq, PartialEq)]
enum GitFileType {
    Symlink,
    Executable,
    File,
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

    process_file_info(top_level, &command.stdout)
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

    process_file_info(top_level, &command.stdout)
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

fn process_file_info(top_level: &Path, ls_files_stdout: &[u8]) -> Result<Vec<FileInfo>> {
    ls_files_stdout
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
            let git_ty = match mode {
                b"120000" => GitFileType::Symlink,
                b"100755" => GitFileType::Executable,
                _ => GitFileType::File,
            };

            let (ty, shebang) = if git_ty == GitFileType::Symlink {
                (FileType::Symlink, None)
            } else {
                // Read the first 8000 bytes and look for a null byte. This is how
                // Git decides if it's binary.
                let full_path = top_level.join(path);
                let mut file = std::fs::File::open(&full_path)?;
                let mut buf = [0; 8000];
                let len = read_up_to(&mut file, &mut buf)?;
                let contents = &buf[..len];

                let is_binary = memchr::memchr(0, contents).is_some();

                let shebang = (git_ty == GitFileType::Executable)
                    .then(|| {
                        let reader = std::io::BufReader::new(contents);
                        reader.lines().next().and_then(|maybe_first_line| {
                            maybe_first_line.ok().and_then(|first_line| {
                                first_line.strip_prefix("#!").map(ToOwned::to_owned)
                            })
                        })
                    })
                    .flatten();

                let ty = match git_ty {
                    GitFileType::Executable => {
                        if is_binary {
                            FileType::ExecutableBinary
                        } else {
                            FileType::ExecutableText
                        }
                    }
                    GitFileType::File => {
                        if is_binary {
                            FileType::Binary
                        } else {
                            FileType::Text
                        }
                    }
                    _ => unreachable!(),
                };
                (ty, shebang)
            };

            Ok(FileInfo {
                path: path.to_owned(),
                ty,
                shebang,
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

/// This is the same as read_exact, except if it reaches EOF it doesn't return
/// an error, and it returns the number of bytes read.
fn read_up_to(file: &mut impl std::io::Read, mut buf: &mut [u8]) -> Result<usize, std::io::Error> {
    let buf_len = buf.len();

    while !buf.is_empty() {
        match file.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                let tmp = buf;
                buf = &mut tmp[n..];
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(buf_len - buf.len())
}


#[cfg(test)]
mod test {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_process_file_info() {
        let dir = tempdir().expect("Failed to create temp dir");

        let text_path = dir.path().join("test.txt");
        std::fs::write(&text_path, "Hello, world!").expect("Failed to write test text file");
        let bin_path = dir.path().join("test.bin");
        std::fs::write(&bin_path, b"Hello \x00!").expect("Failed to write test binary file");

        let status = Command::new("git")
            .arg("init")
            .arg("--initial-branch=master")
            .current_dir(dir.path())
            .status()
            .expect("Failed to run git init");
        assert!(status.success());

        let status = Command::new("git")
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .current_dir(dir.path())
            .status()
            .expect("Failed to run git config user.name");
        assert!(status.success());

        let status = Command::new("git")
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .current_dir(dir.path())
            .status()
            .expect("Failed to run git config user.email");
        assert!(status.success());

        let status = Command::new("git")
            .arg("add")
            .arg(&text_path)
            .arg(&bin_path)
            .current_dir(dir.path())
            .status()
            .expect("Failed to run git add");
        assert!(status.success());

        let status = Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg("Test commit")
            .current_dir(dir.path())
            .status()
            .expect("Failed to run git commit");
        assert!(status.success());

        let mut files = git_tree_files(dir.path(), "HEAD").expect("Failed to get git tree files");
        files.sort();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].ty, FileType::Binary);
        assert_eq!(files[1].ty, FileType::Text);
    }
}
