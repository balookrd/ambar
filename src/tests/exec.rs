use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use super::*;

#[test]
fn a_finished_process_returns_its_code_and_both_streams() {
    let out = SystemRunner::default()
        .run("sh", &["-c", "printf out; printf err >&2; exit 3"])
        .unwrap();
    assert_eq!(out.code, 3);
    assert_eq!(out.stdout, "out");
    assert_eq!(out.stderr, "err");
    assert!(!out.success());
}

#[test]
fn a_process_killed_by_a_signal_has_code_minus_one() {
    let out = SystemRunner::default().run("sh", &["-c", "kill -9 $$"]).unwrap();
    assert_eq!(out.code, -1);
}

#[test]
fn a_program_that_does_not_exist_is_an_error() {
    assert!(SystemRunner::default().run("ambar-no-such-program", &[]).is_err());
}

#[test]
fn arguments_with_whitespace_or_empty_are_quoted() {
    assert_eq!(command_line("x", &["a b", "", "c"]), r#"x "a b" "" c"#);
    assert_eq!(command_line("x", &["a\tb"]), "x \"a\tb\"");
}

#[test]
fn quotes_and_backslashes_inside_a_quoted_argument_are_escaped() {
    assert_eq!(command_line("x", &[r#"say "hi""#]), r#"x "say \"hi\"""#);
    assert_eq!(command_line("x", &[r#"a b\c"#]), r#"x "a b\\c""#);
}

#[test]
fn plain_arguments_are_left_untouched() {
    assert_eq!(
        command_line("modprobe", &["-a", "lru_cache", "/x/y.ko", "a=b"]),
        "modprobe -a lru_cache /x/y.ko a=b"
    );
    assert_eq!(command_line("true", &[]), "true");
}

fn runner_with(timeout: Duration) -> SystemRunner {
    SystemRunner { timeout }
}

#[test]
fn the_default_timeout_is_two_minutes() {
    assert_eq!(DEFAULT_COMMAND_TIMEOUT, Duration::from_secs(120));
    assert_eq!(SystemRunner::default().timeout, DEFAULT_COMMAND_TIMEOUT);
}

#[test]
fn a_command_that_outlives_the_timeout_is_killed_and_reported() {
    let started = Instant::now();
    let err = runner_with(Duration::from_millis(200))
        .run("sleep", &["5"])
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("sleep 5"), "{err}");
}

#[test]
fn a_command_that_finishes_in_time_keeps_its_code_and_streams() {
    let out = runner_with(Duration::from_secs(5))
        .run("sh", &["-c", "echo out; echo err >&2; exit 3"])
        .unwrap();
    assert_eq!(out.code, 3);
    assert_eq!(out.stdout, "out\n");
    assert_eq!(out.stderr, "err\n");
}

#[test]
fn a_command_that_fills_both_pipes_does_not_hang() {
    // Far above the pipe buffer (64 KiB on Linux): the pipes must be read
    // while the process runs.
    let out = runner_with(Duration::from_secs(20))
        .run(
            "sh",
            &[
                "-c",
                "head -c 1048576 /dev/zero | tr '\\0' x; head -c 1048576 /dev/zero | tr '\\0' y >&2",
            ],
        )
        .unwrap();
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.len(), 1 << 20);
    assert_eq!(out.stderr.len(), 1 << 20);
}

// ---- process tree, pipes held after exit, explicit limits ----

/// Whether a process still runs. A zombie counts as gone: in a container
/// without an init process an orphan is not reaped by anyone, but it no
/// longer runs.
fn process_runs(pid: i32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let state = stat.rsplit(')').next().unwrap_or("").trim_start().chars().next();
            !matches!(state, Some('Z') | Some('X'))
        }
        // No /proc entry: either gone, or no procfs (macOS); `kill -0` decides.
        Err(_) => {
            !Path::new("/proc/self").exists()
                && std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
        }
    }
}

/// Waits (up to 3 s) for the process to disappear.
fn gone_soon(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !process_runs(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    !process_runs(pid)
}

/// Reads a PID the script wrote; waits for the file to be non-empty.
fn read_pid(file: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid) = fs::read_to_string(file).unwrap_or_default().trim().parse() {
            return pid;
        }
        assert!(Instant::now() < deadline, "no pid in {}", file.display());
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_killed_command_no_longer_exists_after_the_timeout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("pid");
    let script = format!("echo $$ > {}; sleep 30", file.display());
    let started = Instant::now();
    // 2 s leave a loaded machine time to start `sh` and write the PID before
    // the kill; the bound is the timeout plus 3 s, far below the 30 s sleep.
    let err = runner_with(Duration::from_secs(2))
        .run("sh", &["-c", &script])
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(gone_soon(read_pid(&file)));
}

#[test]
fn a_grandchild_that_keeps_the_pipes_open_is_killed_too() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("pid");
    let script = format!("sleep 30 & echo $! > {}; wait", file.display());
    let started = Instant::now();
    let err = runner_with(Duration::from_secs(2))
        .run("sh", &["-c", &script])
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(gone_soon(read_pid(&file)));
}

#[test]
fn a_background_process_that_holds_the_pipes_after_exit_does_not_block_past_the_deadline() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("pid");
    let script = format!("sleep 30 & echo $! > {}; exit 0", file.display());
    let started = Instant::now();
    let err = runner_with(Duration::from_secs(2))
        .run("sh", &["-c", &script])
        .unwrap_err();
    // Deadline 2 s plus the 1 s grace for the readers, plus 3 s of slack;
    // far below the 30 s the background process would hold the pipes.
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("sh -c"), "{err}");
    assert!(gone_soon(read_pid(&file)));
}

