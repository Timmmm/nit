use std::{fs, path::PathBuf};

use anyhow::{Result, anyhow};
use clap::Parser;
use regex::RegexSetBuilder;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Regex to match.
    #[arg(long)]
    error_regex: Vec<String>,

    /// File to lint.
    files: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let set = RegexSetBuilder::new(&cli.error_regex)
        .multi_line(true)
        .build()?;

    let mut success = true;

    for file in cli.files {
        let text = fs::read_to_string(&file)?;

        for matching_index in set.matches(&text).into_iter() {
            eprintln!(
                "{}: Regex '{}' matches",
                file.display(),
                cli.error_regex[matching_index],
            );
            success = false;
        }
    }

    if success {
        Ok(())
    } else {
        Err(anyhow!("One or more files matched custom error regexes."))
    }
}
