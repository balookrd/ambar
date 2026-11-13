//! Helpers shared by the unit tests of this crate.

use std::cell::RefCell;
use std::io;

use crate::exec::{Output, Runner, command_line};

type Reply = Box<dyn Fn(&str, &[&str]) -> Output>;

/// Records every command line and answers from a closure.
pub(crate) struct FakeRunner {
    calls: RefCell<Vec<String>>,
    reply: Reply,
}

impl FakeRunner {
    pub(crate) fn new(reply: impl Fn(&str, &[&str]) -> Output + 'static) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.calls.borrow_mut().push(command_line(program, args));
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
