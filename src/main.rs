mod bash_paths;
mod config;
mod engine;
mod fetch;
mod file_matching;
mod git;
mod hash_adapter;
mod metadata;
mod serde_glob;
mod serde_regex;
mod unique_filename;
mod wasi_cache;

use anyhow::{anyhow, bail, Result};
use bash_paths::path_to_bash_string;
use clap::{Parser, Subcommand, ValueEnum};
use config::{read_config, Config};
use engine::{get_cache_dir, run_single_linter};
use env_logger::{Builder, Env};
use fetch::fetch_linters;
use file_matching::retain_matching_files;
use git::git_diff_unstaged;
use log::info;
use metadata::{has_metadata, read_metadata};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Parser)]
#[command(
    name = "pre-commit",
    version,
    about = "A CLI for managing pre-commit hooks"
)]
struct Cli {
    #[arg(long, default_value_t = ColorOutput::Auto)]
    color: ColorOutput,

    #[arg(long)]
    quiet: bool,

    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    /// Remove downloaded linters.
    Clean,
    /// Download linters (this will be done automatically but it's useful for Docker images)
    Fetch,
    /// Install git hooks so this will run automatically
    Install(InstallArgs),
    /// Remove git hooks
    Uninstall,
    /// Run configured linters over the files
    Run(RunArgs),
    /// Print a sample config file.
    SampleConfig,
    /// Validate the supplied config.
    ValidateConfig,
    /// Show metadata for a linter WASM file.
    ShowMetadata(ShowMetadataArgs),
    /// Set metadata for a linter WASM file.
    SetMetadata(SetMetadataArgs),
    /// Run the pre-commit hook.
    PreCommit,
    /// Run the pre-push hook.
    PrePush(PrePushArgs),
}

#[derive(Parser)]
struct InstallArgs {
    #[arg(long)]
    hook_type: Option<HookType>,
}

#[derive(Parser)]
struct RunArgs {
    /// Run over all files, not just staged files.
    #[arg(short, long)]
    all: bool,

    #[arg(long)]
    files: Vec<PathBuf>,

    #[arg(long)]
    show_diff_on_failure: bool,
    // TODO (2.0): Add an option not to fix the files. Hooks will always fix files
    // but we can write a VFS layer for WASI that doesn't write the files back
    // to disk if this option is set.
    // #[arg(long)]
    // no_fix: bool,
}

#[derive(Parser)]
struct ShowMetadataArgs {
    /// WASM file to show the metadata for.
    file: PathBuf,
}

#[derive(Parser)]
struct SetMetadataArgs {
    /// WASM file to set the metadata on.
    file: PathBuf,

    /// Path to JSON file containing the metadata.
    #[arg(long)]
    metadata: PathBuf,
}

#[derive(Parser)]
struct PrePushArgs {
    /// Name of the remote (or its URL if it doesn't have a name).
    remote: String,

    /// URL of the remote.
    url: String,
}

#[derive(ValueEnum, Clone)]
enum ColorOutput {
    Auto,
    Always,
    Never,
}

impl std::fmt::Display for ColorOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColorOutput::Auto => write!(f, "auto"),
            ColorOutput::Always => write!(f, "always"),
            ColorOutput::Never => write!(f, "never"),
        }
    }
}

#[derive(ValueEnum, Clone, Default)]
enum HookType {
    #[default]
    PreCommit,
    PrePush,
}

