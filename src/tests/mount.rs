use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::tests::{FakeRunner, out};

const DEV: &str = "/dev/drbd1009";

// Synthetic lines in the mountinfo format, not captured from the stand.
const MOUNTINFO: &str = "\
24 1 254:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw,errors=remount-ro
310 24 147:1009 / /var/lib/kubelet/plugins/ambar/mounts/pv1 rw,relatime shared:90 - ext4 /dev/drbd1009 rw
311 24 147:1009 / /var/lib/kubelet/pods/p1/volumes/pv1 ro,relatime shared:90 - ext4 /dev/drbd1009 rw
312 24 147:1010 / /mnt/with\\040space rw,relatime - ext4 /dev/drbd1010 rw
";

/// A stand-in for a device: `len` bytes with `magic` at offset 1080.
fn device_file(dir: &TempDir, len: usize, magic: [u8; 2]) -> PathBuf {
    let path = dir.path().join("dev");
    let mut bytes = vec![0u8; len];
    if len >= 1082 {
        bytes[1080..1082].copy_from_slice(&magic);
    }
    fs::write(&path, bytes).unwrap();
    path
}

fn mountinfo_file(dir: &TempDir, text: &str) -> PathBuf {
    let path = dir.path().join("mountinfo");
    fs::write(&path, text).unwrap();
    path
}

const EXT4: [u8; 2] = [0x53, 0xef];
const NONE: [u8; 2] = [0, 0];

fn signature_of(magic: [u8; 2], blkid: impl Fn() -> Output + Send + Sync + 'static) -> Result<Signature, MountError> {
    let dir = TempDir::new().unwrap();
    let device = device_file(&dir, 4096, magic);
    let runner = FakeRunner::new(move |_, _| blkid());
    let info = mountinfo_file(&dir, "");
    Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(&device)
}

use crate::exec::Output;

#[test]
fn parses_root_bind_and_escaped_path() {
    let entries = parse_mountinfo(MOUNTINFO);
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].target, Path::new("/"));
    assert_eq!(entries[0].source, "/dev/vda1");
    assert_eq!(entries[0].fstype, "ext4");
    assert!(!entries[0].read_only);
    assert_eq!(entries[2].source, "/dev/drbd1009");
    assert!(entries[2].read_only);
    assert_eq!(entries[3].target, Path::new("/mnt/with space"));
}

#[test]
fn decodes_all_octal_escapes() {
    let line = "5 1 0:1 /a\\011b /x\\134y\\012z rw - tmpfs src\\040dir rw";
    let entries = parse_mountinfo(line);
    assert_eq!(entries[0].root, "/a\tb");
    assert_eq!(entries[0].target, Path::new("/x\\y\nz"));
    assert_eq!(entries[0].source, "src dir");
}

#[test]
fn skips_empty_input_and_lines_without_the_separator() {
    assert!(parse_mountinfo("").is_empty());
    assert!(parse_mountinfo("36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 ext3 /dev/root rw\n").is_empty());
    assert!(parse_mountinfo("1 2 3:4 / - ext4 /dev/x rw\n").is_empty());
}

#[test]
fn mounted_at_finds_the_last_entry_for_a_target() {
    let text = "1 0 0:1 / /m rw - ext4 /dev/a rw\n2 1 0:2 / /m rw - ext4 /dev/b rw\n";
    let entries = parse_mountinfo(text);
    assert_eq!(mounted_at(&entries, Path::new("/m")).unwrap().source, "/dev/b");
    assert!(mounted_at(&entries, Path::new("/other")).is_none());
}

#[test]
fn ext4_needs_the_magic_and_blkid() {
    let got = signature_of(EXT4, || out(0, "ext4\n", ""));
    assert_eq!(got.unwrap(), Signature::Ext4);
}

#[test]
fn empty_needs_no_magic_and_blkid_code_2_with_no_output() {
    // Experiment P9, state 1: code 2, empty stdout and stderr, bytes 00 00.
    let got = signature_of(NONE, || out(2, "", ""));
    assert_eq!(got.unwrap(), Signature::Empty);
}

