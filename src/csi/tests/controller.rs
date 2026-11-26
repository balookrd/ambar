use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tonic::{Code, Request};

use super::*;
use crate::agent::kube::ApiError;
use crate::crd::NodeStatus;
use crate::csi::proto::volume_capability::{AccessMode, MountVolume};
use crate::csi::proto::{CapacityRange, Topology, TopologyRequirement, VolumeContentSource};
use crate::membership::replica_finalizer;
use crate::tests::volumes::{FakeNodes, FakeVolumes, T0, cluster, time};

const GIB: u64 = 1 << 30;

struct Harness {
    volumes: Arc<FakeVolumes>,
    svc: ControllerService,
}

/// Three nodes `n1..n3` with 100 GiB each; short waits so that a test that
/// never becomes ready ends at once on the paused clock.
fn harness() -> Harness {
    let volumes = Arc::new(FakeVolumes::default());
    let mut svc = ControllerService::new(volumes.clone(), Arc::new(FakeNodes(Ok(cluster(&[100 * GIB; 3])))));
    svc.create_wait = Duration::from_secs(10);
    svc.delete_wait = Duration::from_secs(10);
    svc.poll = Duration::from_millis(100);
    Harness { volumes, svc }
}

fn mode(mode: Mode) -> Option<AccessMode> {
    Some(AccessMode { mode: mode as i32 })
}

fn mount_capability(fs_type: &str, access: Mode) -> VolumeCapability {
    VolumeCapability {
        access_mode: mode(access),
        access_type: Some(AccessType::Mount(MountVolume {
            fs_type: fs_type.to_string(),
            ..Default::default()
        })),
    }
}

fn request(name: &str) -> CreateVolumeRequest {
    CreateVolumeRequest {
        name: name.to_string(),
        capacity_range: Some(CapacityRange {
            required_bytes: GIB as i64,
            limit_bytes: 0,
        }),
        volume_capabilities: vec![mount_capability("ext4", Mode::SingleNodeWriter)],
        ..Default::default()
    }
}

fn with_params(mut req: CreateVolumeRequest, pairs: &[(&str, &str)]) -> CreateVolumeRequest {
    req.parameters = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<_, _>>();
    req
}

fn topology(node: &str) -> Topology {
    Topology {
        segments: HashMap::from([(TOPOLOGY_KEY.to_string(), node.to_string())]),
    }
}

fn up_to_date(node: &str) -> NodeStatus {
    NodeStatus {
        name: node.to_string(),
        role: "Secondary".to_string(),
        disk: "UpToDate".to_string(),
        quorum: true,
        connections: vec![],
        message: String::new(),
    }
}

