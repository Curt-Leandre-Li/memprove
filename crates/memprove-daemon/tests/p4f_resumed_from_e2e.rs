//! P4-F (ADR 0014 E6, Owner conditions f to i) through the real binaries:
//! `resume --pack` records the Pack it printed next to the context file,
//! `begin` of the same Task declares it as `resumed_from`, `memproved`
//! writes `capsule.recovered@1` with the Session, the Capsule and the Pack
//! read it back as the Session's own report, and it survives SIGKILL. A
//! different Task, no file, or a damaged file declares nothing; a client
//! cannot write `capsule.recovered`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use p4b_support::{Setup, sequence, step};
use serde_json::Value;

/// The file of the last Pack in the agent's configuration directory.
fn pack_files(setup: &Setup) -> Vec<PathBuf> {
    let directory = setup.agent.config_dir.join("contexts");
    let mut files = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.to_string_lossy().ends_with(".pack.json"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn session<'a>(capsule: &'a Value, session_id: &str) -> &'a Value {
    capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == session_id)
        .unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // One Task across Sessions, step by step.
fn begin_declares_the_pack_resume_printed() {
    let mut setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    step("Session 1 records a verified fact and ends");
    let first = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "P4-F",
    ]);
    let task = first["task_id"].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    agent.ok(&["end", "--reason", "done for now"]);

    step("resume --pack --claims records nothing");
    let claims = agent.run(&["resume", "--pack", "--claims", "1"], false);
    assert_eq!(claims.code, Some(0), "{}", claims.stderr);
    assert!(pack_files(&setup).is_empty());

    step("resume --pack records its Pack next to the context file");
    let pack = agent.run(&["resume", "--pack"], false);
    assert_eq!(pack.code, Some(0), "{}", pack.stderr);
    assert!(pack.stderr.is_empty(), "{}", pack.stderr);
    let files = pack_files(&setup);
    assert_eq!(files.len(), 1);
    assert_eq!(
        fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let recorded: Value = serde_json::from_str(&fs::read_to_string(&files[0]).unwrap()).unwrap();
    let capsule_before = agent.capsule();
    let digest = pack
        .stdout
        .lines()
        .last()
        .unwrap()
        .strip_prefix("Digest: ")
        .unwrap()
        .to_owned();
    assert_eq!(recorded["task_id"], task.as_str());
    assert_eq!(recorded["capsule_digest"], capsule_before["capsule_digest"]);
    assert_eq!(recorded["project_sequence"], sequence(&capsule_before));
    assert_eq!(recorded["pack_digest"], digest.as_str());
    assert_eq!(recorded["budget"], 9000);

    step("begin of the same Task declares it");
    let second = agent.run(&["begin"], false);
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    assert!(
        second
            .stderr
            .contains("declares that it resumes from the Pack of ledger sequence"),
        "{}",
        second.stderr
    );
    assert!(pack_files(&setup).is_empty(), "declared once");
    let second: Value = serde_json::from_str(second.stdout.trim()).unwrap();
    let second_session = second["session_id"].as_str().unwrap().to_owned();
    let capsule = agent.capsule();
    let declared = &session(&capsule, &second_session)["resumed_from"];
    assert_eq!(declared["capsule_digest"], recorded["capsule_digest"]);
    assert_eq!(declared["project_sequence"], recorded["project_sequence"]);
    assert_eq!(declared["pack_digest"], recorded["pack_digest"]);
    assert_eq!(declared["budget"], 9000);
    assert_eq!(declared["declared"], true);
    assert!(session(&capsule, first["session_id"].as_str().unwrap())["resumed_from"].is_null());
    let text = agent.bytes(&["resume", "--pack"]);
    let line = text
        .lines()
        .find(|line| line.contains(&second_session))
        .unwrap()
        .to_owned();
    // P4-H1: the declared digest by its first 12 digits.
    assert!(
        line.ends_with(&format!(
            "; continues from the Pack at ledger sequence {} (self-reported, digest {})",
            recorded["project_sequence"],
            &digest[.."sha256:".len() + 12]
        )),
        "{line}"
    );
    // Reading the Pack again recorded it again; take it out of the way.
    for file in pack_files(&setup) {
        fs::remove_file(file).unwrap();
    }

    step("the Event is in the chain and survives SIGKILL");
    let project = setup.project_id.clone();
    let events = setup
        .operator
        .ok(&["event", "list", "--project", &project, "--limit", "500"]);
    let recovered = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event_type"] == "capsule.recovered")
        .collect::<Vec<_>>();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0]["session_id"], second_session.as_str());
    assert_eq!(recovered[0]["producer"]["name"], "continuityd");
    assert_eq!(recovered[0]["payload"]["declared"], true);
    assert_eq!(recovered[0]["payload"]["task_id"], task.as_str());
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(verified["valid"], true);
    let high_water = sequence(&capsule);
    setup.kill();
    setup.start();
    let after = agent.capsule();
    assert_eq!(sequence(&after), high_water);
    assert_eq!(session(&after, &second_session)["resumed_from"], *declared);
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(verified["valid"], true);

    step("a client cannot write capsule.recovered");
    let envelope = serde_json::json!({
        "envelope_version": "1.0",
        "event_id": second["agent_id"],
        "project_id": project,
        "workspace_id": second["workspace_id"],
        "session_id": second_session,
        "event_type": "capsule.recovered",
        "payload_schema": "capsule.recovered@1",
        "producer": { "name": "agent", "version": "1", "sequence": 1 },
        "privacy_class": "internal",
        "payload": recovered[0]["payload"],
    });
    let envelope_file = setup.root_path.join("forged.json");
    fs::write(&envelope_file, envelope.to_string()).unwrap();
    for cli in [&agent, &setup.operator] {
        let refused = cli.run(&["event", "ingest", envelope_file.to_str().unwrap()], false);
        assert_eq!(refused.code, Some(1));
        assert!(
            refused.stderr.contains("failed: 400 invalid_request"),
            "{}",
            refused.stderr
        );
    }
    assert_eq!(sequence(&agent.capsule()), high_water);
    agent.ok(&["end", "--reason", "second done"]);

    step("no file: begin declares nothing");
    let third = agent.ok(&["begin"]);
    let third_session = third["session_id"].as_str().unwrap().to_owned();
    assert!(session(&agent.capsule(), &third_session)["resumed_from"].is_null());
    agent.ok(&["end", "--reason", "third done"]);

    step("a damaged file: begin declares nothing and says so");
    agent.bytes(&["resume", "--pack"]);
    let files = pack_files(&setup);
    assert_eq!(files.len(), 1);
    fs::write(&files[0], b"{\"task_id\":1}\n").unwrap();
    let fourth = agent.run(&["begin"], false);
    assert_eq!(fourth.code, Some(0), "{}", fourth.stderr);
    assert!(
        fourth
            .stderr
            .contains("does not declare the Pack it resumes from"),
        "{}",
        fourth.stderr
    );
    let fourth: Value = serde_json::from_str(fourth.stdout.trim()).unwrap();
    let fourth_session = fourth["session_id"].as_str().unwrap().to_owned();
    assert!(session(&agent.capsule(), &fourth_session)["resumed_from"].is_null());
    agent.ok(&["end", "--reason", "fourth done"]);
    fs::remove_file(&files[0]).unwrap();

    step("a Pack of another Task is not declared");
    agent.bytes(&["resume", "--pack"]);
    assert_eq!(pack_files(&setup).len(), 1);
    let other = agent.ok(&["begin", "--new-task", "another Task"]);
    let other_session = other["session_id"].as_str().unwrap().to_owned();
    let other_capsule = agent.ok(&["resume"]);
    assert_ne!(other["task_id"], task.as_str());
    assert!(session(&other_capsule, &other_session)["resumed_from"].is_null());
    assert_eq!(pack_files(&setup).len(), 1, "kept for its own Task");
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(verified["valid"], true);
}
