//! Helpers shared by the unit tests of this crate.

use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::drbd::resource;
use crate::exec::{Output, Runner, command_line};
use crate::model::VolumeSpec;
use crate::paths::Paths;

pub(crate) mod volumes;

type Reply = Box<dyn Fn(&str, &[&str]) -> Output + Send + Sync>;

/// Records every command line and answers from a closure.
pub(crate) struct FakeRunner {
    calls: Mutex<Vec<String>>,
    reply: Reply,
}

impl FakeRunner {
    pub(crate) fn new(reply: impl Fn(&str, &[&str]) -> Output + Send + Sync + 'static) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.calls.lock().unwrap().push(command_line(program, args));
        Ok((self.reply)(program, args))
    }
}

pub(crate) fn out(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

// ---- a fake node, shared by the tests of reconcile and of the agent ----

pub(crate) const SPEC: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "initialSync": "Skip",
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

/// Captured on lima-dv-1 (node id 0) of a volume with three copies.
pub(crate) const THREE_INCONSISTENT: &str = include_str!("../../stand/spike/samples/three-inconsistent.json");
pub(crate) const THREE_UPTODATE: &str = include_str!("../../stand/spike/samples/three-uptodate.json");

/// Stderr recorded in experiment P7 (1) for a backing file without metadata.
pub(crate) const P7_NO_METADATA_STDERR: &str = "No valid meta data found\nCommand 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255\n";

pub(crate) fn spec() -> VolumeSpec {
    VolumeSpec::from_json(SPEC).unwrap()
}

pub(crate) fn temp_paths(tmp: &TempDir) -> Paths {
    Paths {
        volumes_dir: tmp.path().join("volumes"),
        res_dir: tmp.path().join("res"),
        dev_dir: tmp.path().join("dev"),
    }
}

/// A fake node: `losetup` remembers the attachment, `drbdadm up` makes the
/// resource appear in `drbdsetup status`, and the skip of the initial sync
/// turns the disks `UpToDate`.
pub(crate) fn fake_node() -> FakeRunner {
    fake_node_with(out(255, "", P7_NO_METADATA_STDERR))
}

/// `fake_node` whose `drbdadm dump-md` answers with `dump_md`.
pub(crate) fn fake_node_with(dump_md: Output) -> FakeRunner {
    fake_node_with_dump_md(move |_| dump_md.clone())
}

/// `fake_node` whose `drbdadm dump-md` answers by whether `create-md` has
/// run on the node before (the argument).
pub(crate) fn fake_node_with_dump_md(dump_md: impl Fn(bool) -> Output + Send + Sync + 'static) -> FakeRunner {
    let attached = Arc::new(AtomicBool::new(false));
    let up = Arc::new(AtomicBool::new(false));
    let skipped = Arc::new(AtomicBool::new(false));
    let created = Arc::new(AtomicBool::new(false));
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) if attached.load(Ordering::SeqCst) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("losetup", ["--find", ..]) => {
            attached.store(true, Ordering::SeqCst);
            out(0, "/dev/loop7\n", "")
        }
        ("losetup", ["-d", _]) => {
            attached.store(false, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["dump-md", _]) => dump_md(created.load(Ordering::SeqCst)),
        ("drbdadm", ["create-md", ..]) => {
            created.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["up", _]) => {
            up.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["down", _]) => {
            up.store(false, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["new-current-uuid", ..]) => {
            skipped.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdsetup", _) if up.load(Ordering::SeqCst) && skipped.load(Ordering::SeqCst) => out(0, THREE_UPTODATE, ""),
        ("drbdsetup", _) if up.load(Ordering::SeqCst) => out(0, THREE_INCONSISTENT, ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    })
}

/// Files of a copy of `SPEC` that lima-dv-1 set up earlier; the device
/// link points at /dev/loop7.
pub(crate) fn prepare_copy(paths: &Paths, applied: bool, synced: bool) {
    for dir in [&paths.volumes_dir, &paths.res_dir, &paths.dev_dir] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(paths.image("spike"), "").unwrap();
    fs::write(paths.ready_marker("spike"), "").unwrap();
    if synced {
        fs::write(paths.synced_marker("spike"), "").unwrap();
    }
    let text = resource::render(&spec(), &paths.device_link("spike"));
    fs::write(paths.res_file("spike"), &text).unwrap();
    if applied {
        fs::write(paths.applied_res("spike"), &text).unwrap();
    }
    std::os::unix::fs::symlink("/dev/loop7", paths.device_link("spike")).unwrap();
}

/// A node where the copy is attached to /dev/loop7 and `drbdsetup status`
/// prints whatever `status` holds; every other command succeeds and
/// changes nothing.
pub(crate) fn configured_node(status: Arc<Mutex<String>>) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
        _ => out(0, "", ""),
    })
}
