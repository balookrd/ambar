//! Node agent: brings the node in line with `ReplicatedVolume` objects. The
//! modules here are pure decisions; Kubernetes calls and commands live elsewhere.

pub mod decide;
pub mod status;
