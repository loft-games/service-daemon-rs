//! Local diagnostics facade for proc-macro errors.
//!
//! Macro code should depend on this facade so diagnostic behavior stays
//! repository-owned instead of depending on a third-party proc-macro diagnostics
//! shim directly.

use quote::ToTokens;

pub(crate) fn compile_error_at<T>(span: T, message: impl Into<String>) -> proc_macro2::TokenStream
where
    T: ToTokens,
{
    syn::Error::new_spanned(span, message.into()).to_compile_error()
}

macro_rules! emit_unused_provider_template_arg_warning {
    ($span:expr, $template:expr, $arg:literal) => {{
        // Rust does not expose stable proc-macro warnings. This facade keeps
        // warning call sites classified while preserving the previous stable
        // behavior, where warnings are ignored.
        let _ = &$span;
        let _ = &$template;
        let _ = $arg;
    }};
}

pub(crate) use emit_unused_provider_template_arg_warning;
