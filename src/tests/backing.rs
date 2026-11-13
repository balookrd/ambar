use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use tempfile::TempDir;

use super::*;
use crate::model::MAX_SIZE_BYTES;
use crate::tests::{FakeRunner, out};

const KIB: u64 = 1024;
const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn metadata_size_matches_the_stand_measurements() {
    // A 1 GiB backing file gave a 1048476 KiB device with two peers, a
    // 1048508 KiB device with one peer and a 1048316 KiB device with
    // `--max-peers=7`.
    assert_eq!(metadata_bytes(GIB, 2), 100 * KIB);
    assert_eq!(metadata_bytes(GIB, 1), 68 * KIB);
    assert_eq!(metadata_bytes(GIB, 7), 260 * KIB);
    assert_eq!((GIB - metadata_bytes(GIB, 2)) / KIB, 1_048_476);
    assert_eq!((GIB - metadata_bytes(GIB, 1)) / KIB, 1_048_508);
    assert_eq!((GIB - metadata_bytes(GIB, 7)) / KIB, 1_048_316);
}

#[test]
fn the_backing_file_leaves_the_requested_size_usable() {
    const MIB: u64 = 1024 * 1024;
    let mut cases = vec![(GIB, 2), (10 * GIB, 7), (4096, 31), (3 * GIB + 4096, 3)];
    // One bitmap extent covers 128 MiB: probe both sides of the first
    // boundary and of the 1 GiB one (eight extents).
    for around in [128 * MIB, GIB] {
        for size in [around - 4096, around, around + 4096] {
            for peers in [1, 7, 31] {
                cases.push((size, peers));
            }
        }
    }
    cases.push((MAX_SIZE_BYTES, 31));
    for (size, peers) in cases {
        let file = backing_file_bytes(size, peers);
        assert_eq!(file % 4096, 0);
        let usable = file - metadata_bytes(file, peers);
        assert!(usable >= size, "size {size} peers {peers}: usable {usable}");
        // The file is the smallest fixpoint rounded up to 4 KiB, and the
        // metadata is a whole number of sectors, so the surplus is below one
        // 4 KiB step.
        assert!(
            usable - size < 4096,
            "size {size} peers {peers}: wastes {}",
            usable - size
        );
    }
}

#[test]
fn creates_a_sparse_file_once() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("volumes").join("spike.img.new");
    create_sparse_file(&path, 64 * 1024 * 1024).unwrap();
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 64 * 1024 * 1024);
    assert!(meta.blocks() * 512 < meta.len(), "file is not sparse");
    assert!(matches!(
        create_sparse_file(&path, 4096),
        Err(BackingError::Io { action: "create", .. })
    ));
}

#[test]
fn the_volume_file_and_its_new_directory_are_private() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("volumes");
    let path = dir.join("spike.img");
    create_sparse_file(&path, 4096).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);

    // A directory that exists already keeps its mode.
    let open_dir = tmp.path().join("open");
    fs::create_dir(&open_dir).unwrap();
    fs::set_permissions(&open_dir, fs::Permissions::from_mode(0o755)).unwrap();
    create_sparse_file(&open_dir.join("b.img"), 4096).unwrap();
    assert_eq!(fs::metadata(&open_dir).unwrap().mode() & 0o777, 0o755);
}

#[test]
fn a_failed_resize_leaves_no_file_behind() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike.img");
    let err = create_sparse_file(&path, u64::MAX).unwrap_err();
    assert!(matches!(err, BackingError::Io { action: "resize", .. }), "{err}");
    assert!(!path.exists());
    // The retry is not blocked by a stub.
    create_sparse_file(&path, 4096).unwrap();
}

#[test]
fn finds_the_loop_device_of_a_file() {
    let runner = FakeRunner::new(|_, _| {
        out(
            0,
            "/dev/loop3: [0048]:131 (/var/lib/rancher/ambar/volumes/spike.img)\n",
            "",
        )
    });
    let found = find_loop(&runner, Path::new("/var/lib/rancher/ambar/volumes/spike.img")).unwrap();
    assert_eq!(found.as_deref(), Some("/dev/loop3"));
    assert_eq!(runner.calls(), ["losetup -j /var/lib/rancher/ambar/volumes/spike.img"]);

    let none = FakeRunner::new(|_, _| out(0, "", ""));
    assert_eq!(find_loop(&none, Path::new("/x.img")).unwrap(), None);
}