/// Stands in for the node agents: every 100 ms every Replica of every object
/// reports `UpToDate`.
fn agents_report(volumes: &Arc<FakeVolumes>) -> tokio::task::JoinHandle<()> {
    let volumes = volumes.clone();
    tokio::spawn(async move {
        loop {
            let names: Vec<String> = volumes.objects.lock().unwrap().keys().cloned().collect();
            for name in names {
                if let Some(object) = volumes.object(&name) {
                    for node in object.spec.nodes.iter().filter(|n| n.kind == NodeKind::Replica) {
                        volumes.set_entry(&name, up_to_date(&node.name));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

async fn create(h: &Harness, req: CreateVolumeRequest) -> Result<CreateVolumeResponse, Status> {
    h.svc.create_volume(Request::new(req)).await.map(|r| r.into_inner())
}

async fn delete(h: &Harness, id: &str) -> Result<(), Status> {
    h.svc
        .delete_volume(Request::new(DeleteVolumeRequest {
            volume_id: id.to_string(),
            ..Default::default()
        }))
        .await
        .map(|_| ())
}

fn object_json(
    name: &str,
    size: u64,
    protocol: &str,
    max_peers: u32,
    minor: u32,
    port: u16,
    replicas: usize,
) -> ReplicatedVolume {
    let nodes: Vec<_> = (0..replicas)
        .map(|i| json!({"name": format!("n{}", i + 1), "nodeId": i, "address": format!("192.0.2.{}", i + 1), "kind": "Replica"}))
        .collect();
    let spec = serde_json::from_value(json!({
        "sizeBytes": size, "protocol": protocol, "minor": minor, "port": port, "maxPeers": max_peers,
        "initialSync": "Skip", "nodes": nodes,
    }))
    .unwrap();
    let mut object = ReplicatedVolume::new(name, spec);
    object.metadata.finalizers = Some(
        (0..replicas)
            .map(|i| replica_finalizer(&format!("n{}", i + 1)))
            .collect(),
    );
    object
}

fn code_of<T: std::fmt::Debug>(result: Result<T, Status>) -> (Code, String) {
    let err = result.unwrap_err();
    (err.code(), err.message().to_string())
}

// ---- CreateVolume, step 1 ----

#[tokio::test(start_paused = true)]
async fn step1_an_empty_name_is_invalid() {
    let h = harness();
    let (code, message) = code_of(create(&h, request("")).await);
    assert_eq!(code, Code::InvalidArgument, "{message}");
}

#[tokio::test(start_paused = true)]
async fn step1_a_name_that_is_not_a_valid_volume_name_is_invalid() {
    let h = harness();
    for name in ["Has-Upper", "-lead", "all", "a_b", &"x".repeat(64)] {
        let (code, message) = code_of(create(&h, request(name)).await);
        assert_eq!(code, Code::InvalidArgument, "{name}");
        assert!(message.contains(&format!("{name:?}")), "{message}");
    }
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

// ---- step 2 ----

#[tokio::test(start_paused = true)]
async fn step2_no_capabilities_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.volume_capabilities.clear();
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("volume_capabilities"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn step2_a_block_capability_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.volume_capabilities.push(VolumeCapability {
        access_mode: mode(Mode::SingleNodeWriter),
        access_type: Some(AccessType::Block(Default::default())),
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("block"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn step2_a_capability_without_an_access_type_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.volume_capabilities = vec![VolumeCapability {
        access_mode: mode(Mode::SingleNodeWriter),
        access_type: None,
    }];
    assert_eq!(code_of(create(&h, req).await).0, Code::InvalidArgument);
}

#[tokio::test(start_paused = true)]
async fn step2_an_fs_type_other_than_ext4_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.volume_capabilities = vec![mount_capability("xfs", Mode::SingleNodeWriter)];
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("xfs"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn step2_a_multi_node_access_mode_is_invalid() {
    let h = harness();
    for unsupported in [
        Mode::MultiNodeMultiWriter,
        Mode::MultiNodeSingleWriter,
        Mode::MultiNodeReaderOnly,
        Mode::Unknown,
    ] {
        let mut req = request("vol");
        req.volume_capabilities = vec![mount_capability("", unsupported)];
        assert_eq!(
            code_of(create(&h, req).await).0,
            Code::InvalidArgument,
            "{unsupported:?}"
        );
    }
    let mut req = request("vol");
    req.volume_capabilities[0].access_mode = None;
    assert_eq!(code_of(create(&h, req).await).0, Code::InvalidArgument);
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn step2_the_single_node_modes_and_an_empty_fs_type_pass() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    for (i, access) in [
        Mode::SingleNodeWriter,
        Mode::SingleNodeSingleWriter,
        Mode::SingleNodeMultiWriter,
        Mode::SingleNodeReaderOnly,
    ]
    .into_iter()
    .enumerate()
    {
        let mut req = request(&format!("vol-{i}"));
        req.volume_capabilities = vec![mount_capability(if i % 2 == 0 { "" } else { "ext4" }, access)];
        create(&h, req).await.unwrap_or_else(|e| panic!("{access:?}: {e}"));
    }
}

// ---- step 3 ----

#[tokio::test(start_paused = true)]
async fn step3_a_content_source_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.volume_content_source = Some(VolumeContentSource {
        r#type: Some(crate::csi::proto::volume_content_source::Type::Volume(
            Default::default(),
        )),
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("volume_content_source"), "{message}");
}

// ---- step 4 ----

#[tokio::test(start_paused = true)]
async fn step4_a_bad_parameter_is_invalid_and_named() {
    let h = harness();
    let (code, message) = code_of(create(&h, with_params(request("vol"), &[("replicas", "many")])).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("replicas") && message.contains("many"), "{message}");
    let (code, message) = code_of(create(&h, with_params(request("vol"), &[("colour", "red")])).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("colour"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn step4_a_negative_size_and_a_huge_one_are_out_of_range() {
    let h = harness();
    let mut req = request("vol");
    req.capacity_range = Some(CapacityRange {
        required_bytes: -1,
        limit_bytes: 0,
    });
    assert_eq!(code_of(create(&h, req).await).0, Code::OutOfRange);
    let mut req = request("vol");
    req.capacity_range = Some(CapacityRange {
        required_bytes: i64::MAX,
        limit_bytes: 0,
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::OutOfRange, "{message}");
}

#[tokio::test(start_paused = true)]
async fn step4_a_limit_below_the_rounded_size_is_out_of_range() {
    let h = harness();
    let mut req = request("vol");
    // 5000 bytes round up to 8192; the limit of 6000 cannot hold that.
    req.capacity_range = Some(CapacityRange {
        required_bytes: 5000,
        limit_bytes: 6000,
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("6000") && message.contains("8192"), "{message}");
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn step4_a_limit_equal_to_the_rounded_size_passes() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    let mut req = request("vol");
    req.capacity_range = Some(CapacityRange {
        required_bytes: 5000,
        limit_bytes: 8192,
    });
    assert_eq!(create(&h, req).await.unwrap().volume.unwrap().capacity_bytes, 8192);
}

// ---- steps 5 and 6 ----

#[tokio::test(start_paused = true)]
async fn step5_an_existing_object_with_the_same_parameters_goes_to_the_wait() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let _agents = agents_report(&h.volumes);
    let reply = create(&h, request("vol")).await.unwrap().volume.unwrap();
    assert_eq!(reply.volume_id, "vol");
    assert_eq!(reply.capacity_bytes, GIB as i64);
    assert_eq!(h.volumes.objects.lock().unwrap().len(), 1);
    // Nothing was placed again: the object still has its minor.
    assert_eq!(h.volumes.object("vol").unwrap().spec.minor, 1000);
}

#[tokio::test(start_paused = true)]
async fn step6_an_existing_object_with_other_parameters_is_already_exists() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let variants = [
        ("size", {
            let mut r = request("vol");
            r.capacity_range = Some(CapacityRange {
                required_bytes: 2 * GIB as i64,
                limit_bytes: 0,
            });
            r
        }),
        ("protocol", with_params(request("vol"), &[("protocol", "A")])),
        ("maxPeers", with_params(request("vol"), &[("maxPeers", "9")])),
        ("replicas", with_params(request("vol"), &[("replicas", "2")])),
    ];
    for (what, req) in variants {
        let (code, message) = code_of(create(&h, req).await);
        assert_eq!(code, Code::AlreadyExists, "{what}: {message}");
        assert!(message.contains("vol"), "{message}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_object_that_is_being_deleted_is_aborted() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    delete(&h, "vol").await.unwrap_err();
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Aborted, "{message}");
}

// ---- step 7 ----

#[test]
fn step7_place_errors_map_to_the_codes_of_the_table() {
    let invalid = [
        PlaceError::Replicas(1),
        PlaceError::NotEnoughNodes { replicas: 4, nodes: 3 },
        PlaceError::UnknownSelectedNode("x".into()),
        PlaceError::MaxPeers {
            max_peers: 1,
            needed: 2,
        },
    ];
    for error in invalid {
        let status = place_status(&error);
        assert_eq!(status.code(), Code::InvalidArgument, "{error}");
        assert_eq!(status.message(), error.to_string());
    }
    let exhausted = [
        PlaceError::NoCapacity {
            node: "n1".into(),
            free: 0,
            needed: 1,
        },
        PlaceError::NoFreeIndex,
    ];
    for error in exhausted {
        assert_eq!(place_status(&error).code(), Code::ResourceExhausted, "{error}");
    }
}

#[tokio::test(start_paused = true)]
async fn step7_one_replica_and_too_many_replicas_are_invalid() {
    let h = harness();
    assert_eq!(
        code_of(create(&h, with_params(request("vol"), &[("replicas", "1")])).await).0,
        Code::InvalidArgument
    );
    let (code, message) = code_of(create(&h, with_params(request("vol"), &[("replicas", "4")])).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("only 3 nodes"), "{message}");
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn step7_an_unknown_selected_node_is_invalid() {
    let h = harness();
    let mut req = request("vol");
    req.accessibility_requirements = Some(TopologyRequirement {
        preferred: vec![topology("ghost")],
        ..Default::default()
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::InvalidArgument);
    assert!(message.contains("ghost"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn step7_max_peers_below_the_other_nodes_is_invalid() {
    let h = harness();
    let (code, message) = code_of(create(&h, with_params(request("vol"), &[("maxPeers", "1")])).await);
    assert_eq!(code, Code::InvalidArgument, "{message}");
}

#[tokio::test(start_paused = true)]
async fn step7_no_capacity_is_resource_exhausted() {
    let h = harness();
    let mut req = request("vol");
    req.capacity_range = Some(CapacityRange {
        required_bytes: 200 * GIB as i64,
        limit_bytes: 0,
    });
    let (code, message) = code_of(create(&h, req).await);
    assert_eq!(code, Code::ResourceExhausted, "{message}");
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn step7_an_unreadable_node_map_is_unavailable_and_names_the_cause() {
    let volumes = Arc::new(FakeVolumes::default());
    let nodes = FakeNodes(Err(ApiError::Other("the ConfigMap ambar-nodes does not exist".into())));
    let svc = ControllerService::new(volumes.clone(), Arc::new(nodes));
    let err = svc.create_volume(Request::new(request("vol"))).await.unwrap_err();
    assert_eq!(err.code(), Code::Unavailable);
    assert!(err.message().contains("ambar-nodes"), "{}", err.message());
}

#[tokio::test(start_paused = true)]
async fn step7_the_object_has_initial_sync_skip_and_the_replica_finalizers() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    // Two replicas of three nodes: the third is a Witness (no finalizer).
    create(&h, with_params(request("vol"), &[("replicas", "2")]))
        .await
        .unwrap();
    let object = h.volumes.object("vol").unwrap();
    assert_eq!(object.spec.initial_sync, crate::model::InitialSync::Skip);
    let replicas: Vec<&str> = object
        .spec
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Replica)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(replicas, ["n1", "n2"]);
    assert_eq!(
        h.volumes.finalizers("vol"),
        [replica_finalizer("n1"), replica_finalizer("n2")]
    );
    assert!(object.spec.nodes.iter().any(|n| n.kind == NodeKind::Witness));
}

#[tokio::test(start_paused = true)]
async fn the_selected_node_comes_from_preferred_first() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    let mut req = with_params(request("vol"), &[("replicas", "2")]);
    req.accessibility_requirements = Some(TopologyRequirement {
        requisite: vec![topology("n1")],
        preferred: vec![topology("n3"), topology("n2")],
    });
    create(&h, req).await.unwrap();
    let object = h.volumes.object("vol").unwrap();
    let replicas: Vec<&str> = object
        .spec
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Replica)
        .map(|n| n.name.as_str())
        .collect();
    // Without the topology the replicas would be n1 and n2.
    assert!(replicas.contains(&"n3"), "{replicas:?}");
}

#[test]
fn selected_node_falls_back_to_requisite_and_is_none_without_requirements() {
    let mut req = request("vol");
    assert_eq!(selected_node(&req), None);
    req.accessibility_requirements = Some(TopologyRequirement {
        requisite: vec![topology("n2"), topology("n3")],
        preferred: vec![],
    });
    assert_eq!(selected_node(&req), Some("n2".to_string()));
    req.accessibility_requirements = Some(TopologyRequirement {
        requisite: vec![topology("n2")],
        preferred: vec![topology("n3")],
    });
    assert_eq!(selected_node(&req), Some("n3".to_string()));
    req.accessibility_requirements = Some(TopologyRequirement {
        requisite: vec![],
        preferred: vec![Topology::default()],
    });
    assert_eq!(selected_node(&req), None);
}

#[tokio::test(start_paused = true)]
async fn two_volumes_in_a_row_get_the_indices_0_and_1() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    create(&h, request("first")).await.unwrap();
    create(&h, request("second")).await.unwrap();
    let (a, b) = (h.volumes.object("first").unwrap(), h.volumes.object("second").unwrap());
    assert_eq!((a.spec.minor, a.spec.port), (1000, 7000));
    assert_eq!((b.spec.minor, b.spec.port), (1001, 7001));
}

#[tokio::test(start_paused = true)]
async fn an_object_being_deleted_still_holds_its_minor_and_budget() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    h.volumes.put(object_json("old", 99 * GIB, "C", 7, 1000, 7000, 3));
    delete(&h, "old").await.unwrap_err();
    assert!(h.volumes.object("old").unwrap().metadata.deletion_timestamp.is_some());
    // The old copy still takes 99 of 100 GiB on every node: a 2 GiB volume does not fit.
    let mut req = request("big");
    req.capacity_range = Some(CapacityRange {
        required_bytes: 2 * GIB as i64,
        limit_bytes: 0,
    });
    assert_eq!(code_of(create(&h, req).await).0, Code::ResourceExhausted);
    // A small one fits and takes the next index.
    create(&h, request("small")).await.unwrap();
    assert_eq!(h.volumes.object("small").unwrap().spec.minor, 1001);
}

// ---- concurrency ----

/// A cache that never shows the objects `names` (as far as any test can count).
fn blind_cache(h: &Harness, names: &[&str]) {
    let mut hidden = h.volumes.hidden.lock().unwrap();
    for name in names {
        hidden.insert(name.to_string(), u32::MAX);
    }
}

fn indices(h: &Harness, names: &[&str]) -> Vec<(u32, u16)> {
    let mut found: Vec<(u32, u16)> = names
        .iter()
        .map(|n| {
            let o = h.volumes.object(n).unwrap();
            (o.spec.minor, o.spec.port)
        })
        .collect();
    found.sort();
    found
}

// V1: placement reads the live list, never the cache.

#[tokio::test(start_paused = true)]
async fn v1_two_sequential_creates_get_different_indices_while_the_cache_lags() {
    let h = harness();
    blind_cache(&h, &["vol-a", "vol-b"]);
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol-a")).await.unwrap();
    create(&h, request("vol-b")).await.unwrap();
    assert_eq!(indices(&h, &["vol-a", "vol-b"]), [(1000, 7000), (1001, 7001)]);
}

#[tokio::test(start_paused = true)]
async fn v1_two_interleaved_creates_get_different_indices_while_the_cache_lags() {
    let h = harness();
    h.volumes.yields.store(true, Ordering::SeqCst);
    blind_cache(&h, &["vol-a", "vol-b"]);
    let _agents = agents_report(&h.volumes);
    let (a, b) = tokio::join!(create(&h, request("vol-a")), create(&h, request("vol-b")));
    a.unwrap();
    b.unwrap();
    assert_eq!(indices(&h, &["vol-a", "vol-b"]), [(1000, 7000), (1001, 7001)]);
}

#[tokio::test(start_paused = true)]
async fn v1_a_handler_dropped_right_after_the_create_leaves_its_indices_taken() {
    let h = harness();
    h.volumes.yields.store(true, Ordering::SeqCst);
    blind_cache(&h, &["vol-a", "vol-b"]);
    // The call is cancelled (the caller went away) as soon as the object is stored.
    tokio::select! {
        biased;
        _ = create(&h, request("vol-a")) => panic!("the call must be cancelled before it ends"),
        _ = h.volumes.created.notified() => {}
    }
    assert!(h.volumes.object("vol-a").is_some());
    h.volumes.yields.store(false, Ordering::SeqCst);
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol-b")).await.unwrap();
    assert_eq!(indices(&h, &["vol-a", "vol-b"]), [(1000, 7000), (1001, 7001)]);
}

#[tokio::test(start_paused = true)]
async fn v1_placement_does_not_wait_for_the_cache() {
    let h = harness();
    blind_cache(&h, &["vol-a"]);
    let _agents = agents_report(&h.volumes);
    let started = tokio::time::Instant::now();
    create(&h, request("vol-a")).await.unwrap();
    // The only time spent is the wait for the replicas (one agent tick), not for a listing.
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

// V2: two Controller instances; the later object withdraws.

/// A rival object `name` with the given minor and port, created at `stamp` (None: no timestamp).
fn rival(name: &str, minor: u32, port: u16, stamp: Option<&str>) -> ReplicatedVolume {
    let mut object = object_json(name, GIB, "C", 7, minor, port, 3);
    object.metadata.creation_timestamp = stamp.map(time);
    object
}

/// The rival appears between the live list of the placement and the create.
fn rival_appears(h: &Harness, object: ReplicatedVolume) {
    *h.volumes.on_create.lock().unwrap() = Some(Box::new(move |fake| fake.put(object)));
}

const OLDER: &str = "2025-12-31T23:00:00Z";
const NEWER: &str = "2026-01-01T01:00:00Z";

#[tokio::test(start_paused = true)]
async fn v2_a_later_object_with_the_same_minor_or_port_is_deleted_and_aborted() {
    for (minor, port) in [(1000, 7999), (1999, 7000), (1000, 7000)] {
        let h = harness();
        rival_appears(&h, rival("rival", minor, port, Some(OLDER)));
        let (code, message) = code_of(create(&h, request("vol")).await);
        assert_eq!(code, Code::Aborted, "{minor} {port}: {message}");
        assert!(message.contains("rival"), "{message}");
        assert_eq!(*h.volumes.deleted.lock().unwrap(), ["vol"], "{minor} {port}");
        assert!(h.volumes.object("vol").unwrap().metadata.deletion_timestamp.is_some());
        assert!(h.volumes.object("rival").unwrap().metadata.deletion_timestamp.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn v2_an_earlier_object_is_kept_and_the_call_goes_to_the_wait() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some(NEWER)));
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol")).await.unwrap();
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
    assert!(h.volumes.object("vol").unwrap().metadata.deletion_timestamp.is_none());
}

#[tokio::test(start_paused = true)]
async fn w1_a_rival_with_the_same_timestamp_wins_whatever_its_name_is() {
    // "aaa" sorts before "vol", "zzz" after it: in both cases the later
    // creator withdraws, there is no name tie-break.
    for rival_name in ["aaa", "zzz"] {
        let h = harness();
        rival_appears(&h, rival(rival_name, 1000, 7000, Some(T0)));
        let (code, message) = code_of(create(&h, request("vol")).await);
        assert_eq!(code, Code::Aborted, "{rival_name}: {message}");
        assert!(message.contains(rival_name), "{message}");
        assert_eq!(*h.volumes.deleted.lock().unwrap(), ["vol"], "{rival_name}");
        assert!(h.volumes.object("vol").unwrap().metadata.deletion_timestamp.is_some());
        assert!(
            h.volumes
                .object(rival_name)
                .unwrap()
                .metadata
                .deletion_timestamp
                .is_none()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn w1_a_rival_one_second_earlier_wins_and_one_second_later_loses() {
    // `T0` is the stamp of the object this call creates.
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some("2025-12-31T23:59:59Z")));
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Aborted);
    assert_eq!(*h.volumes.deleted.lock().unwrap(), ["vol"]);

    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some("2026-01-01T00:00:01Z")));
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol")).await.unwrap();
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v2_no_collision_deletes_nothing() {
    let h = harness();
    rival_appears(&h, rival("rival", 1001, 7001, Some(OLDER)));
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol")).await.unwrap();
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v2_a_rival_without_a_timestamp_is_unclear_and_nothing_is_deleted() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, None));
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Aborted, "{message}");
    assert!(message.contains("rival"), "{message}");
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v2_an_object_created_again_under_the_name_is_never_deleted() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some(OLDER)));
    // Between the create and the check somebody replaces "vol" (another uid).
    *h.volumes.after_create.lock().unwrap() = Some(Box::new(|fake| {
        let mut again = fake.object("vol").unwrap();
        again.metadata.uid = Some("uid-of-somebody-else".to_string());
        fake.put(again);
    }));
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Aborted, "{message}");
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert_eq!(
        h.volumes.object("vol").unwrap().metadata.uid.as_deref(),
        Some("uid-of-somebody-else")
    );
}

#[tokio::test(start_paused = true)]
async fn v2_an_object_missing_from_the_check_is_unclear_and_nothing_is_deleted() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some(OLDER)));
    *h.volumes.after_create.lock().unwrap() = Some(Box::new(|fake| {
        fake.objects.lock().unwrap().remove("vol");
    }));
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Aborted);
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v2_the_withdrawal_deletes_with_the_uid_of_the_create() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some(OLDER)));
    code_of(create(&h, request("vol")).await);
    let uid = h.volumes.object("vol").unwrap().metadata.uid.unwrap();
    assert_eq!(*h.volumes.exact_deletes.lock().unwrap(), [("vol".to_string(), uid)]);
}

#[tokio::test(start_paused = true)]
async fn v2_a_failing_withdrawal_is_unavailable() {
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, Some(OLDER)));
    *h.volumes.fail_exact.lock().unwrap() = Some(ApiError::Other("the API server is down".into()));
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Unavailable);
}

