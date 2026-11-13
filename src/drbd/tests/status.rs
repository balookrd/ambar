use super::*;

macro_rules! sample {
    ($name:literal) => {
        include_str!(concat!("../../../stand/spike/samples/", $name, ".json"))
    };
}

fn one(json: &str) -> ResourceStatus {
    let mut all = parse(json).expect("sample parses");
    assert_eq!(all.len(), 1);
    all.remove(0)
}

const ALL: [&str; 19] = [
    sample!("three-inconsistent"),
    sample!("three-uptodate"),
    sample!("primary-mounted"),
    sample!("peer-disconnected"),
    sample!("quorum-lost"),
    sample!("quorum-suspended"),
    sample!("peer-primary"),
    sample!("diskless-node"),
    sample!("diskless-peer-down"),
    sample!("peer-disconnected-diskful"),
    sample!("resync-target"),
    sample!("standalone-primary"),
    sample!("diskless-peer-killed"),
    sample!("diskless-alone-no-quorum"),
    sample!("stale-diskful-no-quorum"),
    sample!("diskless-stale-peer"),
    sample!("diskless-rebooted-stale-peer"),
    sample!("split-brain-victim"),
    sample!("split-brain-survivor"),
];

#[test]
fn every_captured_sample_parses_without_unknown_states() {
    for json in ALL {
        let st = one(json);
        assert_ne!(st.role, Role::Other, "{}", st.name);
        let dev = st.device().expect("volume 0");
        assert_ne!(dev.disk_state, DiskState::Other, "{}", st.name);
        assert_eq!(st.connections.len(), 2, "{}", st.name);
        for conn in &st.connections {
            assert_ne!(conn.connection_state, ConnectionState::Other, "{}", st.name);
            assert_ne!(conn.peer_role, Role::Other, "{}", st.name);
            let peer = conn
                .peer_devices
                .first()
                .unwrap_or_else(|| panic!("{}: connection {} has no peer devices", st.name, conn.name));
            assert_ne!(peer.peer_disk_state, DiskState::Other, "{}", st.name);
        }
    }
}

#[test]
fn a_mounted_primary_is_open_with_quorum() {
    let st = one(sample!("primary-mounted"));
    assert_eq!(st.name, "spike");
    assert_eq!(st.node_id, 0);
    assert_eq!(st.role, Role::Primary);
    let dev = st.device().unwrap();
    assert_eq!(
        (dev.minor, dev.disk_state, dev.open, dev.client),
        (1000, DiskState::UpToDate, true, false)
    );
    assert_eq!(dev.size, 1_048_476);
    assert!(st.quorum());
    assert!(st.all_peers_connected());
    assert!(st.standalone_peers().is_empty());
}

#[test]
fn an_isolated_primary_has_no_quorum_and_connecting_peers() {
    let st = one(sample!("quorum-lost"));
    assert_eq!(st.role, Role::Primary);
    assert!(!st.quorum());
    assert!(!st.all_peers_connected());
    assert!(
        st.connections
            .iter()
            .all(|c| c.connection_state == ConnectionState::Connecting)
    );
    assert!(st.standalone_peers().is_empty());
}

#[test]
fn standalone_connections_are_reported_by_peer_name() {
    assert_eq!(
        one(sample!("standalone-primary")).standalone_peers(),
        ["lima-dv-2", "lima-dv-3"]
    );
    assert_eq!(one(sample!("split-brain-survivor")).standalone_peers(), ["lima-dv-1"]);
}

#[test]
fn a_diskless_node_is_a_client_without_a_disk() {
    let st = one(sample!("diskless-node"));
    let dev = st.device().unwrap();
    assert!(dev.client);
    assert_eq!(dev.disk_state, DiskState::Diskless);
    assert!(st.quorum());
}

#[test]
fn a_resync_target_reports_what_is_left() {
    let st = one(sample!("resync-target"));
    assert_eq!(st.device().unwrap().disk_state, DiskState::Inconsistent);
    let source = st.connections.iter().find(|c| c.name == "lima-dv-1").unwrap();
    assert_eq!(source.peer_devices[0].replication_state, "SyncTarget");
    assert_eq!(source.peer_devices[0].out_of_sync, 4100);
    let diskless = st.connections.iter().find(|c| c.name == "lima-dv-3").unwrap();
    assert!(diskless.peer_devices[0].peer_client);
}

#[test]
fn only_a_fresh_fully_connected_volume_may_skip_the_initial_sync() {
    assert!(one(sample!("three-inconsistent")).ready_to_skip_initial_sync());

    // Already synchronised, resyncing, isolated, stale or split: never.
    for name in [
        sample!("three-uptodate"),
        sample!("primary-mounted"),
        sample!("resync-target"),
        sample!("quorum-lost"),
        sample!("stale-diskful-no-quorum"),
        sample!("standalone-primary"),
        sample!("split-brain-victim"),
        sample!("diskless-node"),
    ] {
        let st = one(name);
        assert!(!st.ready_to_skip_initial_sync(), "{} on node {}", st.name, st.node_id);
    }
}

