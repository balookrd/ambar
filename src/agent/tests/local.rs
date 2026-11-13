use std::fs;
use std::os::unix::fs::symlink;

use tempfile::TempDir;

use super::*;
use crate::tests::temp_paths;

fn dirs(tmp: &TempDir) -> Paths {
    let paths = temp_paths(tmp);
    for dir in [&paths.volumes_dir, &paths.res_dir, &paths.dev_dir] {
        fs::create_dir_all(dir).unwrap();
    }
    paths
}

fn touch(path: std::path::PathBuf) {
    fs::write(path, "").unwrap();
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// ---- local_volumes ----

#[test]
fn every_kind_of_trace_makes_the_volume_local() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("with-image"));
    touch(paths.res_file("only-res"));
    touch(paths.applied_res("only-applied"));
    symlink("/dev/loop7", paths.device_link("only-link")).unwrap();

    let found = local_volumes(&paths).unwrap();
    assert_eq!(found, names(&["only-applied", "only-link", "only-res", "with-image"]));
}

#[test]
fn a_volume_is_listed_once_however_many_traces_it_has() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    touch(paths.res_file("vol"));
    touch(paths.applied_res("vol"));
    symlink("/dev/loop7", paths.device_link("vol")).unwrap();
    assert_eq!(local_volumes(&paths).unwrap(), names(&["vol"]));
}

#[test]
fn markers_and_lock_files_without_an_image_do_not_make_a_volume_local() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.ready_marker("only-ready"));
    touch(paths.synced_marker("only-synced"));
    touch(paths.leftover_creating_file("only-creating"));
    touch(paths.lock_file("only-lock"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
}

#[test]
fn names_that_are_not_valid_volume_names_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.volumes_dir.join("Upper.img"));
    touch(paths.volumes_dir.join("with space.img"));
    touch(paths.volumes_dir.join("-lead.img"));
    touch(paths.volumes_dir.join("all.img"));
    touch(paths.volumes_dir.join(".img"));
    touch(paths.res_dir.join("bad_name.res"));
    symlink("/dev/loop7", paths.dev_dir.join("Bad")).unwrap();
    touch(paths.image("good"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&["good"]));
}

#[test]
fn files_that_are_not_volume_files_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.res_dir.join("global_common.conf"));
    touch(paths.res_dir.join("drbd.res.bak"));
    touch(paths.volumes_dir.join("notes.txt"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
}

#[test]
fn missing_resource_and_device_directories_mean_nothing_there() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    touch(paths.image("vol"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&["vol"]));
}

// ---- local ----

#[test]
fn local_reports_the_image_and_the_synced_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    let state = local(&paths, "vol").unwrap();
    assert_eq!(
        state,
        Local {
            image: true,
            traces: false,
            synced: false
        }
    );
    touch(paths.synced_marker("vol"));
    let state = local(&paths, "vol").unwrap();
    assert!(state.image && state.synced);
}

#[test]
fn local_counts_resource_files_and_links_as_traces() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.res_file("a"));
    touch(paths.applied_res("b"));
    symlink("/dev/does-not-exist", paths.device_link("c")).unwrap();
    for volume in ["a", "b", "c"] {
        let state = local(&paths, volume).unwrap();
        assert_eq!(
            state,
            Local {
                image: false,
                traces: true,
                synced: false
            },
            "{volume}"
        );
    }
}

#[test]
fn local_does_not_count_markers_or_the_lock_file_as_traces() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.ready_marker("vol"));
    touch(paths.synced_marker("vol"));
    touch(paths.leftover_creating_file("vol"));
    touch(paths.lock_file("vol"));
    let state = local(&paths, "vol").unwrap();
    assert_eq!(
        state,
        Local {
            image: false,
            traces: false,
            synced: true
        }
    );
}

#[test]
fn local_of_an_unknown_volume_is_empty() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    assert_eq!(
        local(&paths, "vol").unwrap(),
        Local {
            image: false,
            traces: false,
            synced: false
        }
    );
}

#[test]
fn local_refuses_a_name_that_is_not_a_valid_volume_name() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    for bad in ["../x", "A", "", "all", "a/b"] {
        let err = local(&paths, bad).unwrap_err();
        assert!(matches!(err, ReconcileError::Spec(_)), "{bad:?}: {err}");
    }
}

#[test]
fn an_unreadable_path_is_an_error_not_absence() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    // A regular file where the volumes directory should be: `stat` of
    // `<file>/vol.img` fails with ENOTDIR, which is not "no such file".
    fs::remove_dir_all(&paths.volumes_dir).unwrap();
    touch(paths.volumes_dir.clone());
    let err = local(&paths, "vol").unwrap_err();
    assert!(matches!(err, ReconcileError::Io { .. }), "{err}");
}

// ---- P2: only what ambar wrote is a trace ----

#[test]
fn a_name_with_no_file_of_ambars_is_neither_a_local_volume_nor_a_trace() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    // The kernel has `foreign` configured; ambar never wrote a file for it.
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
    let state = local(&paths, "foreign").unwrap();
    assert!(!state.traces && !state.image, "{state:?}");
}
