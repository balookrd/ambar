use super::*;
use crate::crd::{MemberNode, NodeKind, ReplicatedVolume, ReplicatedVolumeSpec};
use crate::drbd::status::{Device, DiskState, ResourceStatus, Role};
use crate::tests::volumes::FakeVolumes;
use std::sync::Arc;

fn member(name: &str, node_id: u32, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.to_string(),
        node_id,
        address: format!("192.168.104.{node_id}"),
        kind,
    }
}

fn spec_with_nodes(nodes: Vec<MemberNode>) -> ReplicatedVolumeSpec {
    ReplicatedVolumeSpec {
        size_bytes: 1024,
        protocol: crate::model::Protocol::C,
        max_peers: 7,
        minor: 1000,
        port: 7000,
        nodes,
        initial_sync: Default::default(),
    }
}

fn res_status(role: Role, open: bool) -> ResourceStatus {
    ResourceStatus {
        name: "vol1".to_string(),
        node_id: 1,
        role,
        suspended: false,
        devices: vec![Device {
            volume: 0,
            minor: 1000,
            disk_state: DiskState::Diskless,
            client: true,
            open,
            quorum: true,
            size: 1024,
        }],
        connections: Vec::new(),
    }
}

#[test]
fn should_cleanup_returns_true_for_secondary_client_without_pods() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_has_pods() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), true));
}

#[test]
fn should_cleanup_returns_false_for_replica_or_witness() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Witness),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n1", &spec, Some(&status), false));
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_primary() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Primary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_open() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, true);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_true_when_status_is_none() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    assert!(should_cleanup_client("n2", &spec, None, false));
}

#[test]
fn should_cleanup_returns_false_when_not_a_member() {
    let spec = spec_with_nodes(vec![member("n1", 1, NodeKind::Replica)]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[tokio::test]
async fn leave_client_removes_client_member_from_spec() {
    let fake = Arc::new(FakeVolumes::default());
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let mut rv = ReplicatedVolume::new("vol1", spec);
    rv.metadata.uid = Some("uid-1".to_string());
    fake.create(&rv).await.unwrap();

    let removed = leave_client(&*fake, "n2", "vol1").await.unwrap();
    assert!(removed);

    let updated = fake.get("vol1").await.unwrap().unwrap();
    assert_eq!(updated.spec.nodes.len(), 1);
    assert_eq!(updated.spec.nodes[0].name, "n1");
}

#[tokio::test]
async fn leave_client_noops_when_not_client() {
    let fake = Arc::new(FakeVolumes::default());
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Witness),
    ]);
    let mut rv = ReplicatedVolume::new("vol1", spec);
    rv.metadata.uid = Some("uid-1".to_string());
    fake.create(&rv).await.unwrap();

    let result = leave_client(&*fake, "n2", "vol1").await.unwrap();
    assert!(result);

    let updated = fake.get("vol1").await.unwrap().unwrap();
    assert_eq!(updated.spec.nodes.len(), 2);
}

#[test]
fn cleanup_tracker_debounces_within_interval() {
    use std::time::{Duration, Instant};

    let mut tracker = CleanupTracker::new(DEFAULT_CLEANUP_INTERVAL);
    let t0 = Instant::now();
    assert!(tracker.should_run(t0));
    assert!(!tracker.should_run(t0 + Duration::from_secs(5)));
    assert!(tracker.should_run(t0 + Duration::from_secs(30)));
}

#[tokio::test]
async fn snapshot_busy_or_mounted_skips_status_and_pod_list() {
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let paths = crate::tests::temp_paths(&tmp);
    let runner = Arc::new(crate::tests::FakeRunner::new(|_, _| {
        crate::tests::out(1, "", "drbdsetup must not be called")
    }));
    let volumes = Arc::new(FakeVolumes::default());
    // Любой вызов has_node_pods_for_volume вернёт ошибку, если до него дойдёт.
    volumes.fail.store(true, std::sync::atomic::Ordering::SeqCst);

    let agent = Agent {
        node: "n2".to_string(),
        paths,
        runner: runner.clone(),
        volumes,
        locks: crate::agent::run::VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(false)),
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
    };

    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let rv = ReplicatedVolume::new("vol1", spec);

    // 1. В snapshot устройство open: true -> пропускаем без drbdsetup и has_node_pods_for_volume.
    let busy_snapshot = res_status(Role::Secondary, true);
    let res1 = check_and_cleanup_client_with_snapshot(&agent, "vol1", &rv, Some(&busy_snapshot), &[])
        .await
        .unwrap();
    assert!(res1.is_none());
    assert!(runner.calls().is_empty());

    // 2. В snapshot не занято, но mounted_uids непуст -> пропускаем без drbdsetup и has_node_pods_for_volume.
    let idle_snapshot = res_status(Role::Secondary, false);
    let mounted = vec!["pod-uid-1".to_string()];
    let res2 = check_and_cleanup_client_with_snapshot(&agent, "vol1", &rv, Some(&idle_snapshot), &mounted)
        .await
        .unwrap();
    assert!(res2.is_none());
    assert!(runner.calls().is_empty());
}

fn test_agent(node: &str) -> (Agent, Arc<FakeVolumes>, tempfile::TempDir) {
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let paths = crate::tests::temp_paths(&tmp);
    let runner = Arc::new(crate::tests::FakeRunner::new(|_, _| crate::tests::out(0, "[]\n", "")));
    let volumes = Arc::new(FakeVolumes::default());
    let agent = Agent {
        node: node.to_string(),
        paths,
        runner,
        volumes: volumes.clone(),
        locks: crate::agent::run::VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(true)),
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
    };
    (agent, volumes, tmp)
}

#[tokio::test]
async fn cleanup_skips_volume_while_stage_is_in_progress() {
    let (agent, volumes, _tmp) = test_agent("n2");
    let spec = spec_with_nodes(vec![
        member("dv-1", 0, NodeKind::Replica),
        member("dv-2", 1, NodeKind::Replica),
        member(&agent.node, 2, NodeKind::Client),
    ]);
    let obj = ReplicatedVolume::new("spike", spec);
    volumes.put(obj.clone());

    let _stage_guard = agent.begin_stage("spike");
    let res = check_and_cleanup_client_with_snapshot(&agent, "spike", &obj, None, &[])
        .await
        .unwrap();
    assert!(res.is_none());
    let current = volumes.get("spike").await.unwrap().unwrap();
    assert!(current.spec.nodes.iter().any(|n| n.name == agent.node));
}

#[tokio::test]
async fn cleanup_skips_volume_when_volume_lock_is_held() {
    let (agent, volumes, _tmp) = test_agent("n2");
    let spec = spec_with_nodes(vec![
        member("dv-1", 0, NodeKind::Replica),
        member("dv-2", 1, NodeKind::Replica),
        member(&agent.node, 2, NodeKind::Client),
    ]);
    let obj = ReplicatedVolume::new("spike", spec);
    volumes.put(obj.clone());

    let _held = agent.locks.lock("spike").await;
    // Должен мгновенно вернуть Ok(None), не блокируясь на локе
    let res = tokio::time::timeout(
        Duration::from_millis(50),
        check_and_cleanup_client_with_snapshot(&agent, "spike", &obj, None, &[]),
    )
    .await
    .expect("cleanup must not block on held volume lock")
    .unwrap();
    assert!(res.is_none());
}
