use std::{fs, io, process::ExitCode};

fn main() -> io::Result<ExitCode> {
    for file in std::env::args().skip(1) {
        let contents = fs::read(&file)?;
        if contents.contains(&b'\t') {
            return Ok(ExitCode::from(1));
        }
    }

    Ok(ExitCode::from(0))
}
