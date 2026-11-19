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
    let path = tmp.path().join("spike.img.new");
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
fn the_volume_file_is_private_and_an_existing_directory_keeps_its_mode() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike.img");
    create_sparse_file(&path, 4096).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

    let open_dir = tmp.path().join("open");
    fs::create_dir(&open_dir).unwrap();
    fs::set_permissions(&open_dir, fs::Permissions::from_mode(0o755)).unwrap();
    create_sparse_file(&open_dir.join("b.img"), 4096).unwrap();
    assert_eq!(fs::metadata(&open_dir).unwrap().mode() & 0o777, 0o755);
}

#[test]
fn a_missing_directory_is_an_error_and_is_not_created() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("volumes");
    let err = create_sparse_file(&dir.join("spike.img"), 4096).unwrap_err();
    assert!(matches!(err, BackingError::Io { action: "create", .. }), "{err}");
    assert!(!dir.exists());
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
fn attaches_read_only() {
    let runner = FakeRunner::new(|_, args| {
        if args[0] == "--find" {
            out(0, "/dev/loop3\n", "")
        } else {
            out(0, "", "")
        }
    });
    assert_eq!(
        attach_loop_read_only(&runner, Path::new("/v/spike.img")).unwrap(),
        "/dev/loop3"
    );
    assert_eq!(runner.calls(), ["losetup --find --show --read-only /v/spike.img"]);
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

// ---- loop_holders: the one listing of the loop devices of an image ----

fn holder(device: &str, deleted: bool, confirmed: bool) -> LoopHolder {
    LoopHolder {
        device: device.to_string(),
        deleted,
        confirmed,
    }
}

/// A runner whose `losetup --list` prints `text`.
fn listing(text: String) -> FakeRunner {
    FakeRunner::new(move |_, _| out(0, &text, ""))
}

#[test]
fn loop_holders_tell_a_live_file_from_a_deleted_one() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let runner = listing(format!(
        "/dev/loop1 /other/a.img\n/dev/loop2 {0}/spike.img\n/dev/loop7 {0}/spike.img (deleted)\n/dev/loop8 {0}/spike.img.bak\n",
        dir.display()
    ));
    let found = loop_holders(&runner, &dir.join("spike.img")).unwrap();
    assert_eq!(
        found,
        [holder("/dev/loop2", false, true), holder("/dev/loop7", true, true)]
    );
    assert_eq!(runner.calls(), ["losetup --list --noheadings --output NAME,BACK-FILE"]);
}

#[test]
fn loop_holders_match_the_path_as_given_and_with_the_directory_resolved() {
    // `losetup` stores the canonical path; the volumes directory may be a link.
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    symlink(&real, &link).unwrap();
    let canonical = real.canonicalize().unwrap();
    let text = format!(
        "/dev/loop4 {}/vol.img (deleted)\n/dev/loop5 {}/vol.img\n",
        canonical.display(),
        link.display()
    );
    let found = loop_holders(&listing(text), &link.join("vol.img")).unwrap();
    assert_eq!(
        found,
        [holder("/dev/loop4", true, true), holder("/dev/loop5", false, true)]
    );
}

#[test]
fn loop_holders_report_a_failed_listing_and_an_unresolvable_directory() {
    let tmp = TempDir::new().unwrap();
    let failing = FakeRunner::new(|_, _| out(1, "", "no permission"));
    let err = loop_holders(&failing, &tmp.path().join("vol.img")).unwrap_err();
    assert!(err.to_string().contains("no permission"), "{err}");
    let missing = tmp.path().join("nowhere").join("vol.img");
    assert!(loop_holders(&listing(String::new()), &missing).is_err());
}

#[test]
fn a_line_that_is_not_a_device_and_a_file_is_an_error_not_no_loop() {
    let tmp = TempDir::new().unwrap();
    let image = tmp.path().join("vol.img");
    for text in ["/dev/loop3\n", "garbage\n", "/dev/loop1 /other/a.img\n/dev/loop3\n"] {
        let err = loop_holders(&listing(text.to_string()), &image).unwrap_err();
        assert!(matches!(err, BackingError::UnexpectedOutput { .. }), "{text:?}: {err}");
    }
    // Blank lines are not lines of the listing.
    assert!(loop_holders(&listing("\n  \n".to_string()), &image).unwrap().is_empty());
}

#[test]
fn a_backing_file_that_ends_with_a_star_is_an_error_not_no_loop() {
    // util-linux cuts a long name and puts `*` at its end: the line may be this file's.
    let tmp = TempDir::new().unwrap();
    let image = tmp.path().join("vol.img");
    let text = "/dev/loop1 /other/a.img\n/dev/loop3 /var/lib/rancher/ambar/volumes/a-very-long-na*\n";
    let err = loop_holders(&listing(text.to_string()), &image).unwrap_err();
    assert!(matches!(err, BackingError::UnexpectedOutput { .. }), "{err}");
    assert!(err.to_string().contains("a-very-long-na*"), "{err}");
}

