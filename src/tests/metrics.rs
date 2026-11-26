use crate::drbd::status::{Connection, ConnectionState, Device, DiskState, PeerDevice, ResourceStatus, Role};
use crate::metrics::{MetricsHolder, NodeMetricData, VolumeMetricData, render_metrics};

fn sample_device(open: bool, quorum: bool, disk_state: DiskState) -> Device {
    Device {
        volume: 0,
        minor: 1000,
        disk_state,
        client: false,
        open,
        quorum,
        size: 1024,
    }
}

fn sample_peer_device(disk_state: DiskState, out_of_sync: u64) -> PeerDevice {
    PeerDevice {
        volume: 0,
        replication_state: "Established".to_string(),
        peer_disk_state: disk_state,
        peer_client: false,
        out_of_sync,
    }
}

fn sample_connection(name: &str, state: ConnectionState, peer_disk: DiskState, out_of_sync: u64) -> Connection {
    Connection {
        peer_node_id: 2,
        name: name.to_string(),
        connection_state: state,
        peer_role: Role::Secondary,
        peer_devices: vec![sample_peer_device(peer_disk, out_of_sync)],
    }
}

fn sample_status(
    name: &str,
    role: Role,
    open: bool,
    quorum: bool,
    disk_state: DiskState,
    connections: Vec<Connection>,
) -> ResourceStatus {
    ResourceStatus {
        name: name.to_string(),
        node_id: 1,
        role,
        suspended: false,
        devices: vec![sample_device(open, quorum, disk_state)],
        connections,
    }
}

#[test]
fn render_metrics_empty_node() {
    let data = NodeMetricData {
        node: "lima-dv-1",
        drbd_version: Some("9.3.4"),
        is_tainted: false,
        allocated_bytes: 0,
        volumes: Vec::new(),
    };
    let output = render_metrics(&data);

    assert!(output.contains("# TYPE ambar_module_info gauge"));
    assert!(output.contains("ambar_module_info{version=\"9.3.4\"} 1"));
    assert!(output.contains("# TYPE ambar_node_allocated_bytes gauge"));
    assert!(output.contains("ambar_node_allocated_bytes{node=\"lima-dv-1\"} 0"));
    assert!(output.contains("# TYPE ambar_node_tainted gauge"));
    assert!(output.contains("ambar_node_tainted{node=\"lima-dv-1\"} 0"));
}

