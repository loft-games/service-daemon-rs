//! Common utilities shared across macros.

use syn::Attribute;

/// Helper to check if `#[allow_sync]` is present on the function's attributes.
pub fn has_allow_sync(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .map_or(false, |seg| seg.ident == "allow_sync")
    })
}
