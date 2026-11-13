use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::exec::{Output, Runner};
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
    assert_eq!(
        err,
        LoadError::Command {
            command: format!(
                "insmod {}/drbd.ko usermode_helper=disabled",
                fx.modules.join(KVER).display()
            ),
            code: 1,
            stderr: "insmod: ERROR: Invalid module format".to_string(),
        }
    );
    // modprobe and the failing insmod ran; the transport insmod did not.
    assert_eq!(
        fx.calls(&runner),
        [
            "modprobe -a lru_cache libcrc32c",
            "insmod <dir>/drbd.ko usermode_helper=disabled"
        ]
    );
}

/// Like `Fixture::kernel`, but the transport `insmod` fails when `fail_transport` is set.
fn kernel_with_transport(fx: &Fixture, fail_transport: bool) -> FakeRunner {
    let sys = fx.sys.clone();
    FakeRunner::new(move |program, args| {
        if program == "insmod" {
            if args[0].ends_with("/drbd.ko") {
                mark_loaded(&sys, "drbd", Some("9.3.4"));
            } else if fail_transport {
                return out(1, "", "insmod: ERROR: could not insert module\n");
            } else {
                mark_loaded(&sys, "drbd_transport_tcp", None);
            }
        }
        out(0, "", "")
    })
}

#[test]
fn a_failed_transport_insmod_is_recovered_by_the_next_run() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let broken = kernel_with_transport(&fx, true);
    let err = load(&fx.env(), &broken).unwrap_err();
    let transport = fx.modules.join(KVER).join("drbd_transport_tcp.ko");
    assert!(matches!(
        &err,
        LoadError::Command { command, code: 1, .. } if command == &format!("insmod {}", transport.display())
    ));
    assert!(fx.sys.join("drbd").is_dir());
    assert!(!fx.sys.join("drbd_transport_tcp").exists());

    let healthy = kernel_with_transport(&fx, false);
    assert_eq!(
        load(&fx.env(), &healthy),
        Ok("drbd 9.3.4 is already loaded".to_string())
    );
    assert_eq!(fx.calls(&healthy), ["insmod <dir>/drbd_transport_tcp.ko"]);
    assert!(fx.sys.join("drbd_transport_tcp").is_dir());
}

#[test]
fn a_failing_modprobe_stops_before_any_insmod() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = FakeRunner::new(|program, _| {
        if program == "modprobe" {
            out(1, "", "modprobe: FATAL: Module lru_cache not found\n")
        } else {
            out(0, "", "")
        }
    });
    assert_eq!(
        load(&fx.env(), &runner),
        Err(LoadError::Command {
            command: "modprobe -a lru_cache libcrc32c".to_string(),
            code: 1,
            stderr: "modprobe: FATAL: Module lru_cache not found".to_string(),
        })
    );
    assert_eq!(runner.calls(), ["modprobe -a lru_cache libcrc32c"]);
}

#[test]
fn a_missing_transport_file_after_loading_the_core_is_reported() {
    let fx = Fixture::new().with_ko("drbd.ko");
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Err(LoadError::NoTransport(KVER.to_string())));
    assert_eq!(
        fx.calls(&runner),
        [
            "modprobe -a lru_cache libcrc32c",
            "insmod <dir>/drbd.ko usermode_helper=disabled"
        ]
    );
}

/// A runner that cannot start any program.
struct UnstartableRunner;

impl Runner for UnstartableRunner {
    fn run(&self, _program: &str, _args: &[&str]) -> io::Result<Output> {
        Err(io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }
}

#[test]
fn a_program_that_cannot_start_is_an_io_error() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let err = load(&fx.env(), &UnstartableRunner).unwrap_err();
    assert!(
        matches!(&err, LoadError::Io(text) if text.starts_with("cannot start modprobe")),
        "{err:?}"
    );
}

#[test]
fn an_unreadable_loaded_version_is_an_io_error() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", None);
    let runner = fx.kernel();
    let err = load(&fx.env(), &runner).unwrap_err();
    assert!(
        matches!(&err, LoadError::Io(text) if text.starts_with("cannot read")),
        "{err:?}"
    );
    assert!(runner.calls().is_empty());
}

#[test]
fn versions_outside_the_9_dot_family_are_refused() {
    for version in ["9", "10.0.1", ""] {
        let fx = Fixture::new()
            .with_ko("drbd.ko")
            .with_ko("drbd_transport_tcp.ko")
            .with_loaded("drbd", Some(version));
        let runner = fx.kernel();
        assert_eq!(
            load(&fx.env(), &runner),
            Err(LoadError::InTreeLoaded(version.to_string())),
            "version {version:?}"
        );
        assert!(runner.calls().is_empty(), "version {version:?}");
    }
}

#[test]
fn version_9_0_0_is_accepted() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("9.0.0"))
        .with_loaded("drbd_transport_tcp", None);
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Ok("drbd 9.0.0 is already loaded".to_string()));
}

const TAINT: &str = "module verification failed: signature and/or required key missing - tainting kernel\n";

#[test]
fn stderr_of_a_successful_command_goes_to_the_log_with_a_prefix() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let sys = fx.sys.clone();
    let runner = FakeRunner::new(move |program, args| {
        if program != "insmod" {
            return out(0, "", "");
        }
        if args[0].ends_with("/drbd.ko") {
            mark_loaded(&sys, "drbd", Some("9.3.4"));
            out(0, "", TAINT)
        } else {
            mark_loaded(&sys, "drbd_transport_tcp", None);
            out(0, "", "")
        }
    });
    let mut log = Vec::new();
    load_with_log(&fx.env(), &runner, &mut log).unwrap();
    assert_eq!(String::from_utf8(log).unwrap(), format!("insmod: {TAINT}"));
}

#[test]
fn multi_line_stderr_gets_one_prefix_per_line() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = FakeRunner::new(|program, _| {
        if program == "modprobe" {
            out(0, "", "first\nsecond\n")
        } else {
            out(0, "", "")
        }
    });
    let mut log = Vec::new();
    // insmod does not create the sysfs entries here, so the load itself fails after modprobe.
    let _ = load_with_log(&fx.env(), &runner, &mut log);
    assert_eq!(String::from_utf8(log).unwrap(), "modprobe: first\nmodprobe: second\n");
}

#[test]
fn a_successful_command_without_stderr_logs_nothing() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = fx.kernel();
    let mut log = Vec::new();
    load_with_log(&fx.env(), &runner, &mut log).unwrap();
    assert!(log.is_empty());
}

/// A log that rejects every write.
struct BrokenLog;

impl io::Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_log_that_cannot_be_written_does_not_fail_the_load() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let sys = fx.sys.clone();
    let runner = FakeRunner::new(move |program, args| {
        if program == "insmod" && args[0].ends_with("/drbd.ko") {
            mark_loaded(&sys, "drbd", Some("9.3.4"));
        } else if program == "insmod" {
            mark_loaded(&sys, "drbd_transport_tcp", None);
        }
        out(0, "", TAINT)
    });
    assert_eq!(
        load_with_log(&fx.env(), &runner, &mut BrokenLog),
        Ok(format!("loaded drbd 9.3.4 for kernel {KVER}"))
    );
}