#[test]
fn another_type_without_the_magic_is_other() {
    let got = signature_of(NONE, || out(0, "xfs\n", ""));
    assert_eq!(got.unwrap(), Signature::Other("xfs".to_string()));
}

#[test]
fn magic_present_makes_blkid_code_2_ambiguous() {
    let got = signature_of(EXT4, || out(2, "", ""));
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn medium_type_error_on_stderr_is_not_empty() {
    // Experiment P9, state 3: code 2 with `Wrong medium type`.
    let got = signature_of(NONE, || out(2, "", "blkid: error: /dev/drbd1009: Wrong medium type\n"));
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn stdout_with_code_2_is_not_empty() {
    let got = signature_of(NONE, || out(2, "ext4\n", ""));
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn other_blkid_codes_are_ambiguous() {
    for code in [1, 4, 8, -1] {
        let got = signature_of(NONE, move || out(code, "", ""));
        assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })), "code {code}");
    }
}

#[test]
fn code_0_without_a_type_is_ambiguous() {
    for magic in [NONE, EXT4] {
        let got = signature_of(magic, || out(0, "", ""));
        assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
    }
}

#[test]
fn magic_without_ext4_from_blkid_is_ambiguous() {
    let got = signature_of(EXT4, || out(0, "xfs\n", ""));
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn ext4_from_blkid_without_the_magic_is_ambiguous() {
    let got = signature_of(NONE, || out(0, "ext4\n", ""));
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn a_blkid_that_did_not_run_is_ambiguous() {
    struct Failing;
    impl Runner for Failing {
        fn run(&self, _: &str, _: &[&str]) -> io::Result<Output> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "killed"))
        }
    }
    let dir = TempDir::new().unwrap();
    let device = device_file(&dir, 4096, NONE);
    let info = mountinfo_file(&dir, "");
    let got = Mounter {
        runner: &Failing,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(&device);
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
}

#[test]
fn a_short_device_is_ambiguous_and_blkid_is_not_run() {
    let dir = TempDir::new().unwrap();
    let device = device_file(&dir, 1081, NONE);
    let runner = FakeRunner::new(|_, _| out(2, "", ""));
    let info = mountinfo_file(&dir, "");
    let got = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(&device);
    assert!(matches!(got, Err(MountError::AmbiguousSignature { .. })));
    assert!(runner.calls().is_empty());
}

#[test]
fn a_missing_device_is_an_open_error_and_blkid_is_not_run() {
    // Experiment P9, state 4: blkid code 2 with no output, open fails ENOENT.
    let dir = TempDir::new().unwrap();
    let runner = FakeRunner::new(|_, _| out(2, "", ""));
    let info = mountinfo_file(&dir, "");
    let got = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(&dir.path().join("absent"));
    assert!(matches!(got, Err(MountError::Open { in_use: false, .. })));
    assert!(runner.calls().is_empty());
}

#[test]
fn blkid_is_called_with_the_p9_arguments() {
    let dir = TempDir::new().unwrap();
    let device = device_file(&dir, 4096, NONE);
    let runner = FakeRunner::new(|_, _| out(2, "", ""));
    let info = mountinfo_file(&dir, "");
    Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(&device)
    .unwrap();
    assert_eq!(
        runner.calls(),
        vec![format!("blkid -p -o value -s TYPE {}", device.display())]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn emediumtype_means_in_use() {
    assert!(open_error(&io::Error::from_raw_os_error(libc::EMEDIUMTYPE)));
    assert!(!open_error(&io::Error::from_raw_os_error(libc::ENOENT)));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn emediumtype_means_in_use() {
    // EMEDIUMTYPE is Linux-specific: elsewhere no error means "in use".
    assert!(!open_error(&io::Error::from_raw_os_error(libc::ENOENT)));
}

fn with_mounter<R>(text: &str, code: i32, f: impl FnOnce(&Mounter<'_>, &FakeRunner) -> R) -> R {
    let dir = TempDir::new().unwrap();
    let info = mountinfo_file(&dir, text);
    let runner = FakeRunner::new(move |_, _| out(code, "", "boom\n"));
    let mounter = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    };
    f(&mounter, &runner)
}

#[test]
fn mount_device_is_a_no_op_when_the_same_device_is_mounted() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.mount_device(Path::new(DEV), Path::new("/var/lib/kubelet/plugins/ambar/mounts/pv1"))
            .unwrap();
        assert!(r.calls().is_empty());
    });
}