#[test]
fn an_explicit_limit_overrides_the_default_one() {
    let runner = runner_with(Duration::from_millis(100));
    // The default limit would kill this; the explicit one lets it finish.
    let out = runner
        .run_with_timeout("sh", &["-c", "sleep 0.5; echo done"], Duration::from_secs(10))
        .unwrap();
    assert_eq!(out.stdout, "done\n");
    // And the other way round.
    let err = SystemRunner::default()
        .run_with_timeout("sleep", &["5"], Duration::from_millis(200))
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn a_runner_that_ignores_limits_runs_commands_through_run() {
    struct Plain;
    impl Runner for Plain {
        fn run(&self, program: &str, _: &[&str]) -> io::Result<Output> {
            Ok(Output {
                code: 0,
                stdout: program.to_string(),
                stderr: String::new(),
            })
        }
    }
    let out = Plain.run_with_timeout("x", &[], Duration::from_secs(1)).unwrap();
    assert_eq!(out.stdout, "x");
}

#[test]
fn the_timeout_error_carries_the_stderr_captured_so_far() {
    let err = runner_with(Duration::from_secs(2))
        .run("sh", &["-c", "echo partial-diagnostic >&2; sleep 30"])
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("partial-diagnostic"), "{err}");
}

#[test]
fn the_stderr_in_a_timeout_error_is_truncated_to_2_kib() {
    let err = runner_with(Duration::from_secs(2))
        .run("sh", &["-c", "head -c 100000 /dev/zero | tr '\\0' e >&2; sleep 30"])
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains(&"e".repeat(2048)), "{}", text.len());
    assert!(!text.contains(&"e".repeat(2049)), "{}", text.len());
    assert!(text.len() < 4096, "{}", text.len());
}

#[test]
fn a_timeout_without_stderr_says_so() {
    let err = runner_with(Duration::from_millis(300))
        .run("sleep", &["30"])
        .unwrap_err();
    assert!(err.to_string().contains("stderr was empty"), "{err}");
}

#[test]
fn an_exited_child_is_seen_without_being_reaped() {
    let mut child = std::process::Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !exited_without_reaping(child.id()).unwrap() {
        assert!(Instant::now() < deadline, "the child did not exit");
        thread::sleep(Duration::from_millis(10));
    }
    // Still there to be reaped: the check did not consume the exit status,
    // and calling it again reports the same.
    assert!(exited_without_reaping(child.id()).unwrap());
    assert_eq!(child.wait().unwrap().code(), Some(7));
}

// ---- reaping and error paths (K3) ----

/// A `sleep 30` in its own process group, as `run_with_timeout` starts commands.
fn sleeper() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap()
}

/// `true` when `pid` is no longer a child or process of ours at all: not even
/// a zombie (`kill(pid, 0)` fails with ESRCH).
fn pid_is_gone(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists.
    unsafe { libc::kill(pid as i32, 0) == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) }
}

#[test]
fn kill_and_reap_kills_the_group_and_leaves_no_zombie() {
    let mut child = sleeper();
    let pid = child.id();
    assert!(kill_and_reap(&mut child, pid));
    assert!(pid_is_gone(pid), "the killed child is still a zombie");
}

#[test]
fn reaping_gives_up_after_the_limit_and_does_not_block() {
    let mut child = sleeper();
    let started = Instant::now();
    assert!(!reap_within(&mut child, Duration::from_millis(300)));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(3),
        "{took:?}"
    );
    // Still running, not reaped: kill it for real.
    child.kill().unwrap();
    assert!(reap_within(&mut child, Duration::from_secs(5)));
    assert!(pid_is_gone(child.id()));
}

#[test]
fn a_child_that_was_reaped_by_someone_else_counts_as_reaped() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    let mut status = 0;
    // SAFETY: waits for our own child; the std handle is told nothing.
    assert_eq!(unsafe { libc::waitpid(pid as i32, &mut status, 0) }, pid as i32);
    assert!(reap_within(&mut child, Duration::from_secs(1)));
}

