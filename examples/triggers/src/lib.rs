//! Re-exports all modules for use by integration tests.
//!
//! This crate is structured as a library + binary pair so that
//! integration tests in `tests/` can import providers and handlers.

pub mod providers;
pub mod services;
pub mod trigger_handlers;