// ---- loop_holders: the name-only form of a detached mount ----

/// A runner for an image `<dir>/<vol>.img`: `losetup --list` prints `text`,
/// `losetup -j` prints `same_inode` (device names, one per line).
fn detached_node(text: String, same_inode: &'static [&'static str]) -> FakeRunner {
    FakeRunner::new(move |_, args| {
        if args.first() == Some(&"-j") {
            let lines: String = same_inode
                .iter()
                .map(|d| format!("{d}: [65025]:262217 (/vol.img)\n"))
                .collect();
            return out(0, &lines, "");
        }
        out(0, &text, "")
    })
}

fn image_in(tmp: &TempDir, exists: bool) -> std::path::PathBuf {
    let image = tmp.path().join("vol.img");
    if exists {
        fs::write(&image, b"").unwrap();
    }
    image
}

#[test]
fn a_name_only_line_of_an_existing_image_is_a_holder_when_losetup_j_lists_the_device() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    let runner = detached_node("/dev/loop0 /vol.img\n".to_string(), &["/dev/loop0"]);
    assert_eq!(
        loop_holders(&runner, &image).unwrap(),
        [holder("/dev/loop0", false, true)]
    );
    assert_eq!(runner.calls().len(), 2);
    assert!(runner.calls()[1].starts_with("losetup -j "), "{:?}", runner.calls());
}

#[test]
fn a_name_only_line_of_an_existing_image_is_not_a_holder_when_losetup_j_does_not_list_it() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    // `losetup -j` names another device (or none): the line is a foreign file.
    for same_inode in [&["/dev/loop1"][..], &[][..]] {
        let runner = detached_node("/dev/loop0 /vol.img\n".to_string(), same_inode);
        assert!(loop_holders(&runner, &image).unwrap().is_empty(), "{same_inode:?}");
    }
}

#[test]
fn a_deleted_name_only_line_is_a_deleted_holder_of_a_missing_image() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, false);
    let runner = detached_node("/dev/loop0 /vol.img (deleted)\n".to_string(), &[]);
    assert_eq!(
        loop_holders(&runner, &image).unwrap(),
        [holder("/dev/loop0", true, false)]
    );
    assert_eq!(runner.calls().len(), 1, "no inode lookup for a missing image");
}

#[test]
fn a_name_only_line_of_a_missing_image_is_kept_without_an_inode_lookup() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, false);
    let runner = detached_node("/dev/loop0 /vol.img\n".to_string(), &[]);
    assert_eq!(
        loop_holders(&runner, &image).unwrap(),
        [holder("/dev/loop0", false, false)]
    );
    assert_eq!(runner.calls().len(), 1);
}

#[test]
fn a_deleted_name_only_line_is_kept_even_when_the_image_exists() {
    // The image was recreated under the same name: the old inode is still held.
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    let runner = detached_node("/dev/loop0 /vol.img (deleted)\n".to_string(), &[]);
    assert_eq!(
        loop_holders(&runner, &image).unwrap(),
        [holder("/dev/loop0", true, false)]
    );
}

#[test]
fn only_the_exact_root_form_matches_by_name() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, false);
    let text = "/dev/loop0 /other/dir/vol.img\n/dev/loop1 /vol.imgx\n/dev/loop2 /xvol.img\n\
                /dev/loop3 /x/vol.img (deleted)\n/dev/loop4 vol.img\n";
    let runner = detached_node(text.to_string(), &[]);
    assert!(loop_holders(&runner, &image).unwrap().is_empty());
}

#[test]
fn the_name_form_is_confirmed_once_for_all_unconfirmed_lines() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    let text = format!(
        "/dev/loop0 /vol.img\n/dev/loop1 /vol.img\n/dev/loop2 {}\n/dev/loop3 /vol.img (deleted)\n",
        image.canonicalize().unwrap().display()
    );
    let runner = detached_node(text, &["/dev/loop1", "/dev/loop2"]);
    assert_eq!(
        loop_holders(&runner, &image).unwrap(),
        [
            holder("/dev/loop1", false, true),
            holder("/dev/loop2", false, true),
            holder("/dev/loop3", true, false)
        ]
    );
    assert_eq!(runner.calls().iter().filter(|c| c.starts_with("losetup -j")).count(), 1);
}

#[test]
fn a_failed_inode_lookup_is_an_error_not_no_holder() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    let runner = FakeRunner::new(|_, args| {
        if args.first() == Some(&"-j") {
            return out(1, "", "losetup: denied");
        }
        out(0, "/dev/loop0 /vol.img\n", "")
    });
    let err = loop_holders(&runner, &image).unwrap_err();
    assert!(err.to_string().contains("denied"), "{err}");
}