// W2: a retry re-checks the collision, read only.

#[tokio::test(start_paused = true)]
async fn w2_a_retry_finding_a_colliding_object_is_aborted_and_deletes_nothing() {
    // Same minor, same port, both; the rival is older or newer than "vol":
    // the retry path never decides by age, it deletes nothing.
    for (minor, port) in [(1000, 7999), (1999, 7000), (1000, 7000)] {
        for stamp in [OLDER, NEWER] {
            let h = harness();
            h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
            h.volumes.put(rival("rival", minor, port, Some(stamp)));
            let (code, message) = code_of(create(&h, request("vol")).await);
            assert_eq!(code, Code::Aborted, "{minor} {port} {stamp}: {message}");
            assert!(
                message.contains("\"vol\"") && message.contains("\"rival\""),
                "{message}"
            );
            assert!(message.contains("operator") && message.contains("newer"), "{message}");
            assert!(h.volumes.deleted.lock().unwrap().is_empty());
            assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
            assert!(h.volumes.object("vol").unwrap().metadata.deletion_timestamp.is_none());
            assert!(h.volumes.object("rival").unwrap().metadata.deletion_timestamp.is_none());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn w2_a_retry_without_a_collision_goes_to_the_wait() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    h.volumes.put(rival("rival", 1001, 7001, Some(OLDER)));
    // Nothing reports: the wait runs to its deadline, so the check did not stop it.
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::DeadlineExceeded, "{message}");
    assert_eq!(h.volumes.fresh_lists.load(Ordering::SeqCst), 1);
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn w2_a_retry_of_an_up_to_date_object_succeeds_without_a_live_list() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    // A colliding object exists, but a volume that is up to date is not re-checked.
    h.volumes.put(rival("rival", 1000, 7000, Some(OLDER)));
    for node in ["n1", "n2", "n3"] {
        h.volumes.set_entry("vol", up_to_date(node));
    }
    assert_eq!(
        create(&h, request("vol")).await.unwrap().volume.unwrap().volume_id,
        "vol"
    );
    assert_eq!(h.volumes.fresh_lists.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn w2_a_failing_live_list_on_the_retry_path_is_unavailable() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    h.volumes.fail_fresh.store(true, Ordering::SeqCst);
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Unavailable, "{message}");
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn w2_the_retry_after_an_unclear_abort_does_not_wait_on_a_colliding_pair() {
    // The first attempt created "vol" and aborted as unclear (the rival has
    // no timestamp), leaving both objects; the retry reports the collision
    // at once instead of waiting for the deadline.
    let h = harness();
    rival_appears(&h, rival("rival", 1000, 7000, None));
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Aborted);
    let started = tokio::time::Instant::now();
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Aborted, "{message}");
    assert!(message.contains("operator"), "{message}");
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(h.volumes.exact_deletes.lock().unwrap().is_empty());
}

