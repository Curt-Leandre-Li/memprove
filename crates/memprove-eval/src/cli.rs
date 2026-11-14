//! `memprove eval <command>`: parsing and dispatch, shared by the
//! `memprove` CLI and a future standalone binary.
//!
//! OWNER: workstream W1.

use crate::MemoryProbe;

/// Exit code: done, every trial passed, the gate passed.
pub const EXIT_OK: u8 = 0;
/// Exit code: some trial did not pass, or the gate failed.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: a usage error.
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the command could not be done (a file, `git`, the agent).
pub const EXIT_ERROR: u8 = 3;

/// Runs `memprove eval` with `args` (the words after `eval`) and returns
/// its exit code; `memory` is the embedding program's memory probe.
#[must_use]
pub fn main(args: &[String], memory: Option<&dyn MemoryProbe>) -> u8 {
    let _ = (args, memory);
    eprintln!("memprove eval: not implemented yet");
    EXIT_ERROR
}
