# Whitespace Lint

This lint trims trailing whitespace and ensures there is exactly one newline at the end of files. It also ensures Unix file endings.

The default filter matches all text files (no 0 byte in the first 8000 bytes). It does not have special handling for Windows line endings (CRLF) - they will be modified to be LF. All text editors on Windows support Unix line endings (LF), so I recommend just switching your editors to use that, and configuring Git to fix them on commit like this:

    git config --global core.autocrlf input
