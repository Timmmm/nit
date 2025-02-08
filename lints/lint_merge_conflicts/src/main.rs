use std::{fs, io, process::ExitCode};

fn contains_conflict_markers(content: &str) -> bool {
    content.contains("<<<<<<<") || content.contains("=======") || content.contains(">>>>>>>")
}

fn main() -> io::Result<ExitCode> {
    let mut any_conflict = false;
    for file in std::env::args().skip(1) {
        let content = fs::read_to_string(&file)?;
        if contains_conflict_markers(&content) {
            eprintln!("Error: Merge conflict marker detected in file {}", file);
            any_conflict = true;
        }
    }
    Ok(ExitCode::from(if any_conflict { 1 } else { 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_conflict() {
        let content = "This is a clean file.";
        assert!(!contains_conflict_markers(content));
    }

    #[test]
    fn test_left_conflict() {
        let content = "Hello\n<<<<<<< HEAD\nConflict";
        assert!(contains_conflict_markers(content));
    }

    #[test]
    fn test_equal_conflict() {
        let content = "Conflict marker\n=======\nStill conflict";
        assert!(contains_conflict_markers(content));
    }

    #[test]
    fn test_right_conflict() {
        let content = "Some text\n>>>>>>> branch";
        assert!(contains_conflict_markers(content));
    }
}
