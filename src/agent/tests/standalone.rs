use std::time::{Duration, Instant};

use super::*;
use crate::drbd::status::{Connection, ConnectionState, Device, DiskState, ResourceStatus, Role};

fn resource(name: &str, role: Role, states: &[ConnectionState]) -> ResourceStatus {
    ResourceStatus {
        name: name.to_string(),
        node_id: 1,
        role,
        suspended: false,
        devices: vec![Device {
            volume: 0,
            minor: 1000,
            disk_state: DiskState::UpToDate,
            client: false,
            open: role == Role::Primary,
            quorum: true,
            size: 1024,
        }],
        connections: states
            .iter()
            .enumerate()
            .map(|(i, &state)| Connection {
                peer_node_id: (i + 2) as u32,
                name: format!("peer{}", i + 2),
                connection_state: state,
                peer_role: Role::Secondary,
                peer_devices: Vec::new(),
            })
            .collect(),
    }
}

#[test]
fn healthy_connected_volume_has_no_action() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("v1", Role::Secondary, &[ConnectionState::Connected]);
    assert_eq!(tracker.check(&res, Instant::now()), StandaloneAction::None);
    assert!(!tracker.is_manual_check_needed("v1"));
}

#[test]
fn primary_with_standalone_is_not_adjusted() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("v1", Role::Primary, &[ConnectionState::StandAlone]);
    assert_eq!(tracker.check(&res, Instant::now()), StandaloneAction::None);
    assert!(!tracker.is_manual_check_needed("v1"));
}

#[test]
fn secondary_with_standalone_attempts_adjust_and_waits_interval() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("v1", Role::Secondary, &[ConnectionState::StandAlone]);
    let t0 = Instant::now();

    // Attempt 1
    assert_eq!(tracker.check(&res, t0), StandaloneAction::RunAdjust { attempt: 1 });
    assert!(!tracker.is_manual_check_needed("v1"));

    // 2s later: waiting
    let t2 = t0 + Duration::from_secs(2);
    assert_eq!(tracker.check(&res, t2), StandaloneAction::Wait(Duration::from_secs(3)));

    // 5s later: attempt 2
    let t5 = t0 + Duration::from_secs(5);
    assert_eq!(tracker.check(&res, t5), StandaloneAction::RunAdjust { attempt: 2 });

    // 10s later: attempt 3
    let t10 = t0 + Duration::from_secs(10);
    assert_eq!(tracker.check(&res, t10), StandaloneAction::RunAdjust { attempt: 3 });

    // 15s later: attempts exhausted -> manual check needed
    let t15 = t0 + Duration::from_secs(15);
    assert_eq!(tracker.check(&res, t15), StandaloneAction::ManualCheckNeeded);
    assert!(tracker.is_manual_check_needed("v1"));

    // Stays in manual check needed
    let t20 = t0 + Duration::from_secs(20);
    assert_eq!(tracker.check(&res, t20), StandaloneAction::ManualCheckNeeded);
    assert!(tracker.is_manual_check_needed("v1"));
}

#[test]
fn auto_reset_when_connected() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res_standalone = resource("v1", Role::Secondary, &[ConnectionState::StandAlone]);
    let t0 = Instant::now();

    // Run all 3 attempts
    tracker.check(&res_standalone, t0);
    tracker.check(&res_standalone, t0 + Duration::from_secs(5));
    tracker.check(&res_standalone, t0 + Duration::from_secs(10));
    tracker.check(&res_standalone, t0 + Duration::from_secs(15));
    assert!(tracker.is_manual_check_needed("v1"));

    // Peers reconnect -> Connected
    let res_connected = resource("v1", Role::Secondary, &[ConnectionState::Connected]);
    assert_eq!(
        tracker.check(&res_connected, t0 + Duration::from_secs(20)),
        StandaloneAction::None
    );
    assert!(!tracker.is_manual_check_needed("v1"));

    // Next time it becomes StandAlone, attempts restart from 1
    assert_eq!(
        tracker.check(&res_standalone, t0 + Duration::from_secs(25)),
        StandaloneAction::RunAdjust { attempt: 1 }
    );
}

