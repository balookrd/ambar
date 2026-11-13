use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ambar::agent::kube::KubeVolumes;
use ambar::agent::run::{Agent, VolumeLocks, preflight};
use ambar::cli;
use ambar::csi::{self, ControllerService, IdentityService, NodeService, Services};
use ambar::drbd::adm::Drbd;
use ambar::drbd::status::ResourceStatus;
use ambar::exec::SystemRunner;
use ambar::model::{SpecError, VolumeSpec, valid_volume_name};
use ambar::module_loader::{self, LoaderEnv};
use ambar::paths::Paths;
use ambar::reconcile::Reconciler;
use tokio_util::sync::CancellationToken;

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
    /// Take a volume down on this node and irrevocably delete the local copy
    /// of its data: the backing file, its markers and the resource files.
    #[command(after_help = EXIT_CODES)]
    Remove {
        #[arg(long)]
        name: String,
    },
    /// Run the node agent: load the DRBD module, then reconcile this node with
    /// the `ReplicatedVolume` objects until SIGTERM. Needs the API server
    /// (in-cluster configuration or a kubeconfig).
    Agent {
        /// Kubernetes name of this node; must equal the host name.
        #[arg(long, env = "NODE_NAME")]
        node_name: String,
        /// Unix socket of the CSI gRPC server (Identity, Controller, Node).
        /// A stale socket file is replaced; any other file there is an error.
        #[arg(long, default_value = "/var/lib/kubelet/plugins/ambar/csi.sock")]
        csi_socket: PathBuf,
        /// Namespace of the agent's own objects. Not used yet.
        #[arg(long, env = "POD_NAMESPACE", default_value = "ambar-system")]
        namespace: String,
        #[arg(long, default_value = "/opt/ambar/modules")]
        modules_dir: PathBuf,
    },
    /// Print the CustomResourceDefinition of `ReplicatedVolume` as JSON.
    Crd,
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
            ExitCode::from(cli::exit_code(&err))
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let runner = SystemRunner::default();
    let paths = Paths::default();
    match cli.command {
        Command::LoadModule { modules_dir } => println!("{}", load_module(&modules_dir, &runner)?),
        Command::Agent {
            node_name,
            csi_socket,
            modules_dir,
            ..
        } => run_agent(&node_name, &csi_socket, &modules_dir, paths)?,
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
        Command::Crd => print!("{}", ambar::crd::crd_json()),
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

fn load_module(modules_dir: &Path, runner: &SystemRunner) -> Result<String> {
    let kernel_release = read_trimmed(Path::new("/proc/sys/kernel/osrelease"))?;
    let env = LoaderEnv {
        modules_dir,
        sys_module_dir: Path::new("/sys/module"),
        kernel_release: &kernel_release,
    };
    Ok(module_loader::load(&env, runner)?)
}

/// The checks that need no API server come first: a wrong node name or a
/// missing volumes directory must stop the agent before it touches anything.
fn run_agent(node: &str, csi_socket: &Path, modules_dir: &Path, paths: Paths) -> Result<()> {
    let hostname = read_trimmed(Path::new("/proc/sys/kernel/hostname"))?;
    preflight(node, &hostname, &paths)?;
    let runner = Arc::new(SystemRunner::default());
    eprintln!("ambar-agent: {}", load_module(modules_dir, &runner)?);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(async {
        let client = kube::Client::try_default()
            .await
            .context("cannot configure the Kubernetes client (in-cluster configuration or kubeconfig)")?;
        let agent = Agent {
            node: node.to_string(),
            paths,
            runner,
            volumes: Arc::new(KubeVolumes::new(client)),
            locks: VolumeLocks::default(),
            ready: Arc::new(AtomicBool::new(false)),
        };
        let shutdown = CancellationToken::new();
        tokio::spawn(cancel_on_signal(shutdown.clone()));
        let services = Services {
            identity: IdentityService {
                ready: agent.ready.clone(),
            },
            controller: ControllerService,
            node: NodeService,
        };
        eprintln!("ambar-agent: node {node} started");
        // Either side ending (an error, or the signal) stops the other one.
        let (agent_result, csi_result) = tokio::join!(
            async {
                let result = agent.run(shutdown.clone()).await;
                shutdown.cancel();
                result
            },
            async {
                let result = csi::serve(csi_socket, services, shutdown.clone()).await;
                shutdown.cancel();
                result
            },
        );
        eprintln!("ambar-agent: stopped");
        match (agent_result, csi_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(agent_err), Ok(())) => Err(agent_err.into()),
            (Ok(()), Err(csi_err)) => Err(anyhow::Error::from(csi_err).context("the CSI server stopped")),
            (Err(agent_err), Err(csi_err)) => {
                eprintln!("ambar-agent: the CSI server also failed: {csi_err}");
                Err(agent_err.into())
            }
        }
    })
}

/// SIGTERM (the pod is stopped) or SIGINT: the agent finishes the pass in progress and returns.
async fn cancel_on_signal(shutdown: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        eprintln!("ambar-agent: cannot install the signal handlers; the agent only stops when killed");
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("ambar-agent: stopping");
    shutdown.cancel();
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
