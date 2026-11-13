//! Running external commands behind a trait, so the logic is testable.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// How long `SystemRunner::default()` lets a command run.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a running command is checked for completion.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Exit code; -1 when the process was killed by a signal.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

pub trait Runner: Send + Sync {
    /// Runs a program and waits for it. `Err` means the command did not run
    /// to completion: it could not be started, or it was killed after a
    /// time limit (`io::ErrorKind::TimedOut`).
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output>;

    /// Like `run`, with an explicit time limit instead of the runner's own.
    /// The default ignores the limit, so a fake that only implements `run`
    /// keeps working.
    fn run_with_timeout(&self, program: &str, args: &[&str], timeout: Duration) -> io::Result<Output> {
        let _ = timeout;
        self.run(program, args)
    }
}

/// Runs real processes.
pub struct SystemRunner {
    /// A command still running after this long is killed and reported as an error.
    pub timeout: Duration,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

/// Extra time after the deadline for the reader threads to hand over what
/// they read once the command has exited.
const READER_GRACE: Duration = Duration::from_secs(1);

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.run_with_timeout(program, args, self.timeout)
    }

    /// Every command runs in its own process group. On a timeout the whole
    /// group gets SIGKILL: `drbdadm` starts `drbdmeta` and `drbdsetup` as
    /// children, and killing only the parent would leave them running after
    /// the caller has gone on (and released its lock).
    fn run_with_timeout(&self, program: &str, args: &[&str], timeout: Duration) -> io::Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let group = child.id();
        let timed_out = || {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "`{}` did not finish within {:?} and was killed",
                    command_line(program, args),
                    timeout
                ),
            )
        };
        // The pipes are drained while the process runs: a command that fills
        // a pipe buffer would otherwise block on write and never exit.
        let (sender, results) = mpsc::channel();
        drain(Stream::Stdout, child.stdout.take(), sender.clone());
        drain(Stream::Stderr, child.stderr.take(), sender);
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    kill_group(group);
                    let _ = child.wait();
                    return Err(e);
                }
            }
            if Instant::now() >= deadline {
                kill_group(group);
                let _ = child.wait();
                return Err(timed_out());
            }
            thread::sleep(POLL_INTERVAL);
        };
        // The command has exited, but a background process it started can
        // still hold the pipes open; the readers then never see the end of
        // the stream. Wait for them no longer than the deadline plus a grace.
        let (mut stdout, mut stderr) = (None, None);
        while stdout.is_none() || stderr.is_none() {
            let wait = (deadline + READER_GRACE).saturating_duration_since(Instant::now());
            match results.recv_timeout(wait) {
                Ok((Stream::Stdout, bytes)) => stdout = Some(bytes?),
                Ok((Stream::Stderr, bytes)) => stderr = Some(bytes?),
                Err(RecvTimeoutError::Timeout) => {
                    kill_group(group);
                    return Err(timed_out());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("output reader thread panicked"));
                }
            }
        }
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout.unwrap_or_default()).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// Reads `pipe` to the end on its own thread and sends the bytes over `sender`.
fn drain<R: Read + Send + 'static>(
    stream: Stream,
    pipe: Option<R>,
    sender: mpsc::Sender<(Stream, io::Result<Vec<u8>>)>,
) {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = match pipe {
            Some(mut pipe) => pipe.read_to_end(&mut bytes).map(|_| bytes),
            None => Ok(bytes),
        };
        // The receiver is gone when the caller gave up; nothing to report.
        let _ = sender.send((stream, result));
    });
}

/// SIGKILL to every process of the group `pgid` (the pid of the group leader
/// that `process_group(0)` made). Best effort: the group may be gone already.
fn kill_group(pgid: u32) {
    if let Ok(pgid) = i32::try_from(pgid)
        && pgid > 0
    {
        // SAFETY: `kill` takes plain integers; `-pgid` addresses the group of
        // a child the caller started, never pid 0 or -1 (guarded above).
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Renders a command line for error messages and logs. An argument that is
/// empty or contains whitespace or a double quote is wrapped in double quotes
/// (`"` and `\` inside are escaped), so `["a b"]` and `["a", "b"]` differ.
pub fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = String::from(program);
    for arg in args {
        line.push(' ');
        if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || c == '"') {
            line.push('"');
            for c in arg.chars() {
                if c == '"' || c == '\\' {
                    line.push('\\');
                }
                line.push(c);
            }
            line.push('"');
        } else {
            line.push_str(arg);
        }
    }
    line
}

#[cfg(test)]
#[path = "tests/exec.rs"]
mod tests;
