//! `memproved` process entrypoint.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use memprove_daemon::config::{self, Command, DATA_DIR_ENV, USAGE};

fn main() -> ExitCode {
    let command = config::parse(std::env::args_os().skip(1), std::env::var_os(DATA_DIR_ENV));
    match command {
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("{}", memprove_daemon::VERSION_LINE);
            ExitCode::SUCCESS
        }
        Ok(Command::Bootstrap(config)) => {
            match memprove_daemon::bootstrap::run(&config, &mut std::io::stdout().lock()) {
                Ok(report) => {
                    let destination = config
                        .output()
                        .map_or_else(|| "stdout".to_owned(), |path| path.display().to_string());
                    if report.discarded_pending > 0 {
                        eprintln!(
                            "memproved: discarded {} pending token(s) from an interrupted \
                             bootstrap",
                            report.discarded_pending
                        );
                    }
                    eprintln!(
                        "memproved: issued control token {} (label {:?}); its secret was \
                         written once to {destination} and is not stored",
                        report.token_id, report.client_label
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("memproved: error: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Command::Run(config)) => match memprove_daemon::run(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("memproved: error: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("memproved: error: {error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