#[test]
fn render_metrics_with_volume_uptodate_and_peers() {
    let conns = vec![
        sample_connection("lima-dv-2", ConnectionState::Connected, DiskState::UpToDate, 0),
        sample_connection("lima-dv-3", ConnectionState::Connected, DiskState::Inconsistent, 1024),
    ];
    let status = sample_status("spike", Role::Primary, true, true, DiskState::UpToDate, conns);

    let data = NodeMetricData {
        node: "lima-dv-1",
        drbd_version: Some("9.3.4"),
        is_tainted: false,
        allocated_bytes: 1048576,
        volumes: vec![VolumeMetricData {
            name: "spike",
            status: Some(&status),
            manual_check_needed: false,
        }],
    };
    let output = render_metrics(&data);

    // 1 local UpToDate + 1 peer UpToDate = 2
    assert!(output.contains("ambar_volume_uptodate_replicas{node=\"lima-dv-1\",volume=\"spike\"} 2"));
    // Out of sync bytes: 0 for dv-2, 1024 * 1024 = 1048576 for dv-3
    assert!(
        output.contains("ambar_volume_out_of_sync_bytes{node=\"lima-dv-1\",peer=\"lima-dv-2\",volume=\"spike\"} 0")
    );
    assert!(
        output
            .contains("ambar_volume_out_of_sync_bytes{node=\"lima-dv-1\",peer=\"lima-dv-3\",volume=\"spike\"} 1048576")
    );
    // Quorum is true: 1
    assert!(output.contains("ambar_volume_quorum{node=\"lima-dv-1\",volume=\"spike\"} 1"));
    // StandAlone connections: 0
    assert!(output.contains("ambar_volume_standalone_connections{node=\"lima-dv-1\",volume=\"spike\"} 0"));
    // Manual check needed: 0
    assert!(output.contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 0"));
}

#[test]
fn render_metrics_with_standalone_and_split_brain() {
    let conns = vec![sample_connection(
        "lima-dv-2",
        ConnectionState::StandAlone,
        DiskState::UpToDate,
        0,
    )];
    let status = sample_status("spike", Role::Secondary, false, false, DiskState::UpToDate, conns);

    let data = NodeMetricData {
        node: "lima-dv-1",
        drbd_version: Some("9.3.4"),
        is_tainted: true,
        allocated_bytes: 1048576,
        volumes: vec![VolumeMetricData {
            name: "spike",
            status: Some(&status),
            manual_check_needed: true,
        }],
    };
    let output = render_metrics(&data);

    assert!(output.contains("ambar_node_tainted{node=\"lima-dv-1\"} 1"));
    assert!(output.contains("ambar_volume_quorum{node=\"lima-dv-1\",volume=\"spike\"} 0"));
    assert!(output.contains("ambar_volume_standalone_connections{node=\"lima-dv-1\",volume=\"spike\"} 1"));
    assert!(output.contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 1"));
}

#[test]
fn metrics_holder_stores_and_updates() {
    let holder = crate::metrics::MetricsHolder::new();
    assert_eq!(&*holder.get(), "");

    holder.update("metric_1 10\n".to_string());
    assert_eq!(&*holder.get(), "metric_1 10\n");

    holder.update("metric_1 20\n".to_string());
    assert_eq!(&*holder.get(), "metric_1 20\n");
}

#[test]
fn render_metrics_escapes_special_characters() {
    let data = NodeMetricData {
        node: "node\"with\\quotes",
        drbd_version: Some("9.3\"test"),
        is_tainted: false,
        allocated_bytes: 0,
        volumes: Vec::new(),
    };
    let output = render_metrics(&data);

    assert!(output.contains("ambar_module_info{version=\"9.3\\\"test\"} 1"));
    assert!(output.contains("ambar_node_allocated_bytes{node=\"node\\\"with\\\\quotes\"} 0"));
}

#[test]
fn metrics_holder_shares_arc_and_recovers_from_poisoned_lock() {
    let holder = MetricsHolder::new();
    holder.update("ambar_node_tainted{node=\"n1\"} 0\n".to_string());
    let first = holder.get();
    let second = holder.get();
    assert!(std::sync::Arc::ptr_eq(&first, &second));

    let poisoned = holder.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.write_guard_for_test();
        panic!("boom");
    });
    assert_eq!(&*holder.get(), "ambar_node_tainted{node=\"n1\"} 0\n");
    holder.update("updated\n".to_string());
    assert_eq!(&*holder.get(), "updated\n");
}

#[test]
fn render_metrics_ignores_disconnected_peer_in_uptodate_count() {
    let conns = vec![Connection {
        peer_node_id: 2,
        name: "k3s-2".to_string(),
        connection_state: ConnectionState::Connecting,
        peer_role: Role::Secondary,
        peer_devices: vec![PeerDevice {
            volume: 0,
            replication_state: "Off".to_string(),
            peer_disk_state: DiskState::UpToDate,
            peer_client: false,
            out_of_sync: 0,
        }],
    }];
    let status = sample_status("vol-1", Role::Primary, true, true, DiskState::UpToDate, conns);
    let data = NodeMetricData {
        node: "k3s-1",
        drbd_version: Some("9.3.4"),
        is_tainted: false,
        allocated_bytes: 0,
        volumes: vec![VolumeMetricData {
            name: "vol-1",
            status: Some(&status),
            manual_check_needed: false,
        }],
    };
    let text = render_metrics(&data);
    assert!(
        text.contains("ambar_volume_uptodate_replicas{node=\"k3s-1\",volume=\"vol-1\"} 1\n"),
        "disconnected peer must not count as uptodate: {text}"
    );
}
