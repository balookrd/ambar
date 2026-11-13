use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tonic::{Code, Request};

use super::*;
use crate::agent::kube::Volumes;
use crate::agent::run::VolumeLocks;
use crate::crd::{MemberNode, ReplicatedVolumeSpec, to_volume_spec};
use crate::csi::proto::volume_capability::{AccessMode, access_mode::Mode};
use crate::drbd::resource;
use crate::exec::Output;
use crate::membership::replica_finalizer;
use crate::paths::Paths;
use crate::tests::volumes::{FakeNodes, FakeVolumes, cluster};
use crate::tests::{FakeRunner, out};

const GIB: u64 = 1 << 30;
const VOLUME: &str = "pv1";
const NODE: &str = "n4";
const MINOR: u32 = 1000;

// ---- the simulated node ----

type BlkidHook = Box<dyn Fn(&FakeVolumes) + Send>;

/// What is on the device the volume shows as `/dev/drbd<minor>`.
#[derive(Clone, Copy)]
enum Dev {
    Empty,
    Ext4,
    /// `blkid` says `Wrong medium type`: the checks cannot tell.
    Ambiguous,
    Other(&'static str),
    /// The device file does not exist: it cannot be opened.
    Missing,
}

#[derive(Clone)]
struct Mounted {
    target: String,
    source: String,
    root: String,
    read_only: bool,
    options: Option<String>,
}

/// The commands of one node: DRBD, `blkid`, `mkfs.ext4`, `mount`, `umount`.
/// `mount` and `umount` rewrite the mountinfo file the service reads.
struct Sim {
    volumes: Arc<FakeVolumes>,
    node: String,
    up: AtomicBool,
    usable: AtomicBool,
    /// The resource becomes usable at this call of `drbdsetup status` (0: never by itself).
    usable_at_status_call: AtomicU32,
    status_calls: AtomicU32,
    device: PathBuf,
    blkid: Mutex<Output>,
    mounts: Mutex<Vec<Mounted>>,
    mountinfo: PathBuf,
    /// Replies that replace the command: keyed by `<program>` or `<program> <first argument>`.
    failing: Mutex<BTreeMap<String, Output>>,
    /// What the object showed when `drbdadm down` ran: (entry of this node, member of `spec.nodes`).
    at_down: Mutex<Option<(bool, bool)>>,
    /// Runs when `blkid` is called: another node acts between the signature check and `mkfs`.
    on_blkid: Mutex<Option<BlkidHook>>,
}

impl Sim {
    fn reply(&self, program: &str, args: &[&str]) -> Output {
        {
            let failing = self.failing.lock().unwrap();
            let first = args.first().copied().unwrap_or("");
            if let Some(reply) = failing
                .get(&format!("{program} {first}"))
                .or_else(|| failing.get(program))
            {
                return reply.clone();
            }
        }
        match (program, args) {
            ("drbdsetup", _) => out(0, &self.kernel(), ""),
            ("drbdadm", ["up", _]) => {
                self.up.store(true, SeqCst);
                out(0, "", "")
            }
            ("drbdadm", ["down", _]) => {
                let object = self.volumes.object(VOLUME);
                let entry = self.volumes.entry_of(VOLUME, &self.node).is_some();
                let member = object.is_some_and(|o| o.spec.nodes.iter().any(|n| n.name == self.node));
                *self.at_down.lock().unwrap() = Some((entry, member));
                self.up.store(false, SeqCst);
                out(0, "", "")
            }
            ("blkid", _) => {
                let reply = self.blkid.lock().unwrap().clone();
                if let Some(hook) = self.on_blkid.lock().unwrap().as_ref() {
                    hook(&self.volumes);
                }
                reply
            }
            ("mkfs.ext4", _) => {
                // The device now carries a filesystem.
                let file = fs::OpenOptions::new().write(true).open(&self.device).unwrap();
                file.write_all_at(&[0x53, 0xef], 1080).unwrap();
                *self.blkid.lock().unwrap() = out(0, "ext4\n", "");
                out(0, "", "")
            }
            ("mount", ["-t", "ext4", "-o", options, device, target]) => {
                self.add_mount(device, target, "/", false, Some(options));
                out(0, "", "")
            }
            ("mount", ["-t", "ext4", device, target]) => {
                self.add_mount(device, target, "/", false, None);
                out(0, "", "")
            }
            ("mount", ["--bind", source, target]) => {
                let origin = self
                    .mounts
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|m| m.target == *source)
                    .cloned();
                match origin {
                    Some(origin) => {
                        self.add_mount(&origin.source, target, &origin.root, false, None);
                        out(0, "", "")
                    }
                    None => out(32, "", "mount: special device does not exist"),
                }
            }
            ("mount", ["-o", "remount,bind,ro", target]) => {
                if let Some(m) = self
                    .mounts
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .rev()
                    .find(|m| m.target == *target)
                {
                    m.read_only = true;
                }
                self.write_mountinfo();
                out(0, "", "")
            }
            ("umount", [target]) => {
                let mut mounts = self.mounts.lock().unwrap();
                if let Some(index) = mounts.iter().rposition(|m| m.target == *target) {
                    mounts.remove(index);
                }
                drop(mounts);
                self.write_mountinfo();
                out(0, "", "")
            }
            ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
            _ => out(0, "", ""),
        }
    }

    fn add_mount(&self, source: &str, target: &str, root: &str, read_only: bool, options: Option<&str>) {
        self.mounts.lock().unwrap().push(Mounted {
            target: target.to_string(),
            source: source.to_string(),
            root: root.to_string(),
            read_only,
            options: options.map(str::to_string),
        });
        self.write_mountinfo();
    }

    fn write_mountinfo(&self) {
        let text: String = self
            .mounts
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "{} 1 147:{} {} {} {},relatime - ext4 {} rw\n",
                    300 + i,
                    MINOR,
                    m.root,
                    m.target,
                    if m.read_only { "ro" } else { "rw" },
                    m.source
                )
            })
            .collect();
        fs::write(&self.mountinfo, text).unwrap();
    }

    /// `drbdsetup status --json` as this node sees the object now.
    fn kernel(&self) -> String {
        const EMPTY: &str = "[\n]\n\n";
        let call = self.status_calls.fetch_add(1, SeqCst) + 1;
        let usable_at = self.usable_at_status_call.load(SeqCst);
        if usable_at > 0 && call >= usable_at {
            self.usable.store(true, SeqCst);
        }
        if !self.up.load(SeqCst) {
            return EMPTY.to_string();
        }
        let Some(object) = self.volumes.object(VOLUME) else {
            return EMPTY.to_string();
        };
        let spec = &object.spec;
        let Some(me) = spec.nodes.iter().find(|n| n.name == self.node) else {
            return EMPTY.to_string();
        };
        let has_disk = me.kind == NodeKind::Replica;
        let connections: Vec<Value> = spec
            .nodes
            .iter()
            .filter(|n| n.name != self.node)
            .map(|peer| {
                let peer_disk = peer.kind == NodeKind::Replica;
                json!({
                    "peer-node-id": peer.node_id,
                    "name": peer.name,
                    "connection-state": "Connected",
                    "peer-role": "Secondary",
                    "peer_devices": [{
                        "volume": 0,
                        "replication-state": "Established",
                        "peer-disk-state": if peer_disk { "UpToDate" } else { "Diskless" },
                        "peer-client": !peer_disk,
                        "out-of-sync": 0,
                    }],
                })
            })
            .collect();
        json!([{
            "name": VOLUME,
            "node-id": me.node_id,
            "role": "Secondary",
            "suspended": false,
            "devices": [{
                "volume": 0,
                "minor": spec.minor,
                "disk-state": if has_disk { "UpToDate" } else { "Diskless" },
                "client": !has_disk,
                "open": false,
                "quorum": self.usable.load(SeqCst),
                "size": 1048476,
            }],
            "connections": connections,
        }])
        .to_string()
    }

    fn mounted_at(&self, path: &Path) -> Vec<Mounted> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.mounts
            .lock()
            .unwrap()
            .iter()
            .filter(|m| Path::new(&m.target) == path)
            .cloned()
            .collect()
    }
}

// ---- fixtures ----

struct Env {
    svc: NodeService,
    volumes: Arc<FakeVolumes>,
    sim: Arc<Sim>,
    runner: Arc<FakeRunner>,
    /// Commands of the set-up that `runner_calls` leaves out.
    skip: AtomicUsize,
    /// The lines the service wrote to its log.
    logs: Arc<Mutex<Vec<String>>>,
    paths: Paths,
    staging: PathBuf,
    target: PathBuf,
    dev_dir: PathBuf,
    _tmp: TempDir,
}

fn member(name: &str, node_id: u32, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.to_string(),
        node_id,
        address: format!("192.0.2.{}", node_id + 1),
        kind,
    }
}

