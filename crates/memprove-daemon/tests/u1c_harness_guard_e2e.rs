//! U1 C6 (T21 V1R-6, V3R-N-1): a test harness that panics while it waits
//! for `memproved` to become ready no longer leaves the daemon running.
//! Before, `Daemon::start` of the shared harness spawned the daemon and
//! built its guard only after the readiness loop, so a panic in the loop
//! (a missing `memprove` binary, a readiness timeout) dropped a bare
//! `Child` and left an orphan, with the temporary directory that held its
//! data directory removed.

mod p4b_support;

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    process::Command,
};

use p4b_support::{Cli, Daemon};

/// The processes whose command line names `needle`.
fn processes_naming(needle: &str) -> Vec<u32> {
    let listing = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("ps lists the processes");
    assert!(listing.status.success(), "ps failed");
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

/// The harness panics in the first readiness probe (the CLI's working
/// directory does not exist), after `memproved` has been spawned. No
/// process of that data directory is left.
#[test]
fn a_panic_while_the_daemon_starts_leaves_no_memproved() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let cli = Cli {
        data_dir: data_dir.clone(),
        config_dir: root.path().join("config"),
        token: root.path().join("token"),
        cwd: root.path().join("no-such-directory"),
        label: "guard",
    };
    let needle = format!("--data-dir {}", data_dir.display());
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _daemon = Daemon::start(&cli, &root.path().join("daemon.log"));
    }));
    assert!(panicked.is_err(), "the harness was meant to panic");
    let left = processes_naming(&needle);
    for pid in &left {
        // Not a guard of the test: do not leave the orphan running.
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
    }
    assert!(
        left.is_empty(),
        "memproved was left running after a panic: pids {left:?}"
    );
}