const TWICE: &str = "/dev/loop1: [65025]:262217 (/v/spike.img)\n/dev/loop0: [65025]:262217 (/v/spike.img)\n";

#[test]
fn a_file_attached_twice_lists_both_devices_and_is_ambiguous_for_find_loop() {
    let runner = FakeRunner::new(|_, _| out(0, TWICE, ""));
    let file = Path::new("/v/spike.img");
    assert_eq!(find_loops(&runner, file).unwrap(), ["/dev/loop1", "/dev/loop0"]);
    let err = find_loop(&runner, file).unwrap_err();
    assert!(
        matches!(&err, BackingError::MultipleLoopDevices { devices, .. } if devices.len() == 2),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "/v/spike.img is attached to several loop devices: [\"/dev/loop1\", \"/dev/loop0\"]"
    );
}

#[test]
fn find_loops_of_an_unattached_file_is_empty() {
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    assert!(find_loops(&runner, Path::new("/x.img")).unwrap().is_empty());
}

#[test]
fn losetup_output_that_is_not_a_device_is_rejected() {
    let garbage = FakeRunner::new(|_, _| out(0, "losetup: something odd\n", ""));
    let err = find_loops(&garbage, Path::new("/x.img")).unwrap_err();
    assert!(matches!(err, BackingError::UnexpectedOutput { .. }), "{err}");

    let empty = FakeRunner::new(|_, _| out(0, "\n", ""));
    let err = attach_loop(&empty, Path::new("/x.img")).unwrap_err();
    assert!(
        matches!(&err, BackingError::UnexpectedOutput { output, .. } if output.is_empty()),
        "{err}"
    );

    let relative = FakeRunner::new(|_, _| out(0, "loop3\n", ""));
    let err = attach_loop(&relative, Path::new("/x.img")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "losetup --find --show --direct-io=on /x.img printed unexpected output: \"loop3\""
    );
}

#[test]
fn attaches_with_direct_io_and_detaches() {
    let runner = FakeRunner::new(|_, args| {
        if args[0] == "--find" {
            out(0, "/dev/loop7\n", "")
        } else {
            out(0, "", "")
        }
    });
    assert_eq!(
        attach_loop(&runner, Path::new("/v/spike.img.new")).unwrap(),
        "/dev/loop7"
    );
    detach_loop(&runner, "/dev/loop7").unwrap();
    assert_eq!(
        runner.calls(),
        [
            "losetup --find --show --direct-io=on /v/spike.img.new",
            "losetup -d /dev/loop7"
        ]
    );
}

#[test]
fn a_failing_losetup_is_an_error() {
    let runner = FakeRunner::new(|_, _| out(1, "", "losetup: cannot find an unused loop device\n"));
    let err = attach_loop(&runner, Path::new("/v/spike.img")).unwrap_err();
    assert!(matches!(err, BackingError::Command { code: 1, .. }), "{err}");
}

#[test]
fn links_and_relinks_the_device() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("ambar").join("spike");
    assert_eq!(link_target(&link), None);
    link_device(&link, "/dev/loop3").unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/loop3"));
    link_device(&link, "/dev/loop5").unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/loop5"));
}

#[test]
fn replaces_a_dangling_link() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("spike");
    symlink("/dev/does-not-exist", &link).unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/does-not-exist"));
    link_device(&link, "/dev/loop4").unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/loop4"));
}

#[test]
fn refuses_to_replace_a_regular_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike");
    fs::write(&path, "data").unwrap();
    let err = link_device(&path, "/dev/loop4").unwrap_err();
    match &err {
        BackingError::Io {
            action: "replace",
            source,
            ..
        } => {
            assert_eq!(source.kind(), io::ErrorKind::AlreadyExists);
            assert!(source.to_string().contains("not a symbolic link"), "{source}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "data");
    assert_eq!(link_target(&path), None);
}

#[test]
fn linking_leaves_no_temporary_behind_and_clears_a_stale_one() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("spike");
    symlink("/dev/stale", tmp.path().join(".spike.tmp")).unwrap();
    link_device(&link, "/dev/loop3").unwrap();
    link_device(&link, "/dev/loop5").unwrap();
    let names: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["spike"]);
}
