//! U1 (E1): compiles the build identity in as `MEMPROVE_BUILD_ID` (see
//! `crates/shared/build_id.rs`).

#![forbid(unsafe_code)]

include!("../shared/build_id.rs");

fn main() {
    emit_build_id();
}
