WARNING! This is a work in progress. It's not ready for general use yet.

# Nit

Nit is a secure Python-free alternative to [pre-commit](https://pre-commit.com/). It has the following advantages over pre-commit.

* It's not written in Python:
    * Way faster.
    * Much easier to distribute (single statically linked binary).
    * Avoids the endless installation issues that plague Python.
* Linters are WASI plugins:
    * Naturally cross-platform.
    * Sandboxed; they can only read and write your code. No network access.
    * Many languages are supported.
    * Caching plugins (e.g. in Docker images) is much easier.
    * In future (not implemented yet), the filesystem access can be virtualised so it actually reads Git blobs. Pre-commit lints the wrong files in some cases since it can only lint actual on-disk files. This will also allow a `--no-fix` flag without requiring explicit linter support.

Probably the biggest reasons I wrote this are the Python issues (I have spend a lot of time with numerous colleagues figuring out why Python is barfing on pre-commit), and the safety (I don't really want to give random people complete access to my and my colleagues' machines just to remove tabs).

There are some limitations too:

* Since linters can't run Git some lints are a bit awkward, e.g.
    * Lint to ban submodules
    * Lint to check if files are executable (needs Git because this isn't exposed to the filesystem on Windows)
* WASM is a fair bit slower than native still.
* WASI is still very immature and compiling linters for WASI is quite a pain.

## Installation

Download the latest release from the releases page and put it in your `PATH`.

## Usage

This is similar to pre-commit. Create a `.nit.json5` file (`.jsonc` and `.json` are also accepted) in the root of your repository. Comments and trailing commas are allowed. Here's an example:

```
{
    linters: [
        {
            name: "Ruff (Python format/lint)",
            location: {
                remote: {
                    url: "https://github.com/Timmmm/ruff/releases/download/0.0.1/ruff.wasm",
                    hash: "becf10f9e95dbb08d66b01a662c6041abb53aac64f3af669e81b6abd24b7b015",
                },
            },
        },
    ],
    include: {
        bool: true,
    },
}
```

Then run `nit run --all` in the root of your repository. It will lint all the files in the repository. If you run `nit run` instead it will only lint the files that have changed, as determined by `git diff`.

To install as a git hook, run `nit install`. For compatibility with `pre-commit` this will install as a pre-commit hook by default, though I find pre-push way less annoying so I would recommend `nit install --hook-type pre-push` instead.

## Linters

Linters are WASI modules, plus a special custom section containing some metadata about how to run them.

To compile e.g. Ruff to WASI.

```
rustup toolchain install nightly
rustup +nightly target add wasm32-wasip2

git clone -b user/timh/wasi https://github.com/timmmm/ruff.git
cd ruff

rustup override set nightly

cargo build --release --target wasm32-wasip2

nit set-metadata --metadata metadata.json target\wasm32-wasip2\release\ruff.wasm
```

This uses a branch because WASI is quite immature and I had to fix some stuff.

To compile `rustfmt`:

```
git clone https://github.com/rust-lang/rustfmt.git
cd rustfmt

rustup toolchain install nightly
rustup target add wasm32-wasip2

export CFG_RELEASE=1.85.1-nightly
export CFG_RELEASE_CHANNEL=nightly
export RUSTC_BOOTSTRAP=0

cargo build --release --target wasm32-wasip2
```

## Use in CI

If you are using a custom Docker image for CI, you can bake all of the linters into it so they won't be downloaded each time it runs. Simply run `nit fetch --config <config.json>` in your Dockerfile.

## Environment Variables

Nit respects the following environment variables:

* `NIT_CACHE_DIR`: If set, the location to store downloaded linters.
