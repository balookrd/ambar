//! Prometheus exposition format metrics for ambar (R38).
//!
//! Generates OpenMetrics / Prometheus text representation for node-level
//! and volume-level replication and quorum health.

use crate::drbd::status::{ConnectionState, DiskState, ResourceStatus};
use std::fmt::Write;

pub struct VolumeMetricData<'a> {
    pub name: &'a str,
    pub status: Option<&'a ResourceStatus>,
    pub manual_check_needed: bool,
}

pub struct NodeMetricData<'a> {
    pub node: &'a str,
    pub drbd_version: Option<&'a str>,
    pub is_tainted: bool,
    pub allocated_bytes: u64,
    pub volumes: Vec<VolumeMetricData<'a>>,
}

fn escape_label(val: &str) -> String {
    let mut out = String::with_capacity(val.len());
    for c in val.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Thread-safe storage for the latest rendered metrics exposition text.
#[derive(Default, Clone)]
pub struct MetricsHolder {
    rendered: std::sync::Arc<std::sync::RwLock<String>>,
}

impl MetricsHolder {
    pub fn new() -> Self {
        Self {
            rendered: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
        }
    }

    pub fn update(&self, text: String) {
        if let Ok(mut lock) = self.rendered.write() {
            *lock = text;
        }
    }

    pub fn get(&self) -> String {
        self.rendered.read().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Renders Prometheus text-format exposition for all node metrics.
pub fn render_metrics(data: &NodeMetricData<'_>) -> String {
    let mut out = String::new();
    let node_escaped = escape_label(data.node);

    // 1. ambar_module_info
    out.push_str("# HELP ambar_module_info Loaded DRBD kernel module version.\n");
    out.push_str("# TYPE ambar_module_info gauge\n");
    if let Some(version) = data.drbd_version {
        let _ = writeln!(out, "ambar_module_info{{version=\"{}\"}} 1", escape_label(version));
    }

    // 2. ambar_node_allocated_bytes
    out.push_str("# HELP ambar_node_allocated_bytes Total allocated volume storage on this node in bytes.\n");
    out.push_str("# TYPE ambar_node_allocated_bytes gauge\n");
    let _ = writeln!(
        out,
        "ambar_node_allocated_bytes{{node=\"{node_escaped}\"}} {}",
        data.allocated_bytes
    );

    // 3. ambar_node_tainted
    out.push_str("# HELP ambar_node_tainted 1 if this node carries no-quorum taint, 0 otherwise.\n");
    out.push_str("# TYPE ambar_node_tainted gauge\n");
    let _ = writeln!(
        out,
        "ambar_node_tainted{{node=\"{node_escaped}\"}} {}",
        if data.is_tainted { 1 } else { 0 }
    );

    // 4. ambar_volume_uptodate_replicas
    out.push_str("# HELP ambar_volume_uptodate_replicas Number of replicas in UpToDate disk state for this volume.\n");
    out.push_str("# TYPE ambar_volume_uptodate_replicas gauge\n");
    for vol in &data.volumes {
        let vol_escaped = escape_label(vol.name);
        let uptodate_count = match vol.status {
            Some(status) => {
                let local = status.device().is_some_and(|d| d.disk_state == DiskState::UpToDate) as usize;
                let peers = status
                    .connections
                    .iter()
                    .filter(|c| c.peer_devices.iter().any(|d| d.peer_disk_state == DiskState::UpToDate))
                    .count();
                local + peers
            }
            None => 0,
        };
        let _ = writeln!(
            out,
            "ambar_volume_uptodate_replicas{{node=\"{node_escaped}\",volume=\"{vol_escaped}\"}} {uptodate_count}"
        );
    }

    // 5. ambar_volume_out_of_sync_bytes
    out.push_str("# HELP ambar_volume_out_of_sync_bytes Out of sync bytes with peer.\n");
    out.push_str("# TYPE ambar_volume_out_of_sync_bytes gauge\n");
    for vol in &data.volumes {
        let vol_escaped = escape_label(vol.name);
        if let Some(status) = vol.status {
            for conn in &status.connections {
                let peer_escaped = escape_label(&conn.name);
                let oos_bytes = conn.peer_devices.first().map_or(0, |d| d.out_of_sync * 1024);
                let _ = writeln!(
                    out,
                    "ambar_volume_out_of_sync_bytes{{node=\"{node_escaped}\",peer=\"{peer_escaped}\",volume=\"{vol_escaped}\"}} {oos_bytes}"
                );
            }
        }
    }

    // 6. ambar_volume_quorum
    out.push_str("# HELP ambar_volume_quorum 1 if this node has quorum for the volume, 0 otherwise.\n");
    out.push_str("# TYPE ambar_volume_quorum gauge\n");
    for vol in &data.volumes {
        let vol_escaped = escape_label(vol.name);
        let quorum = vol.status.is_some_and(|s| s.quorum()) as usize;
        let _ = writeln!(
            out,
            "ambar_volume_quorum{{node=\"{node_escaped}\",volume=\"{vol_escaped}\"}} {quorum}"
        );
    }

    // 7. ambar_volume_standalone_connections
    out.push_str("# HELP ambar_volume_standalone_connections Number of connections in StandAlone state.\n");
    out.push_str("# TYPE ambar_volume_standalone_connections gauge\n");
    for vol in &data.volumes {
        let vol_escaped = escape_label(vol.name);
        let standalone_count = vol.status.map_or(0, |s| {
            s.connections
                .iter()
                .filter(|c| c.connection_state == ConnectionState::StandAlone)
                .count()
        });
        let _ = writeln!(
            out,
            "ambar_volume_standalone_connections{{node=\"{node_escaped}\",volume=\"{vol_escaped}\"}} {standalone_count}"
        );
    }

    // 8. ambar_volume_manual_check_needed
    out.push_str(
        "# HELP ambar_volume_manual_check_needed 1 if volume requires manual split-brain resolution, 0 otherwise.\n",
    );
    out.push_str("# TYPE ambar_volume_manual_check_needed gauge\n");
    for vol in &data.volumes {
        let vol_escaped = escape_label(vol.name);
        let needed = vol.manual_check_needed as usize;
        let _ = writeln!(
            out,
            "ambar_volume_manual_check_needed{{node=\"{node_escaped}\",volume=\"{vol_escaped}\"}} {needed}"
        );
    }

    out
}

#[cfg(test)]
#[path = "tests/metrics.rs"]
mod tests;
