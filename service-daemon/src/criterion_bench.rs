#![deny(unsafe_code)]
// Cargo enables cfg(test) but not the libtest harness here. Shared test modules
// consequently retain imports/helpers whose #[test] functions are not emitted.
// Keep these allowances local to this executable, not the production library.
#![allow(dead_code, unused_imports)]
//! Internal benchmark crate: shared production source, no public benchmark API.

// Must be outside include!: a macro-expanded extern crate cannot shadow Cargo's
// --extern service_daemon argument. Both crate roots keep the same self alias.
extern crate self as service_daemon;
include!("crate_root.rs");

#[deny(dead_code, unused_imports)]
mod framework_benches;

fn main() {
    eprintln!(
        "Framework operation benchmarks: cfg(test)={}, high-priority={}, debug_assertions={}",
        cfg!(test),
        cfg!(feature = "high-priority"),
        cfg!(debug_assertions),
    );
    let mut criterion = criterion::Criterion::default().configure_from_args();
    framework_benches::run(&mut criterion);
    criterion.final_summary();
}
