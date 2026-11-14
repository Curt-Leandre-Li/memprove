//! The text `init` writes: the guide in `evals/`, the CI workflow, and the
//! block in `AGENTS.md` / `CLAUDE.md`.

use crate::AgentKind;

/// The line that opens init's block in `AGENTS.md` / `CLAUDE.md`.
pub const BEGIN: &str = "<!-- memprove-eval:begin -->";
/// The line that closes it.
pub const END: &str = "<!-- memprove-eval:end -->";

/// `evals/README.md`: how to write tasks, addressed to a coding agent.
#[must_use]
pub fn readme() -> String {
    README.to_owned()
}

const README: &str = r#"# evals/: tasks for `memprove eval`

For the coding agent that adds or fixes evals. Each `*.json` file here other
than `baseline.json` is one task: a commit to start from, a prompt for a
coding agent, and a check command whose exit code alone decides the result.

## A task file: `evals/<id>.json`

```json
{
  "schema": "memprove.eval.task.v1",
  "id": "empty-input",
  "title": "parser accepts empty input",
  "base": "a1b2c3d",
  "prompt": "Make `parse` accept empty input and return an empty document.",
  "setup": [{"argv": ["npm", "ci"], "timeout_secs": 600}],
  "check": {"argv": ["npm", "test", "--", "parser"], "timeout_secs": 900},
  "check_patch": "evals/empty-input.check.patch",
  "agent_timeout_secs": 1800,
  "tags": ["parser"]
}
```

- `id`: 1 to 64 of `a-z`, `0-9`, `-`, `_`; the file name when left out.
- `base` (HEAD when left out), `title`, `setup`, `check_patch` and `tags`
  may be left out; a member not shown here is an error.
- `argv` runs without a shell; write `["sh", "-c", "..."]` for one.
- `check_patch` (relative to the repository root) is applied after the
  agent, before the check: put the tests that judge the task there, so that
  the agent can neither see nor edit them.

## Which tasks

Real work from this repository, such as a fixed bug or a small feature: a
check that fails before the work and passes after it, less than 15 minutes
for an agent, a narrow check (one package, one test file).
`memprove eval mine --verify` writes such tasks from the history (parent
commit as `base`, message as `prompt`, the commit's test changes as
`check_patch`), keeping those whose check fails on the parent and passes on
the commit. By hand: make the change and its tests on a branch from `base`,
then `git diff <base> HEAD -- <test files> > evals/<id>.check.patch`.

## Check a task file before using it

    memprove eval run evals/<id>.json --agent command -- true

runs the task with an agent that does nothing: `smoke` should come out
`passed`, a task for new work `failed` (if it passes, its check does not test
the work); `patch_failed`, `setup_failed` or `error` mean the file is wrong.

## Run an agent, keep a baseline

    memprove eval run evals/ --agent claude-code     # or --agent codex
    memprove eval report latest
    memprove eval baseline latest --write evals/baseline.json   # commit it
    memprove eval gate latest --baseline evals/baseline.json

Run the gate after changing the model, a prompt, CLAUDE.md, AGENTS.md or
memory. A real agent costs money: start with a few tasks and `--trials 3`.
"#;

/// The repository secret `agent` needs in CI, and the npm package of its
/// CLI.
#[must_use]
pub const fn agent_secret(agent: AgentKind) -> (&'static str, &'static str) {
    match agent {
        AgentKind::Codex => ("OPENAI_API_KEY", "@openai/codex"),
        AgentKind::ClaudeCode | AgentKind::Command => {
            ("ANTHROPIC_API_KEY", "@anthropic-ai/claude-code")
        }
    }
}

/// `.github/workflows/memprove-eval.yml` for `agent` (Claude Code or
/// Codex), pinned to the memprove release of this build.
#[must_use]
pub fn workflow(agent: AgentKind) -> String {
    let (secret, package) = agent_secret(agent);
    let (label, name) = match agent {
        AgentKind::Codex => ("Codex", "codex"),
        AgentKind::ClaudeCode | AgentKind::Command => ("Claude Code", "claude-code"),
    };
    WORKFLOW
        .replace("{version}", concat!("v", env!("CARGO_PKG_VERSION")))
        .replace("{label}", label)
        .replace("{package}", package)
        .replace("{secret}", secret)
        .replace("{agent}", name)
}

const WORKFLOW: &str = r#"# memprove eval: gives the tasks in evals/ to a coding agent and judges each
# by its check command's exit code alone; see evals/README.md. Written by
# `memprove eval init`, which leaves it alone once it is edited: add what the
# checks need (a toolchain, a service, a cache).
#
# A real agent costs money, so this runs by hand (Actions > memprove eval >
# Run workflow) or on a pull request labelled `eval`, not on every push.
name: memprove eval

on:
  workflow_dispatch:
  pull_request:
    types: [opened, synchronize, reopened, labeled]

permissions:
  contents: read

concurrency:
  group: memprove-eval-${{ github.ref }}
  cancel-in-progress: true

env:
  # The memprove release that runs the tasks.
  MEMPROVE_VERSION: {version}