// V3: error mapping and small cases.

#[tokio::test(start_paused = true)]
async fn v3_a_negative_capacity_is_out_of_range_and_names_the_field() {
    let h = harness();
    for (required, limit, field) in [(-1, 0, "required_bytes"), (0, -5, "limit_bytes")] {
        let mut req = request("vol");
        req.capacity_range = Some(CapacityRange {
            required_bytes: required,
            limit_bytes: limit,
        });
        let (code, message) = code_of(create(&h, req).await);
        assert_eq!(code, Code::OutOfRange, "{field}: {message}");
        assert!(message.contains(field), "{message}");
    }
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v3_a_second_delete_of_an_object_that_is_going_away_makes_no_delete_call() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    delete(&h, "vol").await.unwrap_err();
    assert_eq!(*h.volumes.deleted.lock().unwrap(), ["vol"]);
    let (code, _) = code_of(delete(&h, "vol").await);
    assert_eq!(code, Code::DeadlineExceeded);
    assert_eq!(h.volumes.deleted.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn v3_a_rejected_create_is_internal() {
    let h = harness();
    *h.volumes.fail_create.lock().unwrap() = Some(ApiError::Rejected("spec.minor: Invalid value".into()));
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Internal, "{message}");
    assert!(message.contains("spec.minor"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn v3_a_failing_get_live_list_or_create_is_unavailable() {
    let h = harness();
    h.volumes.failing.lock().unwrap().insert("vol".to_string());
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Unavailable);
    h.volumes.failing.lock().unwrap().clear();

    h.volumes.fail_fresh.store(true, Ordering::SeqCst);
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Unavailable);
    h.volumes.fail_fresh.store(false, Ordering::SeqCst);

    *h.volumes.fail_create.lock().unwrap() = Some(ApiError::Other("the API server is down".into()));
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::Unavailable);
    assert!(h.volumes.objects.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn v3_the_cached_list_failing_does_not_matter_to_create() {
    let h = harness();
    h.volumes.fail_list.store(true, Ordering::SeqCst);
    let _agents = agents_report(&h.volumes);
    create(&h, request("vol")).await.unwrap();
    assert_eq!(h.volumes.list_failures.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn v3_a_retry_with_another_selected_node_goes_to_the_wait() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let _agents = agents_report(&h.volumes);
    let mut req = request("vol");
    req.accessibility_requirements = Some(TopologyRequirement {
        preferred: vec![topology("n3")],
        ..Default::default()
    });
    assert_eq!(create(&h, req).await.unwrap().volume.unwrap().volume_id, "vol");
    assert_eq!(h.volumes.objects.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn v3_every_place_error_leaves_no_object() {
    let h = harness();
    let mut unknown_node = request("a");
    unknown_node.accessibility_requirements = Some(TopologyRequirement {
        preferred: vec![topology("ghost")],
        ..Default::default()
    });
    let mut huge = request("b");
    huge.capacity_range = Some(CapacityRange {
        required_bytes: 200 * GIB as i64,
        limit_bytes: 0,
    });
    let requests = [
        unknown_node,
        huge,
        with_params(request("c"), &[("replicas", "1")]),
        with_params(request("d"), &[("replicas", "4")]),
        with_params(request("e"), &[("maxPeers", "1")]),
    ];
    for req in requests {
        let name = req.name.clone();
        code_of(create(&h, req).await);
        assert!(h.volumes.objects.lock().unwrap().is_empty(), "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_second_call_for_a_name_does_not_create_a_second_object() {
    let h = harness();
    h.volumes.yields.store(true, Ordering::SeqCst);
    // Nothing is ready: both calls wait and time out, then a retry follows.
    let (a, b) = tokio::join!(create(&h, request("vol")), create(&h, request("vol")));
    assert_eq!(code_of(a).0, Code::DeadlineExceeded);
    assert_eq!(code_of(b).0, Code::DeadlineExceeded);
    assert_eq!(code_of(create(&h, request("vol")).await).0, Code::DeadlineExceeded);
    assert_eq!(h.volumes.objects.lock().unwrap().len(), 1);
    // The agents report: the retry of the provisioner now succeeds.
    let _agents = agents_report(&h.volumes);
    assert_eq!(
        create(&h, request("vol")).await.unwrap().volume.unwrap().volume_id,
        "vol"
    );
    assert_eq!(h.volumes.objects.lock().unwrap().len(), 1);
}

// ---- step 8 and 9 ----

#[tokio::test(start_paused = true)]
async fn step8_the_wait_ends_with_deadline_exceeded_and_leaves_the_object() {
    let h = harness();
    let started = tokio::time::Instant::now();
    // One of three replicas has not reported.
    let late = h.volumes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        late.set_entry("vol", up_to_date("n1"));
        late.set_entry("vol", up_to_date("n2"));
    });
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::DeadlineExceeded);
    assert!(message.contains("n3") && !message.contains("n1"), "{message}");
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    assert!(h.volumes.object("vol").is_some());
}

#[tokio::test(start_paused = true)]
async fn step8_the_wait_returns_as_soon_as_every_replica_is_up_to_date() {
    let h = harness();
    let late = h.volumes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        for node in ["n1", "n2", "n3"] {
            late.set_entry("vol", up_to_date(node));
        }
    });
    let started = tokio::time::Instant::now();
    create(&h, request("vol")).await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
}

#[tokio::test(start_paused = true)]
async fn step8_an_object_deleted_during_the_wait_is_aborted() {
    let h = harness();
    let vanish = h.volumes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        vanish.objects.lock().unwrap().remove("vol");
    });
    let (code, message) = code_of(create(&h, request("vol")).await);
    assert_eq!(code, Code::Aborted, "{message}");
}

#[tokio::test(start_paused = true)]
async fn step9_the_reply_carries_the_name_the_size_and_no_topology() {
    let h = harness();
    let _agents = agents_report(&h.volumes);
    let mut req = request("vol");
    req.capacity_range = Some(CapacityRange {
        required_bytes: 5000,
        limit_bytes: 0,
    });
    let volume = create(&h, req).await.unwrap().volume.unwrap();
    assert_eq!(volume.volume_id, "vol");
    assert_eq!(volume.capacity_bytes, 8192);
    assert!(volume.accessible_topology.is_empty());
    assert!(volume.volume_context.is_empty());
    assert!(volume.content_source.is_none());
}

// ---- DeleteVolume ----

#[tokio::test(start_paused = true)]
async fn delete_with_an_empty_id_is_invalid() {
    let h = harness();
    assert_eq!(code_of(delete(&h, "").await).0, Code::InvalidArgument);
}

#[tokio::test(start_paused = true)]
async fn delete_of_an_invalid_name_or_a_missing_object_is_ok() {
    let h = harness();
    delete(&h, "Not_A_Name").await.unwrap();
    delete(&h, "gone").await.unwrap();
    assert!(h.volumes.deleted.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn delete_with_a_finalizer_left_is_deadline_exceeded_after_one_delete_call() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let started = tokio::time::Instant::now();
    let (code, message) = code_of(delete(&h, "vol").await);
    assert_eq!(code, Code::DeadlineExceeded);
    assert!(message.contains("vol"), "{message}");
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    assert_eq!(*h.volumes.deleted.lock().unwrap(), ["vol"]);
    assert!(h.volumes.object("vol").unwrap().metadata.deletion_timestamp.is_some());
}

#[tokio::test(start_paused = true)]
async fn delete_returns_ok_when_the_agents_release_the_object() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let agents = h.volumes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let uid = agents.object("vol").unwrap().metadata.uid.unwrap();
        for node in ["n1", "n2", "n3"] {
            agents
                .remove_finalizer("vol", &uid, &replica_finalizer(node))
                .await
                .unwrap();
        }
    });
    delete(&h, "vol").await.unwrap();
    assert!(h.volumes.object("vol").is_none());
    assert_eq!(h.volumes.deleted.lock().unwrap().len(), 1);
}

