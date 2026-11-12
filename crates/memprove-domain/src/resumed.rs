//! ADR 0014 E6 (P4-F): which Context Pack a new Session declares it
//! continues from, and the `capsule.recovered@1` payload `memproved`
//! writes for it. Pure: no IO.
//!
//! The digests are the Session's own declaration (`declared: true`): the
//! daemon checks their format, that the ledger sequence is not above its
//! high-water mark, and that the budget is one the Pack accepts, but it
//! cannot recompute the Capsule or the Pack the client was given.

use serde_json::{Value, json};

use crate::{PACK_MAX_BUDGET, PACK_MIN_BUDGET, Sha256Digest, TaskId};

/// The Pack a Session declares it continues from (`resumed_from`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResumedFrom {
    /// The `capsule_digest` the Pack names on its first line.
    pub capsule_digest: Sha256Digest,
    /// The ledger sequence N the Pack names.
    pub project_sequence: u64,
    /// The Pack's `pack_digest` (its `Digest: ` line).
    pub pack_digest: Sha256Digest,
    /// The Pack's budget, in UTF-16 code units.
    pub budget: u64,
}

/// Why a `resumed_from` is refused (`400 invalid_request`); the text is the
/// `detail`, which names the member and the rule, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumedFromRefusal(pub String);

fn refusal(rule: &str) -> ResumedFromRefusal {
    ResumedFromRefusal(format!("$.resumed_from{rule}"))
}

/// `sha256:<64 lowercase hex>`, the spelling of every wire digest.
fn digest(value: &str) -> Option<Sha256Digest> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| Sha256Digest::parse(hex).ok())
    .flatten()
}

impl ResumedFrom {
    /// Reads `resumed_from` from a request body member: an object with
    /// exactly `capsule_digest`, `project_sequence`, `pack_digest`, and
    /// `budget`; the digests `sha256:<64 lowercase hex>`; the budget within
    /// the Pack's range ([`PACK_MIN_BUDGET`] through [`PACK_MAX_BUDGET`]).
    ///
    /// # Errors
    ///
    /// [`ResumedFromRefusal`] naming the first member that does not hold.
    pub fn from_request(value: &Value) -> Result<Self, ResumedFromRefusal> {
        let Some(object) = value.as_object() else {
            return Err(refusal(" must be an object"));
        };
        let members = [
            "capsule_digest",
            "project_sequence",
            "pack_digest",
            "budget",
        ];
        if object.keys().any(|key| !members.contains(&key.as_str())) {
            return Err(refusal(
                " has a member other than capsule_digest, project_sequence, pack_digest, and budget",
            ));
        }
        let text = |name: &str| object.get(name).and_then(Value::as_str);
        let number = |name: &str| object.get(name).and_then(Value::as_u64);
        Self::new(
            text("capsule_digest"),
            number("project_sequence"),
            text("pack_digest"),
            number("budget"),
        )
    }

    /// Checks the four members as [`Self::from_request`] does; `None` is a
    /// member that is missing or not of its type. The CLI reads its local
    /// file of the last Pack through it (P4-F, Owner condition i).
    ///
    /// # Errors
    ///
    /// [`ResumedFromRefusal`] naming the first member that does not hold.
    pub fn new(
        capsule_digest: Option<&str>,
        project_sequence: Option<u64>,
        pack_digest: Option<&str>,
        budget: Option<u64>,
    ) -> Result<Self, ResumedFromRefusal> {
        let Some(capsule_digest) = capsule_digest.and_then(digest) else {
            return Err(refusal(".capsule_digest must be sha256:<64 lowercase hex>"));
        };
        let Some(pack_digest) = pack_digest.and_then(digest) else {
            return Err(refusal(".pack_digest must be sha256:<64 lowercase hex>"));
        };
        let Some(project_sequence) =
            project_sequence.filter(|sequence| *sequence <= crate::MAX_SAFE_INTEGER)
        else {
            return Err(refusal(
                ".project_sequence must be an integer from 0 through 2^53 - 1",
            ));
        };
        let Some(budget) = budget.filter(|budget| {
            usize::try_from(*budget)
                .is_ok_and(|budget| (PACK_MIN_BUDGET..=PACK_MAX_BUDGET).contains(&budget))
        }) else {
            return Err(refusal(
                ".budget must be an integer from 1000 through 1000000 (the Pack's range)",
            ));
        };
        Ok(Self {
            capsule_digest,
            project_sequence,
            pack_digest,
            budget,
        })
    }