fn base_spec() -> ReplicatedVolumeSpec {
    serde_json::from_value(json!({
        "sizeBytes": GIB, "protocol": "C", "minor": MINOR, "port": 7000, "maxPeers": 7,
        "initialSync": "Done",
        "nodes": [
            member("n1", 0, NodeKind::Replica),
            member("n2", 1, NodeKind::Replica),
            member("n3", 2, NodeKind::Witness),
        ],
    }))
    .unwrap()
}

/// The volume of the tests: replicas `n1`, `n2`, witness `n3`; `NODE` is not a member.
fn volume_object(name: &str, minor: u32, port: u16) -> ReplicatedVolume {
    let mut spec = base_spec();
    spec.minor = minor;
    spec.port = port;
    let mut object = ReplicatedVolume::new(name, spec);
    object.metadata.finalizers = Some(vec![replica_finalizer("n1"), replica_finalizer("n2")]);
    object
}

fn env() -> Env {
    env_for(NODE)
}

fn env_for(node: &str) -> Env {
    let tmp = TempDir::new().unwrap();
    // Canonical, so that the paths the service resolves are the paths of the tests.
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paths = Paths {
        volumes_dir: root.join("volumes"),
        res_dir: root.join("res"),
        dev_dir: root.join("ambar-dev"),
    };
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let dev_dir = root.join("dev");
    fs::create_dir_all(&dev_dir).unwrap();
    let device = dev_dir.join(format!("drbd{MINOR}"));
    let mountinfo = root.join("mountinfo");
    fs::write(&mountinfo, "").unwrap();
    fs::write(&device, vec![0u8; 8192]).unwrap();

    let volumes = Arc::new(FakeVolumes::default());
    let sim = Arc::new(Sim {
        volumes: volumes.clone(),
        node: node.to_string(),
        up: AtomicBool::new(false),
        usable: AtomicBool::new(true),
        usable_at_status_call: AtomicU32::new(0),
        status_calls: AtomicU32::new(0),
        device,
        blkid: Mutex::new(out(2, "", "")),
        mounts: Mutex::new(Vec::new()),
        mountinfo: mountinfo.clone(),
        failing: Mutex::new(BTreeMap::new()),
        at_down: Mutex::new(None),
        on_blkid: Mutex::new(None),
    });
    let runner = Arc::new(FakeRunner::new({
        let sim = sim.clone();
        move |program, args| sim.reply(program, args)
    }));
    let agent = Arc::new(Agent {
        node: node.to_string(),
        paths: paths.clone(),
        runner: runner.clone(),
        volumes: volumes.clone(),
        locks: VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(true)),
    });
    let mut svc = NodeService::new(agent, Arc::new(FakeNodes(Ok(cluster(&[100 * GIB; 4])))));
    svc.mountinfo = mountinfo;
    svc.dev_dir = dev_dir.clone();
    svc.allow_regular_files = true;
    svc.stage_wait = Duration::from_secs(10);
    svc.poll = Duration::from_millis(500);
    let logs = Arc::new(Mutex::new(Vec::new()));
    svc.log = {
        let logs = logs.clone();
        Arc::new(move |line| logs.lock().unwrap().push(line.to_string()))
    };
    Env {
        svc,
        volumes,
        sim,
        runner,
        skip: AtomicUsize::new(0),
        logs,
        paths,
        staging: root.join("staging").join(VOLUME),
        target: root.join("pods").join("p1").join("mount"),
        dev_dir,
        _tmp: tmp,
    }
}

impl Env {
    fn set_device(&self, state: Dev) {
        if matches!(state, Dev::Missing) {
            let _ = fs::remove_file(&self.sim.device);
            return;
        }
        let mut bytes = vec![0u8; 8192];
        if matches!(state, Dev::Ext4) {
            bytes[1080] = 0x53;
            bytes[1081] = 0xef;
        }
        fs::write(&self.sim.device, bytes).unwrap();
        *self.sim.blkid.lock().unwrap() = match state {
            Dev::Empty => out(2, "", ""),
            Dev::Ext4 => out(0, "ext4\n", ""),
            Dev::Ambiguous => out(2, "", "blkid: error: Wrong medium type\n"),
            Dev::Other(found) => out(0, &format!("{found}\n"), ""),
            Dev::Missing => unreachable!(),
        };
    }

    fn fail(&self, command: &str, code: i32, stderr: &str) {
        self.sim
            .failing
            .lock()
            .unwrap()
            .insert(command.to_string(), out(code, "", stderr));
    }

    /// Every command since the set-up.
    fn runner_calls(&self) -> Vec<String> {
        self.runner.calls().into_iter().skip(self.skip.load(SeqCst)).collect()
    }

    fn calls(&self, prefixes: &[&str]) -> Vec<String> {
        self.runner_calls()
            .into_iter()
            .filter(|c| prefixes.iter().any(|p| c.starts_with(p)))
            .collect()
    }

    /// Commands that open, change or mount the device.
    fn device_commands(&self) -> Vec<String> {
        self.calls(&["blkid", "mkfs.ext4", "mount", "umount"])
    }

    fn patches(&self) -> usize {
        self.volumes.calls().iter().filter(|c| c.starts_with("patch ")).count()
    }

    fn members(&self) -> Vec<(String, NodeKind)> {
        self.volumes
            .object(VOLUME)
            .unwrap()
            .spec
            .nodes
            .into_iter()
            .map(|n| (n.name, n.kind))
            .collect()
    }

    fn annotated(&self) -> bool {
        self.volumes
            .object(VOLUME)
            .unwrap()
            .metadata
            .annotations
            .is_some_and(|a| a.get(FORMATTED_ANNOTATION).map(String::as_str) == Some("true"))
    }

    fn annotate(&self) {
        let mut object = self.volumes.object(VOLUME).unwrap();
        object.metadata.annotations = Some([(FORMATTED_ANNOTATION.to_string(), "true".to_string())].into());
        self.volumes.put(object);
    }

    /// `NODE` is a `Client` of the volume, up on this node, with its entry on the
    /// object; the calls of the set-up are forgotten.
    async fn client_up(&self) {
        let mut object = volume_object(VOLUME, MINOR, 7000);
        object.spec.nodes.push(member(NODE, 3, NodeKind::Client));
        self.volumes.put(object);
        self.svc.agent.reconcile_one(VOLUME).await.unwrap();
        assert!(self.sim.up.load(SeqCst));
        assert!(self.volumes.entry_of(VOLUME, NODE).is_some());
        self.volumes.calls.lock().unwrap().clear();
        self.skip.store(self.runner.calls().len(), SeqCst);
    }

    /// The files of a `Replica` copy are in place, so that a pass has nothing to create.
    fn replica_files(&self) {
        let object = self.volumes.object(VOLUME).unwrap();
        let spec = to_volume_spec(VOLUME, &object.spec).unwrap();
        for dir in [&self.paths.volumes_dir, &self.paths.res_dir, &self.paths.dev_dir] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(self.paths.image(VOLUME), "").unwrap();
        fs::write(self.paths.ready_marker(VOLUME), "").unwrap();
        let text = resource::render(&spec, &self.paths.device_link(VOLUME));
        fs::write(self.paths.res_file(VOLUME), &text).unwrap();
        fs::write(self.paths.applied_res(VOLUME), &text).unwrap();
        std::os::unix::fs::symlink("/dev/loop7", self.paths.device_link(VOLUME)).unwrap();
        self.sim.up.store(true, SeqCst);
    }

    fn device_source(&self) -> String {
        self.dev_dir.join(format!("drbd{MINOR}")).to_string_lossy().into_owned()
    }

    async fn stage(&self) -> Result<(), Status> {
        self.svc
            .node_stage_volume(Request::new(self.stage_request()))
            .await
            .map(|_| ())
    }

    fn stage_request(&self) -> NodeStageVolumeRequest {
        NodeStageVolumeRequest {
            volume_id: VOLUME.to_string(),
            staging_target_path: self.staging.to_string_lossy().into_owned(),
            volume_capability: Some(capability(&[])),
            ..Default::default()
        }
    }

    async fn unstage(&self) -> Result<(), Status> {
        self.svc
            .node_unstage_volume(Request::new(NodeUnstageVolumeRequest {
                volume_id: VOLUME.to_string(),
                staging_target_path: self.staging.to_string_lossy().into_owned(),
            }))
            .await
            .map(|_| ())
    }

    fn publish_request(&self) -> NodePublishVolumeRequest {
        NodePublishVolumeRequest {
            volume_id: VOLUME.to_string(),
            staging_target_path: self.staging.to_string_lossy().into_owned(),
            target_path: self.target.to_string_lossy().into_owned(),
            volume_capability: Some(capability(&[])),
            ..Default::default()
        }
    }

    async fn publish(&self, readonly: bool) -> Result<(), Status> {
        let request = NodePublishVolumeRequest {
            readonly,
            ..self.publish_request()
        };
        self.svc.node_publish_volume(Request::new(request)).await.map(|_| ())
    }

    async fn unpublish(&self) -> Result<(), Status> {
        self.svc
            .node_unpublish_volume(Request::new(NodeUnpublishVolumeRequest {
                volume_id: VOLUME.to_string(),
                target_path: self.target.to_string_lossy().into_owned(),
            }))
            .await
            .map(|_| ())
    }

