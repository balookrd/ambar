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

    /// Configures a resource that is not configured in the kernel yet.
    pub fn up(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["up", volume])
    }

    /// Brings an already configured resource in line with its resource file.
    /// The return code is not a verdict: re-read the status afterwards.
    pub fn adjust(&self, volume: &str) -> Result<Output, DrbdError> {
        self.run("drbdadm", &["adjust", volume])
    }

    pub fn down(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["down", volume])
    }

    /// Declares all copies of a brand-new volume identical, so DRBD does not
    /// copy an empty device across the network. Run on one node only, and
    /// only when `ResourceStatus::ready_to_skip_initial_sync` holds.
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
