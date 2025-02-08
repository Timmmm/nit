mod jsonformat;

use std::{fs, io, process::ExitCode};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Indentation string.
    #[arg(long, default_value = "    ")]
    indentation: String,

    /// Files to format.
    files: Vec<String>,
}

fn main() -> io::Result<ExitCode> {
    let args = Args::parse();
    let indentation = match args.indentation.as_str() {
        "  " => jsonformat::Indentation::TwoSpace,
        "    " => jsonformat::Indentation::FourSpace,
        "\t" => jsonformat::Indentation::Tab,
        other => jsonformat::Indentation::Custom(other),
    };
    let mut any_modified = false;
    for file in args.files {
        let content = fs::read(&file)?;

        let mut formatted_content = Vec::new();
        let writer = io::BufWriter::new(&mut formatted_content);

        jsonformat::format_reader_writer(
            content.as_slice(),
            writer,
            indentation,
        )?;

        if formatted_content != content {
            fs::write(&file, formatted_content)?;
            any_modified = true;
        }
    }

    Ok(ExitCode::from(if any_modified { 1 } else { 0 }))
}
