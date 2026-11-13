//! What the node holds for a volume name, read from the file system and the
//! kernel without changing anything.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::drbd::adm::DrbdError;
use crate::drbd::status::{self, ResourceStatus};
use crate::exec::{Runner, command_line};
use crate::model::{SpecError, valid_volume_name};
use crate::paths::Paths;
use crate::reconcile::ReconcileError;

use super::decide::Local;

/// Every resource configured in the kernel, from `drbdsetup status --json`.
/// The same command and the same checks as `Drbd::status`, which returns one
/// resource only.
pub fn kernel_resources(runner: &dyn Runner) -> Result<Vec<ResourceStatus>, DrbdError> {
    let args = ["status", "--json"];
    let out = runner.run("drbdsetup", &args).map_err(|e| DrbdError::Spawn {
        command: command_line("drbdsetup", &args),
        message: e.to_string(),
    })?;
    if !out.success() {
        return Err(DrbdError::Command {
            command: command_line("drbdsetup", &args),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    let text = out.stdout.trim();
    if !text.starts_with('[') {
        let first_line = text.lines().next().unwrap_or("");
        return Err(DrbdError::Status(format!("unexpected output: {first_line}")));
    }
    status::parse(text).map_err(|e| DrbdError::Status(e.to_string()))
}

/// Volume names this node has anything for: backing files, resource files,
/// applied copies, device links and configured kernel resources. Markers and
/// lock files alone do not count. Names that are not valid volume names are
/// skipped.
pub fn local_volumes(paths: &Paths, runner: &dyn Runner) -> Result<BTreeSet<String>, ReconcileError> {
    let mut found = BTreeSet::new();
    names_in(&paths.volumes_dir, Some(".img"), &mut found)?;
    names_in(&paths.res_dir, Some(".res"), &mut found)?;
    names_in(&paths.res_dir, Some(".res.applied"), &mut found)?;
    names_in(&paths.dev_dir, None, &mut found)?;
    for resource in kernel_resources(runner)? {
        found.insert(resource.name);
    }
    found.retain(|name| valid_volume_name(name));
    Ok(found)
}

/// Adds the names of the entries of `dir` that end in `suffix` (any entry
/// when `None`), with the suffix cut off. A missing directory has none.
fn names_in(dir: &Path, suffix: Option<&str>, into: &mut BTreeSet<String>) -> Result<(), ReconcileError> {
    let io_err = |action, path: &Path, source| ReconcileError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("list", dir, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| io_err("list", dir, e))?;
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        match suffix {
            None => {
                into.insert(file_name);
            }
            Some(suffix) => {
                if let Some(name) = file_name.strip_suffix(suffix) {
                    into.insert(name.to_string());
                }
            }
        }
    }
    Ok(())
}

/// What the node holds for `volume`. `configured` is the set of resources
/// the kernel has configured. A path that cannot be inspected is an error,
/// never "absent": the answer decides whether a backing file counts.
pub fn local(paths: &Paths, configured: &BTreeSet<String>, volume: &str) -> Result<Local, ReconcileError> {
    if !valid_volume_name(volume) {
        return Err(SpecError::Name(volume.to_string()).into());
    }
    let image = exists(&paths.image(volume))?;
    let synced = exists(&paths.synced_marker(volume))?;
    let traces = configured.contains(volume)
        || exists(&paths.res_file(volume))?
        || exists(&paths.applied_res(volume))?
        || link_exists(&paths.device_link(volume))?;
    Ok(Local { image, traces, synced })
}

fn exists(path: &Path) -> Result<bool, ReconcileError> {
    path.try_exists().map_err(|source| ReconcileError::Io {
        action: "inspect",
        path: path.display().to_string(),
        source,
    })
}

/// A device link counts even when it dangles, so `symlink_metadata`.
fn link_exists(path: &Path) -> Result<bool, ReconcileError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ReconcileError::Io {
            action: "inspect",
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
#[path = "tests/local.rs"]
mod tests;
