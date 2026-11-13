//! The backing store of a copy: a sparse file attached as a loop device.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use crate::exec::{Runner, command_line};

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
    #[error("cannot start {command}: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("{file} is attached to several loop devices: {devices:?}")]
    MultipleLoopDevices { file: String, devices: Vec<String> },
    #[error("{command} printed unexpected output: {output:?}")]
    UnexpectedOutput { command: String, output: String },
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

/// Creates a sparse file of `bytes` with mode 0600; a missing parent
/// directory is created with mode 0700, an existing one is left as it is.
/// Fails if the file already exists. When the resize fails the file is
/// removed again, so a retry does not meet a zero-length stub.
pub fn create_sparse_file(path: &Path, bytes: u64) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|e| io_err("create directory for", e))?;
    }
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

/// Attaches a file to a free loop device with direct I/O; returns the device.
pub fn attach_loop(runner: &dyn Runner, file: &Path) -> Result<String, BackingError> {
    let path = file.to_string_lossy();
    let args = ["--find", "--show", "--direct-io=on", &path];
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
