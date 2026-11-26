//! Running external commands behind a trait, so the logic is testable.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// How long `SystemRunner::default()` lets a command run.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum `poll(2)` slice while waiting for a running command to exit.
const POLL_SLICE: Duration = Duration::from_millis(5);

/// PIDs of timed-out leaders that could not be reaped within `REAP_LIMIT`
/// (e.g. stuck in kernel D-state) and are retried on subsequent runs.
static UNREAPED_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

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

/// Extra time after the deadline to drain pipes once the command has exited.
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
        reap_lingering_zombies();
        let mut child = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        supervise(&mut child, Instant::now() + timeout, program, args, timeout)
    }
}

/// Retries reaping any previously timed-out leader that remained unreaped
/// (e.g. a process that was in uninterruptible D-state when `REAP_LIMIT` expired).
fn reap_lingering_zombies() {
    let Ok(mut pids) = UNREAPED_PIDS.lock() else {
        return;
    };
    pids.retain(|&pid| {
        let Ok(pid_i32) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: non-blocking `waitpid` on a child PID we previously spawned.
        let rc = unsafe { libc::waitpid(pid_i32, std::ptr::null_mut(), libc::WNOHANG) };
        if rc > 0 {
            return false;
        }
        if rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return false;
        }
        true
    });
}

fn record_unreaped_pid(pid: u32) {
    if let Ok(mut pids) = UNREAPED_PIDS.lock()
        && !pids.contains(&pid)
    {
        pids.push(pid);
    }
}

/// Waits for the leader `child` (already started in its own group) until
/// `deadline` while draining its `stdout`/`stderr` via single-threaded `poll(2)`,
/// then collects any remaining output. Kills the group and reaps the leader on
/// every failure path.
fn supervise(
    child: &mut Child,
    deadline: Instant,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> io::Result<Output> {
    let group = child.id();
    let mut stdout = PipeBuffer::new(child.stdout.take())?;
    let mut stderr = PipeBuffer::new(child.stderr.take())?;

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
                if may_signal_group(&e) && !kill_and_reap(child, group) {
                    record_unreaped_pid(group);
                }
                return Err(e);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let reaped = kill_and_reap(child, group);
            if !reaped {
                record_unreaped_pid(group);
            }
            let _ = stderr.drain_available();
            return Err(timeout_error(program, args, timeout, Some(stderr.data), reaped));
        }

        let remaining = deadline.saturating_duration_since(now);
        if stdout.is_open() || stderr.is_open() {
            if let Err(e) = poll_pipes(stdout.fd(), stderr.fd(), remaining.min(POLL_SLICE)) {
                if !kill_and_reap(child, group) {
                    record_unreaped_pid(group);
                }
                return Err(e);
            }
            if let Err(e) = stdout.drain_available().and_then(|()| stderr.drain_available()) {
                if !kill_and_reap(child, group) {
                    record_unreaped_pid(group);
                }
                return Err(e);
            }
        } else {
            thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }

    // The command has exited, but a background process it started can
    // still hold the pipes open; drain until EOF or `deadline + READER_GRACE`.
    match drain_after_exit(&mut stdout, &mut stderr, deadline + READER_GRACE) {
        Ok(true) => {
            let status = child.wait()?;
            Ok(Output {
                code: status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&stdout.data).into_owned(),
                stderr: String::from_utf8_lossy(&stderr.data).into_owned(),
            })
        }
        Ok(false) => {
            // The leader is still unreaped, so the group id is ours.
            let reaped = kill_and_reap(child, group);
            if !reaped {
                record_unreaped_pid(group);
            }
            let _ = stderr.drain_available();
            Err(timeout_error(program, args, timeout, Some(stderr.data), reaped))
        }
        Err(e) => {
            // The leader has exited but is not reaped yet: reap it so no
            // zombie is left, and stop the rest of the group.
            if !kill_and_reap(child, group) {
                record_unreaped_pid(group);
            }
            Err(e)
        }
    }
}

/// Drains remaining bytes from `stdout` and `stderr` after the leader has exited.
/// Returns `Ok(true)` if both pipes reached EOF before `grace_deadline`, or
/// `Ok(false)` if a background process kept a pipe open past `grace_deadline`.
fn drain_after_exit<R1: Read + AsRawFd, R2: Read + AsRawFd>(
    stdout: &mut PipeBuffer<R1>,
    stderr: &mut PipeBuffer<R2>,
    grace_deadline: Instant,
) -> io::Result<bool> {
    loop {
        stdout.drain_available()?;
        stderr.drain_available()?;
        if !stdout.is_open() && !stderr.is_open() {
            return Ok(true);
        }
        let now = Instant::now();
        if now >= grace_deadline {
            return Ok(false);
        }
        poll_pipes(stdout.fd(), stderr.fd(), grace_deadline.saturating_duration_since(now))?;
    }
}

/// Waits on `stdout_fd` and `stderr_fd` until at least one is ready/closed or
/// `timeout` elapses. `EINTR` is treated as a spurious wakeup (`Ok(())`).
fn poll_pipes(stdout_fd: Option<RawFd>, stderr_fd: Option<RawFd>, timeout: Duration) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let mut nfds = 0;
    if let Some(fd) = stdout_fd {
        fds[nfds].fd = fd;
        nfds += 1;
    }
    if let Some(fd) = stderr_fd {
        fds[nfds].fd = fd;
        nfds += 1;
    }
    if nfds == 0 {
        return Ok(());
    }
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `fds[..nfds]` contains valid open file descriptors.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fcntl` with `F_GETFL`/`F_SETFL` on a valid pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Non-blocking pipe reader that caps captured output at `MAX_OUTPUT_CAPTURE`
/// and discards any further bytes so the child never hits `SIGPIPE`.
struct PipeBuffer<R> {
    pipe: Option<R>,
    data: Vec<u8>,
    truncated: bool,
}

impl<R: Read + AsRawFd> PipeBuffer<R> {
    fn new(pipe: Option<R>) -> io::Result<Self> {
        if let Some(ref p) = pipe {
            set_nonblocking(p.as_raw_fd())?;
        }
        Ok(Self {
            pipe,
            data: Vec::new(),
            truncated: false,
        })
    }

    fn is_open(&self) -> bool {
        self.pipe.is_some()
    }

    fn fd(&self) -> Option<RawFd> {
        self.pipe.as_ref().map(AsRawFd::as_raw_fd)
    }

    fn drain_available(&mut self) -> io::Result<()> {
        let Some(ref mut pipe) = self.pipe else {
            return Ok(());
        };
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => {
                    self.pipe = None;
                    return Ok(());
                }
                Ok(n) => {
                    if !self.truncated {
                        let remaining_cap = MAX_OUTPUT_CAPTURE.saturating_sub(self.data.len());
                        if n <= remaining_cap {
                            self.data.extend_from_slice(&buf[..n]);
                        } else {
                            self.data.extend_from_slice(&buf[..remaining_cap]);
                            self.data.extend_from_slice(TRUNCATED_SUFFIX);
                            self.truncated = true;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// How long to wait for a killed child to be reaped.
const REAP_LIMIT: Duration = Duration::from_secs(5);

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
        thread::sleep(POLL_SLICE);
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

/// Maximum bytes kept in memory per stream (`stdout` / `stderr`). Any further
/// output is drained to `io::sink()` so the child never hits `SIGPIPE`.
pub const MAX_OUTPUT_CAPTURE: usize = 1024 * 1024;
const TRUNCATED_SUFFIX: &[u8] = b"\n... [output truncated]";

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
