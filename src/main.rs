use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{io::Write, path::PathBuf};
use witnessfs::{model::*, store::Store, transfer};

#[derive(Parser)]
#[command(
    version,
    about = "Observed filesystem dependencies for parallel agent workspaces"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".witness")]
    store: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a store from a stable source directory (source is never modified).
    Init {
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Create an isolated snapshot of the current committed revision.
    Branch { name: String },
    /// Show branch lifecycle states.
    Status,
    /// Mount a branch in the foreground. Linux and FUSE required.
    Mount { branch: String, mountpoint: PathBuf },
    /// Freeze an unmounted branch against further reads and writes.
    Seal { branch: String },
    /// Validate observations and publish all writes atomically. Exit 3 on conflict.
    Accept { branch: String },
    /// Display the exact dependencies observed by a branch.
    Observe { branch: String },
    /// Read through the observation layer without mounting (does not follow symlinks).
    Read { branch: String, path: PathBuf },
    /// Export a committed state or private branch to a new directory.
    Export {
        destination: PathBuf,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Show a previously saved acceptance or rejection receipt.
    Receipt { branch: String },
    /// Show storage and dependency statistics.
    Stats,
    /// Run SQLite's structural integrity check.
    Check,
}

fn json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("witnessfs: {e:#}");
            std::process::exit(1)
        }
    }
}
fn run() -> Result<i32> {
    let cli = Cli::parse();
    if let Command::Init { from } = cli.command {
        let mut s = Store::create(&cli.store)?;
        if let Some(source) = from {
            json(&transfer::import(&mut s, &source)?)?;
        } else {
            json(&s.stats()?)?;
        }
        return Ok(0);
    }
    if let Command::Mount { branch, mountpoint } = cli.command {
        #[cfg(target_os = "linux")]
        witnessfs::mount::foreground(&cli.store, &branch, &mountpoint)?;
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (branch, mountpoint);
            anyhow::bail!("FUSE mounting requires Linux; use the included Docker runner on macOS");
        }
        #[allow(unreachable_code)]
        return Ok(0);
    }
    let mut s = Store::open(&cli.store)?;
    match cli.command {
        Command::Branch { name } => json(&s.branch(&name)?)?,
        Command::Status => json(&s.branches()?)?,
        Command::Seal { branch } => {
            s.seal(&branch)?;
            json(&s.info(&branch)?)?;
        }
        Command::Accept { branch } => {
            let receipt = s.accept(&branch)?;
            json(&receipt)?;
            if receipt.outcome == "rejected" {
                return Ok(3);
            }
        }
        Command::Observe { branch } => json(&s.observations(&branch)?)?,
        Command::Receipt { branch } => {
            json(&s.receipt(&branch)?.context("branch has no receipt")?)?
        }
        Command::Export {
            destination,
            branch,
        } => transfer::export(&mut s, branch.as_deref(), &destination)?,
        Command::Stats => json(&s.stats()?)?,
        Command::Check => println!("{}", s.integrity()?),
        Command::Read { branch, path } => {
            let bytes = s.transact(&branch, |v| {
                let n = v.resolve(&path)?.context("file not found")?;
                anyhow::ensure!(
                    n.size <= MAX_IO_SIZE,
                    "use mounted streaming reads for files larger than 16 MiB"
                );
                v.read(n.id, 0, n.size, &path.to_string_lossy())
            })?;
            std::io::stdout().write_all(&bytes)?;
        }
        Command::Init { .. } | Command::Mount { .. } => unreachable!(),
    }
    Ok(0)
}
