use super::*;
use crate::drbd::status::{Device, DiskState, ResourceStatus, Role};

fn status(name: &str, role: Role, open: bool, quorum: bool) -> ResourceStatus {
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
            open,
            quorum,
            size: 1024,
        }],
        connections: Vec::new(),
    }
}

#[test]
fn no_resources_means_no_taint() {
    assert!(!should_taint_node(&[]));
}

#[test]
fn secondary_not_open_without_quorum_means_no_taint() {
    let r = status("vol1", Role::Secondary, false, false);
    assert!(!should_taint_node(&[r]));
}

#[test]
fn primary_with_quorum_means_no_taint() {
    let r = status("vol1", Role::Primary, true, true);
    assert!(!should_taint_node(&[r]));
}

#[test]
fn primary_without_quorum_requires_taint() {
    let r = status("vol1", Role::Primary, true, false);
    assert!(should_taint_node(&[r]));
}

#[test]
fn secondary_open_without_quorum_requires_taint() {
    let r = status("vol1", Role::Secondary, true, false);
    assert!(should_taint_node(&[r]));
}

#[test]
fn all_mounted_must_have_quorum_to_avoid_taint() {
    let r1 = status("vol1", Role::Primary, true, true);
    let r2 = status("vol2", Role::Primary, true, false);
    assert!(should_taint_node(&[r1, r2]));
}

#[test]
fn unmounted_volumes_without_quorum_do_not_taint_healthy_mounted_volumes() {
    let r1 = status("vol1", Role::Primary, true, true);
    let r2 = status("vol2", Role::Secondary, false, false);
    assert!(!should_taint_node(&[r1, r2]));
}

#[test]
fn primary_without_device_entry_requires_taint() {
    let mut r = status("vol1", Role::Primary, false, false);
    r.devices.clear();
    assert!(should_taint_node(&[r]));
}

#[tokio::test]
async fn reconcile_adds_taint_when_needed() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    let changed = reconcile_node_taint(&fake, "node1", true).await.unwrap();
    assert!(changed);
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        &["taint node1 ambar.balookrd.github.io/no-quorum:NoSchedule"]
    );

    // Idempotent second call
    let changed2 = reconcile_node_taint(&fake, "node1", true).await.unwrap();
    assert!(!changed2);
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn reconcile_removes_taint_when_healthy() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    reconcile_node_taint(&fake, "node1", true).await.unwrap();
    fake.calls.lock().unwrap().clear();

    let changed = reconcile_node_taint(&fake, "node1", false).await.unwrap();
    assert!(changed);
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        &["untaint node1 ambar.balookrd.github.io/no-quorum:NoSchedule"]
    );

    // Idempotent second call
    let changed2 = reconcile_node_taint(&fake, "node1", false).await.unwrap();
    assert!(!changed2);
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn reconcile_reports_api_error() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    fake.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(reconcile_node_taint(&fake, "node1", true).await.is_err());
}

#[test]
fn taint_tracker_reconciles_initially_on_change_and_after_resync_interval() {
    let t0 = Instant::now();
    let mut tracker = TaintTracker::new(Duration::from_secs(60));

    assert!(tracker.needs_reconcile(false, t0));
    tracker.record_synced(false, t0);

    // Same state within resync interval -> skipped
    assert!(!tracker.needs_reconcile(false, t0 + Duration::from_secs(59)));
    // Same state at/after resync interval -> periodic resync
    assert!(tracker.needs_reconcile(false, t0 + Duration::from_secs(60)));

    // State transition triggers immediate reconcile even at the same instant
    assert!(tracker.needs_reconcile(true, t0));
    tracker.record_synced(true, t0 + Duration::from_secs(1));
    assert!(!tracker.needs_reconcile(true, t0 + Duration::from_secs(30)));
    assert!(tracker.needs_reconcile(false, t0 + Duration::from_secs(30)));
}
