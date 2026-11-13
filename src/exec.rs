//! Running external commands behind a trait, so the logic is testable.

use std::io;
use std::process::Command;

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
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        let out = Command::new(program).args(args).output()?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
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
