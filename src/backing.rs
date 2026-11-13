//! The backing store of a copy: a sparse file attached as a loop device.

use std::fs::{self, OpenOptions};
use std::io;
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
}

/// Size of DRBD's internal metadata on a backing device of `device_bytes`
/// for `max_peers` peers: 8 sectors of bitmap per extent and peer, plus 72
/// sectors of fixed overhead. Matches three measurements on the stand: a
/// 1 GiB file lost 100 KiB with two peers, 68 KiB with one peer and 260 KiB
/// with `--max-peers=7`.
pub fn metadata_bytes(device_bytes: u64, max_peers: u32) -> u64 {
    let extents = device_bytes.div_ceil(SECTOR).div_ceil(SECTORS_PER_EXTENT);
    (extents * 8 * u64::from(max_peers) + 72) * SECTOR
}

/// Size of the backing file that leaves at least `size_bytes` usable after
/// the metadata is taken off its end. Rounded up to 4 KiB.
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

/// Creates a sparse file of `bytes`. Fails if the file already exists.
pub fn create_sparse_file(path: &Path, bytes: u64) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_err("create", e))?;
    file.set_len(bytes).map_err(|e| io_err("resize", e))
}

/// The loop device a file is attached to, if any.
pub fn find_loop(runner: &dyn Runner, file: &Path) -> Result<Option<String>, BackingError> {
    let path = file.to_string_lossy();
    let out = checked(runner, "losetup", &["-j", &path])?;
    // One line per device: "/dev/loop0: [0048]:1234 (/path/to/file)".
    Ok(out
        .lines()
        .next()
        .and_then(|line| line.split(':').next())
        .map(str::to_string)
        .filter(|d| !d.is_empty()))
}

/// Attaches a file to a free loop device with direct I/O; returns the device.
pub fn attach_loop(runner: &dyn Runner, file: &Path) -> Result<String, BackingError> {
    let path = file.to_string_lossy();
    let out = checked(runner, "losetup", &["--find", "--show", "--direct-io=on", &path])?;
    Ok(out.trim().to_string())
}

pub fn detach_loop(runner: &dyn Runner, device: &str) -> Result<(), BackingError> {
    checked(runner, "losetup", &["-d", device]).map(|_| ())
}

/// Points `link` at `device`, replacing whatever was there.
pub fn link_device(link: &Path, device: &str) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: link.display().to_string(),
        source,
    };
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    match fs::remove_file(link) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err("remove", e)),
    }
    std::os::unix::fs::symlink(device, link).map_err(|e| io_err("link", e))
}

/// Where `link` points, or `None` when it does not exist.
pub fn link_target(link: &Path) -> Option<String> {
    fs::read_link(link).ok().map(|p| p.to_string_lossy().into_owned())
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