impl HookType {
    fn as_str(&self) -> &str {
        match self {
            HookType::PreCommit => "pre-commit",
            HookType::PrePush => "pre-push",
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_level = if cli.quiet { "warn" } else { "info" };
    let env = Env::new()
        .filter_or("NIT_LOG", default_level)
        .write_style("NIT_LOG_STYLE");
    Builder::from_env(env)
        .format_timestamp(None)
        .format_target(false)
        .init();

    match &cli.command {
        SubCommand::Clean => subcommand_clean(&cli).await,
        SubCommand::Fetch => subcommand_fetch(&cli).await,
        SubCommand::Install(args) => subcommand_install(&cli, args).await,
        SubCommand::Uninstall => subcommand_uninstall(&cli).await,
        SubCommand::Run(args) => subcommand_run(&cli, args).await,
        SubCommand::SampleConfig => subcommand_sample_config(&cli).await,
        SubCommand::ValidateConfig => subcommand_validate_config(&cli).await,
        SubCommand::ShowMetadata(args) => subcommand_show_metadata(&cli, args).await,
        SubCommand::SetMetadata(args) => subcommand_set_metadata(&cli, args).await,
        SubCommand::PreCommit => subcommand_pre_commit(&cli).await,
        SubCommand::PrePush(args) => subcommand_pre_push(&cli, args).await,
    }
}

fn find_and_read_config(top_level: &Path, config: &Option<PathBuf>) -> Result<Config> {
    if let Some(path) = config {
        read_config(path)
    } else {
        for filename in &[".nit.json5", ".nit.jsonc", ".nit.json"] {
            let path = top_level.join(filename);
            if path.exists() {
                return read_config(&path);
            }
        }
        bail!("No config file found (.nit.json5/jsonc/json) in the repository");
    }
}

async fn subcommand_clean(_cli: &Cli) -> Result<()> {
    let cache_dir = get_cache_dir().ok_or(anyhow!("Could not determine cache directory"))?;
    fs::remove_dir_all(cache_dir).await?;
    info!("Cache directory cleaned");
    Ok(())
}

async fn subcommand_fetch(cli: &Cli) -> Result<()> {
    let top_level = git::git_top_level()?;
    let config = find_and_read_config(&top_level, &cli.config)?;
    let cache_dir = get_cache_dir().ok_or(anyhow!("Could not determine cache directory"))?;
    fetch_linters(&config.linters, &cache_dir).await
}

async fn subcommand_install(cli: &Cli, args: &InstallArgs) -> Result<()> {
    let current_exe = std::env::current_exe()?;
    let hooks_dir = git::git_hooks_dir()?;
    fs::create_dir_all(&hooks_dir).await?;
    let hook_type = args.hook_type.clone().unwrap_or_default();
    let hook_path = hooks_dir.join(hook_type.as_str());
    if fs::try_exists(&hook_path).await? {
        let content = fs::read(&hook_path).await?;
        if memchr::memmem::find(&content, b"nit").is_none() {
            bail!(
                "Hook '{}' already exists and isn't a Nit hook.",
                hook_type.as_str()
            );
        }
    }
    let exe_path = bash_paths::path_to_bash_string(&current_exe)?;

    let config_arg = if let Some(config) = &cli.config {
        format!("--config {}", path_to_bash_string(config)?)
    } else {
        String::new()
    };

    fs::write(
        &hook_path,
        format!(
            "#!/bin/bash\n\nset -e\n\n{exe_path} {config_arg} {} \"$@\"\n",
            hook_type.as_str()
        ),
    )
    .await?;

    // TODO (0.1): Confirm if we actually need to make it executable on Unix. I think
    // Git might just parse it and run it itself.
    #[cfg(unix)]
    set_executable(&hook_path).await?;

    log::info!("Installed pre-commit hook");
    Ok(())
}

#[cfg(unix)]
async fn set_executable(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).await?;
    let mut permissions = metadata.permissions();

    use std::os::unix::fs::PermissionsExt;

    permissions.set_mode(permissions.mode() | 0o111);

    fs::set_permissions(path, permissions).await?;
    Ok(())
}

async fn subcommand_uninstall(_cli: &Cli) -> Result<()> {
    let hooks_dir = git::git_hooks_dir()?;
    for hook_type in &[HookType::PreCommit, HookType::PrePush] {
        let hook_path = hooks_dir.join(hook_type.as_str());
        let content = fs::read(&hook_path).await?;
        if memchr::memmem::find(&content, b"nit").is_some() {
            fs::remove_file(&hook_path).await?;
            info!("Uninstalled hook '{}'", hook_type.as_str());
        } else {
            info!("Hook '{}' is not a Nit hook.", hook_type.as_str());
        }
    }
    Ok(())
}

async fn subcommand_sample_config(_cli: &Cli) -> Result<()> {
    let sample_config = include_str!("../sample_config.json5");
    println!("{}", sample_config);
    Ok(())
}

async fn subcommand_validate_config(cli: &Cli) -> Result<()> {
    let top_level = git::git_top_level()?;
    let _config = find_and_read_config(&top_level, &cli.config)?;
    info!("Config validated");
    Ok(())
}

async fn subcommand_run(cli: &Cli, args: &RunArgs) -> Result<()> {
    let top_level = git::git_top_level()?;
    let config = find_and_read_config(&top_level, &cli.config)?;

    let files = if args.all {
        git::git_tree_files(&top_level, "HEAD")?
    } else {
        git::git_staged_files(&top_level)?
    };

    run(top_level, config, files).await
}

async fn run(
    top_level: PathBuf,
    config: Config,
    mut files: Vec<git::FileInfo>,
) -> std::result::Result<(), anyhow::Error> {
    let cache_dir = get_cache_dir().ok_or(anyhow!("Could not determine cache directory"))?;

    // Only lint files in `include`.
    retain_matching_files(&mut files, &config.include);

    // 0. Determine the changed files (or find all files).
    // 1. Download the wasm binary (if required).
    // 2. Load it.
    // 3. Run it with `--config` to determine how we should feed it files.
    //      - chunked filenames (chunk length = 0 for all)
    //      - don't feed it anything (e.g. for cargo fmt)
    // 4. Run it over the changed files.

    fetch_linters(&config.linters, &cache_dir).await?;

    let mut diff = git_diff_unstaged(&top_level)?;

    let mut failed = false;

    // Run the linters.
    for linter in config.linters {
        eprintln!("Running linter: {}", linter.name.blue());
        let status = run_single_linter(&files, &cache_dir, &top_level, linter).await?;
        let new_diff = git_diff_unstaged(&top_level)?;

        if !status || diff != new_diff {
            failed = true;
            eprintln!("Linter {}", "failed".red());
        } else {
            eprintln!("Linter {}", "passed".green());
        }
        diff = new_diff;
    }

    if failed {
        bail!("Linting failed");
    }

    Ok(())
}

async fn subcommand_show_metadata(_cli: &Cli, args: &ShowMetadataArgs) -> Result<()> {
    let metadata = read_metadata(&args.file)?;
    println!("{metadata:?}");
    Ok(())
}

async fn subcommand_set_metadata(_cli: &Cli, args: &SetMetadataArgs) -> Result<()> {
    // TODO (1.0): Remove any existing custom metadata sections.

    let mut bytes = fs::read(&args.file).await?;
    if has_metadata(&bytes)? {
        bail!("File already has metadata. Removing it is not yet supported.");
    }
    let metadata_bytes = fs::read(&args.metadata).await?;

    // TODO (1.0): This is simple enough we can do it without an external crate.
    wasm_gen::write_custom_section(&mut bytes, "nit_metadata", &metadata_bytes);

    fs::write(&args.file, bytes).await?;

    Ok(())
}

async fn subcommand_pre_commit(cli: &Cli) -> Result<()> {
    // pre-commit takes no arguments and is run just before commit, so we
    // lint the staged files.
    // TODO (0.1): We should check that these files are clean too since we
    // are actually linting the on-disk files. Not sure what pre-commit does.
    let top_level = git::git_top_level()?;
    let config = find_and_read_config(&top_level, &cli.config)?;

    let files = git::git_staged_files(&top_level)?;

    run(top_level, config, files).await
}

async fn subcommand_pre_push(cli: &Cli, args: &PrePushArgs) -> Result<()> {
    // pre-push gets two arguments, $1 and $2, which are the name of the
    // remote and its URL respectively. If pushing without a named remote
    // then the URL is used for the name. A list of commits that are
    // being pushed is written to stdin, one per line:
    //
    //    <local ref> SP <local sha1> SP <remote ref> SP <remote sha1> LF
    //
    // Pre-commit uses this to find a list of files that have changed in the
    // push and then lints those files, assuming that we have the local
    // ref checked out. For now (without a VFS) we will do the same but
    // also verify we are pushing the current ref and the files are clean.
    //
    // TODO (0.1): Implement pre-push.
    todo!()
}

#[cfg(test)]
mod test {
    use crate::config::Config;

    #[test]
    fn verify_sample_config() {
        let sample_config = include_str!("../sample_config.json5");
        let _config: Config = serde_json5::from_str(&sample_config).unwrap();
    }
}
