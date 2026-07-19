//! Local diagnostics facade for proc-macro errors.
//!
//! This module is intentionally thin while `proc-macro-error2` is still the
//! backend. Macro code should depend on this facade so the backend can be
//! replaced without changing each parser and codegen module again.

macro_rules! abort {
    ($($tokens:tt)*) => {
        proc_macro_error2::abort!($($tokens)*)
    };
}

macro_rules! emit_error {
    ($($tokens:tt)*) => {
        proc_macro_error2::emit_error!($($tokens)*)
    };
}

macro_rules! emit_warning {
    ($($tokens:tt)*) => {
        proc_macro_error2::emit_warning!($($tokens)*)
    };
}

pub(crate) use abort;
pub(crate) use emit_error;
pub(crate) use emit_warning;
