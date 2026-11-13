//! Mounting volumes: `/proc/self/mountinfo` parsing, the filesystem
//! signature check that gates `mkfs`, and idempotent mount / bind / unmount.
//!
//! Safety rule: `mkfs.ext4` on a device that holds a filesystem destroys the
//! data, so `Signature::Empty` is returned only when two independent checks
//! agree: this agent's own read of the ext4 magic and `blkid`. Any error or
//! disagreement is an error, never `Empty`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::exec::{Output, Runner, command_line};

/// Offset of the ext4 superblock magic: superblock at 1024 plus 56.
const EXT4_MAGIC_OFFSET: u64 = 1080;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xef];

/// `blkid -p -o value -s TYPE <dev>` on a device without a filesystem exits
/// with 2 and prints nothing to stdout and stderr (experiment P9, state 1,
/// section "Опыты перед CSI (P7–P9)" of
/// docs/superpowers/specs/2026-11-12-drbd9-spike-results.md). The same code 2
/// comes with `Wrong medium type` on stderr for a device open on another node
/// (state 3) and with empty output for a missing device (state 4), so the
/// code alone never means "empty".
const BLKID_NOTHING_FOUND_CODE: i32 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum Signature {
    Ext4,
    Empty,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Root of the mount within its filesystem (mountinfo field 4). `/` for a
    /// whole-filesystem mount, a subdirectory for some bind mounts.
    pub root: String,
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
    /// `ro` among the per-mount options (mountinfo field 6).
    pub read_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MountError {
    /// open(2) or read of the device failed; `in_use` is true for EMEDIUMTYPE
    /// (the device is open on another node, P9 state 3).
    #[error("cannot read {device}{}: {source}", if *.in_use { " (in use on another node)" } else { "" })]
    Open {
        device: String,
        in_use: bool,
        source: io::Error,
    },
    #[error("`{command}` failed (code {code:?}): {stderr}")]
    Command {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    /// The two signature checks disagree, or blkid failed in a way that is
    /// not "nothing found".
    #[error("cannot tell whether {device} holds a filesystem: {detail}")]
    AmbiguousSignature { device: String, detail: String },
    #[error("{device} holds {found}, not ext4")]
    WrongFilesystem { device: String, found: String },
    #[error("{target} is already a mount point of {source_mounted}")]
    Busy { target: String, source_mounted: String },
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: io::Error,
    },
}

/// Decodes the octal escapes mountinfo uses (`\040`, `\011`, `\012`, `\134`).
/// A backslash not followed by three octal digits is kept as is.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..=i + 3].iter().all(|b| (b'0'..=b'7').contains(b)) {
            let value = u32::from(bytes[i + 1] - b'0') * 64
                + u32::from(bytes[i + 2] - b'0') * 8
                + u32::from(bytes[i + 3] - b'0');
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/self/mountinfo`. Lines without the ` - ` separator or with
/// too few fields are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<MountEntry> {
    let (left, right) = line.split_once(" - ")?;
    // id parent major:minor root mount-point options [optional fields...]
    let left: Vec<&str> = left.split_whitespace().collect();
    if left.len() < 6 {
        return None;
    }
    // fstype source super-options
    let right: Vec<&str> = right.split_whitespace().collect();
    if right.len() < 2 {
        return None;
    }
    Some(MountEntry {
        root: unescape(left[3]),
        target: PathBuf::from(unescape(left[4])),
        read_only: left[5].split(',').any(|option| option == "ro"),
        fstype: right[0].to_string(),
        source: unescape(right[1]),
    })
}

/// The entry mounted at `target`. A later line overlays an earlier one, so
/// the last match wins.
pub fn mounted_at<'a>(entries: &'a [MountEntry], target: &Path) -> Option<&'a MountEntry> {
    entries.iter().rev().find(|entry| entry.target == target)
}

/// Whether an open(2) error means "the device is open on another node".
#[cfg(target_os = "linux")]
pub fn open_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EMEDIUMTYPE)
}

#[cfg(not(target_os = "linux"))]
pub fn open_error(_error: &io::Error) -> bool {
    false
}

pub struct Mounter<'a> {
    pub runner: &'a dyn Runner,
    pub mountinfo: &'a Path,
}

impl Mounter<'_> {
    /// Opens the device read-only, reads the ext4 magic and asks blkid.
    ///
    /// | magic at 1080 | blkid | result |
    /// |---|---|---|
    /// | open/read error | not run | `Open` |
    /// | fewer than 2 bytes | not run | `AmbiguousSignature` |
    /// | `53 ef` | code 0, `ext4` | `Ext4` |
    /// | other | code 2, empty stdout and stderr | `Empty` |
    /// | other | code 0, a type other than `ext4` | `Other(type)` |
    /// | anything else | | `AmbiguousSignature` |
    pub fn signature(&self, device: &Path) -> Result<Signature, MountError> {
        let name = device.to_string_lossy().into_owned();
        let has_magic = read_magic(device, &name)?;

        let args = ["-p", "-o", "value", "-s", "TYPE", name.as_str()];
        let ambiguous = |detail: String| MountError::AmbiguousSignature {
            device: name.clone(),
            detail,
        };
        let output = self
            .runner
            .run("blkid", &args)
            .map_err(|e| ambiguous(format!("`{}` did not run: {e}", command_line("blkid", &args))))?;
        let found = output.stdout.trim();
        match output.code {
            0 => match (has_magic, found) {
                (true, "ext4") => Ok(Signature::Ext4),
                (false, "") | (true, "") => Err(ambiguous("blkid exited 0 with no type".to_string())),
                (true, other) => Err(ambiguous(format!("ext4 magic is present but blkid reports {other:?}"))),
                (false, "ext4") => Err(ambiguous("blkid reports ext4 but the magic is absent".to_string())),
                (false, other) => Ok(Signature::Other(other.to_string())),
            },
            BLKID_NOTHING_FOUND_CODE => {
                if !has_magic && output.stdout.is_empty() && output.stderr.is_empty() {
                    Ok(Signature::Empty)
                } else {
                    Err(ambiguous(format!(
                        "blkid exited {BLKID_NOTHING_FOUND_CODE}: magic present: {has_magic}, stdout {:?}, stderr {:?}",
                        output.stdout, output.stderr
                    )))
                }
            }
            code => Err(ambiguous(format!(
                "blkid exited {code}: stderr {:?}",
                output.stderr.trim()
            ))),
        }
    }

    /// `mkfs.ext4 -q <device>`. The caller runs it only after
    /// `signature` returned `Empty`.
    pub fn mkfs_ext4(&self, device: &Path) -> Result<(), MountError> {
        let name = device.to_string_lossy().into_owned();
        self.run_checked("mkfs.ext4", &["-q", &name]).map(|_| ())
    }

    /// Idempotent: Ok when `target` already has `device` mounted; `Busy` when
    /// something else is mounted there. The source is compared as text.
    pub fn mount_device(&self, device: &Path, target: &Path) -> Result<(), MountError> {
        let device_name = device.to_string_lossy().into_owned();
        let entries = self.entries()?;
        if let Some(entry) = mounted_at(&entries, target) {
            if entry.source != device_name {
                return Err(busy(target, entry));
            }
            if entry.fstype != "ext4" {
                return Err(MountError::WrongFilesystem {
                    device: device_name,
                    found: entry.fstype.clone(),
                });
            }
            return Ok(());
        }
        let target_name = target.to_string_lossy().into_owned();
        self.run_checked("mount", &["-t", "ext4", &device_name, &target_name])
            .map(|_| ())
    }

    /// Idempotent bind mount. An existing mount at `target` counts as done
    /// when `source` is itself a mount point and both entries have the same
    /// filesystem source (mountinfo field after ` - `) and the same root
    /// (field 4): a bind mount shows the same device and root as the mount
    /// it was made from. If `source` is not a mount point the match cannot be
    /// proved and the result is `Busy`. With `read_only`, an existing
    /// read-write bind is remounted read-only.
    pub fn bind(&self, source: &Path, target: &Path, read_only: bool) -> Result<(), MountError> {
        let entries = self.entries()?;
        let source_name = source.to_string_lossy().into_owned();
        let target_name = target.to_string_lossy().into_owned();
        if let Some(existing) = mounted_at(&entries, target) {
            let same = mounted_at(&entries, source)
                .is_some_and(|origin| origin.source == existing.source && origin.root == existing.root);
            if !same {
                return Err(busy(target, existing));
            }
            if read_only && !existing.read_only {
                return self.remount_read_only(&target_name);
            }
            return Ok(());
        }
        self.run_checked("mount", &["--bind", &source_name, &target_name])?;
        if read_only {
            self.remount_read_only(&target_name)?;
        }
        Ok(())
    }

    /// Idempotent: Ok when nothing is mounted at `target`.
    pub fn unmount(&self, target: &Path) -> Result<(), MountError> {
        if mounted_at(&self.entries()?, target).is_none() {
            return Ok(());
        }
        let target_name = target.to_string_lossy().into_owned();
        self.run_checked("umount", &[&target_name]).map(|_| ())
    }

    fn remount_read_only(&self, target: &str) -> Result<(), MountError> {
        self.run_checked("mount", &["-o", "remount,bind,ro", target])
            .map(|_| ())
    }

    fn entries(&self) -> Result<Vec<MountEntry>, MountError> {
        let text = std::fs::read_to_string(self.mountinfo).map_err(|source| MountError::Io {
            action: "read",
            path: self.mountinfo.display().to_string(),
            source,
        })?;
        Ok(parse_mountinfo(&text))
    }

    fn run_checked(&self, program: &str, args: &[&str]) -> Result<Output, MountError> {
        let command = command_line(program, args);
        let output = self.runner.run(program, args).map_err(|e| MountError::Command {
            command: command.clone(),
            code: None,
            stderr: e.to_string(),
        })?;
        if output.success() {
            Ok(output)
        } else {
            Err(MountError::Command {
                command,
                code: Some(output.code),
                stderr: output.stderr.trim().to_string(),
            })
        }
    }
}

fn busy(target: &Path, entry: &MountEntry) -> MountError {
    MountError::Busy {
        target: target.display().to_string(),
        source_mounted: entry.source.clone(),
    }
}

/// Reads the two bytes at the ext4 magic offset. `Ok(true)` when they equal
/// the magic. A short read is ambiguous, never "no magic".
fn read_magic(device: &Path, name: &str) -> Result<bool, MountError> {
    let open = |source: io::Error| MountError::Open {
        device: name.to_string(),
        in_use: open_error(&source),
        source,
    };
    let file = File::open(device).map_err(open)?;
    let mut magic = [0u8; 2];
    match file.read_exact_at(&mut magic, EXT4_MAGIC_OFFSET) {
        Ok(()) => Ok(magic == EXT4_MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(MountError::AmbiguousSignature {
            device: name.to_string(),
            detail: format!("fewer than 2 bytes at offset {EXT4_MAGIC_OFFSET}"),
        }),
        Err(e) => Err(open(e)),
    }
}

#[cfg(test)]
#[path = "tests/mount.rs"]
mod tests;
