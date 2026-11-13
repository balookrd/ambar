use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::tests::{FakeRunner, out};

const KVER: &str = "6.12.116+deb13-arm64";

struct Fixture {
    _tmp: TempDir,
    modules: PathBuf,
    sys: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let modules = tmp.path().join("modules");
        let sys = tmp.path().join("sys");
        fs::create_dir_all(modules.join(KVER)).unwrap();
        fs::create_dir_all(&sys).unwrap();
        Self {
            _tmp: tmp,
            modules,
            sys,
        }
    }

    fn with_ko(self, name: &str) -> Self {
        fs::write(self.modules.join(KVER).join(name), b"").unwrap();
        self
    }

    fn with_loaded(self, module: &str, version: Option<&str>) -> Self {
        mark_loaded(&self.sys, module, version);
        self
    }

    fn env(&self) -> LoaderEnv<'_> {
        LoaderEnv {
            modules_dir: &self.modules,
            sys_module_dir: &self.sys,
            kernel_release: KVER,
        }
    }

    /// A runner whose `insmod` makes the module appear in the fake sysfs,
    /// the way the kernel does.
    fn kernel(&self) -> FakeRunner {
        let sys = self.sys.clone();
        FakeRunner::new(move |program, args| {
            if program == "insmod" {
                if args[0].ends_with("/drbd.ko") {
                    mark_loaded(&sys, "drbd", Some("9.3.4"));
                } else if args[0].ends_with("/drbd_transport_tcp.ko") {
                    mark_loaded(&sys, "drbd_transport_tcp", None);
                }
            }
            out(0, "", "")
        })
    }

    /// Call log with the temporary modules directory replaced by `<dir>`.
    fn calls(&self, runner: &FakeRunner) -> Vec<String> {
        let dir = self.modules.join(KVER).to_string_lossy().into_owned();
        runner.calls().iter().map(|c| c.replace(&dir, "<dir>")).collect()
    }
}

fn mark_loaded(sys: &Path, module: &str, version: Option<&str>) {
    fs::create_dir_all(sys.join(module)).unwrap();
    if let Some(version) = version {
        fs::write(sys.join(module).join("version"), format!("{version}\n")).unwrap();
    }
}

#[test]
fn nothing_loaded_loads_dependencies_core_and_transport_in_order() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = fx.kernel();
    assert_eq!(
        load(&fx.env(), &runner),
        Ok(format!("loaded drbd 9.3.4 for kernel {KVER}"))
    );
    assert_eq!(
        fx.calls(&runner),
        [
            "modprobe -a lru_cache libcrc32c",
            "insmod <dir>/drbd.ko usermode_helper=disabled",
            "insmod <dir>/drbd_transport_tcp.ko",
        ]
    );
}

#[test]
fn drbd9_with_transport_already_loaded_does_nothing() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("9.3.4"))
        .with_loaded("drbd_transport_tcp", None);
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Ok("drbd 9.3.4 is already loaded".to_string()));
    assert!(runner.calls().is_empty());
}

#[test]
fn drbd9_without_transport_loads_only_the_transport() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("9.3.4"));
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Ok("drbd 9.3.4 is already loaded".to_string()));
    assert_eq!(fx.calls(&runner), ["insmod <dir>/drbd_transport_tcp.ko"]);
}

#[test]
fn drbd9_without_transport_and_without_its_file_fails() {
    let fx = Fixture::new().with_ko("drbd.ko").with_loaded("drbd", Some("9.3.4"));
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Err(LoadError::NoTransport(KVER.to_string())));
    assert!(runner.calls().is_empty());
}

#[test]
fn the_in_tree_module_is_refused() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("8.4.11"));
    let runner = fx.kernel();
    let err = load(&fx.env(), &runner).unwrap_err();
    assert_eq!(err, LoadError::InTreeLoaded("8.4.11".to_string()));
    assert_eq!(err.to_string(), "in-tree drbd 8.4 is loaded (version 8.4.11)");
    assert!(runner.calls().is_empty());
}

#[test]
fn a_kernel_without_a_bundled_module_is_refused() {
    let fx = Fixture::new();
    let runner = fx.kernel();
    let err = load(&fx.env(), &runner).unwrap_err();
    assert_eq!(err.to_string(), format!("no drbd module for kernel {KVER}"));
    assert!(runner.calls().is_empty());
}

#[test]
fn a_failing_insmod_is_reported_with_its_stderr() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = FakeRunner::new(|program, _| {
        if program == "insmod" {
            out(1, "", "insmod: ERROR: Invalid module format\n")
        } else {
            out(0, "", "")
        }
    });
    let err = load(&fx.env(), &runner).unwrap_err();
    assert!(
        matches!(&err, LoadError::Command { code: 1, stderr, .. } if stderr == "insmod: ERROR: Invalid module format")
    );
}