    /// The staging path is mounted, as an earlier stage left it.
    fn stage_mount(&self) {
        fs::create_dir_all(&self.staging).unwrap();
        self.sim
            .add_mount(&self.device_source(), &self.staging.to_string_lossy(), "/", false, None);
    }
}

fn capability(flags: &[&str]) -> VolumeCapability {
    VolumeCapability {
        access_mode: Some(AccessMode {
            mode: Mode::SingleNodeWriter as i32,
        }),
        access_type: Some(AccessType::Mount(MountVolume {
            fs_type: "ext4".to_string(),
            mount_flags: flags.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        })),
    }
}

fn code<T: std::fmt::Debug>(result: Result<T, Status>) -> Code {
    result.unwrap_err().code()
}

fn message<T: std::fmt::Debug>(result: Result<T, Status>) -> String {
    result.unwrap_err().message().to_string()
}

// ---- the settings and the small calls ----

#[test]
fn production_settings() {
    let e = env();
    let svc = NodeService::new(e.svc.agent.clone(), Arc::new(FakeNodes(Ok(cluster(&[GIB; 3])))));
    assert_eq!(svc.mountinfo, Path::new("/proc/self/mountinfo"));
    assert_eq!(svc.dev_dir, Path::new("/dev"));
    assert!(
        !svc.allow_regular_files,
        "a regular file is never a device in production"
    );
    assert_eq!(svc.stage_wait, Duration::from_secs(30));
    assert_eq!(svc.poll, Duration::from_millis(500));
}

