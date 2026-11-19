use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use ambar::agent::kube::KubeVolumes;
use ambar::agent::nodes::ConfigMapNodes;
use ambar::agent::run::{Agent, VolumeLocks, preflight};
use ambar::cli::{self, Cli, Command, ReplicaCommand};
use ambar::csi::{self, ControllerService, IdentityService, NodeService, Services};
use ambar::drbd::adm::Drbd;
use ambar::drbd::status::ResourceStatus;
use ambar::exec::SystemRunner;
use ambar::model::{SpecError, VolumeSpec, valid_volume_name};
use ambar::module_loader::{self, LoaderEnv};
use ambar::paths::Paths;
use ambar::reconcile::Reconciler;
use tokio_util::sync::CancellationToken;

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
            namespace,
            modules_dir,
            quorum_loss_grace_period,
            adjust_attempts,
            adjust_interval,
            metrics_addr,
            volume_group,
        } => run_agent(
            &node_name,
            &csi_socket,
            &namespace,
            &modules_dir,
            paths,
            cli::AgentOptions {
                quorum_loss_grace_period: Duration::from_secs(quorum_loss_grace_period),
                adjust_attempts,
                adjust_interval: Duration::from_secs(adjust_interval),
                metrics_addr,
                volume_group,
            },
        )?,
        Command::Apply { file, volume_group } => {
            let json = fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
            let spec = VolumeSpec::from_json(&json)?;
            // Equals `uname -n`, which `drbdadm` matches against `on <host>`.
            let node = read_trimmed(Path::new("/proc/sys/kernel/hostname"))?;
            let done = Reconciler {
                paths: &paths,
                runner: &runner,
                volume_group,
            }
            .apply(&spec, &node)?;
            println!("{}: {done:?}", spec.name);
        }
        Command::Remove { name, volume_group } => {
            let steps = Reconciler {
                paths: &paths,
                runner: &runner,
                volume_group,
            }
            .remove(&name)?;
            println!("{name}: {steps:?}");
        }
        Command::Replica(cmd) => run_replica_cmd(cmd)?,
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
fn run_agent(
    node: &str,
    csi_socket: &Path,
    namespace: &str,
    modules_dir: &Path,
    paths: Paths,
    options: cli::AgentOptions,
) -> Result<()> {
    let hostname = read_trimmed(Path::new("/proc/sys/kernel/hostname"))?;
    preflight(node, &hostname, &paths, options.volume_group.as_deref())?;
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
        // The reconcile loop and the Controller share one object: its list
        // reads the cache the watch fills.
        let volumes = Arc::new(KubeVolumes::new(client.clone()));
        let nodes = Arc::new(ConfigMapNodes::new(client, namespace));
        let controller = ControllerService::new(volumes.clone(), nodes.clone());
        let agent = Arc::new(Agent {
            node: node.to_string(),
            paths,
            runner,
            volumes,
            locks: VolumeLocks::default(),
            ready: Arc::new(AtomicBool::new(false)),
            options: options.clone(),
            metrics: Default::default(),
            is_tainted: Arc::new(AtomicBool::new(false)),
        });
        let shutdown = CancellationToken::new();
        tokio::spawn(cancel_on_signal(shutdown.clone()));
        let services = Services {
            identity: IdentityService {
                ready: agent.ready.clone(),
            },
            controller,
            node: NodeService::new(agent.clone(), nodes),
        };
        eprintln!("ambar-agent: node {node} started");
        // Either side ending (an error, or the signal) stops the other one.
        let metrics_addr = options.metrics_addr.clone();
        let (agent_result, csi_result, metrics_result) = tokio::join!(
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
            async {
                let result = agent.serve_metrics(&metrics_addr, shutdown.clone()).await;
                if let Err(ref e) = result {
                    eprintln!("ambar-agent: the metrics server error: {e}");
                }
                result
            },
        );
        eprintln!("ambar-agent: stopped");
        if let Err(e) = metrics_result {
            eprintln!("ambar-agent: the metrics server error: {e}");
        }
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

fn run_replica_cmd(cmd: ReplicaCommand) -> Result<()> {
    let (action, namespace, timeout) = match cmd {
        ReplicaCommand::Add {
            volume,
            node,
            namespace,
            timeout,
        } => (
            ambar::agent::replica_cmd::ReplicaAction::Add { volume, node },
            namespace,
            timeout,
        ),
        ReplicaCommand::Remove {
            volume,
            node,
            namespace,
            timeout,
        } => (
            ambar::agent::replica_cmd::ReplicaAction::Remove { volume, node },
            namespace,
            timeout,
        ),
        ReplicaCommand::Move {
            volume,
            from,
            to,
            namespace,
            timeout,
        } => (
            ambar::agent::replica_cmd::ReplicaAction::Move { volume, from, to },
            namespace,
            timeout,
        ),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;

    runtime.block_on(async {
        let client = kube::Client::try_default()
            .await
            .context("cannot configure the Kubernetes client (in-cluster configuration or kubeconfig)")?;
        let volumes = KubeVolumes::new(client.clone());
        let nodes = ConfigMapNodes::new(client, &namespace);
        let shutdown = CancellationToken::new();
        tokio::spawn(cancel_on_signal(shutdown.clone()));
        let mut stdout = std::io::stdout();
        ambar::agent::replica_cmd::run_replica(&volumes, &nodes, &action, timeout, &mut stdout, shutdown)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    })
}
