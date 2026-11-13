//! What the node holds for a volume name, read from the file system without
//! changing anything.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::model::{SpecError, valid_volume_name};
use crate::paths::Paths;
use crate::reconcile::{FileState, ReconcileError};

use super::decide::Local;

/// Volume names this node has a file of ambar's for: a backing file, a
/// resource file in `res_dir`, an applied copy of it or a device link in
/// `dev_dir`. Markers and lock files alone do not count, and neither does a
/// resource that exists only in the kernel: ambar writes `<name>.res` for
/// every resource it configures, so a kernel resource without that file is
/// not ambar's and is left alone. Names that are not valid volume names are
/// skipped.
pub fn local_volumes(paths: &Paths) -> Result<BTreeSet<String>, ReconcileError> {
    let mut found = BTreeSet::new();
    names_in(&paths.volumes_dir, Some(".img"), &mut found)?;
    names_in(&paths.res_dir, Some(".res"), &mut found)?;
    names_in(&paths.res_dir, Some(".res.applied"), &mut found)?;
    names_in(&paths.dev_dir, None, &mut found)?;
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

/// What the node holds for `volume`. A trace is a file that ambar writes (see
/// `local_volumes`); a resource in the kernel is not one. A path that cannot
/// be inspected is an error, never "absent": the answer decides whether a
/// backing file counts.
pub fn local(paths: &Paths, volume: &str) -> Result<Local, ReconcileError> {
    if !valid_volume_name(volume) {
        return Err(SpecError::Name(volume.to_string()).into());
    }
    let image = exists(&paths.image(volume))?;
    let synced = exists(&paths.synced_marker(volume))?;
    let traces = exists(&paths.res_file(volume))?
        || exists(&paths.applied_res(volume))?
        || link_exists(&paths.device_link(volume))?;
    Ok(Local { image, traces, synced })
}

/// The state of the backing file of `volume`, as `Reconciler::observe` sees
/// it: absent, present without the ready marker (DRBD metadata was never
/// created on it), or present with it. A path that cannot be inspected is an
/// error, never "absent".
pub fn file_state(paths: &Paths, volume: &str) -> Result<FileState, ReconcileError> {
    if !valid_volume_name(volume) {
        return Err(SpecError::Name(volume.to_string()).into());
    }
    let image = exists(&paths.image(volume))?;
    let ready = exists(&paths.ready_marker(volume))?;
    Ok(match (image, ready) {
        (false, _) => FileState::Absent,
        (true, false) => FileState::Pending,
        (true, true) => FileState::Ready,
    })
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
