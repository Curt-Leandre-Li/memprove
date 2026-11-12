//! U1 B5 (T21 V1-N-3) through the real binaries: a registry Project record
//! that does not decode (its revision 0, its `created_at` not UTF-8; a
//! writer that bypasses `memproved`, CHECK constraints off) makes
//! `project get` and `ledger verify` exit 1 with `409 capsule_tampered`
//! and the fixed detail of a Project whose state database is not bound to
//! it. Before, `project get` of revision 0 received a `500` without a body
//! and exited 4 ("response violates the contract"), and with `created_at`
//! not UTF-8 every read answered `503 service_not_ready`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, step};

#[test]
fn b5_a_registry_record_that_does_not_decode_exits_1_with_a_problem() {
    for (case, value) in [
        ("revision 0", "revision = 0"),
        ("created_at not UTF-8", "created_at = CAST(X'FF' AS TEXT)"),
    ] {
        step(case);
        let mut setup = Setup::new(&["a"]);
        let operator = &setup.operator;
        let project = setup.project_id.clone();
        assert_eq!(
            operator.run(&["project", "get", &project], false).code,
            Some(0)
        );
        setup.stop();
        let output = std::process::Command::new("sqlite3")
            .arg(setup.operator.data_dir.join("registry.sqlite"))
            .arg(format!(
                "PRAGMA ignore_check_constraints = ON; \
                 UPDATE projects SET {value} WHERE project_id = '{project}';"
            ))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        setup.start();
        for words in [
            &["project", "get", project.as_str()][..],
            &["ledger", "verify", "--project", project.as_str()],
        ] {
            let run = setup.operator.run(words, false);
            assert_eq!(run.code, Some(1), "{case}: {words:?}: {}", run.stderr);
            assert!(
                run.stderr.contains("capsule_tampered"),
                "{case}: {words:?}: {}",
                run.stderr
            );
            assert!(
                run.stderr.contains("registry record does not decode"),
                "{case}: {words:?}: {}",
                run.stderr
            );
            assert!(
                !run.stderr.contains("violates the contract"),
                "{case}: {words:?}: {}",
                run.stderr
            );
        }
        setup.stop();
    }
}
