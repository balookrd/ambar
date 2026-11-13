use std::fs;
use std::os::unix::fs::MetadataExt;

use tempfile::TempDir;

use super::*;
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
    for (size, peers) in [(GIB, 2), (GIB, 7), (10 * GIB, 7), (4096, 31), (3 * GIB + 4096, 3)] {
        let file = backing_file_bytes(size, peers);
        assert_eq!(file % 4096, 0);
        let usable = file - metadata_bytes(file, peers);
        assert!(usable >= size, "size {size} peers {peers}: usable {usable}");
        assert!(
            usable - size < 2 * 4096 + 8 * 512 * u64::from(peers),
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