#[test]
fn has_any_loop_reference_finds_unconfirmed_and_ignores_absent() {
    let tmp = TempDir::new().unwrap();
    let image = image_in(&tmp, true);
    let runner_with_ref = FakeRunner::new(|_, _| out(0, "/dev/loop0 /vol.img\n", ""));
    assert!(crate::backing::has_any_loop_reference(&runner_with_ref, &image).unwrap());

    let runner_without_ref = FakeRunner::new(|_, _| out(0, "/dev/loop0 /other.img\n", ""));
    assert!(!crate::backing::has_any_loop_reference(&runner_without_ref, &image).unwrap());
}

#[test]
fn test_lvm_create_and_remove() {
    let runner = FakeRunner::new(|program, args| match (program, args) {
        ("lvs", ["--noheadings", "-o", "lv_name", "--", "/dev/k3s-vg/pvc-test"]) => out(0, "pvc-test\n", ""),
        ("lvcreate", ["-y", "-L", "1073741824B", "-n", "pvc-test", "--", "k3s-vg"]) => out(0, "", ""),
        ("blkdiscard", ["--", "/dev/k3s-vg/pvc-test"]) => out(0, "", ""),
        ("lvremove", ["-y", "--", "/dev/k3s-vg/pvc-test"]) => out(0, "", ""),
        _ => out(1, "", "unexpected"),
    });

    assert!(crate::backing::find_lvm_volume(&runner, "k3s-vg", "pvc-test").unwrap());
    crate::backing::create_lvm_volume(&runner, "k3s-vg", "pvc-test", 1073741824).unwrap();
    crate::backing::remove_lvm_volume(&runner, "k3s-vg", "pvc-test").unwrap();
    assert_eq!(
        runner.calls(),
        [
            "lvs --noheadings -o lv_name -- /dev/k3s-vg/pvc-test",
            "lvcreate -y -L 1073741824B -n pvc-test -- k3s-vg",
            "blkdiscard -- /dev/k3s-vg/pvc-test",
            "blkdiscard -- /dev/k3s-vg/pvc-test",
            "lvremove -y -- /dev/k3s-vg/pvc-test"
        ]
    );
}

#[test]
fn test_lvm_create_fallback_to_wipefs_when_blkdiscard_fails() {
    let runner = FakeRunner::new(|program, args| match (program, args) {
        ("lvcreate", ["-y", "-L", "1073741824B", "-n", "pvc-test", "--", "k3s-vg"]) => out(0, "", ""),
        ("blkdiscard", ["--", "/dev/k3s-vg/pvc-test"]) => out(
            1,
            "",
            "blkdiscard: /dev/k3s-vg/pvc-test: BLKDISCARD ioctl failed: Operation not supported\n",
        ),
        ("wipefs", ["-a", "-f", "--", "/dev/k3s-vg/pvc-test"]) => out(0, "", ""),
        _ => out(1, "", "unexpected"),
    });

    crate::backing::create_lvm_volume(&runner, "k3s-vg", "pvc-test", 1073741824).unwrap();
    assert_eq!(
        runner.calls(),
        [
            "lvcreate -y -L 1073741824B -n pvc-test -- k3s-vg",
            "blkdiscard -- /dev/k3s-vg/pvc-test",
            "wipefs -a -f -- /dev/k3s-vg/pvc-test"
        ]
    );
}

#[test]
fn test_lvm_find_not_found() {
    let runner = FakeRunner::new(|_, _| out(5, "", "Failed to find logical volume \"k3s-vg/pvc-missing\"\n"));
    assert!(!crate::backing::find_lvm_volume(&runner, "k3s-vg", "pvc-missing").unwrap());
}

#[test]
fn test_lvm_find_vg_missing_returns_error() {
    let runner = FakeRunner::new(|_, _| {
        out(
            5,
            "",
            "  Volume group \"k3s-vg\" not found\n  Cannot process volume group k3s-vg\n",
        )
    });
    let err = crate::backing::find_lvm_volume(&runner, "k3s-vg", "pvc-missing").unwrap_err();
    assert!(matches!(err, BackingError::Command { .. }));
}

#[test]
fn test_lvm_validation_errors() {
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    assert!(matches!(
        crate::backing::find_lvm_volume(&runner, "../evil", "pvc-test"),
        Err(BackingError::InvalidVolumeGroup(_))
    ));
    assert!(matches!(
        crate::backing::create_lvm_volume(&runner, "k3s-vg", "invalid name!", 1024),
        Err(BackingError::InvalidVolumeName(_))
    ));
    assert!(matches!(
        crate::backing::remove_lvm_volume(&runner, "-evil-vg", "pvc-test"),
        Err(BackingError::InvalidVolumeGroup(_))
    ));
}
