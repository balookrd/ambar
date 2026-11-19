//! Thin wrappers around `drbdadm` and `drbdsetup`.
//!
//! Return codes of `drbdadm` do not reliably tell whether an operation took
//! effect (see the experiment record, C2 and C3): callers re-read the status
//! after a state-changing command instead of trusting the code. These
//! wrappers therefore fail only when a command clearly did not run.

use std::time::Duration;

use crate::exec::{DEFAULT_COMMAND_TIMEOUT, Output, Runner, command_line};

use super::status::{self, ResourceStatus};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DrbdError {
    #[error("`{command}` did not run to completion: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("cannot parse drbdsetup status: {0}")]
    Status(String),
    #[error("volume {volume:?}: cannot tell whether the backing file holds DRBD metadata: {detail}")]
    MetadataCheck { volume: String, detail: String },
}

/// Text `drbdadm dump-md` prints on stderr for a backing file without DRBD
/// metadata (exit code 255). Recorded in experiment P7 (1), section "Опыты
/// перед CSI (P7–P10)" of docs/superpowers/specs/2026-11-12-drbd9-spike-results.md.
pub const NO_METADATA_MARKER: &str = "No valid meta data found";

/// Start of the text `drbdadm dump-md` prints on stderr, with exit code 255
/// and an empty stdout, for metadata whose activity log was not applied:
/// `Found meta data is "unclean", please apply-al first`. Recorded in
/// experiment P10 (2)-(6), same section of the experiment record. After
/// `up` and `down` DRBD answers this way, so the metadata was attached at
/// least once.
pub const UNCLEAN_METADATA_MARKER: &str = "Found meta data is \"unclean\"";

/// What `drbdadm dump-md` found on the backing device of a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metadata {
    /// The file holds no DRBD metadata.
    Absent,
    /// Metadata exactly as `create-md` leaves it (P10 (1)): the copy was
    /// never configured in the kernel. Writing it again destroys nothing.
    Fresh,
    /// Anything else: the copy was attached or connected (P10 (2)-(6), also
    /// "unclean" metadata, which `dump-md` reports after `up` and `down`),
    /// took part in a volume, or the dump is not recognised.
    Used,
}

/// `current-uuid` of metadata that was just created (P10 (1), both runs).
const JUST_CREATED_UUID: u64 = 0x4;
/// Peer slots and `history-uuids` entries in every dump of P10, with
/// `max-peers 7`.
const DUMP_SLOTS: usize = 32;
/// Top-level fields of the dump right after `create-md` (P10 (1), both runs),
/// except `version`, `max-peers` (any value that matches the number of
/// bitmaps) and `device-uuid` (random, any hex value). Numbers are as the
/// dump prints them: `node-id` in decimal, the others as listed.
const FRESH_TOP: [(&str, i128); 10] = [
    ("node-id", -1),
    ("current-uuid", JUST_CREATED_UUID as i128),
    ("flags", 0x80),
    ("members", 0),
    ("features", 1),
    ("la-size-sect", 0),
    ("bm-byte-per-bit", 4096),
    ("la-peer-max-bio-size", 0),
    ("al-stripes", 1),
    ("al-stripe-size-4k", 8),
];

/// Decides from the stdout of a successful `drbdadm dump-md` whether the
/// metadata is in the just-created state (`Fresh`) or not (`Used`).
///
/// `Fresh` only when the dump is recognised completely and every field is at
/// the value it has right after `create-md` (P10 (1), both runs): the
/// top-level fields of `FRESH_TOP` (`node-id -1`, `current-uuid`
/// `0x0000000000000004`, `flags 0x00000080`, `members` 0, `features` 1,
/// `la-size-sect` 0, `bm-byte-per-bit 4096`, `la-peer-max-bio-size` 0,
/// `al-stripes` 1, `al-stripe-size-4k` 8), and in all 32 peer slots
/// `bitmap-index -1`, `bitmap-uuid`, `bitmap-dagtag` and `flags` 0; all
/// `history-uuids` and every bitmap are zero, `version "v09"`, `max-peers`
/// equals the number of bitmaps. The dumps of the attached or connected
/// states (P10 (2), (3)) differ in `flags`, `members`, `bitmap-index`,
/// `la-peer-max-bio-size` and the peer `flags`, so they are `Used`. Anything
/// else (another value, a repeated key, an unknown line, a missing field, a
/// truncated or empty dump) is `Used`: a wrong `Fresh` lets `create-md
/// --force` destroy a copy. Comments (the timestamp, `device-uuid` which
/// differs per run) are ignored.
pub fn classify_dump(dump: &str) -> Metadata {
    if is_just_created(dump).is_some() {
        Metadata::Fresh
    } else {
        Metadata::Used
    }
}