#[test]
fn forget_clears_standalone_state() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("v1", Role::Secondary, &[ConnectionState::StandAlone]);
    let t0 = Instant::now();
    tracker.check(&res, t0);
    tracker.forget("v1");

    assert_eq!(
        tracker.check(&res, t0 + Duration::from_secs(1)),
        StandaloneAction::RunAdjust { attempt: 1 }
    );
}

#[test]
fn handle_standalone_executes_drbd_adjust() {
    let runner = crate::tests::FakeRunner::new(|_, _| crate::tests::out(0, "", ""));
    let drbd = Drbd { runner: &runner };
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("v1", Role::Secondary, &[ConnectionState::StandAlone]);

    let adjusted = handle_standalone(&mut tracker, &drbd, &res, Instant::now()).unwrap();
    assert!(adjusted);
    assert_eq!(runner.calls(), &["drbdadm adjust v1"]);

    // Next immediate call: waiting -> no adjust
    let adjusted2 = handle_standalone(&mut tracker, &drbd, &res, Instant::now()).unwrap();
    assert!(!adjusted2);
    assert_eq!(runner.calls().len(), 1);
}

#[test]
fn handle_standalone_rejects_invalid_volume_name() {
    let runner = crate::tests::FakeRunner::new(|_, _| crate::tests::out(0, "", ""));
    let drbd = Drbd { runner: &runner };
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let res = resource("../evil", Role::Secondary, &[ConnectionState::StandAlone]);

    let adjusted = handle_standalone(&mut tracker, &drbd, &res, Instant::now()).unwrap();
    assert!(!adjusted);
    assert!(runner.calls().is_empty());
}

#[test]
fn retain_volumes_drops_vanished_entries() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(5));
    let r1 = resource("v1", Role::Secondary, &[ConnectionState::StandAlone]);
    let r2 = resource("v2", Role::Secondary, &[ConnectionState::StandAlone]);
    let t0 = Instant::now();

    assert_eq!(tracker.check(&r1, t0), StandaloneAction::RunAdjust { attempt: 1 });
    assert_eq!(tracker.check(&r2, t0), StandaloneAction::RunAdjust { attempt: 1 });

    let active: std::collections::HashSet<&str> = ["v1"].into_iter().collect();
    tracker.retain_volumes(&active);

    // v1 сохраняет счётчик попыток
    assert_eq!(
        tracker.check(&r1, t0 + Duration::from_secs(5)),
        StandaloneAction::RunAdjust { attempt: 2 }
    );
    // v2 удалён и начинает с attempt: 1
    assert_eq!(
        tracker.check(&r2, t0 + Duration::from_secs(5)),
        StandaloneAction::RunAdjust { attempt: 1 }
    );
}

#[test]
fn standalone_resets_when_standalone_peer_reconnects_even_if_third_peer_is_connecting() {
    let mut tracker = StandaloneTracker::new(2, Duration::from_secs(5));
    let mut t = Instant::now();

    // peer2 в StandAlone, peer3 в Connecting
    let res_sa = resource(
        "v1",
        Role::Secondary,
        &[ConnectionState::StandAlone, ConnectionState::Connecting],
    );
    assert_eq!(tracker.check(&res_sa, t), StandaloneAction::RunAdjust { attempt: 1 });
    t += Duration::from_secs(5);
    assert_eq!(tracker.check(&res_sa, t), StandaloneAction::RunAdjust { attempt: 2 });
    t += Duration::from_secs(5);
    assert_eq!(tracker.check(&res_sa, t), StandaloneAction::ManualCheckNeeded);
    assert!(tracker.is_manual_check_needed("v1"));

    // peer2 восстановился (Connected), а peer3 всё ещё Connecting (третий узел выключен)
    let res_reconnected = resource(
        "v1",
        Role::Secondary,
        &[ConnectionState::Connected, ConnectionState::Connecting],
    );
    assert_eq!(tracker.check(&res_reconnected, t), StandaloneAction::None);
    assert!(!tracker.is_manual_check_needed("v1"));

    // Если peer2 снова уйдёт в StandAlone, счётчик начинается с 1
    t += Duration::from_secs(5);
    assert_eq!(tracker.check(&res_sa, t), StandaloneAction::RunAdjust { attempt: 1 });
}
