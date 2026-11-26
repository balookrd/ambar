//! Typed view of `drbdsetup status --json`.
//!
//! Only the fields the agent acts on are modelled; unknown keys are ignored
//! and unknown enum values map to `Other`, so a newer DRBD does not break
//! parsing. The JSON mixes kebab-case and snake_case keys (`disk-state`,
//! `peer_devices`); the renames below follow the captured samples in
//! `stand/spike/samples/`.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Role {
    Primary,
    Secondary,
    Unknown,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum DiskState {
    Diskless,
    Attaching,
    Detaching,
    Failed,
    Negotiating,
    Inconsistent,
    Outdated,
    DUnknown,
    Consistent,
    UpToDate,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ConnectionState {
    StandAlone,
    Disconnecting,
    Unconnected,
    Timeout,
    BrokenPipe,
    NetworkFailure,
    ProtocolError,
    TearDown,
    Connecting,
    Connected,
    #[serde(other)]
    Other,
}

impl Role {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Primary => "Primary",
            Self::Secondary => "Secondary",
            Self::Unknown => "Unknown",
            Self::Other => "Other",
        }
    }
}

impl DiskState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Diskless => "Diskless",
            Self::Attaching => "Attaching",
            Self::Detaching => "Detaching",
            Self::Failed => "Failed",
            Self::Negotiating => "Negotiating",
            Self::Inconsistent => "Inconsistent",
            Self::Outdated => "Outdated",
            Self::DUnknown => "DUnknown",
            Self::Consistent => "Consistent",
            Self::UpToDate => "UpToDate",
            Self::Other => "Other",
        }
    }
}

impl ConnectionState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StandAlone => "StandAlone",
            Self::Disconnecting => "Disconnecting",
            Self::Unconnected => "Unconnected",
            Self::Timeout => "Timeout",
            Self::BrokenPipe => "BrokenPipe",
            Self::NetworkFailure => "NetworkFailure",
            Self::ProtocolError => "ProtocolError",
            Self::TearDown => "TearDown",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Other => "Other",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for DiskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Device {
    pub volume: u32,
    pub minor: u32,
    #[serde(rename = "disk-state")]
    pub disk_state: DiskState,
    /// `true` on a node that attaches without a disk.
    pub client: bool,
    /// `true` while something holds the device open (a mount, for example).
    pub open: bool,
    pub quorum: bool,
    /// Usable size in KiB; 0 when DRBD does not report it.
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PeerDevice {
    pub volume: u32,
    /// `Established`, `Off`, `SyncTarget`, `SyncSource` and others.
    #[serde(rename = "replication-state")]
    pub replication_state: String,
    #[serde(rename = "peer-disk-state")]
    pub peer_disk_state: DiskState,
    #[serde(rename = "peer-client")]
    pub peer_client: bool,
    /// KiB that still have to be resynchronised with this peer.
    #[serde(rename = "out-of-sync")]
    pub out_of_sync: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Connection {
    #[serde(rename = "peer-node-id")]
    pub peer_node_id: u32,
    pub name: String,
    #[serde(rename = "connection-state")]
    pub connection_state: ConnectionState,
    #[serde(rename = "peer-role")]
    pub peer_role: Role,
    pub peer_devices: Vec<PeerDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceStatus {
    pub name: String,
    #[serde(rename = "node-id")]
    pub node_id: u32,
    pub role: Role,
    pub suspended: bool,
    pub devices: Vec<Device>,
    pub connections: Vec<Connection>,
}

/// Parses the JSON array printed by `drbdsetup status --json`.
pub fn parse(json: &str) -> Result<Vec<ResourceStatus>, serde_json::Error> {
    serde_json::from_str(json)
}

impl ResourceStatus {
    /// Volume 0: every ambar resource has exactly one volume.
    pub fn device(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.volume == 0)
    }

    pub fn quorum(&self) -> bool {
        self.device().is_some_and(|d| d.quorum)
    }

    /// `true` when every connection is `Connected`, and also when there are no
    /// connections at all. Do not read it as "the volume is replicated": check
    /// `connections` separately.
    pub fn all_peers_connected(&self) -> bool {
        self.connections
            .iter()
            .all(|c| c.connection_state == ConnectionState::Connected)
    }

    /// Names of the peers whose connection is `StandAlone`.
    pub fn standalone_peers(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter(|c| c.connection_state == ConnectionState::StandAlone)
            .map(|c| c.name.as_str())
            .collect()
    }

    /// Checks that every status field is consistent with a freshly created
    /// volume: this node is a `Secondary` with an `Inconsistent` diskful,
    /// closed, unsuspended volume 0; there is at least one connection, all
    /// are `Connected` and every peer is a `Secondary` (a `Primary` peer may
    /// have written); each peer has a volume-0 device in replication state
    /// `Established` whose disk is `Inconsistent`, or `Diskless` with
    /// `peer-client` set (a node attached without a disk).
    ///
    /// This is a necessary condition, not a sufficient one: status alone cannot
    /// prove that the volume never held data. The caller must additionally
    /// know that this agent has just created the volume (the reconcile logic
    /// keeps a local marker for that) before skipping the initial full sync.
    /// Any unknown or unexpected value makes the result `false`.
    pub fn ready_to_skip_initial_sync(&self) -> bool {
        let Some(local) = self.device() else {
            return false;
        };
        let local_fresh = local.disk_state == DiskState::Inconsistent
            && !local.client
            && !local.open
            && self.role == Role::Secondary
            && !self.suspended;
        let peer_fresh = |p: &PeerDevice| {
            p.replication_state == "Established"
                && (p.peer_disk_state == DiskState::Inconsistent
                    || (p.peer_client && p.peer_disk_state == DiskState::Diskless))
        };
        let peers_fresh = self.connections.iter().all(|c| {
            c.connection_state == ConnectionState::Connected
                && c.peer_role == Role::Secondary
                && c.peer_devices.iter().find(|p| p.volume == 0).is_some_and(peer_fresh)
        });
        local_fresh && !self.connections.is_empty() && peers_fresh
    }
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
