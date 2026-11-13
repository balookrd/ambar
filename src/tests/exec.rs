use super::*;

#[test]
fn a_finished_process_returns_its_code_and_both_streams() {
    let out = SystemRunner
        .run("sh", &["-c", "printf out; printf err >&2; exit 3"])
        .unwrap();
    assert_eq!(out.code, 3);
    assert_eq!(out.stdout, "out");
    assert_eq!(out.stderr, "err");
    assert!(!out.success());
}

#[test]
fn a_process_killed_by_a_signal_has_code_minus_one() {
    let out = SystemRunner.run("sh", &["-c", "kill -9 $$"]).unwrap();
    assert_eq!(out.code, -1);
}

#[test]
fn a_program_that_does_not_exist_is_an_error() {
    assert!(SystemRunner.run("ambar-no-such-program", &[]).is_err());
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