#[test]
fn mount_device_reports_a_different_source_as_busy() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        let err = m
            .mount_device(
                Path::new("/dev/drbd1"),
                Path::new("/var/lib/kubelet/plugins/ambar/mounts/pv1"),
            )
            .unwrap_err();
        assert!(matches!(err, MountError::Busy { ref source_mounted, .. } if source_mounted == DEV));
        assert!(r.calls().is_empty());
    });
}

#[test]
fn mount_device_rejects_the_same_device_with_another_filesystem() {
    let text = "1 0 0:1 / /m rw - xfs /dev/drbd1009 rw\n";
    with_mounter(text, 0, |m, r| {
        let err = m.mount_device(Path::new(DEV), Path::new("/m")).unwrap_err();
        assert!(matches!(err, MountError::WrongFilesystem { ref found, .. } if found == "xfs"));
        assert!(r.calls().is_empty());
    });
}

#[test]
fn mount_device_runs_one_mount_when_nothing_is_mounted() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.mount_device(Path::new(DEV), Path::new("/new")).unwrap();
        assert_eq!(r.calls(), vec!["mount -t ext4 /dev/drbd1009 /new"]);
    });
}

#[test]
fn a_failing_mount_is_a_command_error() {
    with_mounter(MOUNTINFO, 32, |m, _| {
        let err = m.mount_device(Path::new(DEV), Path::new("/new")).unwrap_err();
        assert!(matches!(err, MountError::Command { code: Some(32), ref stderr, .. } if stderr == "boom"));
    });
}

#[test]
fn a_missing_mountinfo_is_an_error() {
    let dir = TempDir::new().unwrap();
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    let info = dir.path().join("absent");
    let err = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .unmount(Path::new("/x"))
    .unwrap_err();
    assert!(matches!(err, MountError::Io { .. }));
    assert!(runner.calls().is_empty());
}

#[test]
fn unmount_is_a_no_op_when_nothing_is_mounted() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.unmount(Path::new("/nowhere")).unwrap();
        assert!(r.calls().is_empty());
    });
}

#[test]
fn unmount_runs_umount_for_a_mount_point() {
    let dir = TempDir::new().unwrap();
    let info = mountinfo_file(&dir, MOUNTINFO);
    let runner_info = info.clone();
    // The umount removes the entry.
    let runner = FakeRunner::new(move |_, _| {
        fs::write(&runner_info, "").unwrap();
        out(0, "", "")
    });
    Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .unmount(Path::new("/mnt/with space"))
    .unwrap();
    assert_eq!(runner.calls(), vec!["umount \"/mnt/with space\""]);
}

#[test]
fn mkfs_runs_quiet_and_without_discard() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.mkfs_ext4(Path::new(DEV)).unwrap();
        assert_eq!(r.calls(), vec!["mkfs.ext4 -q -E nodiscard /dev/drbd1009"]);
    });
}

#[test]
fn bind_read_only_runs_two_commands_in_order() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.bind(
            Path::new("/var/lib/kubelet/plugins/ambar/mounts/pv1"),
            Path::new("/t"),
            true,
        )
        .unwrap();
        assert_eq!(
            r.calls(),
            vec![
                "mount --bind /var/lib/kubelet/plugins/ambar/mounts/pv1 /t",
                "mount -o remount,bind,ro /t"
            ]
        );
    });
}

