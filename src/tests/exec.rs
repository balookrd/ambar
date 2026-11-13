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
