# memprove plugin for coding agents

One skill, `memprove-setup`, so that a single sentence to your coding agent
sets memprove up in the repository you are in:

> Set up memprove in this repository.

The agent then installs memprove when it is missing (the installer at
https://memprove.dev/install.sh, which checks the release's SHA-256), and runs
memprove's own commands: `memprove init` for the memory, `memprove eval init`
for the evals, `memprove policy init` for the tool rules when the installed
release has `memprove policy`, and `memprove doctor`. It reports which files
changed and what is left for you to do. The skill holds no code of its own:
every change comes from memprove's installer and commands, and each is safe
to run again.

The agent does not commit, push, change your shell profile, write Claude
Code's allow rules or run an evaluation with a real agent (which would spend
API credits) unless you ask it to.

## Install

Claude Code:

```
/plugin install memprove --marketplace memprove/memprove
```

or, in two steps, `/plugin marketplace add memprove/memprove` and
`/plugin install memprove@memprove`. From a shell:
`claude plugin marketplace add memprove/memprove`, then
`claude plugin install memprove@memprove`. The skill is
`/memprove:memprove-setup`, and Claude Code also picks it on its own when you
ask for memprove.

Codex:

```bash
codex plugin marketplace add memprove/memprove
codex plugin add memprove@memprove
```

Codex reads the same `.claude-plugin/marketplace.json`; the skill is
`memprove:memprove-setup`.

Cursor reads `.cursor-plugin/marketplace.json`: import
`https://github.com/memprove/memprove` as a plugin repository, or copy
`skills/memprove-setup/` into `~/.cursor/skills/`.

Any other agent that reads `SKILL.md` files (the Agent Skills format) can use
the folder `skills/memprove-setup/` as it is. An agent that can fetch a web
page needs no install at all:

> Set up memprove in this repository by following
> https://raw.githubusercontent.com/memprove/memprove/main/plugins/memprove/skills/memprove-setup/SKILL.md

## Updates

The plugin sets no `version`, so its version is the commit it was installed
from, and `claude plugin update memprove@memprove` (or
`codex plugin marketplace upgrade`) brings the skill up to date. The skill
works with memprove 0.2.0 and later and checks for `memprove policy` before it
uses it.

## Licence

Apache License, Version 2.0, as the rest of the repository.
