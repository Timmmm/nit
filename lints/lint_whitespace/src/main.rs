use std::{fs, io, process::ExitCode};

fn main() -> io::Result<ExitCode> {
    let mut any_modified = false;
    for file in std::env::args().skip(1) {
        let mut contents = fs::read(&file)?;

        let modified_0 = strip_trailing_whitespace(&mut contents);

        let modified_1 = ensure_newline_at_end(&mut contents);

        if modified_0 || modified_1 {
            fs::write(&file, contents)?;
            any_modified = true;
        }
    }

    Ok(ExitCode::from(if any_modified { 1 } else { 0 }))
}

/// Strip trailing whitespace. This also magically fixes \r\n endings.
fn strip_trailing_whitespace(contents: &mut Vec<u8>) -> bool {
    let mut modified = false;

    let mut in_ending = true;
    retain_rev(contents, |c| {
        if c == b'\n' {
            in_ending = true;
            true
        } else {
            in_ending &= c.is_ascii_whitespace();
            modified |= in_ending;
            !in_ending
        }
    });

    modified
}

/// Ensure exactly two newlines at the end of the file. Trailing whitespace
/// after the newlines should already have been stripped.
fn ensure_newline_at_end(contents: &mut Vec<u8>) -> bool {
    let orig_len = contents.len();
    let orig_ends_width = contents.ends_with(b"\n\n");

    contents.truncate(contents.trim_ascii_end().len());
    contents.push(b'\n');
    contents.push(b'\n');

    contents.len() != orig_len || !orig_ends_width
}

/// Like `retain`, but in reverse. Based on `retain` before it was optimised
/// here: https://github.com/rust-lang/rust/pull/81126/files
fn retain_rev(v: &mut Vec<u8>, mut f: impl FnMut(u8) -> bool) {
    let len = v.len();
    let mut del = 0;
    for i in (0..len).rev() {
        if !f(v[i]) {
            del += 1;
        } else if del > 0 {
            v[i + del] = v[i];
        }
    }
    if del > 0 {
        v.copy_within(del.., 0);
        v.truncate(len - del);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_strip_trailing_whitespace() {
        let mut contents = b"\nhello there\n\nworld\n".to_vec();
        let modified = strip_trailing_whitespace(&mut contents);
        assert_eq!(modified, false);
        assert_eq!(contents, b"\nhello there\n\nworld\n");

        let mut contents = b"\n\n  ".to_vec();
        let modified = strip_trailing_whitespace(&mut contents);
        assert_eq!(modified, true);
        assert_eq!(contents, b"\n\n");
    }

    #[test]
    fn test_ensure_newline_at_end() {
        let mut contents = b"\nhello there\n\nworld\n\n".to_vec();
        let modified = ensure_newline_at_end(&mut contents);
        assert_eq!(modified, false);
        assert_eq!(contents, b"\nhello there\n\nworld\n\n");

        let mut contents = b"\nhello there\n\nworld".to_vec();
        let modified = ensure_newline_at_end(&mut contents);
        assert_eq!(modified, true);
        assert_eq!(contents, b"\nhello there\n\nworld\n\n");

        let mut contents = b"\nhello there\n\nworld\n".to_vec();
        let modified = ensure_newline_at_end(&mut contents);
        assert_eq!(modified, true);
        assert_eq!(contents, b"\nhello there\n\nworld\n\n");
    }
}