#[tokio::test]
async fn node_get_info_names_the_node_and_its_topology() {
    let e = env();
    let info = e
        .svc
        .node_get_info(Request::new(NodeGetInfoRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.node_id, NODE);
    assert_eq!(info.max_volumes_per_node, 0);
    assert_eq!(info.accessible_topology.unwrap().segments[TOPOLOGY_KEY], NODE);
}

#[tokio::test]
async fn node_get_capabilities_has_stage_unstage() {
    let e = env();
    let caps = e
        .svc
        .node_get_capabilities(Request::new(NodeGetCapabilitiesRequest {}))
        .await
        .unwrap()
        .into_inner()
        .capabilities;
    assert_eq!(caps.len(), 1);
    let Some(node_service_capability::Type::Rpc(rpc_cap)) = &caps[0].r#type else {
        panic!("not an rpc capability");
    };
    assert_eq!(rpc_cap.r#type, rpc::Type::StageUnstageVolume as i32);
}

#[tokio::test]
async fn the_other_node_calls_are_unimplemented() {
    let e = env();
    let err = e
        .svc
        .node_get_volume_stats(Request::new(NodeGetVolumeStatsRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
    let err = e
        .svc
        .node_expand_volume(Request::new(NodeExpandVolumeRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
}

// ---- NodeStageVolume: the table of the module doc ----

#[tokio::test(start_paused = true)]
async fn stage_row1_is_unavailable_until_the_agent_is_ready() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.svc.agent.ready.store(false, SeqCst);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(
        e.volumes.calls().is_empty(),
        "nothing is read before the agent is ready"
    );
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row2_rejects_bad_arguments_before_touching_anything() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    let good = e.stage_request();
    let mut bad = Vec::new();
    for volume_id in ["", "Bad_Name", "all", "-x"] {
        bad.push(NodeStageVolumeRequest {
            volume_id: volume_id.into(),
            ..good.clone()
        });
    }
    for staging in ["", "relative/path"] {
        bad.push(NodeStageVolumeRequest {
            staging_target_path: staging.into(),
            ..good.clone()
        });
    }
    bad.push(NodeStageVolumeRequest {
        volume_capability: None,
        ..good.clone()
    });
    bad.push(NodeStageVolumeRequest {
        volume_capability: Some(VolumeCapability {
            access_mode: None,
            access_type: Some(AccessType::Block(Default::default())),
        }),
        ..good.clone()
    });
    bad.push(NodeStageVolumeRequest {
        volume_capability: Some(VolumeCapability {
            access_mode: None,
            access_type: None,
        }),
        ..good.clone()
    });
    let mut xfs = capability(&[]);
    xfs.access_type = Some(AccessType::Mount(MountVolume {
        fs_type: "xfs".into(),
        ..Default::default()
    }));
    bad.push(NodeStageVolumeRequest {
        volume_capability: Some(xfs),
        ..good.clone()
    });
    for (i, request) in bad.into_iter().enumerate() {
        let result = e.svc.node_stage_volume(Request::new(request)).await;
        assert_eq!(code(result), Code::InvalidArgument, "request {i}");
    }
    assert!(e.volumes.calls().is_empty());
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row2_an_empty_fs_type_is_accepted() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    let mut cap = capability(&[]);
    cap.access_type = Some(AccessType::Mount(MountVolume::default()));
    let request = NodeStageVolumeRequest {
        volume_capability: Some(cap),
        ..e.stage_request()
    };
    e.svc.node_stage_volume(Request::new(request)).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn stage_row3_a_missing_volume_is_not_found() {
    let e = env();
    assert_eq!(code(e.stage().await), Code::NotFound);
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row3_a_deleting_volume_is_not_found() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.volumes.delete(VOLUME).await.unwrap();
    assert!(e.volumes.object(VOLUME).unwrap().metadata.deletion_timestamp.is_some());
    assert_eq!(code(e.stage().await), Code::NotFound);
    assert_eq!(e.patches(), 0, "a deleting volume gets no new member");
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row4_a_staged_volume_is_ok_and_runs_nothing() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    let before = e.members();
    e.stage().await.unwrap();
    assert!(e.runner_calls().is_empty(), "{:?}", e.runner_calls());
    assert_eq!(e.patches(), 0);
    assert_eq!(e.members(), before);
}

#[tokio::test(start_paused = true)]
async fn stage_row4_another_source_at_the_staging_path_is_failed_precondition() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    fs::create_dir_all(&e.staging).unwrap();
    e.sim
        .add_mount("/dev/other", &e.staging.to_string_lossy(), "/", false, None);
    assert_eq!(code(e.stage().await), Code::FailedPrecondition);
    assert_eq!(e.patches(), 0);
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row5_a_node_that_is_not_a_member_becomes_a_client() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.stage().await.unwrap();
    let members = e.members();
    assert!(members.contains(&(NODE.to_string(), NodeKind::Client)), "{members:?}");
    assert_eq!(members.len(), 4);
    let stored = e.volumes.object(VOLUME).unwrap().spec.nodes;
    let own = stored.iter().find(|n| n.name == NODE).unwrap();
    assert_eq!(
        (own.node_id, own.address.as_str()),
        (3, "192.0.2.4"),
        "taken from the node map"
    );
}

#[tokio::test(start_paused = true)]
async fn stage_row5_a_replica_keeps_its_membership() {
    let e = env_for("n1");
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.replica_files();
    e.set_device(Dev::Ext4);
    let before = e.volumes.object(VOLUME).unwrap().spec.nodes;
    e.stage().await.unwrap();
    assert_eq!(e.volumes.object(VOLUME).unwrap().spec.nodes, before);
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn stage_row5_a_node_missing_from_the_node_map_is_failed_precondition() {
    let e = env_for("zz");
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("zz"), "{}", status.message());
    assert_eq!(e.patches(), 0);
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row5_no_room_is_resource_exhausted() {
    let e = env();
    let mut object = volume_object(VOLUME, MINOR, 7000);
    object.spec.max_peers = 2; // three members already
    e.volumes.put(object);
    assert_eq!(code(e.stage().await), Code::ResourceExhausted);
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn stage_row5_a_taken_node_id_is_resource_exhausted() {
    let e = env();
    let mut object = volume_object(VOLUME, MINOR, 7000);
    // n3 holds the node id the node map gives to n4.
    object.spec.nodes[2].node_id = 3;
    e.volumes.put(object);
    assert_eq!(code(e.stage().await), Code::ResourceExhausted);
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn stage_row5_a_taken_address_is_resource_exhausted() {
    let e = env();
    let mut object = volume_object(VOLUME, MINOR, 7000);
    // n3 holds the address the node map gives to n4.
    object.spec.nodes[2].address = "192.0.2.4".to_string();
    e.volumes.put(object);
    assert_eq!(code(e.stage().await), Code::ResourceExhausted);
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn stage_row5_an_unreadable_node_map_is_unavailable() {
    let mut e = env();
    e.svc.nodes = Arc::new(FakeNodes(Err(ApiError::Other("the API server is down".into()))));
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn stage_row6_a_refused_volume_is_failed_precondition() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    // Another volume with the same minor and port: the agent refuses both.
    e.volumes.put(volume_object("other", MINOR, 7000));
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("refuses"), "{}", status.message());
    assert!(e.device_commands().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row6_a_pass_that_fails_is_unavailable() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.fail("drbdadm up", 1, "cannot configure");
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(e.device_commands().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage_row7_waits_for_a_usable_device() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.sim.usable.store(false, SeqCst);
    e.sim.usable_at_status_call.store(12, SeqCst);
    e.stage().await.unwrap();
    assert!(
        e.sim.status_calls.load(SeqCst) >= 12,
        "the status was polled until the resource became usable"
    );
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn stage_row7_a_device_that_never_becomes_usable_is_unavailable_and_unopened() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.sim.usable.store(false, SeqCst);
    let text = message(e.stage().await);
    assert!(text.contains("not usable"), "{text}");
    assert!(e.device_commands().is_empty(), "{:?}", e.device_commands());
}

#[tokio::test(start_paused = true)]
async fn stage_row7_the_pass_is_repeated_while_it_reports_progress() {
    let mut e = env();
    e.svc.stage_wait = Duration::from_secs(3);
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.sim.usable.store(false, SeqCst);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    // The first pass configured the resource (progress), the second found nothing to do.
    assert_eq!(e.volumes.lists.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn stage_row8_signature_empty_runs_mkfs_once_then_mount() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    e.stage().await.unwrap();
    let device = e.device_source();
    assert_eq!(
        e.calls(&["mkfs.ext4", "mount"]),
        [
            format!("mkfs.ext4 -q -E nodiscard {device}"),
            format!("mount -t ext4 {device} {}", e.staging.display())
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn stage_row8_signature_ext4_does_not_run_mkfs() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.stage().await.unwrap();
    assert!(e.calls(&["mkfs.ext4"]).is_empty());
    assert_eq!(e.calls(&["mount"]).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn stage_row8_an_ambiguous_signature_runs_neither_mkfs_nor_mount() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ambiguous);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(e.calls(&["mkfs.ext4", "mount"]).is_empty());
    assert!(!e.annotated());
}

#[tokio::test(start_paused = true)]
async fn stage_row8_a_device_that_cannot_be_opened_runs_neither_mkfs_nor_mount() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Missing);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(e.calls(&["blkid", "mkfs.ext4", "mount"]).is_empty());
    assert!(!e.annotated());
}

#[tokio::test(start_paused = true)]
async fn stage_row8_another_filesystem_is_failed_precondition_and_is_not_formatted() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Other("xfs"));
    let text = message(e.stage().await);
    assert!(text.contains("xfs"), "{text}");
    assert!(e.calls(&["mkfs.ext4", "mount"]).is_empty());
}

#[test]
fn stage_row8_a_device_open_on_another_node_is_unavailable_and_kubelet_retries() {
    let open = |in_use| MountError::Open {
        device: "/dev/drbd1000".into(),
        in_use,
        source: io::Error::other("Wrong medium type"),
    };
    // A pod move: the old node still holds the device. A final code would make every retry
    // join, fail and tear down again.
    let status = plan_format(Err(open(true)), false).unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(status.message().contains("another node"), "{}", status.message());
    assert_eq!(
        plan_format(Err(open(false)), false).unwrap_err().code(),
        Code::Unavailable
    );
}

#[tokio::test(start_paused = true)]
async fn stage_row9_creates_the_staging_directory_and_mounts_the_drbd_device() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    assert!(!e.staging.exists());
    e.stage().await.unwrap();
    assert!(e.staging.is_dir());
    let mounted = e.sim.mounted_at(&e.staging);
    assert_eq!(mounted.len(), 1);
    // Rule C: the device is `<dev_dir>/drbd<minor>` of the spec, not a link.
    assert_eq!(mounted[0].source, e.device_source());
    assert!(mounted[0].source.ends_with("/drbd1000"));
}

#[tokio::test(start_paused = true)]
async fn stage_row9_a_failed_mount_is_failed_precondition_with_stderr() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.fail("mount -t", 32, "mount: wrong fs type, bad superblock\n");
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("bad superblock"), "{}", status.message());
}

// ---- rule A: when mkfs runs ----

#[tokio::test(start_paused = true)]
async fn rule_a_the_annotation_is_set_after_mkfs() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    assert!(!e.annotated());
    e.stage().await.unwrap();
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1);
    assert!(e.annotated());
}

#[tokio::test(start_paused = true)]
async fn rule_a_an_ext4_device_without_the_annotation_gets_it() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.stage().await.unwrap();
    assert!(e.calls(&["mkfs.ext4"]).is_empty());
    assert!(e.annotated());
}

#[tokio::test(start_paused = true)]
async fn rule_a_an_ext4_device_with_the_annotation_is_mounted_without_a_patch_for_it() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.annotate();
    e.set_device(Dev::Ext4);
    e.stage().await.unwrap();
    // One patch: the membership. Nothing for the annotation.
    assert_eq!(e.patches(), 1);
    assert!(e.annotated());
}

#[tokio::test(start_paused = true)]
async fn rule_a_an_empty_device_with_the_annotation_is_not_formatted() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.annotate();
    e.set_device(Dev::Empty);
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(
        status.message().contains("refusing to format again"),
        "{}",
        status.message()
    );
    assert!(e.calls(&["mkfs.ext4", "mount"]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn rule_a_a_failed_annotation_after_mkfs_fails_the_call_and_the_retry_never_formats_again() {
    let e = env();
    // The node is a member already, so the only patch is the annotation.
    e.client_up().await;
    e.set_device(Dev::Empty);
    *e.volumes.conflicts.lock().unwrap() = 100;
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1);
    assert!(!e.annotated());
    assert!(
        e.calls(&["mount"]).is_empty(),
        "not mounted before the annotation is on the object"
    );

    // The API server is back; the device now holds a filesystem.
    *e.volumes.conflicts.lock().unwrap() = 0;
    e.stage().await.unwrap();
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1, "no second mkfs");
    assert!(e.annotated());
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

#[test]
fn rule_a_the_decision_table() {
    use Format::*;
    let ambiguous = || MountError::AmbiguousSignature {
        device: "d".into(),
        detail: "x".into(),
    };
    let open = || MountError::Open {
        device: "d".into(),
        in_use: false,
        source: io::Error::other("x"),
    };
    let not_block = || MountError::NotABlockDevice { device: "d".into() };
    let io_error = || MountError::Io {
        action: "read",
        path: "p".into(),
        source: io::Error::other("x"),
    };
    for annotated in [false, true] {
        let formats = |s| plan_format(s, annotated).is_ok_and(|f| f == FormatThenMark);
        assert_eq!(formats(Ok(Signature::Empty)), !annotated);
        assert!(!formats(Ok(Signature::Ext4)));
        assert!(!formats(Ok(Signature::Other("xfs".into()))));
        assert!(!formats(Err(ambiguous())));
        assert!(!formats(Err(open())));
        assert!(!formats(Err(not_block())));
        assert!(!formats(Err(io_error())));
    }
    assert_eq!(plan_format(Ok(Signature::Ext4), true).unwrap(), Mount);
    assert_eq!(plan_format(Ok(Signature::Ext4), false).unwrap(), MarkThenMount);
}

// ---- rule B: the node is visible before it opens the volume ----

#[tokio::test(start_paused = true)]
async fn rule_b_a_node_without_an_entry_on_the_object_does_not_open_the_volume() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    // The status write of `reconcile_one` does not reach the object.
    e.volumes.drop_status_writes.store(true, SeqCst);
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(status.message().contains("status.nodes"), "{}", status.message());
    assert!(e.device_commands().is_empty(), "{:?}", e.device_commands());
    assert!(!e.annotated());
}

// ---- rule D: every call is safe to repeat ----

#[tokio::test(start_paused = true)]
async fn rule_d_stage_twice_formats_and_mounts_once() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    e.stage().await.unwrap();
    let commands = e.runner_calls().len();
    let patches = e.patches();
    e.stage().await.unwrap();
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1);
    assert_eq!(e.calls(&["mount"]).len(), 1);
    assert_eq!(e.runner_calls().len(), commands, "the second call runs no command");
    assert_eq!(e.patches(), patches);
}

#[tokio::test(start_paused = true)]
async fn rule_d_a_stage_that_stopped_before_the_mount_is_finished_by_the_retry() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    // The first call formats and marks, then the mount fails.
    e.fail("mount -t", 32, "busy");
    assert_eq!(code(e.stage().await), Code::FailedPrecondition);
    e.sim.failing.lock().unwrap().clear();
    e.stage().await.unwrap();
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1);
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

// ---- rule E: locking ----

#[tokio::test(start_paused = true)]
async fn rule_e_the_critical_section_of_the_stage_waits_for_the_volume_lock() {
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Ext4);
    let guard = e.svc.agent.locks.lock(VOLUME).await;
    let svc = e.svc.clone();
    let staging = e.staging.clone();
    let task = tokio::spawn(async move { svc.stage_locked(VOLUME.to_string(), staging, None).await });
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!task.is_finished(), "the stage ran while the volume lock was held");
    assert!(e.device_commands().is_empty());
    drop(guard);
    task.await.unwrap().unwrap();
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn rule_e_unstage_waits_for_the_volume_lock() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    let guard = e.svc.agent.locks.lock(VOLUME).await;
    let svc = e.svc.clone();
    let staging = e.staging.clone();
    let task = tokio::spawn(async move { svc.unstage_locked(VOLUME.to_string(), staging).await });
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!task.is_finished(), "the unstage ran while the volume lock was held");
    assert!(e.runner_calls().is_empty());
    drop(guard);
    task.await.unwrap().unwrap();
    assert!(e.sim.mounted_at(&e.staging).is_empty());
}

