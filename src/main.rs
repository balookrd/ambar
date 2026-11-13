use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ambar::exec::SystemRunner;
use ambar::module_loader::{self, LoaderEnv};

#[derive(Parser)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load the DRBD 9 kernel module bundled in the image.
    LoadModule {
        #[arg(long, default_value = "/opt/ambar/modules")]
        modules_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ambar: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let runner = SystemRunner;
    match cli.command {
        Command::LoadModule { modules_dir } => {
            let kernel_release = read_trimmed(Path::new("/proc/sys/kernel/osrelease"))?;
            let env = LoaderEnv {
                modules_dir: &modules_dir,
                sys_module_dir: Path::new("/sys/module"),
                kernel_release: &kernel_release,
            };
            println!("{}", module_loader::load(&env, &runner)?);
        }
    }
    Ok(())
}

fn read_trimmed(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.trim().to_string())
}
