//! The backing store of a copy: a sparse file attached as a loop device.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::exec::{Runner, command_line};
use crate::model::{valid_volume_group_name, valid_volume_name};

const SECTOR: u64 = 512;
/// One bitmap extent covers 2^18 sectors.
const SECTORS_PER_EXTENT: u64 = 1 << 18;

#[derive(Debug, thiserror::Error)]
pub enum BackingError {
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: io::Error,
    },
    #[error("`{command}` did not run to completion: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("{file} is attached to several loop devices: {devices:?}")]
    MultipleLoopDevices { file: String, devices: Vec<String> },
    #[error("{command} printed unexpected output: {output:?}")]
    UnexpectedOutput { command: String, output: String },
    #[error("invalid volume group name {0:?}")]
    InvalidVolumeGroup(String),
    #[error("invalid volume name {0:?}")]
    InvalidVolumeName(String),
}

/// Size of DRBD's internal metadata on a backing device of `device_bytes`
/// for `max_peers` peers: 8 sectors of bitmap per extent and peer, plus 72
/// sectors of fixed overhead. The result is a whole number of 4 KiB, because
/// `(8 * k + 72) * 512 = 4096 * (k + 9)`. Matches three measurements on the
/// stand: a 1 GiB file lost 100 KiB with two peers, 68 KiB with one peer and
/// 260 KiB with `--max-peers=7`.
///
/// Inputs are bounded: `validate` keeps the volume size at or below
/// `MAX_SIZE_BYTES` (2^50) and `max_peers` at 31. At the maximum size the
/// metadata is about 31 * 2^35 bytes (below 2^40), so the backing device is
/// below 2^50 + 2^40 bytes: about 2^23 + 2^13 extents of 2^18 sectors. The
/// product `extents * 8 * peers` stays below (2^23 + 2^13 + 1) * 8 * 31,
/// which is below 2^32, and the final multiplication by 512 stays below
/// 2^41. No `u64` operation here can overflow.
pub fn metadata_bytes(device_bytes: u64, max_peers: u32) -> u64 {
    let extents = device_bytes.div_ceil(SECTOR).div_ceil(SECTORS_PER_EXTENT);
    (extents * 8 * u64::from(max_peers) + 72) * SECTOR
}

/// Size of the backing file that leaves at least `size_bytes` usable after
/// the metadata is taken off its end. Rounded up to 4 KiB.
///
/// Same bounds as `metadata_bytes`: `size_bytes <= MAX_SIZE_BYTES` (2^50)
/// and `max_peers <= 31`. The file is below 2^50 plus the metadata (below
/// 2^40), so `size_bytes + metadata_bytes(..)` and `next_multiple_of(4096)`
/// stay far below 2^64. The loop converges because each extent (128 MiB)
/// added to the file adds 8 * `max_peers` sectors (4 KiB * `max_peers`, at
/// most 124 KiB) of metadata: under 0.1% of the extent, so the growth per
/// iteration shrinks geometrically.
pub fn backing_file_bytes(size_bytes: u64, max_peers: u32) -> u64 {
    let mut file = size_bytes;
    loop {
        let needed = (size_bytes + metadata_bytes(file, max_peers)).next_multiple_of(4096);
        if needed == file {
            return file;
        }
        file = needed;
    }
}

/// Creates a sparse file of `bytes` with mode 0600. The parent directory
/// must exist: a missing one is an error, never created here (a fresh empty
/// volumes directory would hide a storage that is not mounted). Fails if the
/// file already exists. When the resize fails the file is
/// removed again, so a retry does not meet a zero-length stub.
pub fn create_sparse_file(path: &Path, bytes: u64) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| io_err("create", e))?;
    if let Err(source) = file.set_len(bytes) {
        let _ = fs::remove_file(path);
        return Err(io_err("resize", source));
    }
    Ok(())
}

/// The loop device a file is attached to, if any. Several devices are an
/// error: acting on one of them would leave the others holding the file.
pub fn find_loop(runner: &dyn Runner, file: &Path) -> Result<Option<String>, BackingError> {
    let mut devices = find_loops(runner, file)?;
    if devices.len() > 1 {
        return Err(BackingError::MultipleLoopDevices {
            file: file.display().to_string(),
            devices,
        });
    }
    Ok(devices.pop())
}