/// `0x` followed by hex digits only (no sign, no empty digits).
fn hex(value: &str) -> Option<u64> {
    let digits = value.strip_prefix("0x")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

fn zero_hex(value: &str) -> Option<()> {
    (hex(value)? == 0).then_some(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Top,
    Peer,
    History,
    Bitmap,
}

/// The four fields every `peer[n]` block carries.
#[derive(Default)]
struct Slot {
    index: bool,
    uuid: bool,
    dagtag: bool,
    flags: bool,
}

/// Records `value` under `key`; `None` when the key was already seen.
fn put(fields: &mut Vec<(&'static str, i128)>, key: &'static str, value: i128) -> Option<()> {
    if fields.iter().any(|(k, _)| *k == key) {
        return None;
    }
    fields.push((key, value));
    Some(())
}

/// `Some(())` when the dump is recognised and shows the just-created state.
fn is_just_created(dump: &str) -> Option<()> {
    let mut versions = 0usize;
    let mut version_ok = false;
    let mut max_peers: Option<usize> = None;
    let mut device_uuid = false;
    let mut fields: Vec<(&'static str, i128)> = Vec::new();
    let mut slots = 0usize;
    let mut history: Option<usize> = None;
    let mut bitmaps = 0usize;
    let mut bits_set: Vec<(usize, u64)> = Vec::new();
    let mut block = Block::Top;
    let mut slot = Slot::default();

    for raw in dump.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("# bits-set[") {
            let (index, count) = rest.split_once("] ")?;
            bits_set.push((index.parse().ok()?, count.strip_suffix(';')?.parse().ok()?));
            continue;
        }
        let line = line.split('#').next()?.trim();
        if line.is_empty() {
            continue;
        }
        match block {
            Block::Top => {
                if line == "history-uuids {" {
                    if history.replace(0).is_some() {
                        return None;
                    }
                    block = Block::History;
                } else if let Some(n) = line.strip_prefix("peer[").and_then(|r| r.strip_suffix("] {")) {
                    if n.parse::<usize>().ok()? != slots {
                        return None;
                    }
                    slot = Slot::default();
                    block = Block::Peer;
                } else if let Some(n) = line.strip_prefix("bitmap[").and_then(|r| r.strip_suffix("] {")) {
                    if n.parse::<usize>().ok()? != bitmaps {
                        return None;
                    }
                    block = Block::Bitmap;
                } else {
                    let (key, value) = line.split_once(' ')?;
                    let value = value.strip_suffix(';')?;
                    match key {
                        "version" => {
                            versions += 1;
                            version_ok = value == "\"v09\"";
                        }
                        "max-peers" => {
                            if max_peers.replace(value.parse().ok()?).is_some() {
                                return None;
                            }
                        }
                        "device-uuid" => {
                            hex(value)?;
                            if std::mem::replace(&mut device_uuid, true) {
                                return None;
                            }
                        }
                        "node-id" => put(&mut fields, "node-id", value.parse::<i64>().ok()?.into())?,
                        "current-uuid" => put(&mut fields, "current-uuid", hex(value)?.into())?,
                        "flags" => put(&mut fields, "flags", hex(value)?.into())?,
                        "members" => put(&mut fields, "members", hex(value)?.into())?,
                        "features" => put(&mut fields, "features", hex(value)?.into())?,
                        "la-size-sect" => put(&mut fields, "la-size-sect", value.parse::<u64>().ok()?.into())?,
                        "bm-byte-per-bit" => put(&mut fields, "bm-byte-per-bit", value.parse::<u64>().ok()?.into())?,
                        "la-peer-max-bio-size" => {
                            put(&mut fields, "la-peer-max-bio-size", value.parse::<u64>().ok()?.into())?
                        }
                        "al-stripes" => put(&mut fields, "al-stripes", value.parse::<u64>().ok()?.into())?,
                        "al-stripe-size-4k" => {
                            put(&mut fields, "al-stripe-size-4k", value.parse::<u64>().ok()?.into())?
                        }
                        _ => return None,
                    }
                }
            }
            Block::Peer => {
                if line == "}" {
                    if !(slot.index && slot.uuid && slot.dagtag && slot.flags) {
                        return None;
                    }
                    slots += 1;
                    block = Block::Top;
                    continue;
                }
                let (key, value) = line.split_once(' ')?;
                let value = value.strip_suffix(';')?;
                let seen = match key {
                    "bitmap-index" => {
                        (value.parse::<i64>().ok()? == -1).then_some(())?;
                        &mut slot.index
                    }
                    "bitmap-uuid" => {
                        zero_hex(value)?;
                        &mut slot.uuid
                    }
                    "bitmap-dagtag" => {
                        zero_hex(value)?;
                        &mut slot.dagtag
                    }
                    "flags" => {
                        zero_hex(value)?;
                        &mut slot.flags
                    }
                    _ => return None,
                };
                if std::mem::replace(seen, true) {
                    return None;
                }
            }
            Block::History => {
                if line == "}" {
                    block = Block::Top;
                    continue;
                }
                for value in line.split(';').map(str::trim).filter(|v| !v.is_empty()) {
                    zero_hex(value)?;
                    *history.as_mut()? += 1;
                }
            }
            Block::Bitmap => {
                if line == "}" {
                    bitmaps += 1;
                    block = Block::Top;
                    continue;
                }
                for token in line.split([' ', ';']).filter(|t| !t.is_empty()) {
                    if token.starts_with("0x") {
                        zero_hex(token)?;
                    } else if token != "times" && !token.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                }
            }
        }
    }

    let max_peers = max_peers.filter(|n| *n > 0)?;
    let all_zero: Vec<(usize, u64)> = (0..max_peers).map(|i| (i, 0)).collect();
    let recognised = versions == 1
        && version_ok
        && block == Block::Top
        && fields.len() == FRESH_TOP.len()
        && FRESH_TOP.iter().all(|top| fields.contains(top))
        && slots == DUMP_SLOTS
        && history == Some(DUMP_SLOTS)
        && bitmaps == max_peers
        && bits_set == all_zero;
    recognised.then_some(())
}

/// Time limit of `drbdadm create-md` for a volume of `size_bytes`: the
/// default limit at least, otherwise 120 s plus one second per started GiB
/// (1 GiB: 121 s, 1 TiB: 1144 s). The duration of `create-md` on large
/// volumes was not measured; the formula leaves room for the bitmap, which
/// grows with the volume, and is not derived from data.
pub fn create_md_timeout(size_bytes: u64) -> Duration {
    let seconds = 120u64.saturating_add(size_bytes.div_ceil(1 << 30));
    DEFAULT_COMMAND_TIMEOUT.max(Duration::from_secs(seconds))
}

pub struct Drbd<'a> {
    pub runner: &'a dyn Runner,
}

impl Drbd<'_> {
    /// Writes fresh metadata onto the backing device of `volume`, a volume
    /// of `size_bytes`; the size only sets the time limit (`create_md_timeout`).
    /// Destroys existing metadata: call only for a backing file without the
    /// readiness marker `<volume>.img.ready`; the reconcile logic enforces it.
    pub fn create_md(&self, volume: &str, max_peers: u32, size_bytes: u64) -> Result<(), DrbdError> {
        let peers = format!("--max-peers={max_peers}");
        let args = ["create-md", "--force", &peers, volume];
        let out = self
            .runner
            .run_with_timeout("drbdadm", &args, create_md_timeout(size_bytes))
            .map_err(|e| DrbdError::Spawn {
                command: command_line("drbdadm", &args),
                message: e.to_string(),
            })?;
        Self::judge("drbdadm", &args, out)
    }

    /// Reads the metadata of the resource that is not configured in the
    /// kernel. For exit code 0 the dump decides (`classify_dump`): `Fresh`
    /// for the state right after `create-md`, `Used` for everything else,
    /// including a dump that cannot be read (P7 (2), P10). For a non-zero
    /// exit code stderr decides, in this order: `UNCLEAN_METADATA_MARKER` is
    /// `Used` (P10 (2)-(6): the metadata was attached at least once), then
    /// `NO_METADATA_MARKER` is `Absent` (P7 (1)); when both appear the answer
    /// is `Used`. Any other failure is an error: it proves nothing about the
    /// file. That includes a busy device with the resource up (P7 (3), code
    /// 20).
    pub fn metadata(&self, volume: &str) -> Result<Metadata, DrbdError> {
        let out = self.run("drbdadm", &["dump-md", volume])?;
        if out.success() {
            return Ok(classify_dump(&out.stdout));
        }
        if out.stderr.contains(UNCLEAN_METADATA_MARKER) {
            return Ok(Metadata::Used);
        }
        if out.stderr.contains(NO_METADATA_MARKER) {
            return Ok(Metadata::Absent);
        }
        Err(DrbdError::MetadataCheck {
            volume: volume.to_string(),
            detail: format!("drbdadm dump-md failed with code {}: {}", out.code, out.stderr.trim()),
        })
    }

    /// Configures a resource that is not configured in the kernel yet.
    pub fn up(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["up", volume])
    }

    /// Brings an already configured resource in line with its resource file.
    /// The caller reads the exit code from the `Output`: non-zero means the
    /// kernel rejected the change, zero is not proof, so re-read the status
    /// as well.
    pub fn adjust(&self, volume: &str) -> Result<Output, DrbdError> {
        self.run("drbdadm", &["adjust", volume])
    }

    pub fn down(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["down", volume])
    }

    /// Declares all copies of a brand-new volume identical, so DRBD does not
    /// copy an empty device across the network. Run on one node only: the
    /// leader (lowest `nodeId` among the nodes with a copy), only when
    /// `ResourceStatus::ready_to_skip_initial_sync` holds and the local
    /// synced marker is absent; the reconcile logic enforces all three.
    pub fn skip_initial_sync(&self, volume: &str) -> Result<(), DrbdError> {
        let target = format!("{volume}/0");
        self.checked("drbdadm", &["new-current-uuid", "--clear-bitmap", &target])
    }

    /// Status of all resources currently configured in the kernel.
    pub fn all_statuses(&self) -> Result<Vec<ResourceStatus>, DrbdError> {
        let args = ["status", "--json"];
        let out = self.run("drbdsetup", &args)?;
        if !out.success() {
            return Err(DrbdError::Command {
                command: command_line("drbdsetup", &args),
                code: out.code,
                stderr: out.stderr.trim().to_string(),
            });
        }
        let text = out.stdout.trim();
        if !text.starts_with('[') {
            // Observed on the stand: with nothing configured drbdsetup still
            // prints a JSON array (empty) and returns 0, so any other output
            // is an error.
            let first_line = text.lines().next().unwrap_or("");
            return Err(DrbdError::Status(format!("unexpected output: {first_line}")));
        }
        status::parse(text).map_err(|e| DrbdError::Status(e.to_string()))
    }

    /// Status of one resource; `None` when it is not configured in the kernel.
    pub fn status(&self, volume: &str) -> Result<Option<ResourceStatus>, DrbdError> {
        let all = self.all_statuses()?;
        Ok(all.into_iter().find(|r| r.name == volume))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<Output, DrbdError> {
        self.runner.run(program, args).map_err(|e| DrbdError::Spawn {
            command: command_line(program, args),
            message: e.to_string(),
        })
    }

    fn checked(&self, program: &str, args: &[&str]) -> Result<(), DrbdError> {
        let out = self.run(program, args)?;
        Self::judge(program, args, out)
    }

    fn judge(program: &str, args: &[&str], out: Output) -> Result<(), DrbdError> {
        if out.success() {
            return Ok(());
        }
        Err(DrbdError::Command {
            command: command_line(program, args),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        })
    }
}

#[cfg(test)]
#[path = "tests/adm.rs"]
mod tests;
