use std::path::{Component, Path, Prefix};

use anyhow::{anyhow, bail, Result};

/// Convert a path to a string that can be used in Bash. This is necessary on
/// Windows because Git runs hooks in Git Bash, which uses Mingw paths
/// (/c/foo/bar instead of C:\foo\bar).
pub fn path_to_bash_string(path: &Path) -> Result<String> {
    Ok(if cfg!(windows) {
        let mut out = String::new();
        let mut needs_slash = false;
        for component in path.components() {
            match component {
                Component::Prefix(prefix_component) => {
                    match prefix_component.kind() {
                        Prefix::Disk(disk) | Prefix::VerbatimDisk(disk) => {
                            out.push('/');
                            out.push(disk.to_ascii_lowercase() as char);
                        }
                        _ => bail!("Unsupported UNC path prefix: {prefix_component:?}"),
                    }
                    needs_slash = true;
                }
                Component::RootDir => {
                    out.push('/');
                    needs_slash = false;
                }
                Component::CurDir => {
                    if needs_slash {
                        out.push('/')
                    }
                    out.push('.');
                    needs_slash = true;
                }
                Component::ParentDir => {
                    if needs_slash {
                        out.push('/')
                    }
                    out.push('.');
                    out.push('.');
                    needs_slash = true;
                }
                Component::Normal(os_str) => {
                    if needs_slash {
                        out.push('/')
                    }
                    out.push_str(
                        os_str
                            .to_str()
                            .ok_or(anyhow!("Could not convert path to UTF-8: {path:?}"))?,
                    );
                    needs_slash = true;
                }
            }
        }
        out
    } else {
        path.to_str()
            .ok_or(anyhow!("Could not convert path to UTF-8: {path:?}"))?
            .to_owned()
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_path_to_bash_string_windows() {
        assert_eq!(
            path_to_bash_string("C:\\foo\\bar".as_ref()).unwrap(),
            "/c/foo/bar"
        );
        assert_eq!(path_to_bash_string("foo\\bar".as_ref()).unwrap(), "foo/bar");
        assert_eq!(
            path_to_bash_string("c:\\.\\foo\\..\\bar".as_ref()).unwrap(),
            "/c/foo/../bar"
        );
        assert_eq!(path_to_bash_string("c:\\".as_ref()).unwrap(), "/c/");
    }
}