// ---- ValidateVolumeCapabilities and the rest ----

async fn validate(
    h: &Harness,
    id: &str,
    caps: Vec<VolumeCapability>,
) -> Result<ValidateVolumeCapabilitiesResponse, Status> {
    h.svc
        .validate_volume_capabilities(Request::new(ValidateVolumeCapabilitiesRequest {
            volume_id: id.to_string(),
            volume_capabilities: caps,
            ..Default::default()
        }))
        .await
        .map(|r| r.into_inner())
}

#[tokio::test(start_paused = true)]
async fn validate_of_a_missing_volume_is_not_found() {
    let h = harness();
    let caps = vec![mount_capability("ext4", Mode::SingleNodeWriter)];
    assert_eq!(code_of(validate(&h, "gone", caps.clone()).await).0, Code::NotFound);
    assert_eq!(code_of(validate(&h, "Not_A_Name", caps).await).0, Code::NotFound);
}

#[tokio::test(start_paused = true)]
async fn validate_confirms_supported_capabilities_and_explains_the_others() {
    let h = harness();
    h.volumes.put(object_json("vol", GIB, "C", 7, 1000, 7000, 3));
    let caps = vec![mount_capability("ext4", Mode::SingleNodeWriter)];
    let reply = validate(&h, "vol", caps.clone()).await.unwrap();
    assert_eq!(reply.confirmed.unwrap().volume_capabilities, caps);

    let bad = vec![mount_capability("ext4", Mode::MultiNodeMultiWriter)];
    let reply = validate(&h, "vol", bad).await.unwrap();
    assert!(reply.confirmed.is_none());
    assert!(reply.message.contains("MULTI_NODE_MULTI_WRITER"), "{}", reply.message);
}