jobs:
  eval:
    if: github.event_name == 'workflow_dispatch' || contains(github.event.pull_request.labels.*.name, 'eval')
    runs-on: ubuntu-latest
    timeout-minutes: 180
    steps:
      - uses: actions/checkout@v4
        with:
          # Tasks start from earlier commits: fetch the whole history.
          fetch-depth: 0

      - name: Install memprove
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          dir="$RUNNER_TEMP/memprove"
          mkdir -p "$dir"
          gh release download "$MEMPROVE_VERSION" --repo memprove/memprove \
            --pattern "memprove-*-x86_64-unknown-linux-gnu.tar.gz*" --dir "$dir"
          (cd "$dir" && sha256sum -c ./*.sha256)
          tar -xzf "$dir"/memprove-*.tar.gz -C "$dir"
          for bin in "$dir"/memprove-*/bin; do echo "$bin" >> "$GITHUB_PATH"; done

      - uses: actions/setup-node@v4
        with:
          node-version: 22

      - name: Install {label}
        run: npm install -g {package}

      - name: Run the tasks
        id: run
        env:
          {secret}: ${{ secrets.{secret} }}
        run: |
          # Exit 1 means some trial did not pass, which the gate judges;
          # 2 and 3 mean the run itself could not be done.
          code=0
          memprove eval run evals/ --agent {agent} --no-memory || code=$?
          test "$code" -le 1

      - name: Report
        if: ${{ !cancelled() && steps.run.outcome != 'skipped' }}
        run: |
          if [ -f evals/baseline.json ]; then
            memprove eval report latest --baseline evals/baseline.json >> "$GITHUB_STEP_SUMMARY"
          else
            memprove eval report latest >> "$GITHUB_STEP_SUMMARY"
          fi

      - name: Gate against evals/baseline.json
        if: ${{ !cancelled() && steps.run.outcome != 'skipped' && hashFiles('evals/baseline.json') != '' }}
        run: memprove eval gate latest --baseline evals/baseline.json

      - name: Keep the run directory
        if: ${{ !cancelled() && steps.run.outcome != 'skipped' }}
        uses: actions/upload-artifact@v4
        with:
          name: memprove-eval-runs
          path: .git/memprove-eval/runs/
          include-hidden-files: true
          if-no-files-found: ignore
"#;

/// The block init keeps in `AGENTS.md` / `CLAUDE.md`, markers included:
/// at most five lines between them.
#[must_use]
pub fn agents_block(agent: AgentKind) -> String {
    let name = match agent {
        AgentKind::Codex => "codex",
        AgentKind::ClaudeCode | AgentKind::Command => "claude-code",
    };
    format!(
        "{BEGIN}\n\
         ## Evals\n\
         `evals/` holds tasks for `memprove eval` (a start commit, a prompt, a check command); `evals/README.md` says how to write one, and `memprove eval mine --verify` makes them from the history.\n\
         Check a new task file with `memprove eval run evals/<id>.json --agent command -- true`.\n\
         After changing the model, a prompt, CLAUDE.md, AGENTS.md or memory, run `memprove eval run evals/ --agent {name}`, then `memprove eval gate latest --baseline evals/baseline.json`.\n\
         {END}\n"
    )
}

/// `current` with `block` in place of its marked block, or with `block`
/// appended (after a blank line) when it has none.
///
/// # Errors
/// One sentence when `current` has a begin marker without an end marker
/// after it.
pub fn splice(current: &str, block: &str) -> Result<String, String> {
    let Some(start) = current.find(BEGIN) else {
        let mut text = current.to_owned();
        if !text.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
        }
        text.push_str(block);
        return Ok(text);
    };
    let Some(offset) = current[start..].find(END) else {
        return Err(format!(
            "has `{BEGIN}` without `{END}` after it; add the end marker or remove the begin marker"
        ));
    };
    let mut end = start + offset + END.len();
    if current[end..].starts_with('\n') {
        end += 1;
    }
    Ok(format!("{}{block}{}", &current[..start], &current[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_has_at_most_five_lines_between_its_markers() {
        for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
            let block = agents_block(agent);
            let lines: Vec<&str> = block.lines().collect();
            assert_eq!(lines.first(), Some(&BEGIN));
            assert_eq!(lines.last(), Some(&END));
            assert!(lines.len() - 2 <= 5, "{block}");
        }
    }

    #[test]
    fn splice_appends_then_replaces_in_place() {
        let block = agents_block(AgentKind::ClaudeCode);
        let first = splice("# Rules\nBe brief.", &block).expect("append");
        assert_eq!(first, format!("# Rules\nBe brief.\n\n{block}"));
        let edited = first.replace("## Evals", "## Old evals") + "\n# After\n";
        let second = splice(&edited, &block).expect("replace");
        assert_eq!(second, format!("# Rules\nBe brief.\n\n{block}\n# After\n"));
        assert_eq!(splice(&second, &block).expect("again"), second);
        assert_eq!(splice("", &block).expect("empty"), block);
        assert!(splice(&format!("x\n{BEGIN}\ny\n"), &block).is_err());
    }

    #[test]
    fn the_readme_is_short_and_the_workflow_is_filled_in() {
        assert!(
            readme().lines().count() <= 60,
            "{}",
            readme().lines().count()
        );
        let codex = workflow(AgentKind::Codex);
        assert!(codex.contains("npm install -g @openai/codex"));
        assert!(codex.contains("OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}"));
        assert!(codex.contains("--agent codex --no-memory"));
        let claude = workflow(AgentKind::ClaudeCode);
        assert!(claude.contains(concat!("MEMPROVE_VERSION: v", env!("CARGO_PKG_VERSION"))));
        for placeholder in ["{version}", "{label}", "{package}", "{secret}", "{agent}"] {
            assert!(!claude.contains(placeholder), "{placeholder}");
        }
    }
}