#[test]
fn unknown_states_do_not_break_parsing() {
    let json = sample!("three-uptodate")
        .replace("\"role\": \"Secondary\"", "\"role\": \"Tertiary\"")
        .replace("\"disk-state\": \"UpToDate\"", "\"disk-state\": \"Sparkling\"");
    let st = one(&json);
    assert_eq!(st.role, Role::Other);
    assert_eq!(st.device().unwrap().disk_state, DiskState::Other);

    let json = sample!("three-uptodate").replace(
        "\"connection-state\": \"Connected\"",
        "\"connection-state\": \"Warping\"",
    );
    let st = one(&json);
    assert!(
        st.connections
            .iter()
            .all(|c| c.connection_state == ConnectionState::Other)
    );
    assert!(!st.all_peers_connected());
}

fn fresh() -> ResourceStatus {
    one(sample!("three-inconsistent"))
}

fn dev0(st: &mut ResourceStatus) -> &mut Device {
    st.devices.iter_mut().find(|d| d.volume == 0).expect("volume 0")
}

fn peer0(st: &mut ResourceStatus) -> &mut PeerDevice {
    st.connections[0]
        .peer_devices
        .iter_mut()
        .find(|p| p.volume == 0)
        .expect("peer volume 0")
}

#[test]
fn the_fresh_sample_may_skip_the_initial_sync() {
    assert!(fresh().ready_to_skip_initial_sync());
}

#[test]
fn a_peer_without_a_disk_does_not_block_the_skip() {
    let mut st = fresh();
    let p = peer0(&mut st);
    p.peer_disk_state = DiskState::Diskless;
    p.peer_client = true;
    assert!(st.ready_to_skip_initial_sync());
}

/// Each case changes exactly one thing in the fresh sample, so the guard it
/// targets is the only reason for the rejection.
#[test]
fn every_single_deviation_from_the_fresh_sample_blocks_the_skip() {
    type Mutation = fn(&mut ResourceStatus);
    let cases: &[(&str, Mutation)] = &[
        ("no connections", |st| st.connections.clear()),
        ("connection Connecting", |st| {
            st.connections[0].connection_state = ConnectionState::Connecting
        }),
        ("connection StandAlone", |st| {
            st.connections[0].connection_state = ConnectionState::StandAlone
        }),
        ("connection state unknown", |st| {
            st.connections[0].connection_state = ConnectionState::Other
        }),
        ("peer without peer devices", |st| st.connections[0].peer_devices.clear()),
        ("peer device only for volume 1", |st| {
            st.connections[0].peer_devices[0].volume = 1
        }),
        ("peer UpToDate", |st| peer0(st).peer_disk_state = DiskState::UpToDate),
        ("peer Outdated", |st| peer0(st).peer_disk_state = DiskState::Outdated),
        ("peer Consistent", |st| {
            peer0(st).peer_disk_state = DiskState::Consistent
        }),
        ("peer DUnknown", |st| peer0(st).peer_disk_state = DiskState::DUnknown),
        ("peer disk state unknown", |st| {
            peer0(st).peer_disk_state = DiskState::Other
        }),
        ("peer Diskless without peer-client (failed disk)", |st| {
            let p = peer0(st);
            p.peer_disk_state = DiskState::Diskless;
            p.peer_client = false;
        }),
        ("peer SyncSource", |st| {
            peer0(st).replication_state = "SyncSource".into()
        }),
        ("peer SyncTarget", |st| {
            peer0(st).replication_state = "SyncTarget".into()
        }),
        ("local role Primary", |st| st.role = Role::Primary),
        ("peer role Primary", |st| st.connections[0].peer_role = Role::Primary),
        ("local device open", |st| dev0(st).open = true),
        ("resource suspended", |st| st.suspended = true),
        ("local device client", |st| dev0(st).client = true),
        ("local disk UpToDate", |st| dev0(st).disk_state = DiskState::UpToDate),
        ("local disk Diskless", |st| dev0(st).disk_state = DiskState::Diskless),
    ];
    let wrongly_accepted: Vec<&str> = cases
        .iter()
        .filter(|(_, mutate)| {
            let mut st = fresh();
            mutate(&mut st);
            st.ready_to_skip_initial_sync()
        })
        .map(|(name, _)| *name)
        .collect();
    assert!(wrongly_accepted.is_empty(), "wrongly accepted: {wrongly_accepted:?}");
}
