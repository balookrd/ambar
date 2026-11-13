//! Thin wrappers around `drbdadm` and `drbdsetup`.
//!
//! Return codes of `drbdadm` do not reliably tell whether an operation took
//! effect (see the experiment record, C2 and C3): callers re-read the status
//! after a state-changing command instead of trusting the code. These
//! wrappers therefore fail only when a command clearly did not run.

use crate::exec::{Output, Runner, command_line};

use super::status::{self, ResourceStatus};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DrbdError {
    #[error("cannot start {command}: {message}")]
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
/// перед CSI (P7-P9)" of docs/superpowers/specs/2026-11-12-drbd9-spike-results.md.
pub const NO_METADATA_MARKER: &str = "No valid meta data found";

/// What `drbdadm dump-md` found on the backing device of a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metadata {
    Present,
    Absent,
}

pub struct Drbd<'a> {
    pub runner: &'a dyn Runner,
}

impl Drbd<'_> {
    /// Writes fresh metadata onto the backing device of `volume`.
    /// Destroys existing metadata: call only for a backing file without the
    /// readiness marker `<volume>.img.ready`; the reconcile logic enforces it.
    pub fn create_md(&self, volume: &str, max_peers: u32) -> Result<(), DrbdError> {
        let peers = format!("--max-peers={max_peers}");
        self.checked("drbdadm", &["create-md", "--force", &peers, volume])
    }

    /// Reads the metadata of the resource that is not configured in the
    /// kernel. `Absent` only for a non-zero exit code together with
    /// `NO_METADATA_MARKER` on stderr (P7 (1)); `Present` for exit code 0
    /// (P7 (2)); any other result, such as a busy device with the resource up
    /// (P7 (3), code 20), is an error: it proves nothing about the file.
    pub fn metadata(&self, volume: &str) -> Result<Metadata, DrbdError> {
        let out = self.run("drbdadm", &["dump-md", volume])?;
        if out.success() {
            return Ok(Metadata::Present);
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

    /// Status of one resource; `None` when it is not configured in the kernel.
    pub fn status(&self, volume: &str) -> Result<Option<ResourceStatus>, DrbdError> {
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
        let all = status::parse(text).map_err(|e| DrbdError::Status(e.to_string()))?;
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