#[tokio::test(start_paused = true)]
async fn rule_e_the_pass_of_the_stage_does_not_deadlock_with_its_own_lock() {
    // `reconcile_one` takes the volume lock itself; the whole stage, which
    // calls it before its own critical section, finishes.
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    tokio::time::timeout(Duration::from_secs(60), e.stage())
        .await
        .unwrap()
        .unwrap();
}

// ---- rule H: mount flags, read-only ----

#[tokio::test(start_paused = true)]
async fn rule_h_valid_mount_flags_reach_the_device_mount() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    let request = NodeStageVolumeRequest {
        volume_capability: Some(capability(&["noatime", "commit=60", "errors=remount-ro"])),
        ..e.stage_request()
    };
    e.svc.node_stage_volume(Request::new(request)).await.unwrap();
    let mounted = e.sim.mounted_at(&e.staging);
    assert_eq!(
        mounted[0].options.as_deref(),
        Some("noatime,commit=60,errors=remount-ro")
    );
    assert_eq!(
        e.calls(&["mount"]),
        [format!(
            "mount -t ext4 -o noatime,commit=60,errors=remount-ro {} {}",
            e.device_source(),
            e.staging.display()
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn rule_h_an_unsafe_mount_flag_is_invalid_argument_and_nothing_runs() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    for flag in ["", "a b", "x;rm", "a,b", "$(id)", "a/b", "ro\n", "é"] {
        let request = NodeStageVolumeRequest {
            volume_capability: Some(capability(&["noatime", flag])),
            ..e.stage_request()
        };
        let status = e.svc.node_stage_volume(Request::new(request)).await.unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument, "flag {flag:?}");
    }
    assert!(e.runner_calls().is_empty());
    assert!(e.volumes.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn rule_h_a_stage_with_flags_twice_mounts_once() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    let request = || NodeStageVolumeRequest {
        volume_capability: Some(capability(&["noatime"])),
        ..e.stage_request()
    };
    e.svc.node_stage_volume(Request::new(request())).await.unwrap();
    e.svc.node_stage_volume(Request::new(request())).await.unwrap();
    assert_eq!(e.calls(&["mount"]).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn rule_h_the_options_mount_is_idempotent_and_busy_like_mount_device() {
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Ext4);
    // The critical section directly: a mount of another source at the path is refused.
    fs::create_dir_all(&e.staging).unwrap();
    e.sim
        .add_mount("/dev/other", &e.staging.to_string_lossy(), "/", false, None);
    let status = e
        .svc
        .stage_locked(VOLUME.to_string(), e.staging.clone(), Some("noatime".into()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(e.calls(&["mount"]).is_empty());
}

#[tokio::test]
async fn rule_h_a_bind_of_the_other_mode_is_already_exists() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(true).await.unwrap();
    assert_eq!(code(e.publish(false).await), Code::AlreadyExists);
    assert!(
        e.sim.mounted_at(&e.target)[0].read_only,
        "the existing bind is left as it was"
    );
}

// ---- NodePublishVolume / NodeUnpublishVolume ----

#[tokio::test]
async fn publish_rejects_bad_arguments() {
    let e = env();
    e.stage_mount();
    let good = e.publish_request();
    let bad = [
        NodePublishVolumeRequest {
            volume_id: "".into(),
            ..good.clone()
        },
        NodePublishVolumeRequest {
            volume_id: "Bad_Name".into(),
            ..good.clone()
        },
        NodePublishVolumeRequest {
            staging_target_path: "".into(),
            ..good.clone()
        },
        NodePublishVolumeRequest {
            target_path: "".into(),
            ..good.clone()
        },
        NodePublishVolumeRequest {
            target_path: "relative".into(),
            ..good.clone()
        },
        NodePublishVolumeRequest {
            volume_capability: None,
            ..good.clone()
        },
    ];
    for (i, request) in bad.into_iter().enumerate() {
        let result = e.svc.node_publish_volume(Request::new(request)).await;
        assert_eq!(code(result), Code::InvalidArgument, "request {i}");
    }
    assert!(e.runner_calls().is_empty());
    assert!(!e.target.exists());
}

#[tokio::test]
async fn publish_without_a_mount_at_the_staging_path_is_failed_precondition() {
    let e = env();
    assert_eq!(code(e.publish(false).await), Code::FailedPrecondition);
    assert!(
        !e.target.exists(),
        "the target is not created for a volume that is not staged"
    );
    assert!(e.runner_calls().is_empty());
}

#[tokio::test]
async fn publish_bind_mounts_the_staged_volume_into_the_target() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(false).await.unwrap();
    assert!(e.target.is_dir());
    let bound = e.sim.mounted_at(&e.target);
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].source, e.device_source());
    assert!(!bound[0].read_only);
    assert_eq!(
        e.calls(&["mount"]),
        [format!("mount --bind {} {}", e.staging.display(), e.target.display())]
    );
}

#[tokio::test]
async fn publish_read_only_remounts_the_bind_read_only() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(true).await.unwrap();
    assert!(e.sim.mounted_at(&e.target)[0].read_only);
    assert_eq!(e.calls(&["mount -o remount,bind,ro"]).len(), 1);
}

#[tokio::test]
async fn publish_twice_is_ok_and_mounts_once() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(false).await.unwrap();
    let commands = e.runner_calls().len();
    e.publish(false).await.unwrap();
    assert_eq!(e.runner_calls().len(), commands);
    assert_eq!(e.sim.mounted_at(&e.target).len(), 1);
}

#[tokio::test]
async fn unpublish_rejects_bad_arguments() {
    let e = env();
    for request in [
        NodeUnpublishVolumeRequest {
            volume_id: "".into(),
            target_path: "/x".into(),
        },
        NodeUnpublishVolumeRequest {
            volume_id: VOLUME.into(),
            target_path: "".into(),
        },
        NodeUnpublishVolumeRequest {
            volume_id: VOLUME.into(),
            target_path: "relative".into(),
        },
    ] {
        let result = e.svc.node_unpublish_volume(Request::new(request)).await;
        assert_eq!(code(result), Code::InvalidArgument);
    }
}

#[tokio::test]
async fn unpublish_unmounts_the_target_and_removes_the_empty_directory() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(false).await.unwrap();
    e.unpublish().await.unwrap();
    assert!(e.sim.mounted_at(&e.target).is_empty());
    assert!(!e.target.exists());
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1, "the staged volume stays");
}

#[tokio::test]
async fn unpublish_twice_and_of_nothing_is_ok_without_commands() {
    let e = env();
    e.unpublish().await.unwrap();
    assert!(e.runner_calls().is_empty());
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(false).await.unwrap();
    e.unpublish().await.unwrap();
    let commands = e.runner_calls().len();
    e.unpublish().await.unwrap();
    assert_eq!(e.runner_calls().len(), commands);
}

#[tokio::test]
async fn unpublish_leaves_a_directory_that_is_not_empty() {
    let e = env();
    fs::create_dir_all(&e.target).unwrap();
    fs::write(e.target.join("file"), "data").unwrap();
    e.unpublish().await.unwrap();
    assert!(e.target.join("file").exists());
}

#[tokio::test]
async fn unpublish_with_a_failing_umount_is_failed_precondition() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.publish(false).await.unwrap();
    e.fail("umount", 32, "target is busy");
    assert_eq!(code(e.unpublish().await), Code::FailedPrecondition);
    assert!(e.target.exists());
}

// ---- NodeUnstageVolume ----

