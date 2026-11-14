//! A baseline: the per-task pass counts of one run, kept in the repository
//! (`evals/baseline.json`) so later runs are compared with it.
//!
//! The file is an object with the members `schema`
//! (`memprove.eval.baseline.v1`), `run_id`, `agent`, `model` and `tasks`,
//! in that order; each task is `{"id", "passed", "trials"}`, in the order
//! the run ran them. Unknown members are refused.

use std::path::Path;

use crate::{
    BASELINE_SCHEMA, EvalError, Outcome, RunRecord,
    json::{Out, Reader},
};

/// The pass counts of one task in the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineTask {
    /// The task's id.
    pub id: String,
    /// Trials that passed.
    pub passed: u32,
    /// Trials run.
    pub trials: u32,
}

/// A baseline file (`memprove.eval.baseline.v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    /// The run it was taken from.
    pub run_id: String,
    /// The agent kind of that run.
    pub agent: String,
    /// Its model, when one was named.
    pub model: Option<String>,
    /// Every task of that run, in its order.
    pub tasks: Vec<BaselineTask>,
}

const TOP_MEMBERS: [&str; 5] = ["schema", "run_id", "agent", "model", "tasks"];
const TASK_MEMBERS: [&str; 3] = ["id", "passed", "trials"];

impl Baseline {
    /// The baseline of `run`: every task of [`RunRecord::tasks`] in its
    /// order, then any task that has results but no entry there, each with
    /// its passed and total trials.
    #[must_use]
    pub fn from_run(run: &RunRecord) -> Self {
        let mut tasks: Vec<BaselineTask> = run
            .tasks
            .iter()
            .map(|task| BaselineTask {
                id: task.id.clone(),
                passed: 0,
                trials: 0,
            })
            .collect();
        for result in &run.results {
            let index = if let Some(index) = tasks.iter().position(|task| task.id == result.task) {
                index
            } else {
                tasks.push(BaselineTask {
                    id: result.task.clone(),
                    passed: 0,
                    trials: 0,
                });
                tasks.len() - 1
            };
            let task = &mut tasks[index];
            task.trials = task.trials.saturating_add(1);
            if result.outcome == Outcome::Passed {
                task.passed = task.passed.saturating_add(1);
            }
        }
        Self {
            run_id: run.run_id.clone(),
            agent: run.agent.kind.name().to_owned(),
            model: run.agent.model.clone(),
            tasks,
        }
    }

