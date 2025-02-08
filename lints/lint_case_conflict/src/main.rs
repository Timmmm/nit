use ignore::WalkBuilder;
use std::process::ExitCode;

fn main() -> Result<ExitCode, String> {
    let mut filenames = vec![];

    for result in WalkBuilder::new(".")
        .ignore(false)
        .parents(false)
        .hidden(false)
        .git_global(false)
        .require_git(false)
        .follow_links(false)
        .build()
    {
        let entry = result.map_err(|err| format!("Error: {err}"))?;
        let path_str = entry.path().to_string_lossy();
        let path_uppercase = path_str.to_uppercase();
        let path_original = path_str.to_string();

        filenames.push((path_uppercase, path_original));
    }

    filenames.sort();

    let mut conflict = false;

    for window in filenames.windows(2) {
        let (upper_0, orig_0) = &window[0];
        let (upper_1, orig_1) = &window[1];

        if upper_0 == upper_1 {
            eprintln!("Filename conflict: {} and {}", orig_0, orig_1);
            conflict = true;
        }
    }

    Ok(ExitCode::from(if conflict { 1 } else { 0 }))
}
