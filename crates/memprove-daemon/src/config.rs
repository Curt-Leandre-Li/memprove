//! Command-line and environment configuration for `memproved`.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
};

/// Environment variable naming the data directory when `--data-dir` is absent.
pub const DATA_DIR_ENV: &str = "MEMPROVE_DATA_DIR";

/// File name of the Local Control API socket inside the data directory.
pub const SOCKET_FILE_NAME: &str = "memproved.sock";

/// File name of the single-instance lock inside the data directory.
pub const LOCK_FILE_NAME: &str = "memproved.lock";

/// Longest socket path accepted, in bytes.
///
/// `sockaddr_un.sun_path` holds 104 bytes on macOS and 108 on Linux, including
/// the terminating NUL; the smaller limit is enforced on every platform.
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

/// Label of the bootstrap control token when `--label` is absent.
pub const DEFAULT_BOOTSTRAP_LABEL: &str = "bootstrap-control";

/// Usage text printed for `--help` and configuration errors.
pub const USAGE: &str = "\
Usage: memproved [--data-dir <PATH>]
       memproved bootstrap [--data-dir <PATH>] [--label <LABEL>] [--output <PATH>]

Runs the memprove daemon in the foreground, serving the Local Control API
over a Unix socket inside the data directory.

`bootstrap` is an offline maintenance command: while the daemon is stopped and
no active control token exists, it issues the first control token and prints
its secret exactly once (to stdout, or to a new 0600 file with --output). Only
the secret's SHA-256 verifier is stored.

Options:
  --data-dir <PATH>  Data directory (default: $MEMPROVE_DATA_DIR)
  --label <LABEL>    Bootstrap token label (default: bootstrap-control)
  --output <PATH>    Write the bootstrap secret to a new 0600 file instead
  -h, --help         Print this help
  -V, --version      Print the version";

/// Validated daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    data_dir: PathBuf,
}

impl DaemonConfig {
    /// Creates a configuration for `data_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the path is empty or the derived socket
    /// path cannot fit in `sockaddr_un`.
    pub fn new(data_dir: PathBuf) -> Result<Self, ConfigError> {
        if data_dir.as_os_str().is_empty() {
            return Err(ConfigError::EmptyDataDir);
        }
        let config = Self { data_dir };
        let socket_len = config.socket_path().as_os_str().len();
        if socket_len > MAX_SOCKET_PATH_BYTES {
            return Err(ConfigError::SocketPathTooLong {
                length: socket_len,
                limit: MAX_SOCKET_PATH_BYTES,
            });
        }
        Ok(config)
    }

    /// Returns the configured data directory.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Returns the Local Control API socket path.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.data_dir.join(SOCKET_FILE_NAME)
    }

    /// Returns the single-instance lock path.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.data_dir.join(LOCK_FILE_NAME)
    }
}

/// Validated `memproved bootstrap` configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapConfig {
    daemon: DaemonConfig,
    label: String,
    output: Option<PathBuf>,
}

impl BootstrapConfig {
    /// Data directory configuration.
    #[must_use]
    pub fn daemon(&self) -> &DaemonConfig {
        &self.daemon
    }

    /// Label for the bootstrap token.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// File to receive the secret instead of stdout.
    #[must_use]
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }
}

/// What the command line asks the process to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon.
    Run(DaemonConfig),
    /// Issue the first control token offline.
    Bootstrap(BootstrapConfig),
    /// Print usage and exit successfully.
    Help,
    /// Print the version and exit successfully.
    Version,
}

