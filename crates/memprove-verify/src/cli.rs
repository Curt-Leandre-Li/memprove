//! `memprove verify <command>`: parsing and dispatch.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use memprove_eval::git;

use crate::{
    VerifyError, hook,
    init::{self, InitSpec},
    receipt::Verdict,
    run::{self, Options},
    state_dir,
};

/// Exit code: done; for run, the verdict is `pass`.
pub const EXIT_OK: u8 = 0;
/// Exit code: for run, the verdict is `fail`.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: a usage error.
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the command could not be done (a file, `git`, no contract).
pub const EXIT_ERROR: u8 = 3;
/// Exit code: for run, the verdict is left to a person.
pub const EXIT_NEEDS_HUMAN: u8 = 4;

const EXITS: &str = "Exit status:
  0  done; for run, the verdict is pass
  1  for run, the verdict is fail: a check does not exit 0 on a clean copy of the work
  2  usage error
  3  the command could not be done (a file, git, no contract)
  4  for run, the verdict needs a person: the checks pass only with the work's own
     changes to protected paths, or the work adds a skip marker
";

/// `memprove verify --help`.
pub const USAGE: &str = "memprove verify: check that work is done by running the repository's own checks on a clean copy of it

Usage:
  memprove verify init [--hook] [--dry-run]
  memprove verify run  [--base <REV>] [--commit <REV>] [--contract <FILE>] [--out-root <DIR>]
                       [--format text|json] [--keep-worktrees]
  memprove verify show [<RUN_DIR> | latest] [--format text|json]
  memprove verify hook

The contract (.memprove/verify.json, schema memprove.verify.contract.v1) names the
checks, commands that must exit 0, an optional setup, and the protected paths: what
the work should not need to change to pass, its tests. `run` writes the files being
verified (the working tree as it stands, without what Git ignores) to a new directory
that holds nothing else and runs the checks there; exit codes alone judge. When the
work changes a protected path, the checks run again with every protected path as at
the base commit; work that passes only with its own changes to them is left to a
person, as is work that adds a skip marker. The contract is read from the base commit,
so the work being verified cannot weaken it. Every run writes a receipt under
<git common dir>/memprove-verify/runs/<run id>/.

`memprove verify <command> --help` describes one command.

";

struct Help {
    name: &'static str,
    summary: &'static str,
    usage: &'static str,
    options: &'static [(&'static str, &'static str)],
    notes: &'static str,
}

const HELP: &[Help] = &[
    Help {
        name: "init",
        summary: "write the contract; with --hook, add the Claude Code hooks",
        usage: "memprove verify init [--hook] [--dry-run]",
        options: &[
            (
                "--hook",
                "add the SessionStart and Stop handlers to .claude/settings.local.json",
            ),
            (
                "--dry-run",
                "list the files that would be written; write nothing",
            ),
        ],
        notes: "Writes .memprove/verify.json with the test command and the protected paths guessed\n\
                from the repository's files (Cargo.toml, package.json with a test script,\n\
                pyproject.toml / pytest.ini / setup.cfg, go.mod, a Makefile with a test target). A\n\
                contract that exists is left as it is. With --hook, the handlers run `memprove\n\
                verify hook`; handlers of it already there are replaced, everything else in the\n\
                settings file stays. Running it again changes nothing.",
    },
    Help {
        name: "run",
        summary: "verify the work and write a receipt",
        usage: "memprove verify run [--base <REV>] [--commit <REV>] [--contract <FILE>] [--out-root <DIR>]\n\
                \x20                   [--format text|json] [--keep-worktrees]",
        options: &[
            (
                "--base <REV>",
                "the commit the work is compared with; default the merge base with the branch's upstream, else HEAD",
            ),
            (
                "--commit <REV>",
                "verify this commit's files, not the working tree; its base defaults to its first parent when the branch has no upstream",
            ),
            (
                "--contract <FILE>",
                "read the contract from this file, not from the base commit",
            ),
            (
                "--out-root <DIR>",
                "where run directories go; default <git common dir>/memprove-verify/runs",
            ),
            (
                "--format <FORMAT>",
                "text (default): the summary and a `test result:` line; json: the receipt",
            ),
            (
                "--keep-worktrees",
                "keep the clean copies the checks ran in, under the system's temporary directory",
            ),
        ],
        notes: "The contract is the base commit's .memprove/verify.json; when the base has none,\n\
                the work's own. Setup commands and checks run without a shell in the clean copy,\n\
                with MEMPROVE_VERIFY_RUN, MEMPROVE_VERIFY_STAGE (candidate or restored) and\n\
                MEMPROVE_VERIFY_CACHE (a directory that outlives the run, for what is slow to\n\
                rebuild) set. `memprove run --test-format cargo -- memprove verify run` records\n\
                the verification as test Evidence.",
    },
    Help {
        name: "show",
        summary: "print a run's summary or receipt",
        usage: "memprove verify show [<RUN_DIR> | latest] [--format text|json]",
        options: &[
            (
                "<RUN_DIR> | latest",
                "the run directory; default latest, the newest under <git common dir>/memprove-verify/runs",
            ),
            (
                "--format <FORMAT>",
                "text (default): the summary; json: the receipt",
            ),
        ],
        notes: "Exits 0 whatever the run's verdict.",
    },
    Help {
        name: "hook",
        summary: "the Claude Code SessionStart and Stop hook",
        usage: "memprove verify hook",
        options: &[],
        notes: "Reads the hook's JSON on standard input as untrusted input. At SessionStart it\n\
                records the commit the session starts at, the base of the session's\n\
                verifications. At Stop it verifies the working tree: fail keeps the agent working\n\
                and tells it what failed, at most 3 times in a session; a verdict that needs a\n\
                person, and an error of the verification itself, are shown to the person and do\n\
                not hold the agent. A tree already verified is not verified again. Without a\n\
                contract, or when the tree is the base's, it prints nothing. Always exits 0.",
    },
];