#[tokio::test]
async fn unstage_rejects_bad_arguments() {
    let e = env();
    for request in [
        NodeUnstageVolumeRequest {
            volume_id: "".into(),
            staging_target_path: "/x".into(),
        },
        NodeUnstageVolumeRequest {
            volume_id: "Bad_Name".into(),
            staging_target_path: "/x".into(),
        },
        NodeUnstageVolumeRequest {
            volume_id: VOLUME.into(),
            staging_target_path: "".into(),
        },
        NodeUnstageVolumeRequest {
            volume_id: VOLUME.into(),
            staging_target_path: "relative".into(),
        },
    ] {
        let result = e.svc.node_unstage_volume(Request::new(request)).await;
        assert_eq!(code(result), Code::InvalidArgument);
    }
    assert!(e.runner_calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn unstage_of_a_client_unmounts_takes_the_resource_down_and_removes_the_membership() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    assert!(e.paths.res_file(VOLUME).exists());
    e.unstage().await.unwrap();

    assert!(e.sim.mounted_at(&e.staging).is_empty());
    assert_eq!(e.calls(&["drbdadm down"]), ["drbdadm down pv1"]);
    assert!(!e.paths.res_file(VOLUME).exists(), "the resource file is removed");
    assert!(e.volumes.entry_of(VOLUME, NODE).is_none());
    assert!(!e.members().iter().any(|(name, _)| name == NODE), "{:?}", e.members());
    assert_eq!(e.members().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn rule_f_the_resource_goes_down_before_the_entry_and_the_membership_change() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.unstage().await.unwrap();
    // When `drbdadm down` ran, the object still showed this node as a member with its entry.
    assert_eq!(*e.sim.at_down.lock().unwrap(), Some((true, true)));
    // The entry goes before the membership.
    let calls = e.volumes.calls();
    let removed = calls
        .iter()
        .position(|c| c == &format!("status {VOLUME} {NODE} removed"))
        .unwrap();
    let edited = calls.iter().position(|c| c == &format!("patch {VOLUME}")).unwrap();
    assert!(removed < edited, "{calls:?}");
}

#[tokio::test(start_paused = true)]
async fn rule_f_a_failed_down_keeps_the_membership_and_the_entry() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.fail("drbdadm down", 11, "Device '1000' is held open by someone");
    let status = e.unstage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("held open"), "{}", status.message());
    assert!(e.members().contains(&(NODE.to_string(), NodeKind::Client)));
    assert!(e.volumes.entry_of(VOLUME, NODE).is_some());
    assert!(e.paths.res_file(VOLUME).exists(), "no local file is removed");
    assert_eq!(e.patches(), 0);
    assert!(
        !e.volumes.calls().iter().any(|c| c.contains("removed")),
        "{:?}",
        e.volumes.calls()
    );
}

#[tokio::test(start_paused = true)]
async fn rule_f_an_entry_that_cannot_be_removed_keeps_the_membership() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    // Another field manager co-owns the entry and the status patch is refused.
    e.volumes.co_owned.store(true, SeqCst);
    e.volumes.fail_status_patch.store(true, SeqCst);
    assert_eq!(code(e.unstage().await), Code::Unavailable);
    assert!(e.members().contains(&(NODE.to_string(), NodeKind::Client)));
    assert_eq!(e.patches(), 0);
    // The retry, with the API server healthy, finishes.
    e.volumes.fail_status_patch.store(false, SeqCst);
    e.unstage().await.unwrap();
    assert!(!e.members().iter().any(|(name, _)| name == NODE));
}

#[tokio::test(start_paused = true)]
async fn unstage_with_a_failing_umount_changes_nothing() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.fail("umount", 32, "target is busy");
    let status = e.unstage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("busy"), "{}", status.message());
    assert!(
        e.volumes.calls().is_empty(),
        "the object is not read, let alone changed"
    );
    assert!(e.calls(&["drbdadm"]).is_empty());
    assert!(e.members().contains(&(NODE.to_string(), NodeKind::Client)));
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn unstage_on_a_witness_keeps_the_membership_and_runs_no_down() {
    let e = env_for("n3");
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.unstage().await.unwrap();
    assert!(e.sim.mounted_at(&e.staging).is_empty());
    assert!(e.calls(&["drbdadm"]).is_empty());
    assert!(e.members().contains(&("n3".to_string(), NodeKind::Witness)));
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn unstage_on_a_replica_keeps_the_membership_and_runs_no_down() {
    let e = env_for("n1");
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.replica_files();
    e.stage_mount();
    e.unstage().await.unwrap();
    assert!(e.calls(&["drbdadm"]).is_empty());
    assert!(e.members().contains(&("n1".to_string(), NodeKind::Replica)));
    assert!(e.paths.image(VOLUME).exists(), "the copy stays");
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn unstage_on_a_node_that_is_not_a_member_does_nothing_but_the_unmount() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.stage_mount();
    e.unstage().await.unwrap();
    assert!(e.calls(&["drbdadm"]).is_empty());
    assert_eq!(e.patches(), 0);
    assert_eq!(e.members().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn unstage_of_a_missing_volume_is_ok() {
    let e = env();
    e.stage_mount();
    e.unstage().await.unwrap();
    assert!(e.sim.mounted_at(&e.staging).is_empty());
}

#[tokio::test(start_paused = true)]
async fn unstage_twice_is_ok_and_runs_nothing_the_second_time() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.unstage().await.unwrap();
    let commands = e.runner_calls().len();
    let calls = e.volumes.calls().len();
    e.unstage().await.unwrap();
    assert_eq!(e.runner_calls().len(), commands);
    assert_eq!(e.volumes.calls().len(), calls + 1, "only the read of the object");
}

// ---- rule I: the mapping of the mounter's errors ----

#[test]
fn rule_i_mount_errors_map_to_codes() {
    let io_error = || io::Error::other("x");
    let cases = [
        (
            MountError::Open {
                device: "d".into(),
                in_use: true,
                source: io_error(),
            },
            Code::Unavailable,
        ),
        (
            MountError::Open {
                device: "d".into(),
                in_use: false,
                source: io_error(),
            },
            Code::Unavailable,
        ),
        (
            MountError::AmbiguousSignature {
                device: "d".into(),
                detail: "x".into(),
            },
            Code::Unavailable,
        ),
        (MountError::NotABlockDevice { device: "d".into() }, Code::Unavailable),
        (
            MountError::Io {
                action: "read",
                path: "p".into(),
                source: io_error(),
            },
            Code::Unavailable,
        ),
        (
            MountError::WrongFilesystem {
                device: "d".into(),
                found: "xfs".into(),
            },
            Code::FailedPrecondition,
        ),
        (
            MountError::Busy {
                target: "t".into(),
                source_mounted: "s".into(),
            },
            Code::FailedPrecondition,
        ),
        (
            MountError::StillMounted {
                target: "t".into(),
                attempts: 8,
            },
            Code::FailedPrecondition,
        ),
        (
            MountError::Command {
                command: "mount".into(),
                code: Some(32),
                stderr: "boom".into(),
            },
            Code::FailedPrecondition,
        ),
        (MountError::ModeMismatch { target: "t".into() }, Code::AlreadyExists),
    ];
    for (error, expected) in cases {
        let text = error.to_string();
        let status = mount_status(error);
        assert_eq!(status.code(), expected, "{text}");
        assert!(status.message().contains(&text), "{} / {text}", status.message());
    }
}

// ---- X5: mount flags: allow-list and no echo ----

#[test]
fn mount_options_accept_the_allow_list() {
    assert_eq!(mount_options(&[]).unwrap(), None);
    let allowed = [
        "noatime",
        "nodiratime",
        "relatime",
        "strictatime",
        "lazytime",
        "discard",
        "nodiscard",
        "ro",
        "rw",
        "nosuid",
        "nodev",
        "noexec",
        "sync",
        "dirsync",
        "commit=0",
        "commit=60",
        "commit=0123456789",
        "data=ordered",
        "barrier=1",
        "errors=remount-ro",
    ];
    for flag in allowed {
        assert_eq!(
            mount_options(&[flag.to_string()]).unwrap().as_deref(),
            Some(flag),
            "{flag}"
        );
    }
    let all: Vec<String> = allowed.iter().map(|f| f.to_string()).collect();
    assert_eq!(mount_options(&all).unwrap().unwrap(), allowed.join(","));
}

#[test]
fn mount_options_reject_everything_else() {
    for bad in [
        "loop",
        "journal_dev=8",
        "errors=panic",
        "X-mount.mkdir",
        "offset=1",
        "",
        "a,b",
        "noatime,ro",
        " ",
        "a;b",
        "`x`",
        "a\u{0}b",
        "a/b",
        "commit=",
        "commit=x",
        "commit=6 0",
        "commit=-1",
        "Commit=1",
        "NOATIME",
        "x-a.b_c",
        "data=journal",
        // They weaken crash and error handling of the tenant's filesystem.
        "data=writeback",
        "barrier=0",
        "errors=continue",
        "barrier=2",
        "errors=remount-ro,loop",
    ] {
        let status = mount_options(&["noatime".into(), bad.into()]).unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument, "{bad:?}");
    }
}

#[test]
fn a_rejected_flag_is_named_in_the_error_and_an_empty_one_by_position() {
    let status = mount_options(&["noatime".into(), "loop".into()]).unwrap_err();
    assert!(status.message().contains("mount_flags[1]"), "{}", status.message());
    assert!(status.message().contains("loop"), "{}", status.message());
    let status = mount_options(&["".into()]).unwrap_err();
    assert!(
        status.message().contains("mount_flags[0] is empty"),
        "{}",
        status.message()
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_mount_with_flags_does_not_print_the_flags() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.fail("mount -t", 32, "mount: wrong fs type, bad superblock\n");
    let request = NodeStageVolumeRequest {
        volume_capability: Some(capability(&["noatime", "commit=77"])),
        ..e.stage_request()
    };
    let status = e.svc.node_stage_volume(Request::new(request)).await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    let text = status.message();
    assert!(text.contains("bad superblock"), "{text}");
    assert!(text.contains("-o <redacted>"), "{text}");
    assert!(!text.contains("noatime") && !text.contains("commit=77"), "{text}");
    // The command that ran did carry them.
    assert!(e.calls(&["mount"])[0].contains("noatime,commit=77"));
}

// ---- X1: a failed stage does not leave a Client membership behind ----

#[test]
fn only_retriable_codes_keep_the_membership() {
    use Code::*;
    for code in [Unavailable, Aborted, DeadlineExceeded, ResourceExhausted, Cancelled] {
        assert!(!is_final(code), "{code:?}");
    }
    for code in [
        Ok,
        Unknown,
        InvalidArgument,
        NotFound,
        AlreadyExists,
        PermissionDenied,
        FailedPrecondition,
        OutOfRange,
        Unimplemented,
        Internal,
        DataLoss,
        Unauthenticated,
    ] {
        assert!(is_final(code), "{code:?}");
    }
}

fn is_client(e: &Env) -> bool {
    e.members().contains(&(NODE.to_string(), NodeKind::Client))
}

/// The stage failed for good after this call added the node: the node is out of the
/// object, its entry is gone, the resource is down, and the error is the original one.
fn assert_rolled_back(e: &Env, status: &Status, code: Code, text: &str) {
    assert_eq!(status.code(), code, "{}", status.message());
    assert!(status.message().contains(text), "{}", status.message());
    assert!(!e.members().iter().any(|(name, _)| name == NODE), "{:?}", e.members());
    assert_eq!(e.members().len(), 3);
    assert!(e.volumes.entry_of(VOLUME, NODE).is_none());
    assert!(!e.sim.up.load(SeqCst), "the local resource was taken down");
    assert_eq!(e.calls(&["drbdadm down"]), ["drbdadm down pv1"]);
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.sim.mounted_at(&e.staging).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_final_error_of_the_pass_removes_the_client_this_call_added() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    // Another volume with the same minor and port: the agent refuses both.
    e.volumes.put(volume_object("other", MINOR, 7000));
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("refuses"), "{}", status.message());
    assert!(!e.members().iter().any(|(name, _)| name == NODE), "{:?}", e.members());
    assert!(e.volumes.entry_of(VOLUME, NODE).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_foreign_filesystem_removes_the_client_this_call_added() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Other("xfs"));
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "xfs");
    assert!(e.logs.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_volume_formatted_before_removes_the_client_this_call_added() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.annotate();
    e.set_device(Dev::Empty);
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "refusing to format again");
    assert!(e.calls(&["mkfs.ext4"]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_mount_removes_the_client_this_call_added() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ext4);
    e.fail("mount -t", 32, "mount: wrong fs type, bad superblock\n");
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "bad superblock");
}