#[test]
fn echild_from_waitid_means_there_is_nothing_to_signal() {
    // Pid 1 is not our child: `waitid` fails with ECHILD.
    let err = exited_without_reaping(1).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ECHILD), "{err}");
    assert!(!may_signal_group(&err));
    assert!(may_signal_group(&io::Error::from_raw_os_error(libc::EINTR)));
    assert!(may_signal_group(&io::Error::other("anything else")));
}

#[test]
fn waiting_for_the_pipes_stops_after_the_deadline_plus_the_grace() {
    use std::os::fd::FromRawFd;
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid 2-element array for `pipe`.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: newly created pipe file descriptors.
    let (read_end, _write_end) = unsafe { (fs::File::from_raw_fd(fds[0]), fs::File::from_raw_fd(fds[1])) };
    let mut stdout = PipeBuffer::new(Some(read_end)).unwrap();
    let mut stderr: PipeBuffer<fs::File> = PipeBuffer::new(None).unwrap();
    let started = Instant::now();
    let drained = drain_after_exit(&mut stdout, &mut stderr, Instant::now() + READER_GRACE).unwrap();
    let took = started.elapsed();
    assert!(!drained);
    assert!(
        took >= READER_GRACE - Duration::from_millis(100) && took < READER_GRACE + Duration::from_secs(3),
        "{took:?}"
    );
}

#[test]
fn a_timeout_error_says_when_the_process_could_not_be_reaped() {
    let reaped = timeout_error("sleep", &["30"], Duration::from_secs(2), Some(Vec::new()), true).to_string();
    let stuck = timeout_error("sleep", &["30"], Duration::from_secs(2), Some(Vec::new()), false).to_string();
    assert!(!reaped.contains("reaped"), "{reaped}");
    assert!(stuck.contains("could not be reaped within 5s"), "{stuck}");
    assert!(stuck.contains("zombie"), "{stuck}");
    assert!(stuck.starts_with(&reaped), "{stuck}");
}

#[test]
fn reap_lingering_zombies_reaps_exited_child_and_clears_echild() {
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    std::mem::forget(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !exited_without_reaping(pid).unwrap() {
        assert!(Instant::now() < deadline, "the child did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    record_unreaped_pid(pid);
    record_unreaped_pid(u32::MAX / 2);
    reap_lingering_zombies();
    assert!(pid_is_gone(pid), "the exited child was left as a zombie");
    let pids = UNREAPED_PIDS.lock().unwrap();
    assert!(!pids.contains(&pid));
    assert!(!pids.contains(&(u32::MAX / 2)));
}

/// Whether the process runs: it exists and is not a zombie. `/proc` where
/// there is one (the Linux container has no `ps`), `ps` otherwise (macOS).
fn alive(pid: i32) -> bool {
    if Path::new("/proc/self").exists() {
        return process_runs(pid);
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !state.is_empty() && !state.starts_with('Z')
}

#[test]
fn a_leader_reaped_elsewhere_gives_an_error_and_no_signal_to_its_group() {
    // `sh` leads a group with a background `sleep` in it; the test kills and
    // reaps the leader behind the handle's back, as if another waiter had.
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("sleeper");
    let script = format!("sleep 30 & echo $! > {}; wait", file.display());
    let mut child = std::process::Command::new("sh")
        .args(["-c", &script])
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let sleeper = read_pid(&file);
    assert!(alive(sleeper));
    let mut status = 0;
    // SAFETY: our own child; only the leader is signalled and reaped.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
    }
    let err = supervise(
        &mut child,
        Instant::now() + Duration::from_secs(5),
        "sh",
        &[],
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ECHILD), "{err}");
    // A signal would have reached the sleeper by now.
    thread::sleep(Duration::from_millis(500));
    let survived = alive(sleeper);
    // SAFETY: cleanup of the group the test created.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    assert!(
        survived,
        "the group was signalled although the leader was not ours any more"
    );
}

#[test]
fn drain_caps_captured_stdout_without_sigpipe() {
    let runner = SystemRunner::default();
    // Writes 2 MiB to stdout and exits 0 only if write succeeded without SIGPIPE.
    let script = "dd if=/dev/zero bs=65536 count=32 2>/dev/null | tr '\\0' 'a'";
    let out = runner.run("/bin/sh", &["-c", script]).expect("command must finish");
    assert_eq!(out.code, 0, "writer must not die with SIGPIPE");
    assert!(out.stdout.len() <= MAX_OUTPUT_CAPTURE + 64);
    assert!(out.stdout.ends_with("\n... [output truncated]"));
}

#[test]
fn fast_command_finishes_without_poll_interval_sleep() {
    let start = Instant::now();
    let out = SystemRunner::default().run("true", &[]).unwrap();
    assert!(out.success());
    assert!(start.elapsed() < Duration::from_millis(18), "{:?}", start.elapsed());
}

#[test]
fn command_runs_with_c_locale() {
    let out = SystemRunner::default()
        .run("sh", &["-c", "printf '%s' \"$LC_ALL\""])
        .unwrap();
    assert!(out.success());
    assert_eq!(out.stdout, "C");
}