/// Invalid command line or environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Neither `--data-dir` nor the environment variable was supplied.
    MissingDataDir,
    /// The data directory path was empty.
    EmptyDataDir,
    /// An option was not followed by a value.
    MissingValue(&'static str),
    /// An option was supplied more than once.
    DuplicateOption(&'static str),
    /// The bootstrap label is empty, longer than 128 characters, or contains
    /// control characters.
    InvalidLabel,
    /// An argument was not recognised.
    UnknownArgument(String),
    /// The socket path does not fit in `sockaddr_un`.
    SocketPathTooLong {
        /// Socket path length in bytes.
        length: usize,
        /// Maximum accepted length in bytes.
        limit: usize,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDataDir => write!(
                formatter,
                "no data directory configured; pass --data-dir <PATH> or set {DATA_DIR_ENV}"
            ),
            Self::EmptyDataDir => formatter.write_str("the data directory path is empty"),
            Self::MissingValue(option) => write!(formatter, "{option} requires a value"),
            Self::DuplicateOption(option) => write!(formatter, "{option} was given more than once"),
            Self::InvalidLabel => formatter
                .write_str("--label must be 1 to 128 characters without control characters"),
            Self::UnknownArgument(argument) => write!(formatter, "unknown argument {argument:?}"),
            Self::SocketPathTooLong { length, limit } => write!(
                formatter,
                "socket path would be {length} bytes, above the {limit}-byte Unix socket limit; \
                 choose a shorter data directory"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parses process arguments (without the program name) and the data-directory
/// environment value. `--data-dir` takes precedence over the environment.
///
/// # Errors
///
/// Returns [`ConfigError`] for unknown, duplicate, or incomplete arguments, or
/// when no usable data directory is configured.
pub fn parse<I>(args: I, env_data_dir: Option<OsString>) -> Result<Command, ConfigError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    let bootstrap = args.peek().is_some_and(|first| first == "bootstrap");
    if bootstrap {
        args.next();
    }
    let mut data_dir: Option<OsString> = None;
    let mut label: Option<OsString> = None;
    let mut output: Option<OsString> = None;
    while let Some(argument) = args.next() {
        if argument == "-h" || argument == "--help" {
            return Ok(Command::Help);
        } else if argument == "-V" || argument == "--version" {
            return Ok(Command::Version);
        }
        let (slot, name, value) = if let Some(value) = option_value(&argument, "--data-dir") {
            (&mut data_dir, "--data-dir", value)
        } else if let Some(value) = option_value(&argument, "--label").filter(|_| bootstrap) {
            (&mut label, "--label", value)
        } else if let Some(value) = option_value(&argument, "--output").filter(|_| bootstrap) {
            (&mut output, "--output", value)
        } else {
            return Err(ConfigError::UnknownArgument(
                argument.to_string_lossy().into_owned(),
            ));
        };
        let value = match value {
            OptionValue::Inline(value) => value,
            OptionValue::Next => args.next().ok_or(ConfigError::MissingValue(name))?,
        };
        if slot.replace(value).is_some() {
            return Err(ConfigError::DuplicateOption(name));
        }
    }
    let data_dir = data_dir
        .or(env_data_dir)
        .ok_or(ConfigError::MissingDataDir)?;
    let daemon = DaemonConfig::new(PathBuf::from(data_dir))?;
    if !bootstrap {
        return Ok(Command::Run(daemon));
    }
    let label = match label {
        None => DEFAULT_BOOTSTRAP_LABEL.to_owned(),
        Some(label) => label
            .into_string()
            .ok()
            .filter(|label| {
                (1..=128).contains(&label.chars().count()) && !label.chars().any(char::is_control)
            })
            .ok_or(ConfigError::InvalidLabel)?,
    };
    Ok(Command::Bootstrap(BootstrapConfig {
        daemon,
        label,
        output: output.map(PathBuf::from),
    }))
}

/// Where an option's value is.
enum OptionValue {
    /// `--name value`: in the next argument.
    Next,
    /// `--name=value`.
    Inline(OsString),
}

/// Matches `--name` or `--name=value`.
fn option_value(argument: &OsStr, name: &str) -> Option<OptionValue> {
    if argument == name {
        return Some(OptionValue::Next);
    }
    strip_option_value(argument, &format!("{name}=")).map(OptionValue::Inline)
}

fn strip_option_value(argument: &OsStr, prefix: &str) -> Option<OsString> {
    argument
        .to_str()
        .and_then(|text| text.strip_prefix(prefix))
        .map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn run_dir(command: Command) -> PathBuf {
        match command {
            Command::Run(config) => config.data_dir().to_path_buf(),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn data_dir_flag_forms_are_accepted() {
        assert_eq!(
            run_dir(parse(args(&["--data-dir", "/tmp/a"]), None).unwrap()),
            PathBuf::from("/tmp/a")
        );
        assert_eq!(
            run_dir(parse(args(&["--data-dir=/tmp/b"]), None).unwrap()),
            PathBuf::from("/tmp/b")
        );
    }

    #[test]
    fn environment_is_the_fallback_and_flag_wins() {
        let env = Some(OsString::from("/tmp/env"));
        assert_eq!(
            run_dir(parse(args(&[]), env.clone()).unwrap()),
            PathBuf::from("/tmp/env")
        );
        assert_eq!(
            run_dir(parse(args(&["--data-dir", "/tmp/flag"]), env).unwrap()),
            PathBuf::from("/tmp/flag")
        );
    }

    #[test]
    fn missing_data_dir_is_reported() {
        assert_eq!(parse(args(&[]), None), Err(ConfigError::MissingDataDir));
        assert_eq!(
            parse(args(&[]), Some(OsString::new())),
            Err(ConfigError::EmptyDataDir)
        );
        assert!(
            ConfigError::MissingDataDir
                .to_string()
                .contains(DATA_DIR_ENV)
        );
    }

    #[test]
    fn malformed_arguments_are_rejected() {
        assert_eq!(
            parse(args(&["--data-dir"]), None),
            Err(ConfigError::MissingValue("--data-dir"))
        );
        assert_eq!(
            parse(args(&["--data-dir", "/a", "--data-dir", "/b"]), None),
            Err(ConfigError::DuplicateOption("--data-dir"))
        );
        assert_eq!(
            parse(args(&["--tcp"]), None),
            Err(ConfigError::UnknownArgument("--tcp".to_owned()))
        );
    }

    fn bootstrap_of(command: Command) -> BootstrapConfig {
        match command {
            Command::Bootstrap(config) => config,
            other => panic!("expected Bootstrap, got {other:?}"),
        }
    }

    #[test]
    fn bootstrap_subcommand_is_parsed() {
        let config =
            bootstrap_of(parse(args(&["bootstrap", "--data-dir", "/tmp/a"]), None).unwrap());
        assert_eq!(config.daemon().data_dir(), Path::new("/tmp/a"));
        assert_eq!(config.label(), DEFAULT_BOOTSTRAP_LABEL);
        assert_eq!(config.output(), None);

        let config = bootstrap_of(
            parse(
                args(&["bootstrap", "--label=ops", "--output", "/tmp/t"]),
                Some(OsString::from("/tmp/env")),
            )
            .unwrap(),
        );
        assert_eq!(config.daemon().data_dir(), Path::new("/tmp/env"));
        assert_eq!(config.label(), "ops");
        assert_eq!(config.output(), Some(Path::new("/tmp/t")));
    }

    #[test]
    fn bootstrap_options_are_validated() {
        assert_eq!(
            parse(
                args(&["bootstrap", "--label", ""]),
                Some(OsString::from("/d"))
            ),
            Err(ConfigError::InvalidLabel)
        );
        assert_eq!(
            parse(
                args(&["bootstrap", "--label", "a\tb"]),
                Some(OsString::from("/d"))
            ),
            Err(ConfigError::InvalidLabel)
        );
        assert_eq!(
            parse(args(&["bootstrap", "--output"]), Some(OsString::from("/d"))),
            Err(ConfigError::MissingValue("--output"))
        );
        assert_eq!(
            parse(args(&["--label", "x"]), Some(OsString::from("/d"))),
            Err(ConfigError::UnknownArgument("--label".to_owned()))
        );
        assert_eq!(
            parse(
                args(&["bootstrap", "--token", "x"]),
                Some(OsString::from("/d"))
            ),
            Err(ConfigError::UnknownArgument("--token".to_owned()))
        );
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse(args(&["--help"]), None), Ok(Command::Help));
        assert_eq!(parse(args(&["-V"]), None), Ok(Command::Version));
    }

    #[test]
    fn derived_paths_live_inside_the_data_dir() {
        let config = DaemonConfig::new(PathBuf::from("/tmp/data")).unwrap();
        assert_eq!(
            config.socket_path(),
            PathBuf::from("/tmp/data/memproved.sock")
        );
        assert_eq!(
            config.lock_path(),
            PathBuf::from("/tmp/data/memproved.lock")
        );
    }

    #[test]
    fn overlong_socket_path_is_rejected() {
        let long = format!("/{}", "d".repeat(MAX_SOCKET_PATH_BYTES));
        assert!(matches!(
            DaemonConfig::new(PathBuf::from(long)),
            Err(ConfigError::SocketPathTooLong { .. })
        ));
        let fits = format!(
            "/{}",
            "d".repeat(MAX_SOCKET_PATH_BYTES - 2 - SOCKET_FILE_NAME.len())
        );
        assert!(DaemonConfig::new(PathBuf::from(fits)).is_ok());
    }
}
