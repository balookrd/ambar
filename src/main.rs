use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ambar::drbd::adm::Drbd;
use ambar::drbd::status::ResourceStatus;
use ambar::exec::SystemRunner;
use ambar::model::{SpecError, VolumeSpec, valid_volume_name};
use ambar::module_loader::{self, LoaderEnv};
use ambar::paths::Paths;
use ambar::reconcile::{ReconcileError, Reconciler};

/// Exit code of `ambar apply` and `ambar remove` when another process holds
/// the volume lock: EX_TEMPFAIL from `sysexits.h`.
const EXIT_BUSY: u8 = 75;

const EXIT_CODES: &str = "Exit codes:\n  0   done\n  75  the volume is locked by another ambar process; try again later\n  1   any other error";

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
    /// Bring this node in line with a volume description. The node is the
    /// host name: `drbdadm` picks the `on <host>` section by it.
    #[command(after_help = EXIT_CODES)]
    Apply {
        /// JSON file with the volume description.
        #[arg(long)]
        file: PathBuf,
    },
    /// Take a volume down on this node and delete its local files.
    #[command(after_help = EXIT_CODES)]
    Remove {
        #[arg(long)]
        name: String,
    },
    /// Print the DRBD state of a volume on this node.
    Status {
        #[arg(long)]
        name: String,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ambar: {err:#}");
            exit_code(&err)
        }
    }
}

fn exit_code(err: &anyhow::Error) -> ExitCode {
    match err.downcast_ref::<ReconcileError>() {
        Some(ReconcileError::Busy { .. }) => ExitCode::from(EXIT_BUSY),
        _ => ExitCode::FAILURE,
    }
}

fn run(cli: Cli) -> Result<()> {
    let runner = SystemRunner;
    let paths = Paths::default();
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
        Command::Apply { file } => {
            let json = fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
            let spec = VolumeSpec::from_json(&json)?;
            // Equals `uname -n`, which `drbdadm` matches against `on <host>`.
            let node = read_trimmed(Path::new("/proc/sys/kernel/hostname"))?;
            let done = Reconciler {
                paths: &paths,
                runner: &runner,
            }
            .apply(&spec, &node)?;
            println!("{}: {done:?}", spec.name);
        }
        Command::Remove { name } => {
            let steps = Reconciler {
                paths: &paths,
                runner: &runner,
            }
            .remove(&name)?;
            println!("{name}: {steps:?}");
        }
        Command::Status { name } => match status_of(&runner, &name)? {
            None => println!("{name}: not configured"),
            Some(status) => {
                let device = status.device().context("the resource has no volume 0")?;
                println!(
                    "{name}: role={:?} disk={:?} quorum={} open={}",
                    status.role, device.disk_state, device.quorum, device.open
                );
                for conn in &status.connections {
                    let peer_disk = conn
                        .peer_devices
                        .iter()
                        .find(|p| p.volume == 0)
                        .map(|p| format!("{:?}", p.peer_disk_state));
                    println!(
                        "  {}: {:?} role={:?} disk={}",
                        conn.name,
                        conn.connection_state,
                        conn.peer_role,
                        peer_disk.as_deref().unwrap_or("-")
                    );
                }
            }
        },
    }
    Ok(())
}

fn status_of(runner: &SystemRunner, name: &str) -> Result<Option<ResourceStatus>> {
    if !valid_volume_name(name) {
        return Err(SpecError::Name(name.to_string()).into());
    }
    Ok(Drbd { runner }.status(name)?)
}

fn read_trimmed(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.trim().to_string())
}