#[tokio::test(start_paused = true)]
async fn a_retriable_error_keeps_the_client_this_call_added() {
    // UNAVAILABLE from the signature check: kubelet retries, the membership stays.
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ambiguous);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(is_client(&e), "{:?}", e.members());
    assert!(e.sim.up.load(SeqCst));
    assert!(e.calls(&["drbdadm down"]).is_empty());
    assert!(e.volumes.entry_of(VOLUME, NODE).is_some());
    // The retry finds the node as a member and does not add it again.
    let patches = e.patches();
    e.set_device(Dev::Ext4);
    e.stage().await.unwrap();
    assert_eq!(e.patches(), patches + 1, "only the annotation");
}

#[tokio::test(start_paused = true)]
async fn a_device_that_never_becomes_usable_keeps_the_client() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.sim.usable.store(false, SeqCst);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(is_client(&e));
    assert!(e.calls(&["drbdadm down"]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_client_of_an_earlier_call_is_removed_by_a_failed_stage() {
    // Replaces `a_client_of_an_earlier_call_is_not_removed_by_a_failed_stage`: the teardown
    // is decided by the state, not by who added the membership.
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Other("xfs"));
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "xfs");
}

#[tokio::test(start_paused = true)]
async fn a_retriable_error_keeps_a_client_of_an_earlier_call() {
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Ambiguous);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(is_client(&e));
    assert!(e.volumes.entry_of(VOLUME, NODE).is_some());
    assert!(e.calls(&["drbdadm down"]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_retriable_error_and_then_a_final_one_leave_no_client() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Ambiguous);
    assert_eq!(code(e.stage().await), Code::Unavailable);
    assert!(is_client(&e));
    assert!(e.calls(&["drbdadm down"]).is_empty());
    // The retry finds the node as a member (`AlreadyMember`) and ends final.
    e.set_device(Dev::Other("xfs"));
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "xfs");
}

/// The volume is mounted at `other` on this node (another pod of the same PVC).
fn mount_elsewhere(e: &Env, other: &str, source: &str) {
    fs::create_dir_all(other).unwrap();
    e.sim.add_mount(source, other, "/", false, None);
}