    /// The daemon's own check against the ledger: N is at most the
    /// Project's high-water mark when the Session starts.
    ///
    /// # Errors
    ///
    /// [`ResumedFromRefusal`] when `project_sequence` is above `high_water`.
    pub fn check_sequence(&self, high_water: u64) -> Result<(), ResumedFromRefusal> {
        if self.project_sequence <= high_water {
            Ok(())
        } else {
            Err(ResumedFromRefusal(
                "$.resumed_from.project_sequence is above the Project's latest ledger sequence"
                    .to_owned(),
            ))
        }
    }

    /// The member as the request and the Capsule spell it, with
    /// `declared: true` when `declared` (the Capsule's `sessions[]`).
    #[must_use]
    pub fn wire(&self, declared: bool) -> Value {
        let mut value = json!({
            "capsule_digest": format!("sha256:{}", self.capsule_digest),
            "project_sequence": self.project_sequence,
            "pack_digest": format!("sha256:{}", self.pack_digest),
            "budget": self.budget,
        });
        if declared {
            value["declared"] = json!(true);
        }
        value
    }

    /// The `capsule.recovered@1` payload: `{task_id, capsule_digest,
    /// project_sequence, pack_digest, budget, declared: true}`.
    #[must_use]
    pub fn payload(&self, task_id: &TaskId) -> Value {
        let mut payload = self.wire(true);
        payload["task_id"] = json!(task_id);
        payload
    }

    /// Reads a `capsule.recovered@1` payload of `task_id`; `None` for one of
    /// another Task or one that does not decode.
    #[must_use]
    pub fn from_payload(task_id: &TaskId, payload: &Value) -> Option<Self> {
        let object = payload.as_object()?;
        if object.len() != 6
            || object.get("task_id")?.as_str()? != task_id.as_str()
            || object.get("declared")? != &Value::Bool(true)
        {
            return None;
        }
        let mut wire = payload.clone();
        let wire_object = wire.as_object_mut()?;
        wire_object.remove("task_id");
        wire_object.remove("declared");
        Self::from_request(&wire).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn good() -> Value {
        json!({
            "capsule_digest": hex('a'),
            "project_sequence": 7,
            "pack_digest": hex('b'),
            "budget": 9000,
        })
    }

    #[test]
    fn a_well_formed_resumed_from_round_trips_through_the_payload() {
        let task_id = TaskId::new();
        let resumed = ResumedFrom::from_request(&good()).unwrap();
        assert_eq!(resumed.project_sequence, 7);
        assert_eq!(resumed.budget, 9000);
        let payload = resumed.payload(&task_id);
        assert_eq!(payload["declared"], json!(true));
        assert_eq!(payload["task_id"], json!(task_id.as_str()));
        assert_eq!(
            ResumedFrom::from_payload(&task_id, &payload),
            Some(resumed.clone())
        );
        assert_eq!(ResumedFrom::from_payload(&TaskId::new(), &payload), None);
        assert!(resumed.check_sequence(7).is_ok());
        assert!(resumed.check_sequence(6).is_err());
    }

    #[test]
    fn every_malformed_member_is_refused() {
        let mut cases = Vec::new();
        for (name, value) in [
            ("capsule_digest", json!("sha256:ABC")),
            ("capsule_digest", json!(hex('A'))),
            ("capsule_digest", json!("a".repeat(64))),
            ("pack_digest", json!(hex('g'))),
            ("pack_digest", json!(null)),
            ("project_sequence", json!(-1)),
            ("project_sequence", json!(9_007_199_254_740_992_u64)),
            ("project_sequence", json!("7")),
            ("budget", json!(999)),
            ("budget", json!(1_000_001)),
            ("budget", json!(9000.5)),
        ] {
            let mut body = good();
            body[name] = value;
            cases.push(body);
        }
        let mut extra = good();
        extra["declared"] = json!(true);
        cases.push(extra);
        let mut missing = good();
        missing.as_object_mut().unwrap().remove("budget");
        cases.push(missing);
        cases.push(json!("sha256:x"));
        for case in cases {
            let refusal = ResumedFrom::from_request(&case).unwrap_err();
            assert!(refusal.0.starts_with("$.resumed_from"), "{case}");
        }
        for budget in [1000, 1_000_000] {
            let mut body = good();
            body["budget"] = json!(budget);
            assert!(ResumedFrom::from_request(&body).is_ok());
        }
    }
}
