//! U7 P3 (D2 F11, Owner O5; ADR 0014 "Amendment (U7, W-P)"), through the
//! real `memprove`: `hook install --allow-tools` writes three rules into
//! `permissions.allow`, `mcp__memprove`, `Bash(memprove run:*)`, and
//! `Bash(<this executable's canonical absolute path> run:*)`, once each and
//! again unchanged; `mcp print-config` suggests the same three; `doctor`'s
//! `path` line passes when the directory of this executable is on `PATH`
//! and otherwise warns with the `export PATH=…` line that puts it there.
//!
//! Everything runs in a temporary HOME, configuration directory, and
//! repository; no daemon is started.

mod u1e_support;

use std::{fs, path::PathBuf, process::Output};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{cli_binary, isolated, repository, short_tempdir, text};

struct Place {
    _guard: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    repo: PathBuf,
}

impl Place {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let home = root.join("h");
        let config = root.join("cfg");
        let repo = root.join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        repository(&repo);
        Self {
            _guard: guard,
            home,
            config,
            repo,
        }
    }

    /// `memprove <words>` in the repository with `PATH` set to `path`.
    fn run(&self, path: &str, words: &[&str]) -> Output {
        let output = isolated(cli_binary())
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .env("PATH", path)
            .args(words)
            .output()
            .unwrap();
        println!(
            "$ memprove {}\n{}{}[exit {:?}]",
            words.join(" "),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        output
    }

    fn allow(&self) -> Vec<String> {
        let settings = parse_strict(
            &fs::read_to_string(self.repo.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        strings(&settings, &["permissions", "allow"])
    }

    /// The status and the text of `doctor`'s line `check`.
    fn doctor_line(&self, path: &str, check: &str) -> (String, String) {
        let output = self.run(path, &["doctor"]);
        text(&output.stdout)
            .lines()
            .find_map(|line| {
                let (status, rest) = line.split_once(' ')?;
                let (name, detail) = rest.split_once(": ")?;
                (name == check).then(|| (status.to_owned(), detail.trim_start().to_owned()))
            })
            .unwrap_or_else(|| panic!("no {check} line"))
    }
}

fn strings(value: &JsonValue, path: &[&str]) -> Vec<String> {
    path.iter()
        .fold(value, |value, name| value.get(name).unwrap())
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// This executable's canonical absolute path, and its directory.
fn executable() -> (String, String) {
    let path = fs::canonicalize(cli_binary()).unwrap();
    let dir = path.parent().unwrap().to_str().unwrap().to_owned();
    (path.to_str().unwrap().to_owned(), dir)
}

fn rules() -> [String; 3] {
    let (path, _) = executable();
    [
        "mcp__memprove".to_owned(),
        "Bash(memprove run:*)".to_owned(),
        format!("Bash({path} run:*)"),
    ]
}

const SYSTEM_PATH: &str = "/usr/bin:/bin";

#[test]
fn u7_p3_allow_tools_writes_three_rules_once_and_print_config_suggests_them() {
    let place = Place::new();
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    fs::write(
        place.repo.join(".claude/settings.local.json"),
        r#"{"permissions": {"allow": ["Bash(ls:*)", "mcp__memprove"]}}"#,
    )
    .unwrap();

    // Without --allow-tools the three are named, not written.
    let installed = place.run(SYSTEM_PATH, &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    assert_eq!(place.allow(), ["Bash(ls:*)", "mcp__memprove"]);
    let [mcp, bash, absolute] = rules();
    assert!(
        text(&installed.stderr).contains(&format!(
            "the rules an unattended `claude -p` needs are \"{mcp}\", \"{bash}\", and \
             \"{absolute}\""
        )),
        "{}",
        text(&installed.stderr)
    );

    // With it: the ones missing are added after those there, once.
    let allowed = place.run(SYSTEM_PATH, &["hook", "install", "--allow-tools"]);
    assert_eq!(allowed.status.code(), Some(0));
    let mut expected = vec!["Bash(ls:*)".to_owned()];
    expected.extend(rules());
    assert_eq!(place.allow(), expected);
    let bytes = fs::read(place.repo.join(".claude/settings.local.json")).unwrap();
    let again = place.run(SYSTEM_PATH, &["hook", "install", "--allow-tools"]);
    assert!(text(&again.stderr).contains("not written"));
    assert_eq!(
        fs::read(place.repo.join(".claude/settings.local.json")).unwrap(),
        bytes
    );

    // print-config suggests the same three, in that order.
    let printed = text(&place.run(SYSTEM_PATH, &["mcp", "print-config"]).stdout);
    let settings_part = printed
        .split("==> ")
        .find(|part| part.starts_with(".claude/settings.local.json <=="))
        .unwrap();
    let value = parse_strict(settings_part.split_once('\n').unwrap().1.trim()).unwrap();
    assert_eq!(strings(&value, &["permissions", "allow"]), rules());
}

#[test]
fn u7_p3_doctor_reports_whether_the_executable_is_on_path() {
    let place = Place::new();
    let (_, dir) = executable();
    let (status, detail) = place.doctor_line(&format!("{SYSTEM_PATH}:{dir}"), "path");
    assert_eq!(status, "PASS", "{detail}");
    assert!(detail.starts_with(&format!("{dir} is on PATH")), "{detail}");

    let (status, detail) = place.doctor_line(SYSTEM_PATH, "path");
    assert_eq!(status, "WARN", "{detail}");
    assert!(
        detail.ends_with(&format!("export PATH=\"{dir}:$PATH\"")),
        "{detail}"
    );
    assert!(
        detail.contains(&format!("{dir} is not on PATH")),
        "{detail}"
    );
    // Outside every Git worktree the line is there too, before workspace.
    let output = isolated(cli_binary())
        .current_dir(&place.home)
        .env("HOME", &place.home)
        .env("MEMPROVE_CONFIG_DIR", &place.config)
        .env("PATH", SYSTEM_PATH)
        .arg("doctor")
        .output()
        .unwrap();
    let names = text(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')?
                .1
                .split_once(": ")
                .map(|(n, _)| n.to_owned())
        })
        .collect::<Vec<_>>();
    let at = |name: &str| names.iter().position(|seen| seen == name).unwrap();
    assert_eq!(at("path"), at("config-dir") + 1, "{names:?}");
    // U10 N9: and the `guide` line follows it, before `workspace`.
    assert_eq!(at("guide"), at("path") + 1, "{names:?}");
    assert_eq!(at("workspace"), at("guide") + 1, "{names:?}");
}
