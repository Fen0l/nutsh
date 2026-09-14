//! Developer tasks: `cargo xtask <command>`.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "nutsh developer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Regenerate crates/catalog/src/generated.rs from specs/
    GenCatalog,
    /// Regenerate the coverage tables in README.md and docs/coverage.md
    GenCoverage,
    /// Record redacted fixtures from a live Prism Central into crates/mockpc/fixtures-lab
    Record {
        #[arg(long, env = "NUTSH_HOST")]
        host: String,
        #[arg(long, env = "NUTSH_PORT", default_value_t = 9440)]
        port: u16,
        #[arg(long, env = "NUTSH_USERNAME")]
        username: String,
        /// Skip TLS certificate verification
        #[arg(long)]
        insecure: bool,
        /// PEM bundle to trust in addition to the system roots
        #[arg(long)]
        ca_bundle: Option<PathBuf>,
        /// Comma-separated kind ids or aliases; default: every top-level GA kind but the identity and secret ones
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
        /// Pages of 100 to record per kind
        #[arg(long, default_value_t = 1)]
        pages: u32,
        /// Never the curated crates/mockpc/fixtures tree: the test suite asserts against it
        #[arg(long, default_value = "crates/mockpc/fixtures-lab")]
        out: PathBuf,
    },
    /// Re-apply the recorder's masking to fixtures recorded earlier, in place
    Scrub {
        /// Files or trees to rewrite; never the curated crates/mockpc/fixtures tree
        #[arg(default_value = "crates/mockpc/fixtures-lab")]
        paths: Vec<PathBuf>,
        /// Report what would change and write nothing
        #[arg(long)]
        check: bool,
    },
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the workspace")
        .to_path_buf()
}

fn main() -> anyhow::Result<()> {
    // Before clap and before the runtime: `remove_var` is only sound while this is the
    // single thread, and no child process may inherit the secret. Every subcommand,
    // including the ones that never ask for a password, starts from a scrubbed environment.
    let from_env = xtask::record::env_password()?;
    let cli = Cli::parse();
    match cli.command {
        Command::GenCatalog => xtask::catalog::generate(&workspace_root()),
        Command::GenCoverage => xtask::coverage::generate(&workspace_root()),
        Command::Record {
            host,
            port,
            username,
            insecure,
            ca_bundle,
            kinds,
            pages,
            out,
        } => {
            let password = match from_env {
                Some(p) => p,
                None => rpassword::prompt_password("Password: ")?,
            };
            let out = if out.is_absolute() {
                out
            } else {
                workspace_root().join(out)
            };
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(xtask::record::record(xtask::record::RecordArgs {
                host,
                port,
                username,
                password,
                insecure,
                ca_bundle,
                kinds,
                pages,
                out,
                curated_dir: workspace_root().join("crates/mockpc/fixtures"),
            }))
        }
        Command::Scrub { paths, check } => {
            let root = workspace_root();
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| if p.is_absolute() { p } else { root.join(p) })
                .collect();
            xtask::record::scrub(&paths, &root.join("crates/mockpc/fixtures"), check)
        }
    }
}
