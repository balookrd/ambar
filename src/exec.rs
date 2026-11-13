//! Running external commands behind a trait, so the logic is testable.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
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
    /// Runs a program and waits for it. `Err` means it could not be started.
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output>;
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

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // The pipes are drained while the process runs: a command that fills
        // a pipe buffer would otherwise block on write and never exit.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                // Best effort: the process may have exited in the meantime.
                let _ = child.kill();
                let _ = child.wait();
                // The reader threads are not joined: a grandchild that
                // inherited the pipes could keep them open indefinitely.
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "`{}` did not finish within {:?} and was killed",
                        command_line(program, args),
                        self.timeout
                    ),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout: collect(stdout)?,
            stderr: collect(stderr)?,
        })
    }
}

/// Reads `pipe` to the end on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes)?;
        }
        Ok(bytes)
    })
}

fn collect(reader: JoinHandle<io::Result<Vec<u8>>>) -> io::Result<String> {
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("output reader thread panicked"))??;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