#[tokio::test(start_paused = true)]
async fn a_client_whose_volume_is_mounted_elsewhere_on_this_node_is_kept() {
    let e = env();
    e.client_up().await;
    let other = e._tmp.path().join("other-staging");
    mount_elsewhere(&e, &other.to_string_lossy(), &e.device_source());
    e.set_device(Dev::Other("xfs"));
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(is_client(&e));
    assert!(e.volumes.entry_of(VOLUME, NODE).is_some());
    assert!(e.sim.up.load(SeqCst));
    assert!(e.calls(&["drbdadm down"]).is_empty());
    assert!(e.logs.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_mount_of_another_device_does_not_keep_the_client() {
    let e = env();
    e.client_up().await;
    let other = e._tmp.path().join("other-staging");
    let foreign = e.dev_dir.join("drbd1001").to_string_lossy().into_owned();
    mount_elsewhere(&e, &other.to_string_lossy(), &foreign);
    e.set_device(Dev::Other("xfs"));
    let status = e.stage().await.unwrap_err();
    assert_rolled_back(&e, &status, Code::FailedPrecondition, "xfs");
}

#[tokio::test(start_paused = true)]
async fn the_teardown_does_nothing_when_the_staging_path_is_a_mount_of_another_source() {
    // Only the staging-path condition applies: the other source is not this volume's device.
    let e = env();
    e.client_up().await;
    fs::create_dir_all(&e.staging).unwrap();
    e.sim
        .add_mount("/dev/other", &e.staging.to_string_lossy(), "/", false, None);
    e.svc
        .teardown_after_failed_stage(VOLUME.to_string(), e.staging.clone())
        .await
        .unwrap();
    assert!(is_client(&e));
    assert!(e.calls(&["drbdadm down"]).is_empty());
    assert!(e.volumes.entry_of(VOLUME, NODE).is_some());
}

#[tokio::test(start_paused = true)]
async fn a_replica_and_a_witness_are_not_touched_by_a_failed_stage() {
    for (node, kind) in [("n1", NodeKind::Replica), ("n3", NodeKind::Witness)] {
        let e = env_for(node);
        e.volumes.put(volume_object(VOLUME, MINOR, 7000));
        if kind == NodeKind::Replica {
            e.replica_files();
        }
        e.set_device(Dev::Other("xfs"));
        let before = e.volumes.object(VOLUME).unwrap().spec.nodes;
        let status = e.stage().await.unwrap_err();
        assert_eq!(status.code(), Code::FailedPrecondition, "{node}");
        assert_eq!(e.volumes.object(VOLUME).unwrap().spec.nodes, before, "{node}");
        assert!(e.calls(&["drbdadm down"]).is_empty(), "{node}");
        assert_eq!(e.patches(), 0, "{node}");
        // Not even an attempt at a teardown (its refusal would be logged).
        assert!(
            e.logs.lock().unwrap().is_empty(),
            "{node}: {:?}",
            e.logs.lock().unwrap()
        );
        if kind == NodeKind::Replica {
            assert!(e.paths.image(VOLUME).exists(), "{node}");
            assert!(e.paths.res_file(VOLUME).exists(), "{node}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_teardown_returns_the_original_error_and_keeps_the_membership() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Other("xfs"));
    e.fail("drbdadm down", 11, "Device '1000' is held open by someone");
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("xfs"), "{}", status.message());
    assert!(!status.message().contains("held open"), "{}", status.message());
    assert!(is_client(&e), "the node stays a member: the next attempt finds it");
    let logs = e.logs.lock().unwrap().clone();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(logs[0].contains("held open") && logs[0].contains("pv1"), "{logs:?}");
    // The next attempt sees a member, fails the same way and, the cleanup working now,
    // tears it down: the decision is by state, not by who added the membership.
    e.sim.failing.lock().unwrap().clear();
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(!is_client(&e), "{:?}", e.members());
    assert!(e.volumes.entry_of(VOLUME, NODE).is_none());
}

#[tokio::test(start_paused = true)]
async fn the_teardown_does_nothing_when_the_staging_path_is_mounted() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.svc
        .teardown_after_failed_stage(VOLUME.to_string(), e.staging.clone())
        .await
        .unwrap();
    assert!(is_client(&e));
    assert!(e.calls(&["drbdadm down"]).is_empty());
    assert_eq!(e.sim.mounted_at(&e.staging).len(), 1);
}

// ---- X2: the annotation is read again right before mkfs ----

fn another_node_acts(e: &Env, act: impl Fn(&FakeVolumes) + Send + 'static) {
    *e.sim.on_blkid.lock().unwrap() = Some(Box::new(act));
}

#[tokio::test(start_paused = true)]
async fn mkfs_does_not_run_when_another_node_annotated_the_volume_after_the_signature_check() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    another_node_acts(&e, |volumes| {
        let mut object = volumes.object(VOLUME).unwrap();
        object.metadata.annotations = Some([(FORMATTED_ANNOTATION.to_string(), "true".to_string())].into());
        volumes.put(object);
    });
    let status = e.stage().await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(e.calls(&["mkfs.ext4", "mount"]).is_empty(), "{:?}", e.device_commands());
    assert!(is_client(&e), "a retriable error keeps the membership");
}

#[tokio::test(start_paused = true)]
async fn mkfs_does_not_run_when_the_volume_vanished_was_deleted_or_replaced_meanwhile() {
    type Change = fn(&FakeVolumes);
    let changes: [(&str, Change); 3] = [
        ("gone", |volumes| {
            volumes.objects.lock().unwrap().remove(VOLUME);
        }),
        ("deleting", |volumes| {
            let mut object = volumes.object(VOLUME).unwrap();
            object.metadata.deletion_timestamp = Some(crate::tests::volumes::time(crate::tests::volumes::T0));
            volumes.put(object);
        }),
        ("another uid", |volumes| {
            let mut object = volumes.object(VOLUME).unwrap();
            object.metadata.uid = Some("another-uid".to_string());
            volumes.put(object);
        }),
    ];
    for (what, change) in changes {
        let e = env();
        e.volumes.put(volume_object(VOLUME, MINOR, 7000));
        e.set_device(Dev::Empty);
        another_node_acts(&e, change);
        let status = e.stage().await.unwrap_err();
        assert_eq!(status.code(), Code::Unavailable, "{what}: {}", status.message());
        assert!(
            e.calls(&["mkfs.ext4", "mount"]).is_empty(),
            "{what}: {:?}",
            e.device_commands()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_object_before_mkfs_formats_as_before() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    e.set_device(Dev::Empty);
    another_node_acts(&e, |_| {});
    e.stage().await.unwrap();
    assert_eq!(e.calls(&["mkfs.ext4"]).len(), 1);
}

// ---- X3: usable is checked again under the lock ----

#[tokio::test(start_paused = true)]
async fn a_resource_that_is_not_usable_under_the_lock_is_not_opened() {
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Empty);
    // Usable during the wait (`client_up` made it so), not usable by the time of the lock.
    e.sim.usable.store(false, SeqCst);
    let status = e
        .svc
        .stage_locked(VOLUME.to_string(), e.staging.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(status.message().contains("not usable"), "{}", status.message());
    assert!(e.device_commands().is_empty(), "{:?}", e.device_commands());
    assert!(!e.annotated());
}

#[tokio::test(start_paused = true)]
async fn a_resource_that_is_down_under_the_lock_is_not_opened() {
    let e = env();
    e.client_up().await;
    e.set_device(Dev::Empty);
    e.sim.up.store(false, SeqCst);
    let status = e
        .svc
        .stage_locked(VOLUME.to_string(), e.staging.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(e.device_commands().is_empty(), "{:?}", e.device_commands());
}

// ---- X4: publish checks what is staged ----

fn write_mountinfo_line(e: &Env, source: &str, fstype: &str) {
    fs::create_dir_all(&e.staging).unwrap();
    let line = format!(
        "300 1 147:1000 / {} rw,relatime - {fstype} {source} rw\n",
        e.staging.display()
    );
    fs::write(&e.sim.mountinfo, line).unwrap();
}

#[tokio::test]
async fn publish_refuses_a_staging_mount_of_another_device() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    write_mountinfo_line(&e, "/dev/other", "ext4");
    assert_eq!(code(e.publish(false).await), Code::FailedPrecondition);
    assert!(!e.target.exists());
    assert!(e.runner_calls().is_empty());
}

#[tokio::test]
async fn publish_refuses_a_staging_mount_that_is_not_ext4() {
    let e = env();
    e.volumes.put(volume_object(VOLUME, MINOR, 7000));
    write_mountinfo_line(&e, &e.device_source(), "xfs");
    let text = message(e.publish(false).await);
    assert!(text.contains("xfs"), "{text}");
    assert!(!e.target.exists());
    assert!(e.runner_calls().is_empty());
}

#[tokio::test]
async fn publish_takes_the_minor_from_a_fresh_read_of_the_object() {
    let e = env();
    // The staging path holds drbd1000; the object now says 1001.
    e.volumes.put(volume_object(VOLUME, MINOR + 1, 7000));
    e.stage_mount();
    assert_eq!(code(e.publish(false).await), Code::FailedPrecondition);
    assert!(!e.target.exists());
    assert!(e.runner_calls().is_empty());
}

#[tokio::test]
async fn publish_of_a_volume_without_an_object_is_not_found() {
    let e = env();
    e.stage_mount();
    assert_eq!(code(e.publish(false).await), Code::NotFound);
    assert!(!e.target.exists());
    assert!(e.runner_calls().is_empty());
}

// ---- X6: errors of a detached task are logged ----

#[tokio::test(start_paused = true)]
async fn an_unstage_whose_handler_is_gone_logs_its_failure() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.fail("umount", 32, "target is busy");
    let guard = e.svc.agent.locks.lock(VOLUME).await;
    let svc = e.svc.clone();
    let request = NodeUnstageVolumeRequest {
        volume_id: VOLUME.to_string(),
        staging_target_path: e.staging.to_string_lossy().into_owned(),
    };
    let handler = tokio::spawn(async move { svc.node_unstage_volume(Request::new(request)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    // The client gives up while the work waits for the volume lock.
    handler.abort();
    let _ = handler.await;
    assert!(e.logs.lock().unwrap().is_empty());
    drop(guard);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let logs = e.logs.lock().unwrap().clone();
    assert_eq!(logs.len(), 1, "{logs:?}");
    for part in ["NodeUnstageVolume", "pv1", "target is busy", "FailedPrecondition"] {
        assert!(logs[0].contains(part), "{part}: {logs:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_failure_with_its_handler_alive_is_returned_and_not_logged() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    e.fail("umount", 32, "target is busy");
    assert_eq!(code(e.unstage().await), Code::FailedPrecondition);
    assert!(e.logs.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_detached_task_that_succeeds_after_its_handler_is_gone_logs_nothing() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    let guard = e.svc.agent.locks.lock(VOLUME).await;
    let svc = e.svc.clone();
    let request = NodeUnstageVolumeRequest {
        volume_id: VOLUME.to_string(),
        staging_target_path: e.staging.to_string_lossy().into_owned(),
    };
    let handler = tokio::spawn(async move { svc.node_unstage_volume(Request::new(request)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    handler.abort();
    let _ = handler.await;
    drop(guard);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(e.logs.lock().unwrap().is_empty());
    assert!(e.sim.mounted_at(&e.staging).is_empty(), "the work was finished anyway");
    assert!(!is_client(&e));
}

// ---- X7: a repeated stage of a staged volume ----

#[tokio::test(start_paused = true)]
async fn a_repeated_stage_of_a_mounted_path_is_ok_although_the_object_is_deleting() {
    let e = env();
    e.client_up().await;
    e.stage_mount();
    let mut object = e.volumes.object(VOLUME).unwrap();
    object.metadata.deletion_timestamp = Some(crate::tests::volumes::time(crate::tests::volumes::T0));
    e.volumes.put(object);
    e.stage().await.unwrap();
    assert!(e.runner_calls().is_empty(), "{:?}", e.runner_calls());
    assert_eq!(e.patches(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_repeated_stage_of_a_mounted_path_is_ok_although_the_object_is_gone() {
    let e = env();
    e.stage_mount();
    e.stage().await.unwrap();
    assert!(e.runner_calls().is_empty(), "{:?}", e.runner_calls());
}

#[tokio::test(start_paused = true)]
async fn a_stage_without_object_and_without_this_volumes_mount_is_not_found() {
    let e = env();
    fs::create_dir_all(&e.staging).unwrap();
    // Another device, and a device of this volume with another filesystem.
    e.sim
        .add_mount("/dev/other", &e.staging.to_string_lossy(), "/", false, None);
    assert_eq!(code(e.stage().await), Code::NotFound);
    write_mountinfo_line(&e, &e.device_source(), "xfs");
    assert_eq!(code(e.stage().await), Code::NotFound);
    // The source must be `<dev_dir>/drbd<digits>`: nothing else counts as this volume's device.
    for name in ["drbd", "drbdx", "drbd10x0", "drbd-1", "drbd1000/x"] {
        let source = e.dev_dir.join(name).to_string_lossy().into_owned();
        write_mountinfo_line(&e, &source, "ext4");
        assert_eq!(code(e.stage().await), Code::NotFound, "{name}");
    }
    assert!(e.runner_calls().is_empty());
}
