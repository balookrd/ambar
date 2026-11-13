//! Typed view of `drbdsetup status --json`.
//!
//! Only the fields the agent acts on are modelled; unknown keys are ignored
//! and unknown enum values map to `Other`, so a newer DRBD does not break
//! parsing. The JSON mixes kebab-case and snake_case keys (`disk-state`,
//! `peer_devices`); the renames below follow the captured samples in
//! `stand/spike/samples/`.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Role {
    Primary,
    Secondary,
    Unknown,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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

    /// `true` only for a freshly created volume that has never held data:
    /// this node has an `Inconsistent` disk, every peer is connected, and
    /// every peer either has an `Inconsistent` disk too or attaches without
    /// one. Only in this state may the initial full sync be skipped.
    pub fn ready_to_skip_initial_sync(&self) -> bool {
        let local_inconsistent = self.device().is_some_and(|d| d.disk_state == DiskState::Inconsistent);
        let peers_fresh = self.connections.iter().all(|c| {
            c.peer_devices.iter().all(|p| {
                matches!(p.peer_disk_state, DiskState::Inconsistent)
                    || (p.peer_client && p.peer_disk_state == DiskState::Diskless)
            })
        });
        local_inconsistent && !self.connections.is_empty() && self.all_peers_connected() && peers_fresh
    }
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