fn help_of(entry: &Help) -> String {
    let mut text = format!(
        "memprove verify {}: {}\n\nUsage:\n  {}\n",
        entry.name, entry.summary, entry.usage
    );
    if !entry.options.is_empty() {
        text.push_str("\nOptions:\n");
        for (option, meaning) in entry.options {
            writeln!(text, "  {option}\n      {meaning}").expect("writing to a String cannot fail");
        }
    }
    write!(text, "\n{}\n\n{EXITS}", entry.notes).expect("writing to a String cannot fail");
    text
}

/// Runs `memprove verify <words>` and returns its exit code.
#[must_use]
pub fn main(words: &[String]) -> u8 {
    let Some((command, rest)) = words.split_first() else {
        eprint!("{USAGE}{EXITS}");
        return EXIT_USAGE;
    };
    if command == "--help" || command == "-h" || command == "help" {
        print!("{USAGE}{EXITS}");
        return EXIT_OK;
    }
    let Some(entry) = HELP.iter().find(|entry| entry.name == command) else {
        eprintln!(
            "memprove verify: error: unknown command {command:?}\nRun `memprove verify --help` for the commands."
        );
        return EXIT_USAGE;
    };
    if rest.iter().any(|word| word == "--help" || word == "-h") {
        print!("{}", help_of(entry));
        return EXIT_OK;
    }
    if entry.name == "hook" {
        // The hook never fails the client: whatever happens, exit 0.
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(output) = hook::hook(&mut std::io::stdin().lock(), &cwd) {
            print!("{output}");
        }
        return EXIT_OK;
    }
    match Args::parse(rest).and_then(|args| dispatch(entry.name, &args)) {
        Ok(code) => code,
        Err(VerifyError::Usage(detail)) => {
            eprintln!(
                "memprove verify {command}: error: {detail}\nRun `memprove verify {command} --help` for its options."
            );
            EXIT_USAGE
        }
        Err(error) => {
            eprintln!("memprove verify {command}: error: {error}");
            EXIT_ERROR
        }
    }
}

/// The words of one command: positionals, options with values, flags.
#[derive(Debug, Default)]
struct Args {
    positionals: Vec<String>,
    options: Vec<(String, String)>,
    flags: Vec<String>,
}

const FLAGS: &[&str] = &["--hook", "--dry-run", "--keep-worktrees"];

