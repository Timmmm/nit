use std::{
    io::{self, Read as _},
    path::Path,
    process::ExitCode,
};

fn main() -> io::Result<ExitCode> {
    let mut fail = false;

    for file in std::env::args().skip(1) {
        if file_needs_to_be_executable(Path::new(&file))? {
            eprintln!("Not executable: {}", file);
            fail = true;
        }
    }

    Ok(ExitCode::from(if fail { 1 } else { 0 }))
}

// TODO: We can auto-fix this too by marking it executable.
// Maybe we need to provide a Git interface. Or we could just make the
// linter link with gitoxide.
fn file_needs_to_be_executable(path: &Path) -> io::Result<bool> {
    let metadata = std::fs::metadata(path)?;
    let permissions = metadata.permissions();
    // TODO: We actually need to use Git to check for executable permissions
    // anyway since they don't exist on Windows.
    let is_executable: bool = todo!();

    Ok(!is_executable && {
        // Check if the file is a script (e.g., starts with a shebang)
        let mut file = std::fs::File::open(path)?;
        let mut buffer = [0; 2];
        match file.read_exact(&mut buffer) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
            other => other,
        }?;
        buffer == [b'#', b'!']
    })
}