#[test]
fn bind_read_write_runs_one_command() {
    with_mounter(MOUNTINFO, 0, |m, r| {
        m.bind(Path::new("/s"), Path::new("/t"), false).unwrap();
        assert_eq!(r.calls(), vec!["mount --bind /s /t"]);
    });
}

#[test]
fn an_existing_matching_read_only_bind_is_done() {
    let source = Path::new("/var/lib/kubelet/plugins/ambar/mounts/pv1");
    with_mounter(MOUNTINFO, 0, |m, r| {
        // The existing bind is read-only already.
        m.bind(source, Path::new("/var/lib/kubelet/pods/p1/volumes/pv1"), true)
            .unwrap();
        assert!(r.calls().is_empty());
    });
}

#[test]
fn an_existing_read_write_bind_is_remounted_read_only() {
    let text = "\
1 0 0:1 / /s rw - ext4 /dev/drbd1009 rw
2 0 0:1 / /t rw - ext4 /dev/drbd1009 rw
";
    with_mounter(text, 0, |m, r| {
        m.bind(Path::new("/s"), Path::new("/t"), true).unwrap();
        assert_eq!(r.calls(), vec!["mount -o remount,bind,ro /t"]);
    });
}

#[test]
fn a_target_mounted_from_another_device_or_root_is_busy_for_bind() {
    let text = "\
1 0 0:1 / /s rw - ext4 /dev/drbd1009 rw
2 0 0:2 / /other-device rw - ext4 /dev/drbd1010 rw
3 0 0:1 /sub /other-root rw - ext4 /dev/drbd1009 rw
";
    with_mounter(text, 0, |m, r| {
        for target in ["/other-device", "/other-root"] {
            let err = m.bind(Path::new("/s"), Path::new(target), false).unwrap_err();
            assert!(matches!(err, MountError::Busy { .. }), "{target}");
        }
        // The source is not a mount point: the match cannot be proved.
        let err = m
            .bind(Path::new("/not-a-mount"), Path::new("/other-root"), false)
            .unwrap_err();
        assert!(matches!(err, MountError::Busy { .. }));
        assert!(r.calls().is_empty());
    });
}

// ---- N1: backup superblocks ----

/// A sparse stand-in for a device of `len` bytes with `bytes` written at each
/// offset; nothing is allocated for the holes.
fn sparse_device(dir: &TempDir, len: u64, writes: &[(u64, [u8; 2])]) -> PathBuf {
    use std::os::unix::fs::FileExt;
    let path = dir.path().join("dev");
    let file = fs::File::create(&path).unwrap();
    file.set_len(len).unwrap();
    for (offset, bytes) in writes {
        file.write_at(bytes, *offset).unwrap();
    }
    path
}

/// Magic offset of the first backup superblock per block size.
const BACKUP_OFFSETS: [u64; 3] = [8193 * 1024 + 56, 16384 * 2048 + 56, 32768 * 4096 + 56];

fn signature_of_file(
    device: &Path,
    dir: &TempDir,
    blkid: impl Fn() -> Output + Send + Sync + 'static,
) -> (Result<Signature, MountError>, Vec<String>) {
    let runner = FakeRunner::new(move |_, _| blkid());
    let info = mountinfo_file(dir, "");
    let got = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .signature(device);
    (got, runner.calls())
}

#[test]
fn a_backup_superblock_magic_alone_is_ambiguous() {
    for offset in BACKUP_OFFSETS {
        let dir = TempDir::new().unwrap();
        let device = sparse_device(&dir, offset + 4096, &[(offset, EXT4)]);
        let (got, calls) = signature_of_file(&device, &dir, || out(2, "", ""));
        match got {
            Err(MountError::AmbiguousSignature { detail, .. }) => {
                assert!(detail.contains(&offset.to_string()), "{detail}");
            }
            other => panic!("offset {offset}: {other:?}"),
        }
        assert!(calls.is_empty(), "blkid must not run, offset {offset}");
    }
}