/// Every loop device a file is attached to, in the order `losetup` prints them.
pub fn find_loops(runner: &dyn Runner, file: &Path) -> Result<Vec<String>, BackingError> {
    let path = file.to_string_lossy();
    let args = ["-j", &path];
    let out = checked(runner, "losetup", &args)?;
    // One line per device: "/dev/loop0: [0048]:1234 (/path/to/file)".
    out.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let device = line.split(':').next().unwrap_or("");
            device_name("losetup", &args, device, line)
        })
        .collect()
}

/// A loop device that holds an image, and whether it holds the file under its
/// live path or only the deleted inode (`losetup` prints `<path> (deleted)`).
/// `confirmed` is true when the line names the image by a full path or when the
/// inode confirmed a name-only line; it is false for a name-only line that was
/// kept without that confirmation (the image is missing, or the line says
/// `(deleted)`). An unconfirmed holder is a reason to keep a copy, never a
/// proof that a device reads this image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopHolder {
    pub device: String,
    pub deleted: bool,
    pub confirmed: bool,
}

/// The loop devices attached to the backing file `image`, also when the file
/// has been deleted: a loop device keeps the deleted inode. `losetup -j
/// <path>` cannot be used for this: it looks the file up by its path and says
/// nothing for a path that no longer exists (checked on the stand), so the
/// whole listing is read. This is the one place that matches a listing
/// against an image.
///
/// A line belongs to `image` when its BACK-FILE, without a trailing
/// ` (deleted)`, is one of:
///
/// * the image path as given;
/// * the image path with its parent directory resolved (`losetup` stores the
///   canonical path);
/// * exactly `/<file name of image>`. The kernel renders the backing path
///   through the mount that the attaching process had open. The agent pod
///   attaches through its own bind mount of the volumes directory; once that
///   pod is replaced the mount is detached and the same loop device is
///   listed as `/<name>.img`, in the new pod and on the host (stand,
///   Task 13). Without this form a restarted agent would find no loop device
///   of its volumes.
///
/// The third form identifies the file by its name only. A holder matched by
/// it alone and not marked `(deleted)` is confirmed by inode while the image
/// exists: `losetup -j <image>` must list that device, otherwise the line is
/// somebody else's file and is dropped. When the image does not exist, or the
/// line says `(deleted)`, the holder is kept: that is the conservative
/// direction, "something may still hold this volume's file". Residual
/// imprecision: a foreign file named `/<name>.img` at the root of another
/// detached mount is taken for ours in those two cases; volume names are
/// `pvc-<uuid>`, so a collision is not expected.
///
/// Anything that cannot be understood is an error, never "no loop device": a
/// failed `losetup`, a parent directory that cannot be resolved, a non-empty
/// line that is not a device followed by a backing file, and a backing file
/// ending in `*` (util-linux cuts long names that way, so the line may be
/// this image's).
/// Lists loop devices that hold `image`, without verifying inode for name-only matches.
fn raw_loop_holders(runner: &dyn Runner, image: &Path) -> Result<Vec<LoopHolder>, BackingError> {
    let mut candidates = vec![image.to_string_lossy().into_owned()];
    let mut detached_form = None;
    if let (Some(parent), Some(name)) = (image.parent(), image.file_name()) {
        let parent = parent.canonicalize().map_err(|source| BackingError::Io {
            action: "resolve",
            path: parent.display().to_string(),
            source,
        })?;
        let resolved = parent.join(name).to_string_lossy().into_owned();
        if !candidates.contains(&resolved) {
            candidates.push(resolved);
        }
        detached_form = Some(format!("/{}", name.to_string_lossy()));
    }
    let args = ["--list", "--noheadings", "--output", "NAME,BACK-FILE"];
    let listing = checked(runner, "losetup", &args)?;
    // Every match; `confirmed` starts true for a match by path and false for a
    // match by the name form alone.
    let mut holders: Vec<LoopHolder> = Vec::new();
    for line in listing.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let unexpected = || BackingError::UnexpectedOutput {
            command: command_line("losetup", &args),
            output: line.to_string(),
        };
        let (device, back_file) = line.split_once(char::is_whitespace).ok_or_else(unexpected)?;
        let back_file = back_file.trim();
        if back_file.ends_with('*') {
            return Err(unexpected());
        }
        let (path, deleted) = match back_file.strip_suffix(" (deleted)") {
            Some(path) => (path, true),
            None => (back_file, false),
        };
        let by_path = candidates.iter().any(|candidate| candidate == path);
        let by_name = detached_form.as_deref() == Some(path);
        if by_path || by_name {
            holders.push(LoopHolder {
                device: device.to_string(),
                deleted,
                confirmed: by_path,
            });
        }
    }
    Ok(holders)
}

