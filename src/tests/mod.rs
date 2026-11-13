//! Helpers shared by the unit tests of this crate.

use std::io;
use std::sync::Mutex;

use crate::exec::{Output, Runner, command_line};

type Reply = Box<dyn Fn(&str, &[&str]) -> Output + Send + Sync>;

/// Records every command line and answers from a closure.
pub(crate) struct FakeRunner {
    calls: Mutex<Vec<String>>,
    reply: Reply,
}

impl FakeRunner {
    pub(crate) fn new(reply: impl Fn(&str, &[&str]) -> Output + Send + Sync + 'static) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.calls.lock().unwrap().push(command_line(program, args));
        Ok((self.reply)(program, args))
    }
}

pub(crate) fn out(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}