#[test]
fn a_large_empty_device_is_empty() {
    let dir = TempDir::new().unwrap();
    let device = sparse_device(&dir, 200 << 20, &[]);
    let (got, calls) = signature_of_file(&device, &dir, || out(2, "", ""));
    assert_eq!(got.unwrap(), Signature::Empty);
    assert_eq!(calls.len(), 1);
}

#[test]
fn a_device_ending_before_the_backup_offsets_is_empty() {
    // Shorter than the first 1 KiB backup superblock, otherwise empty.
    let dir = TempDir::new().unwrap();
    let device = sparse_device(&dir, BACKUP_OFFSETS[0], &[]);
    let (got, _) = signature_of_file(&device, &dir, || out(2, "", ""));
    assert_eq!(got.unwrap(), Signature::Empty);
}

#[test]
fn a_backup_magic_cut_by_the_end_of_the_device_is_beyond_the_end() {
    // One byte of the two fits: the magic cannot be there.
    let dir = TempDir::new().unwrap();
    let device = sparse_device(&dir, BACKUP_OFFSETS[0] + 1, &[(BACKUP_OFFSETS[0], [0x53, 0])]);
    let (got, _) = signature_of_file(&device, &dir, || out(2, "", ""));
    assert_eq!(got.unwrap(), Signature::Empty);
}

#[test]
fn a_backup_magic_does_not_change_a_primary_ext4() {
    let dir = TempDir::new().unwrap();
    let device = sparse_device(&dir, 200 << 20, &[(1080, EXT4), (BACKUP_OFFSETS[2], EXT4)]);
    let (got, _) = signature_of_file(&device, &dir, || out(0, "ext4\n", ""));
    assert_eq!(got.unwrap(), Signature::Ext4);
}

// ---- N5: only block devices ----

#[test]
fn a_regular_file_is_not_a_block_device_in_production() {
    let dir = TempDir::new().unwrap();
    let device = device_file(&dir, 4096, NONE);
    let runner = FakeRunner::new(|_, _| out(2, "", ""));
    let info = mountinfo_file(&dir, "");
    let got = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: false,
    }
    .signature(&device);
    assert!(matches!(got, Err(MountError::NotABlockDevice { .. })));
    assert!(runner.calls().is_empty());
}

#[test]
fn a_directory_and_a_character_device_are_rejected_even_when_files_are_allowed() {
    let dir = TempDir::new().unwrap();
    for path in [dir.path(), Path::new("/dev/null")] {
        let (got, calls) = signature_of_file(path, &dir, || out(2, "", ""));
        assert!(
            matches!(got, Err(MountError::NotABlockDevice { .. })),
            "{}",
            path.display()
        );
        assert!(calls.is_empty());
    }
}

// ---- N2: canonical target paths ----

/// A real directory `real` and a symlink `link` to it inside `dir`; returns
/// (symlink, canonical real path).
fn linked_dirs(dir: &TempDir) -> (PathBuf, PathBuf) {
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    (link, fs::canonicalize(&real).unwrap())
}

#[test]
fn mount_device_resolves_a_symlinked_target() {
    let dir = TempDir::new().unwrap();
    let (link, real) = linked_dirs(&dir);
    let text = format!("1 0 0:1 / {} rw - ext4 {DEV} rw\n", real.display());
    with_mounter(&text, 0, |m, r| {
        m.mount_device(Path::new(DEV), &link).unwrap();
        assert!(r.calls().is_empty());
    });
}

#[test]
fn bind_resolves_a_symlinked_target_and_source() {
    let dir = TempDir::new().unwrap();
    let (link, real) = linked_dirs(&dir);
    let other = dir.path().join("src");
    fs::create_dir(&other).unwrap();
    let other_link = dir.path().join("src-link");
    std::os::unix::fs::symlink(&other, &other_link).unwrap();
    let other = fs::canonicalize(&other).unwrap();
    let text = format!(
        "1 0 0:1 / {} rw - ext4 {DEV} rw\n2 0 0:1 / {} ro - ext4 {DEV} rw\n",
        other.display(),
        real.display()
    );
    with_mounter(&text, 0, |m, r| {
        m.bind(&other_link, &link, true).unwrap();
        assert!(r.calls().is_empty());
    });
}

