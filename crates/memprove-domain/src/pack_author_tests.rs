//! U10 V6 (Owner O5, E1 revision 13; D3 §11 item 6): a statement line names
//! its author Session's agent: `from Session k「<name>」` when Session k declared one,
//! `from Session k` for a Session that declared none,
//! and `from unlisted Session <id>` for one the Capsule does not list, as before.
//! The strict reader of `pack_parse.rs` reads the name back and refuses a
//! line whose name is missing, wrong, or on a Session that declared none.

use serde_json::{Value, json};

use super::parse::check;
use super::{capsule_value, fixture};
use crate::{PackCapsule, Sha256Digest, render_pack, render_pack_claims, select_pack};

fn render_at(value: &Value, budget: usize) -> String {
    let capsule = PackCapsule::from_json(&value.to_string()).expect("a Pack capsule");
    render_pack(&select_pack(&capsule, budget).expect("fits"))
}

fn redigest(text: &str) -> String {
    let body = &text[..=text.trim_end_matches('\n').rfind('\n').unwrap()];
    format!(
        "{body}Digest: sha256:{}\n",
        Sha256Digest::of_bytes(body.as_bytes())
    )
}

fn claim_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.starts_with('#')).collect()
}

#[test]
fn u10_v6_a_statement_line_names_the_agent_of_its_session() {
    let unnamed = capsule_value(&fixture());
    let mut value = unnamed.clone();
    value["sessions"][0]["agent"] =
        json!({"name": "u10-s「s」", "role": "sub_agent", "parent_session_id": null});
    for budget in [9_000, 100_000] {
        let text = render_at(&value, budget);
        check(&value, &text, budget).unwrap_or_else(|error| panic!("{budget}: {error}\n{text}"));
        let plain = render_at(&unnamed, budget);
        check(&unnamed, &plain, budget).unwrap();
        let named = claim_lines(&text)
            .into_iter()
            .filter(|line| line.contains("from Session 1「"))
            .collect::<Vec<_>>();
        assert!(!named.is_empty(), "{text}");
        for line in &named {
            assert!(
                line.contains("; from Session 1「u10-s\\「s\\」」)"),
                "{line}"
            );
        }
        assert!(
            claim_lines(&text)
                .iter()
                .any(|line| line.contains("; from Session 2)")),
            "{text}"
        );
        // Without the agent, each line is the same line without the name.
        if budget == 100_000 {
            assert_eq!(
                claim_lines(&text)
                    .iter()
                    .map(|line| line.replace("from Session 1「u10-s\\「s\\」」", "from Session 1"))
                    .collect::<Vec<_>>(),
                claim_lines(&plain)
            );
        }
        // A name left out, a wrong one, or one on a Session without an
        // agent is not the Pack's.
        let line = named[0];
        for forged in [
            line.replace("from Session 1「u10-s\\「s\\」」", "from Session 1"),
            line.replace(
                "from Session 1「u10-s\\「s\\」」",
                "from Session 1「other」",
            ),
        ] {
            let forged = redigest(&text.replacen(line, &forged, 1));
            assert!(check(&value, &forged, budget).is_err(), "{forged}");
        }
        let unnamed_line = claim_lines(&plain)
            .into_iter()
            .find(|line| line.contains("from Session 2)"))
            .unwrap();
        let forged = redigest(&plain.replacen(
            unnamed_line,
            &unnamed_line.replace("from Session 2)", "from Session 2「x」)"),
            1,
        ));
        assert!(check(&unnamed, &forged, budget).is_err(), "{forged}");
    }
    // `--claims` writes the same lines.
    let capsule = PackCapsule::from_json(&value.to_string()).unwrap();
    let full = render_pack_claims(&capsule, &[(1, 8)]).unwrap();
    assert!(full.contains("from Session 1「u10-s\\「s\\」」"), "{full}");
}
