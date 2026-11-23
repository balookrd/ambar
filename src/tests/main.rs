#[cfg(feature = "jemalloc")]
#[test]
fn malloc_conf_disables_the_calloc_memset_threshold() {
    let conf = std::str::from_utf8(super::MALLOC_CONF).expect("MALLOC_CONF is ASCII");
    let conf = conf.strip_suffix('\0').expect("MALLOC_CONF is NUL-terminated");
    let pairs: Vec<&str> = conf.split(',').collect();
    for expected in [
        "background_thread:true",
        "dirty_decay_ms:5000",
        "muzzy_decay_ms:0",
        "calloc_madvise_threshold:0",
    ] {
        assert!(pairs.contains(&expected), "missing {expected:?} in {conf:?}");
    }
}

/// The built-in `malloc_conf` symbol has to reach jemalloc, or none of its
/// settings apply; `background_thread` stands in for the whole string. Linux
/// only: elsewhere tikv-jemalloc-sys prefixes its symbols and the unprefixed
/// `malloc_conf` is not read.
#[cfg(all(target_os = "linux", feature = "jemalloc"))]
#[test]
fn malloc_conf_reaches_jemalloc() {
    // SAFETY: `opt.background_thread` is a read-only `bool` mallctl.
    let background = unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"opt.background_thread\0") }
        .expect("opt.background_thread is readable");
    assert!(background, "built-in malloc_conf was not applied");
}

#[cfg(all(unix, feature = "jemalloc"))]
#[test]
fn jemalloc_page_matches_the_kernel_page() {
    let (jemalloc, kernel) = super::allocator_page_sizes().expect("both page sizes are readable");
    assert_eq!(jemalloc, kernel, "jemalloc page {jemalloc} vs kernel page {kernel}");
}

#[cfg(all(target_os = "linux", feature = "jemalloc"))]
#[test]
fn jemalloc_background_thread_survives_fork_exec() {
    use ambar::exec::{Runner, SystemRunner};

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let runner = SystemRunner::default();
        for i in 0..20 {
            let out = tokio::task::spawn_blocking({
                let r = &runner;
                // SAFETY/NOTE: Runner::run blocks on fork/exec + reader threads.
                let timeout = r.timeout;
                move || {
                    SystemRunner { timeout }
                        .run("/bin/true", &[])
                        .expect("fork/exec should succeed under jemalloc")
                }
            })
            .await
            .unwrap();
            assert_eq!(out.code, 0, "iteration {i} failed");
        }
    });
}
