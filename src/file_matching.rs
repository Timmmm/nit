use regex::Regex;
use serde::Deserialize;

use crate::git::{FileInfo, FileType};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchExpression {
    /// Matches a Glob (* and ?).
    #[serde(with = "crate::serde_glob")]
    Glob(glob::Pattern),
    /// Matches a regex on the path.
    #[serde(with = "crate::serde_regex")]
    Regex(Regex),
    /// Is a specific file type.
    Type(FileType),
    /// Shebang matches this regex.
    #[serde(with = "crate::serde_regex")]
    ShebangRegex(Regex),
    /// Not operator.
    Not(Box<MatchExpression>),
    /// Or operator.
    Or(Vec<MatchExpression>),
    /// And operator.
    And(Vec<MatchExpression>),
    /// Bool literal.
    Bool(bool),
}

// TODO (1.0): Add broad matching based on the extension, i.e. text-file extensions.

/// Returns true if `file` matches `expr`.
fn file_matches(file: &FileInfo, expr: &MatchExpression) -> bool {
    match expr {
        MatchExpression::Glob(glob_pattern) => file
            .path
            .to_str()
            .map_or(false, |path| glob_pattern.matches(path)),
        MatchExpression::Regex(re) => file.path.to_str().map_or(false, |path| re.is_match(path)),
        MatchExpression::Type(ty) => ty == &file.ty,
        MatchExpression::ShebangRegex(re) => file
            .shebang
            .as_ref()
            .map_or(false, |shebang| re.is_match(shebang)),
        MatchExpression::Not(inner) => !file_matches(file, inner),
        MatchExpression::Or(inner) => inner.iter().any(|inner| file_matches(file, inner)),
        MatchExpression::And(inner) => inner.iter().all(|inner| file_matches(file, inner)),
        MatchExpression::Bool(b) => *b,
    }
}

/// Filter `files` according to the match `expr`.
pub fn matching_files<'a>(files: &'a [FileInfo], expr: &MatchExpression) -> Vec<&'a FileInfo> {
    files.iter().filter(|f| file_matches(f, expr)).collect()
}

/// Filter `files` according to the match `expr` (in-place version).
pub fn retain_matching_files<'a>(files: &mut Vec<FileInfo>, expr: &MatchExpression) {
    files.retain(|f| file_matches(f, expr))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::git::FileType;

    #[test]
    fn test_matching_files() {
        let files = vec![FileInfo {
            path: "foo.rs".into(),
            ty: FileType::Text,
            shebang: None,
        }];

        let expr = MatchExpression::Glob(glob::Pattern::new("*.rs").unwrap());
        let matches = matching_files(&files, &expr);
        assert_eq!(matches.len(), 1);
    }
}
