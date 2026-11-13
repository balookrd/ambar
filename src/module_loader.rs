//! Loads the DRBD 9 kernel module that ships in the agent image.
//!
//! Same rules as `image/load-module.sh`; the image test `image/test-loader.sh`
//! covers the shell version, the unit tests here cover this one.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::exec::{Runner, command_line};

pub struct LoaderEnv<'a> {
    /// Directory with one subdirectory per kernel release.
    pub modules_dir: &'a Path,
    /// `/sys/module` on a real node.
    pub sys_module_dir: &'a Path,
    /// Output of `uname -r`.
    pub kernel_release: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("in-tree drbd 8.4 is loaded (version {0})")]
    InTreeLoaded(String),
    #[error("no drbd module for kernel {0}")]
    NoModule(String),
    #[error("no drbd_transport_tcp module for kernel {0}")]
    NoTransport(String),
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("{0}")]
    Io(String),
}

/// Makes sure DRBD 9 and its TCP transport are loaded.
/// Returns a one-line description of what was found or done.
pub fn load(env: &LoaderEnv, runner: &dyn Runner) -> Result<String, LoadError> {
    load_with_log(env, runner, &mut std::io::stderr())
}

/// Same as [`load`]; stderr of commands that succeed is copied to `log`,
/// one line at a time, prefixed with the program name (the shell version lets
/// `modprobe` and `insmod` write such warnings to the container log).
pub fn load_with_log(env: &LoaderEnv, runner: &dyn Runner, log: &mut dyn Write) -> Result<String, LoadError> {
    let dir = env.modules_dir.join(env.kernel_release);
    let version_file = env.sys_module_dir.join("drbd").join("version");

    if env.sys_module_dir.join("drbd").is_dir() {
        let loaded = read_version(&version_file)?;
        if !loaded.starts_with("9.") {
            return Err(LoadError::InTreeLoaded(loaded));
        }
        // A previous run may have died between the two insmod calls:
        // resources cannot connect without the transport.
        load_transport(env, &dir, runner, log)?;
        return Ok(format!("drbd {loaded} is already loaded"));
    }

    let core = dir.join("drbd.ko");
    if !core.is_file() {
        return Err(LoadError::NoModule(env.kernel_release.to_string()));
    }

    // Dependencies come from the host module tree mounted at /lib/modules.
    run(runner, log, "modprobe", &["-a", "lru_cache", "libcrc32c"])?;
    run(
        runner,
        log,
        "insmod",
        &[&core.to_string_lossy(), "usermode_helper=disabled"],
    )?;
    load_transport(env, &dir, runner, log)?;
    let loaded = read_version(&version_file)?;
    Ok(format!("loaded drbd {loaded} for kernel {}", env.kernel_release))
}

fn load_transport(env: &LoaderEnv, dir: &Path, runner: &dyn Runner, log: &mut dyn Write) -> Result<(), LoadError> {
    if env.sys_module_dir.join("drbd_transport_tcp").is_dir() {
        return Ok(());
    }
    let transport = dir.join("drbd_transport_tcp.ko");
    if !transport.is_file() {
        return Err(LoadError::NoTransport(env.kernel_release.to_string()));
    }
    run(runner, log, "insmod", &[&transport.to_string_lossy()])
}

fn read_version(path: &Path) -> Result<String, LoadError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| LoadError::Io(format!("cannot read {}: {e}", path.display())))
}

fn run(runner: &dyn Runner, log: &mut dyn Write, program: &str, args: &[&str]) -> Result<(), LoadError> {
    let command = command_line(program, args);
    let out = runner
        .run(program, args)
        .map_err(|e| LoadError::Io(format!("cannot start {command}: {e}")))?;
    if out.success() {
        // A broken log must not fail the load.
        for line in out.stderr.trim().lines() {
            let _ = writeln!(log, "{program}: {line}");
        }
        return Ok(());
    }
    Err(LoadError::Command {
        command,
        code: out.code,
        stderr: out.stderr.trim().to_string(),
    })
}

#[cfg(test)]
#[path = "tests/module_loader.rs"]
mod tests;