#[test]
fn unmount_resolves_a_symlinked_target() {
    let dir = TempDir::new().unwrap();
    let (link, real) = linked_dirs(&dir);
    let info = mountinfo_file(&dir, &format!("1 0 0:1 / {} rw - ext4 {DEV} rw\n", real.display()));
    let runner_info = info.clone();
    let runner = FakeRunner::new(move |_, _| {
        fs::write(&runner_info, "").unwrap();
        out(0, "", "")
    });
    Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .unmount(&link)
    .unwrap();
    assert_eq!(runner.calls(), vec![format!("umount {}", real.display())]);
}

// ---- N3: stacked mounts ----

fn stacked_unmount(depth: usize) -> (Result<(), MountError>, Vec<String>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = TempDir::new().unwrap();
    let line = |n: usize| format!("{n} 0 0:1 / /m rw - ext4 {DEV} rw\n");
    let info = mountinfo_file(&dir, &(0..depth).map(line).collect::<String>());
    let left = Arc::new(AtomicUsize::new(depth));
    let (runner_info, runner_left) = (info.clone(), Arc::clone(&left));
    // Every umount removes the top entry of the stack.
    let runner = FakeRunner::new(move |_, _| {
        let now = runner_left.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        fs::write(&runner_info, (0..now).map(line).collect::<String>()).unwrap();
        out(0, "", "")
    });
    let got = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    }
    .unmount(Path::new("/m"));
    (got, runner.calls())
}

#[test]
fn unmount_repeats_until_the_stack_is_gone() {
    let (got, calls) = stacked_unmount(3);
    got.unwrap();
    assert_eq!(calls, vec!["umount /m"; 3]);
}

#[test]
fn unmount_gives_up_after_eight_attempts() {
    let (got, calls) = stacked_unmount(9);
    assert!(matches!(got, Err(MountError::StillMounted { .. })), "{got:?}");
    assert_eq!(calls.len(), 8);
    let (got, calls) = stacked_unmount(8);
    got.unwrap();
    assert_eq!(calls.len(), 8);
}

// ---- N4: read-only mismatch ----

#[test]
fn a_read_write_bind_over_an_existing_read_only_bind_is_a_mode_mismatch() {
    let source = Path::new("/var/lib/kubelet/plugins/ambar/mounts/pv1");
    with_mounter(MOUNTINFO, 0, |m, r| {
        let err = m
            .bind(source, Path::new("/var/lib/kubelet/pods/p1/volumes/pv1"), false)
            .unwrap_err();
        assert!(matches!(err, MountError::ModeMismatch { .. }), "{err:?}");
        assert!(r.calls().is_empty());
    });
}

// ---- N6: small items ----

#[test]
fn whitespace_only_blkid_output_at_code_2_is_ambiguous() {
    for (stdout, stderr) in [("\n", ""), ("", " \n")] {
        let got = signature_of(NONE, move || out(2, stdout, stderr));
        assert!(
            matches!(got, Err(MountError::AmbiguousSignature { .. })),
            "{stdout:?} {stderr:?}"
        );
    }
}

#[test]
fn a_missing_mountinfo_is_an_error_for_mount_device_and_bind() {
    let dir = TempDir::new().unwrap();
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    let info = dir.path().join("absent");
    let mounter = Mounter {
        runner: &runner,
        mountinfo: &info,
        allow_regular_files: true,
    };
    let err = mounter.mount_device(Path::new(DEV), Path::new("/x")).unwrap_err();
    assert!(matches!(err, MountError::Io { .. }));
    let err = mounter.bind(Path::new("/s"), Path::new("/x"), false).unwrap_err();
    assert!(matches!(err, MountError::Io { .. }));
    assert!(runner.calls().is_empty());
}