/// Returns true if `losetup --list` mentions `image` by path or name, confirmed or not.
pub fn has_any_loop_reference(runner: &dyn Runner, image: &Path) -> Result<bool, BackingError> {
    raw_loop_holders(runner, image).map(|h| !h.is_empty())
}

pub fn loop_holders(runner: &dyn Runner, image: &Path) -> Result<Vec<LoopHolder>, BackingError> {
    let holders = raw_loop_holders(runner, image)?;
    // The inode confirms a name-only match of a live line while the image exists.
    if !holders.iter().any(|holder| !holder.confirmed && !holder.deleted) {
        return Ok(holders);
    }
    let exists = image.try_exists().map_err(|source| BackingError::Io {
        action: "inspect",
        path: image.display().to_string(),
        source,
    })?;
    // `Some` only when the image exists: the devices that hold its inode.
    let same_inode = if exists { Some(find_loops(runner, image)?) } else { None };
    Ok(holders
        .into_iter()
        .filter_map(|mut holder| {
            if holder.confirmed || holder.deleted {
                return Some(holder);
            }
            match &same_inode {
                // The image exists: a name-only line is ours only if the inode says so.
                Some(devices) => devices.contains(&holder.device).then(|| {
                    holder.confirmed = true;
                    holder
                }),
                // No image to compare with: kept, unconfirmed.
                None => Some(holder),
            }
        })
        .collect())
}

/// Attaches a file to a free loop device with direct I/O; returns the device.
pub fn attach_loop(runner: &dyn Runner, file: &Path) -> Result<String, BackingError> {
    let path = file.to_string_lossy();
    let args = ["--find", "--show", "--direct-io=on", &path];
    let out = checked(runner, "losetup", &args)?;
    let printed = out.trim();
    device_name("losetup", &args, printed, printed)
}

/// Attaches a file to a free loop device in read-only mode; returns the device.
pub fn attach_loop_read_only(runner: &dyn Runner, file: &Path) -> Result<String, BackingError> {
    let path = file.to_string_lossy();
    let args = ["--find", "--show", "--read-only", &path];
    let out = checked(runner, "losetup", &args)?;
    let printed = out.trim();
    device_name("losetup", &args, printed, printed)
}

pub fn detach_loop(runner: &dyn Runner, device: &str) -> Result<(), BackingError> {
    checked(runner, "losetup", &["-d", device]).map(|_| ())
}

/// Points `link` at `device`, replacing an existing symbolic link (also a
/// dangling one) atomically: the new link is created under a temporary name
/// next to it and renamed over the old one. Anything that is not a symbolic
/// link is refused and left untouched.
pub fn link_device(link: &Path, device: &str) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: link.display().to_string(),
        source,
    };
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    match fs::symlink_metadata(link) {
        Ok(meta) if !meta.file_type().is_symlink() => {
            return Err(io_err(
                "replace",
                io::Error::new(io::ErrorKind::AlreadyExists, "not a symbolic link"),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err("inspect", e)),
    }
    let name = link
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = link.with_file_name(format!(".{name}.tmp"));
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err("remove", e)),
    }
    std::os::unix::fs::symlink(device, &tmp).map_err(|e| io_err("link", e))?;
    fs::rename(&tmp, link).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_err("rename into", e)
    })
}