    /// The task `id`, when the baseline has it.
    #[must_use]
    pub fn task(&self, id: &str) -> Option<&BaselineTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    /// The file's text.
    #[must_use]
    pub fn to_json(&self) -> String {
        Out::Object(vec![
            ("schema", Out::str(BASELINE_SCHEMA)),
            ("run_id", Out::str(&self.run_id)),
            ("agent", Out::str(&self.agent)),
            ("model", Out::opt(self.model.as_deref(), Out::str)),
            (
                "tasks",
                Out::Array(
                    self.tasks
                        .iter()
                        .map(|task| {
                            Out::Object(vec![
                                ("id", Out::str(&task.id)),
                                ("passed", Out::uint(u64::from(task.passed))),
                                ("trials", Out::uint(u64::from(task.trials))),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
        .render()
    }

    /// Reads a baseline file's text (from `path`, for messages).
    ///
    /// # Errors
    /// [`EvalError::Invalid`] naming the first member that is wrong.
    pub fn from_json(text: &str, path: Option<&Path>) -> Result<Self, EvalError> {
        let value = crate::json::parse(text, path)?;
        let top = Reader::new(&value, path, "")?;
        top.deny_unknown(&TOP_MEMBERS)?;
        let schema = top.str("schema")?;
        if schema != BASELINE_SCHEMA {
            return Err(top.error(
                "schema",
                &format!("must be {BASELINE_SCHEMA}, not {schema}"),
            ));
        }
        let run_id = non_empty(&top, "run_id")?;
        let agent = non_empty(&top, "agent")?;
        let model = top.opt_str("model")?.map(str::to_owned);
        let mut tasks: Vec<BaselineTask> = Vec::new();
        for (index, item) in top.array("tasks")?.iter().enumerate() {
            let at = format!("tasks[{index}]");
            let reader = Reader::new(item, path, &at)?;
            reader.deny_unknown(&TASK_MEMBERS)?;
            let task = BaselineTask {
                id: non_empty(&reader, "id")?,
                passed: count(&reader, "passed")?,
                trials: count(&reader, "trials")?,
            };
            if task.passed > task.trials {
                return Err(reader.error(
                    "passed",
                    &format!("is {}, more than `trials` ({})", task.passed, task.trials),
                ));
            }
            if tasks.iter().any(|earlier| earlier.id == task.id) {
                return Err(reader.error(
                    "id",
                    &format!("repeats the task `{}` listed before it", task.id),
                ));
            }
            tasks.push(task);
        }
        Ok(Self {
            run_id,
            agent,
            model,
            tasks,
        })
    }
}

fn non_empty(reader: &Reader<'_>, member: &str) -> Result<String, EvalError> {
    let value = reader.str(member)?;
    if value.is_empty() {
        return Err(reader.error(member, "must not be empty"));
    }
    Ok(value.to_owned())
}

fn count(reader: &Reader<'_>, member: &str) -> Result<u32, EvalError> {
    u32::try_from(reader.u64(member)?).map_err(|_| reader.error(member, "is too large"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::model::fixtures::sample_run;

    fn sample() -> Baseline {
        Baseline::from_run(&sample_run())
    }

    #[test]
    fn from_run_counts_each_task_in_run_order() {
        let baseline = sample();
        assert_eq!(baseline.run_id, "20261114T221323Z-a1b2c3");
        assert_eq!(baseline.agent, "claude-code");
        assert_eq!(baseline.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            baseline.tasks,
            vec![
                BaselineTask {
                    id: "empty-input".to_owned(),
                    passed: 1,
                    trials: 1,
                },
                BaselineTask {
                    id: "truncated-header".to_owned(),
                    passed: 0,
                    trials: 1,
                },
            ]
        );
    }

    #[test]
    fn from_run_counts_several_trials_and_tasks_without_any() {
        let mut run = sample_run();
        let mut second = run.results[0].clone();
        second.trial = 2;
        second.outcome = Outcome::Failed;
        run.results.insert(1, second);
        run.results.remove(2);
        let baseline = Baseline::from_run(&run);
        assert_eq!((baseline.tasks[0].passed, baseline.tasks[0].trials), (1, 2));
        assert_eq!((baseline.tasks[1].passed, baseline.tasks[1].trials), (0, 0));
    }

    #[test]
    fn json_round_trips_byte_for_byte() {
        let baseline = sample();
        let text = baseline.to_json();
        assert_eq!(
            text,
            "{\n  \"schema\": \"memprove.eval.baseline.v1\",\n  \"run_id\": \
             \"20261114T221323Z-a1b2c3\",\n  \"agent\": \"claude-code\",\n  \"model\": \
             \"claude-sonnet-5-5\",\n  \"tasks\": [\n    {\n      \"id\": \"empty-input\",\n      \
             \"passed\": 1,\n      \"trials\": 1\n    },\n    {\n      \"id\": \
             \"truncated-header\",\n      \"passed\": 0,\n      \"trials\": 1\n    }\n  ]\n}\n"
        );
        let back = Baseline::from_json(&text, None).expect("reads back");
        assert_eq!(back, baseline);
        assert_eq!(back.to_json(), text);
    }

    #[test]
    fn a_null_model_and_no_tasks_round_trip() {
        let baseline = Baseline {
            run_id: "r".to_owned(),
            agent: "command".to_owned(),
            model: None,
            tasks: Vec::new(),
        };
        let text = baseline.to_json();
        assert!(text.contains("\"model\": null"));
        assert!(text.contains("\"tasks\": []"));
        assert_eq!(Baseline::from_json(&text, None).expect("reads"), baseline);
    }

    fn refused(text: &str) -> String {
        Baseline::from_json(text, Some(Path::new("evals/baseline.json")))
            .expect_err("refused")
            .to_string()
    }

    #[test]
    fn errors_name_the_file_and_member() {
        let good = sample().to_json();
        assert_eq!(
            refused(&good.replace(BASELINE_SCHEMA, "memprove.eval.baseline.v0")),
            "evals/baseline.json: `schema` must be memprove.eval.baseline.v1, not \
             memprove.eval.baseline.v0"
        );
        assert_eq!(
            refused(&good.replace("\"model\"", "\"modle\"")),
            "evals/baseline.json: `modle` is not a known member"
        );
        assert_eq!(
            refused(&good.replace("\"passed\": 0", "\"passed\": 0, \"note\": 1")),
            "evals/baseline.json: `tasks[1].note` is not a known member"
        );
        assert_eq!(
            refused(&good.replace("\"passed\": 0", "\"passed\": 2")),
            "evals/baseline.json: `tasks[1].passed` is 2, more than `trials` (1)"
        );
        assert_eq!(
            refused(&good.replace("\"passed\": 0", "\"passed\": -1")),
            "evals/baseline.json: `tasks[1].passed` must be a non-negative integer"
        );
        assert_eq!(
            refused(&good.replace("truncated-header", "empty-input")),
            "evals/baseline.json: `tasks[1].id` repeats the task `empty-input` listed before it"
        );
        assert_eq!(
            refused(&good.replace("\"agent\": \"claude-code\"", "\"agent\": \"\"")),
            "evals/baseline.json: `agent` must not be empty"
        );
        assert_eq!(
            refused(&good.replace(
                "\"trials\": 1\n    }\n  ]",
                "\"trials\": 4294967296\n    }\n  ]"
            )),
            "evals/baseline.json: `tasks[1].trials` is too large"
        );
        assert!(refused("[]").contains("the document must be an object"));
        assert!(refused("{").starts_with("evals/baseline.json: not valid JSON"));
    }

    #[test]
    fn task_finds_by_id() {
        let baseline = sample();
        assert_eq!(baseline.task("truncated-header").map(|t| t.trials), Some(1));
        assert!(baseline.task("nope").is_none());
    }
}
