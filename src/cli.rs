//! Mapping of errors to the process exit code and CLI definition of the `ambar` binary.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::reconcile::ReconcileError;

/// Options for the node agent runtime and failure handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOptions {
    pub quorum_loss_grace_period: Duration,
    pub adjust_attempts: u32,
    pub adjust_interval: Duration,
    pub metrics_addr: String,
    pub volume_group: Option<String>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            quorum_loss_grace_period: Duration::from_secs(30),
            adjust_attempts: 3,
            adjust_interval: Duration::from_secs(5),
            metrics_addr: "0.0.0.0:9770".to_string(),
            volume_group: None,
        }
    }
}

/// Exit code of `ambar apply` and `ambar remove` when another process holds
/// the volume lock: EX_TEMPFAIL from `sysexits.h`.
pub const EXIT_BUSY: u8 = 75;

/// Exit code for any failure other than a busy volume.
pub const EXIT_FAILURE: u8 = 1;

pub const EXIT_CODES: &str = "Exit codes:\n  0   done\n  75  the volume is locked by another ambar process; try again later\n  1   any other error";

pub const REPLICA_AFTER_HELP: &str = "\
Exit codes:
  0   the replica change is complete
  1   the change failed, timed out, or was rejected
";

#[derive(Parser, Debug)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum ReplicaCommand {
    /// Add a replica copy of a volume on a node.
    #[command(after_help = REPLICA_AFTER_HELP)]
    Add {
        /// Name of the volume.
        volume: String,
        /// Name of the node where the replica copy will be added.
        node: String,
        #[arg(long, env = "POD_NAMESPACE", default_value = "ambar-system")]
        namespace: String,
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
    /// Remove a replica copy of a volume from a node.
    #[command(after_help = REPLICA_AFTER_HELP)]
    Remove {
        /// Name of the volume.
        volume: String,
        /// Name of the node whose replica copy will be removed.
        node: String,
        #[arg(long, env = "POD_NAMESPACE", default_value = "ambar-system")]
        namespace: String,
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
    /// Move a replica copy of a volume from one node to another.
    #[command(after_help = REPLICA_AFTER_HELP)]
    Move {
        /// Name of the volume.
        volume: String,
        /// Source node whose replica copy will be moved.
        from: String,
        /// Destination node where the replica copy will be moved.
        to: String,
        #[arg(long, env = "POD_NAMESPACE", default_value = "ambar-system")]
        namespace: String,
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
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
        /// LVM Volume Group for direct block storage of DRBD volumes.
        #[arg(long)]
        volume_group: Option<String>,
    },
    /// Take a volume down on this node and irrevocably delete the local copy
    /// of its data: the backing file, its markers and the resource files.
    #[command(after_help = EXIT_CODES)]
    Remove {
        /// Name of the volume.
        #[arg(long)]
        name: String,
        /// LVM Volume Group for direct block storage of DRBD volumes.
        #[arg(long)]
        volume_group: Option<String>,
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
        /// Namespace of the agent's own objects: the ConfigMap `ambar-nodes`
        /// is read from it.
        #[arg(long, env = "POD_NAMESPACE", default_value = "ambar-system")]
        namespace: String,
        #[arg(long, default_value = "/opt/ambar/modules")]
        modules_dir: PathBuf,
        /// Grace period in seconds before evicting pods when quorum is lost.
        #[arg(long, default_value = "30")]
        quorum_loss_grace_period: u64,
        /// Maximum attempts to adjust a resource in StandAlone before flagging manual check needed.
        #[arg(long, default_value = "3")]
        adjust_attempts: u32,
        /// Interval in seconds between adjust attempts for a StandAlone resource.
        #[arg(long, default_value = "5")]
        adjust_interval: u64,
        /// TCP address to expose Prometheus metrics.
        #[arg(long, default_value = "0.0.0.0:9770")]
        metrics_addr: String,
        /// LVM Volume Group for direct block storage of DRBD volumes.
        #[arg(long)]
        volume_group: Option<String>,
    },
    /// Manage volume replicas: add, remove, or move copies.
    #[command(subcommand, after_help = REPLICA_AFTER_HELP)]
    Replica(ReplicaCommand),
    /// Print the CustomResourceDefinition of `ReplicatedVolume` as JSON.
    Crd,
    /// Print the DRBD state of a volume on this node.
    Status {
        #[arg(long)]
        name: String,
    },
}

/// 75 when `err` is (or wraps) `ReconcileError::Busy`, 1 for anything else.
pub fn exit_code(err: &anyhow::Error) -> u8 {
    match err.downcast_ref::<ReconcileError>() {
        Some(ReconcileError::Busy { .. }) => EXIT_BUSY,
        _ => EXIT_FAILURE,
    }
}

#[cfg(test)]
#[path = "tests/cli.rs"]
mod tests;