#[tokio::test(start_paused = true)]
async fn controller_capabilities_are_create_delete_volume() {
    let h = harness();
    let caps = h
        .svc
        .controller_get_capabilities(Request::new(ControllerGetCapabilitiesRequest {}))
        .await
        .unwrap()
        .into_inner()
        .capabilities;
    assert_eq!(caps.len(), 1);
    match caps[0].r#type {
        Some(controller_service_capability::Type::Rpc(rpc)) => {
            assert_eq!(rpc.r#type, rpc::Type::CreateDeleteVolume as i32)
        }
        ref other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn other_calls_are_unimplemented() {
    let h = harness();
    let err = h
        .svc
        .get_capacity(Request::new(GetCapacityRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
    let err = h
        .svc
        .controller_publish_volume(Request::new(ControllerPublishVolumeRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
}

#[tokio::test(start_paused = true)]
async fn create_volume_reads_node_map_outside_placing_lock() {
    struct NotifyingNodes {
        loaded: Arc<tokio::sync::Notify>,
        nodes: crate::config::ClusterNodes,
    }

    #[async_trait::async_trait]
    impl crate::agent::nodes::NodeMap for NotifyingNodes {
        async fn load(&self) -> Result<crate::config::ClusterNodes, ApiError> {
            self.loaded.notify_one();
            Ok(self.nodes.clone())
        }
    }

    let volumes = Arc::new(FakeVolumes::default());
    let loaded = Arc::new(tokio::sync::Notify::new());
    let nodes = Arc::new(NotifyingNodes {
        loaded: loaded.clone(),
        nodes: cluster(&[100 * GIB; 3]),
    });
    let svc = Arc::new(ControllerService::new(volumes, nodes));

    let guard = svc.placing.lock().await;
    let svc_task = svc.clone();
    let wanted = PlaceRequest {
        size_bytes: GIB,
        replicas: 2,
        protocol: crate::model::Protocol::C,
        max_peers: 3,
        selected_node: None,
    };
    let task = tokio::spawn(async move { svc_task.find_or_create("vol-outside-lock", &wanted).await });

    tokio::time::timeout(Duration::from_secs(1), loaded.notified())
        .await
        .expect("nodes.load() must run without waiting for self.placing");

    drop(guard);
    task.await.unwrap().unwrap();
}
