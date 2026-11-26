//! Mounting volumes: `/proc/self/mountinfo` parsing, the filesystem
//! signature check that gates `mkfs`, and idempotent mount / bind / unmount.
//!
//! Safety rule: `mkfs.ext4` on a device that holds a filesystem destroys the
//! data, so `Signature::Empty` is returned only when independent checks
//! agree: this agent's own reads (primary and backup ext4 superblock magic)
//! and `blkid`. Any error or disagreement is an error, never `Empty`.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, FileTypeExt};
use std::path::{Path, PathBuf};

use crate::exec::{Output, Runner, command_line};

/// Offset of the ext4 superblock magic: superblock at 1024 plus 56.
const EXT4_MAGIC_OFFSET: u64 = 1080;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xef];

/// Magic offset of the first backup superblock for block sizes 1, 2 and
/// 4 KiB: (first block of the backup, block size) with the 56-byte magic
/// offset inside the superblock added. `wipefs -a` zeroes only the primary
/// magic, so a magic here with an absent primary one means a damaged
/// filesystem, not an empty device.
const EXT4_BACKUP_MAGIC_OFFSETS: [u64; 3] = [8193 * 1024 + 56, 16384 * 2048 + 56, 32768 * 4096 + 56];

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
    #[error("{device} is not a block device")]
    NotABlockDevice { device: String },
    #[error("{target} is already a mount point of {source_mounted}")]
    Busy { target: String, source_mounted: String },
    /// A read-write bind was requested at a target that already carries a
    /// read-only bind of the same source.
    #[error("{target} is already bound read-only")]
    ModeMismatch { target: String },
    #[error("{target} is still mounted after {attempts} umount attempts")]
    StillMounted { target: String, attempts: usize },
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
    let mut left_it = left.split_whitespace();
    let _id = left_it.next()?;
    let _parent = left_it.next()?;
    let _major_minor = left_it.next()?;
    let root = left_it.next()?;
    let target = left_it.next()?;
    let options = left_it.next()?;

    // fstype source super-options
    let mut right_it = right.split_whitespace();
    let fstype = right_it.next()?;
    let source = right_it.next()?;

    Some(MountEntry {
        root: unescape(root),
        target: PathBuf::from(unescape(target)),
        read_only: options.split(',').any(|option| option == "ro"),
        fstype: fstype.to_string(),
        source: unescape(source),
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
    /// Test-only: accept a regular file as the device in `signature`. `false`
    /// in production construction.
    pub allow_regular_files: bool,
}

/// How many times `unmount` calls `umount` on a stack of mounts.
const MAX_UMOUNT_ATTEMPTS: usize = 8;

impl Mounter<'_> {
    /// Opens the device read-only, reads the ext4 magic at the primary and
    /// the backup superblocks and asks blkid.
    ///
    /// `Empty` requires all of: the open and the primary read succeed with
    /// non-magic bytes; the device is a block device; no backup superblock
    /// magic at a block size whose offset lies inside the device; blkid exit
    /// code 2; blkid stdout empty; blkid stderr empty.
    ///
    /// | opened | primary magic at 1080 | backup magic | blkid | result |
    /// |---|---|---|---|---|
    /// | open/read error | | | not run | `Open` |
    /// | not a block device | | | not run | `NotABlockDevice` |
    /// | | fewer than 2 bytes | | not run | `AmbiguousSignature` |
    /// | | absent | present | not run | `AmbiguousSignature` |
    /// | | `53 ef` | | code 0, `ext4` | `Ext4` |
    /// | | absent | absent | code 2, empty stdout and stderr | `Empty` |
    /// | | absent | absent | code 0, a type other than `ext4` | `Other(type)` |
    /// | | anything else | | | `AmbiguousSignature` |
    pub fn signature(&self, device: &Path) -> Result<Signature, MountError> {
        let name = device.to_string_lossy().into_owned();
        let has_magic = self.read_magics(device, &name)?;

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

    /// `mkfs.ext4 -q -E nodiscard <device>`. The caller runs it only after
    /// `signature` returned `Empty`. `nodiscard` limits the damage of a wrong
    /// decision (no TRIM of the whole device); the discard behaviour through
    /// DRBD over a loop device was not measured [not measured].
    pub fn mkfs_ext4(&self, device: &Path) -> Result<(), MountError> {
        let name = device.to_string_lossy().into_owned();
        self.run_checked("mkfs.ext4", &["-q", "-E", "nodiscard", &name])
            .map(|_| ())
    }

    /// Idempotent: Ok when `target` already has `device` mounted; `Busy` when
    /// something else is mounted there. The source is compared as text with
    /// the mountinfo source, so pass the device as `/dev/drbd<minor>`, not
    /// through a symlink. `target` is resolved with `fs::canonicalize` when it
    /// exists.
    pub fn mount_device(&self, device: &Path, target: &Path) -> Result<(), MountError> {
        let device_name = device.to_string_lossy().into_owned();
        let target = &resolve(target)?;
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
    /// read-write bind is remounted read-only; without it, an existing
    /// read-only bind is `ModeMismatch`. Source and target are resolved with
    /// `fs::canonicalize` when they exist.
    pub fn bind(&self, source: &Path, target: &Path, read_only: bool) -> Result<(), MountError> {
        let (source, target) = (&resolve(source)?, &resolve(target)?);
        let entries = self.entries()?;
        let source_name = source.to_string_lossy().into_owned();
        let target_name = target.to_string_lossy().into_owned();
        if let Some(existing) = mounted_at(&entries, target) {
            let same = mounted_at(&entries, source)
                .is_some_and(|origin| origin.source == existing.source && origin.root == existing.root);
            if !same {
                return Err(busy(target, existing));
            }
            if !read_only && existing.read_only {
                return Err(MountError::ModeMismatch { target: target_name });
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

    /// Idempotent: Ok when nothing is mounted at `target`. `target` is
    /// resolved with `fs::canonicalize`; a path that does not exist has
    /// nothing mounted. A stack of mounts is unmounted one `umount` at a time,
    /// re-reading mountinfo after each, at most `MAX_UMOUNT_ATTEMPTS` times.
    pub fn unmount(&self, target: &Path) -> Result<(), MountError> {
        let target = &resolve(target)?;
        let target_name = target.to_string_lossy().into_owned();
        for _ in 0..MAX_UMOUNT_ATTEMPTS {
            if mounted_at(&self.entries()?, target).is_none() {
                return Ok(());
            }
            self.run_checked("umount", &[&target_name])?;
        }
        if mounted_at(&self.entries()?, target).is_none() {
            return Ok(());
        }
        Err(MountError::StillMounted {
            target: target_name,
            attempts: MAX_UMOUNT_ATTEMPTS,
        })
    }

    fn remount_read_only(&self, target: &str) -> Result<(), MountError> {
        self.run_checked("mount", &["-o", "remount,bind,ro", target])
            .map(|_| ())
    }

    pub fn entries(&self) -> Result<Vec<MountEntry>, MountError> {
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

/// `fs::canonicalize` for a path that exists; a path that does not exist is
/// returned as given. Any other error is an error: in doubt, not "unmounted".
pub fn resolve(path: &Path) -> Result<PathBuf, MountError> {
    match std::fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(source) => Err(MountError::Io {
            action: "resolve",
            path: path.display().to_string(),
            source,
        }),
    }
}

impl Mounter<'_> {
    /// Opens the device once and checks its type. `Ok(true)` when the primary
    /// magic is present. A primary magic that is absent while a backup magic
    /// is present, a short primary read or a short read inside the device is
    /// ambiguous, never "no magic". Backup offsets at or beyond the end of
    /// the device (end found by seeking to it) are skipped.
    fn read_magics(&self, device: &Path, name: &str) -> Result<bool, MountError> {
        let open = |source: io::Error| MountError::Open {
            device: name.to_string(),
            in_use: open_error(&source),
            source,
        };
        let ambiguous = |detail: String| MountError::AmbiguousSignature {
            device: name.to_string(),
            detail,
        };
        let file = File::open(device).map_err(open)?;
        let file_type = file.metadata().map_err(open)?.file_type();
        if !(file_type.is_block_device() || (self.allow_regular_files && file_type.is_file())) {
            return Err(MountError::NotABlockDevice {
                device: name.to_string(),
            });
        }
        let mut magic = [0u8; 2];
        match file.read_exact_at(&mut magic, EXT4_MAGIC_OFFSET) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ambiguous(format!("fewer than 2 bytes at offset {EXT4_MAGIC_OFFSET}")));
            }
            Err(e) => return Err(open(e)),
        }
        if magic == EXT4_MAGIC {
            return Ok(true);
        }
        let size = (&file).seek(SeekFrom::End(0)).map_err(open)?;
        for offset in EXT4_BACKUP_MAGIC_OFFSETS {
            if offset + 2 > size {
                continue;
            }
            file.read_exact_at(&mut magic, offset).map_err(open)?;
            if magic == EXT4_MAGIC {
                return Err(ambiguous(format!(
                    "ext4 magic at the backup superblock offset {offset} but not at {EXT4_MAGIC_OFFSET}"
                )));
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "tests/mount.rs"]
mod tests;