impl Args {
    fn parse(words: &[String]) -> Result<Self, VerifyError> {
        let mut args = Self::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if FLAGS.contains(&word.as_str()) {
                args.flags.push(word.clone());
            } else if let Some(name) = word.strip_prefix("--") {
                let (name, value) = match name.split_once('=') {
                    Some((name, value)) => (name.to_owned(), value.to_owned()),
                    None => (
                        name.to_owned(),
                        words
                            .next()
                            .cloned()
                            .ok_or_else(|| VerifyError::Usage(format!("--{name} needs a value")))?,
                    ),
                };
                args.options.push((format!("--{name}"), value));
            } else {
                args.positionals.push(word.clone());
            }
        }
        Ok(args)
    }

    fn allow(
        &self,
        options: &[&str],
        flags: &[&str],
        positionals: usize,
    ) -> Result<(), VerifyError> {
        if let Some(name) = self
            .options
            .iter()
            .map(|(name, _)| name)
            .chain(&self.flags)
            .find(|name| !options.contains(&name.as_str()) && !flags.contains(&name.as_str()))
        {
            return Err(VerifyError::Usage(format!("unknown option {name}")));
        }
        if let Some(extra) = self.positionals.get(positionals) {
            return Err(VerifyError::Usage(format!("unexpected argument {extra:?}")));
        }
        Ok(())
    }

    fn one(&self, name: &str) -> Result<Option<&str>, VerifyError> {
        let mut values = self.options.iter().filter(|(option, _)| option == name);
        let first = values.next().map(|(_, value)| value.as_str());
        if values.next().is_some() {
            return Err(VerifyError::Usage(format!("{name} is given twice")));
        }
        Ok(first)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// `--format`: whether it is `json`.
    fn json(&self) -> Result<bool, VerifyError> {
        match self.one("--format")? {
            None | Some("text") => Ok(false),
            Some("json") => Ok(true),
            Some(other) => Err(VerifyError::Usage(format!(
                "--format must be text or json, not {other:?}"
            ))),
        }
    }
}

fn dispatch(name: &str, args: &Args) -> Result<u8, VerifyError> {
    let cwd = std::env::current_dir().map_err(|error| VerifyError::io(Path::new("."), error))?;
    match name {
        "init" => init_command(args, &cwd),
        "run" => run_command(args, &cwd),
        "show" => show_command(args, &cwd),
        _ => unreachable!("the help table names every command"),
    }
}

fn init_command(args: &Args, cwd: &Path) -> Result<u8, VerifyError> {
    args.allow(&[], &["--hook", "--dry-run"], 0)?;
    let repo_root = git::toplevel(cwd)?;
    let executable = std::env::current_exe()
        .map_err(|error| VerifyError::io(Path::new("memprove"), error))?
        .to_string_lossy()
        .into_owned();
    let spec = InitSpec {
        repo_root,
        hook: args.flag("--hook"),
        executable,
        dry_run: args.flag("--dry-run"),
    };
    let report = init::init(&spec)?;
    let verb = if spec.dry_run { "would write" } else { "wrote" };
    for path in &report.written {
        println!("{verb} {}", path.display());
    }
    for path in &report.unchanged {
        println!("unchanged {}", path.display());
    }
    for path in &report.kept {
        println!("kept {} (it is yours to edit)", path.display());
    }
    for note in &report.notes {
        println!("note: {note}");
    }
    Ok(EXIT_OK)
}

fn run_command(args: &Args, cwd: &Path) -> Result<u8, VerifyError> {
    args.allow(
        &["--base", "--commit", "--contract", "--out-root", "--format"],
        &["--keep-worktrees"],
        0,
    )?;
    let json = args.json()?;
    let mut options = Options::new(&git::toplevel(cwd)?)?;
    options.base = args.one("--base")?.map(str::to_owned);
    options.commit = args.one("--commit")?.map(str::to_owned);
    options.contract_file = args.one("--contract")?.map(PathBuf::from);
    if let Some(out_root) = args.one("--out-root")? {
        options.out_root = PathBuf::from(out_root);
    }
    options.keep_worktrees = args.flag("--keep-worktrees");
    let (receipt, run_dir) = run::verify(&options)?;
    if json {
        print!("{}", receipt.to_json());
    } else {
        println!("{}{}", receipt.summary(&run_dir), receipt.libtest_summary());
    }
    Ok(match receipt.verdict {
        Verdict::Pass => EXIT_OK,
        Verdict::Fail => EXIT_FAILED,
        Verdict::NeedsHuman => EXIT_NEEDS_HUMAN,
    })
}

fn show_command(args: &Args, cwd: &Path) -> Result<u8, VerifyError> {
    args.allow(&["--format"], &[], 1)?;
    let json = args.json()?;
    let out_root = state_dir(&git::toplevel(cwd)?)?.join("runs");
    let run_dir = run::resolve_run_dir(&out_root, args.positionals.first().map(String::as_str))?;
    let file = run_dir.join(if json {
        run::RECEIPT_FILE
    } else {
        run::SUMMARY_FILE
    });
    let text = fs::read_to_string(&file).map_err(|error| VerifyError::io(&file, error))?;
    print!("{text}");
    Ok(EXIT_OK)
}
