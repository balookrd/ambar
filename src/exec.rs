//! Running external commands behind a trait, so the logic is testable.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
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
        // The pipes are drained while the process runs: a command that fills
        // a pipe buffer would otherwise block on write and never exit.
        let (sender, results) = mpsc::channel();
        drain(Stream::Stdout, child.stdout.take(), sender.clone());
        drain(Stream::Stderr, child.stderr.take(), sender);
        supervise(&mut child, &results, Instant::now() + timeout, program, args, timeout)
    }
}

/// Waits for the leader `child` (already started in its own group, its pipes
/// drained into `results`) until `deadline`, then collects its output. Kills
/// the group and reaps the leader on every failure path.
fn supervise(
    child: &mut Child,
    results: &mpsc::Receiver<(Stream, io::Result<Vec<u8>>)>,
    deadline: Instant,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> io::Result<Output> {
    let group = child.id();
    // The direct child is only observed here, never reaped, until the
    // group has been dealt with: while the leader is an unreaped zombie
    // its pid cannot be reused, so `kill(-group)` can only reach our own
    // group. Reaping first would free the pid and let the kill hit an
    // unrelated process group.
    loop {
        match exited_without_reaping(group) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => {
                // ECHILD: the child was reaped by someone else, so its
                // pid may belong to an unrelated process group by now.
                if may_signal_group(&e) {
                    kill_and_reap(child, group);
                }
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let reaped = kill_and_reap(child, group);
            return Err(timeout_error(
                program,
                args,
                timeout,
                stderr_after_kill(results, None),
                reaped,
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
    // The command has exited, but a background process it started can
    // still hold the pipes open; the readers then never see the end of
    // the stream. Wait for them no longer than the deadline plus a grace.
    match collect_output(results, deadline) {
        Collected::Done(stdout, stderr) => {
            let status = child.wait()?;
            Ok(Output {
                code: status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
        Collected::TimedOut(stderr) => {
            // The leader is still unreaped, so the group id is ours.
            let reaped = kill_and_reap(child, group);
            Err(timeout_error(
                program,
                args,
                timeout,
                stderr_after_kill(results, stderr),
                reaped,
            ))
        }
        Collected::Failed(e) => {
            // The leader has exited but is not reaped yet: reap it so no
            // zombie is left, and stop the rest of the group.
            kill_and_reap(child, group);
            Err(e)
        }
    }
}

/// How long to wait for a killed child to be reaped.
const REAP_LIMIT: Duration = Duration::from_secs(5);

/// Result of waiting for both readers once the leader has exited.
#[derive(Debug)]
enum Collected {
    Done(Vec<u8>, Vec<u8>),
    /// Not both readers delivered before the deadline plus the grace; carries
    /// the stderr if that one did.
    TimedOut(Option<Vec<u8>>),
    /// A reader failed or its thread is gone.
    Failed(io::Error),
}

fn collect_output(results: &mpsc::Receiver<(Stream, io::Result<Vec<u8>>)>, deadline: Instant) -> Collected {
    let (mut stdout, mut stderr) = (None, None);
    while stdout.is_none() || stderr.is_none() {
        let wait = (deadline + READER_GRACE).saturating_duration_since(Instant::now());
        match results.recv_timeout(wait) {
            Ok((Stream::Stdout, Ok(bytes))) => stdout = Some(bytes),
            Ok((Stream::Stderr, Ok(bytes))) => stderr = Some(bytes),
            Ok((_, Err(e))) => return Collected::Failed(e),
            Err(RecvTimeoutError::Timeout) => return Collected::TimedOut(stderr),
            Err(RecvTimeoutError::Disconnected) => {
                return Collected::Failed(io::Error::other("output reader thread panicked"));
            }
        }
    }
    Collected::Done(stdout.unwrap_or_default(), stderr.unwrap_or_default())
}

/// `false` for ECHILD from `waitid`: the child is not ours any more (reaped
/// elsewhere), so its group id may have been reused and must not be signalled.
fn may_signal_group(error: &io::Error) -> bool {
    error.raw_os_error() != Some(libc::ECHILD)
}

/// SIGKILL to the group, then reap the leader, waiting at most `REAP_LIMIT`.
/// `false` when the leader had not exited by then (a process stuck in the
/// kernel ignores SIGKILL until it returns); it stays a zombie of ours.
fn kill_and_reap(child: &mut Child, group: u32) -> bool {
    kill_group(group);
    reap_within(child, REAP_LIMIT)
}

/// Polls `try_wait` for up to `limit`. A child that is already gone (reaped
/// by someone else, `try_wait` fails) counts as reaped.
fn reap_within(child: &mut Child, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => return true,
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// The error for a command that was killed after its time limit.
fn timeout_error(program: &str, args: &[&str], timeout: Duration, stderr: Option<Vec<u8>>, reaped: bool) -> io::Error {
    let captured = match stderr {
        Some(bytes) if !bytes.is_empty() => format!(
            "; stderr so far: {}",
            truncate_utf8(String::from_utf8_lossy(&bytes).trim(), STDERR_IN_ERROR)
        ),
        Some(_) => "; stderr was empty".to_string(),
        None => "; its output was not available".to_string(),
    };
    let zombie = if reaped {
        String::new()
    } else {
        format!(
            "; the process could not be reaped within {REAP_LIMIT:?} after SIGKILL and stays as a zombie until the \
             agent exits"
        )
    };
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "`{}` did not finish within {:?} and was killed{}{}",
            command_line(program, args),
            timeout,
            captured,
            zombie
        ),
    )
}

/// How much of the captured stderr a timeout error carries.
const STDERR_IN_ERROR: usize = 2048;

/// The first `max` bytes of `text`, cut at a character boundary.
fn truncate_utf8(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// After the group was killed the pipes close and the readers finish; waits
/// up to `READER_GRACE` for the stderr reader. `None` when it did not
/// deliver (a process that left the group still holds the pipe) or failed.
fn stderr_after_kill(
    results: &mpsc::Receiver<(Stream, io::Result<Vec<u8>>)>,
    have: Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if have.is_some() {
        return have;
    }
    let until = Instant::now() + READER_GRACE;
    loop {
        match results.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((Stream::Stderr, bytes)) => return bytes.ok(),
            Ok((Stream::Stdout, _)) => {}
            Err(_) => return None,
        }
    }
}

/// Whether the child `pid` has exited, without reaping it (`WNOWAIT`): the
/// zombie stays, so the pid and the group id stay reserved until the caller
/// calls `Child::wait`.
fn exited_without_reaping(pid: u32) -> io::Result<bool> {
    let id = libc::id_t::from(pid);
    // SAFETY: `siginfo_t` is plain data for which all-zero is valid; `waitid`
    // writes into it. `P_PID` with the pid of our own child.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // With WNOHANG and nothing to report `si_pid` stays zero.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: filled in by a successful `waitid`.
    let reported = unsafe { info.si_pid() };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let reported = info.si_pid;
    Ok(reported != 0)
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
