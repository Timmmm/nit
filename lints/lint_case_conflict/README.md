# Lint for file case conflicts

Fails if any filenames differ only in case. This uses Rust's `.to_uppercase()` to normalise the paths, which may not match what Windows actually does.