/// Where `link` points, or `None` when `link` is not a symbolic link (or
/// does not exist). A dangling link still has a target.
pub fn link_target(link: &Path) -> Option<String> {
    fs::read_link(link).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Returns the block device path for an LVM logical volume in a volume group: `/dev/{vg}/{lv_name}`.
pub fn lvm_device_path(vg: &str, lv_name: &str) -> String {
    format!("/dev/{vg}/{lv_name}")
}

/// Checks whether an LVM logical volume exists in the volume group via `lvs`.
pub fn find_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<bool, BackingError> {
    if !valid_volume_group_name(vg) {
        return Err(BackingError::InvalidVolumeGroup(vg.to_string()));
    }
    if !valid_volume_name(lv_name) {
        return Err(BackingError::InvalidVolumeName(lv_name.to_string()));
    }
    let dev = lvm_device_path(vg, lv_name);
    let args = ["--noheadings", "-o", "lv_name", "--", &dev];
    match checked(runner, "lvs", &args) {
        Ok(_) => Ok(true),
        Err(BackingError::Command { command, code, stderr }) => {
            let lower = stderr.to_lowercase();
            if !lower.contains("volume group") && (stderr.contains("not found") || stderr.contains("Failed to find")) {
                Ok(false)
            } else {
                Err(BackingError::Command { command, code, stderr })
            }
        }
        Err(e) => Err(e),
    }
}

/// Creates a new LVM logical volume in `vg` of `bytes` size via `lvcreate`.
/// Discards or wipes the contents of the logical volume so stale filesystem or
/// DRBD metadata does not confuse subsequent mounts or syncs.
pub fn create_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str, bytes: u64) -> Result<(), BackingError> {
    if !valid_volume_group_name(vg) {
        return Err(BackingError::InvalidVolumeGroup(vg.to_string()));
    }
    if !valid_volume_name(lv_name) {
        return Err(BackingError::InvalidVolumeName(lv_name.to_string()));
    }
    let size_arg = format!("{bytes}B");
    checked(runner, "lvcreate", &["-y", "-L", &size_arg, "-n", lv_name, "--", vg])?;
    let dev = lvm_device_path(vg, lv_name);
    if checked(runner, "blkdiscard", &["--", &dev]).is_err() {
        let _ = checked(runner, "wipefs", &["-a", "-f", "--", &dev]);
    }
    Ok(())
}

/// Irrevocably removes an LVM logical volume via `lvremove`.
/// Discards data prior to removal if supported to return blocks to the pool.
pub fn remove_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<(), BackingError> {
    if !valid_volume_group_name(vg) {
        return Err(BackingError::InvalidVolumeGroup(vg.to_string()));
    }
    if !valid_volume_name(lv_name) {
        return Err(BackingError::InvalidVolumeName(lv_name.to_string()));
    }
    let dev = lvm_device_path(vg, lv_name);
    let _ = checked(runner, "blkdiscard", &["--", &dev]);
    checked(runner, "lvremove", &["-y", "--", &dev])?;
    Ok(())
}

/// Accepts a device name that `losetup` printed; anything else is an error
/// so that a blank or malformed line never becomes a link target.
fn device_name(program: &str, args: &[&str], device: &str, line: &str) -> Result<String, BackingError> {
    if device.starts_with("/dev/") {
        return Ok(device.to_string());
    }
    Err(BackingError::UnexpectedOutput {
        command: command_line(program, args),
        output: line.to_string(),
    })
}

fn checked(runner: &dyn Runner, program: &str, args: &[&str]) -> Result<String, BackingError> {
    let command = command_line(program, args);
    let out = runner.run(program, args).map_err(|e| BackingError::Spawn {
        command: command.clone(),
        message: e.to_string(),
    })?;
    if out.success() {
        return Ok(out.stdout);
    }
    Err(BackingError::Command {
        command,
        code: out.code,
        stderr: out.stderr.trim().to_string(),
    })
}

#[cfg(test)]
#[path = "tests/backing.rs"]
mod tests;
