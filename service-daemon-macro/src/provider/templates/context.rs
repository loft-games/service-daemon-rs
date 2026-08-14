//! Shared setup for provider templates.

use quote::{format_ident, quote};

use super::super::impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};

/// Parses `derive(...)` attributes and checks whether `Clone` is present.
/// This handles derive entries with generic arguments, such as
/// `derive(MyMacro<A, B>, Clone)`.
pub(super) fn has_clone_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        // Parse derive arguments as paths so generic derives with commas
        // remain intact (e.g., `MyMacro<A, B>`).
        attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        )
        .is_ok_and(|paths| {
            paths.iter().any(|path| {
                // Match bare `Clone` or qualified `std::clone::Clone` / `core::clone::Clone`
                path.segments.last().is_some_and(|seg| seg.ident == "Clone")
            })
        })
    })
}

/// Shared context for all template-based providers.
///
/// Encapsulates the shared setup code (root manager name generation, Clone derive
/// detection, constructor, and provider capability impls) that every template
/// needs. Individual templates only supply their struct body and convenience
/// methods.
pub(super) struct TemplateContext<'a> {
    pub(super) struct_name: &'a syn::Ident,
    pub(super) vis: &'a syn::Visibility,
    pub(super) attrs: &'a [syn::Attribute],
    pub(super) clone_derive: proc_macro2::TokenStream,
    pub(super) provided_impl: proc_macro2::TokenStream,
}

impl<'a> TemplateContext<'a> {
    /// Creates a new template context with shared setup tokens pre-computed.
    ///
    /// In-memory templates use `Self::default()` as the constructor. They default
    /// to the full provider capability set: snapshot resolution, managed-state
    /// injection, and watch notifications backed by `StateManager` snapshot
    /// publication.
    pub(super) fn new(
        struct_name: &'a syn::Ident,
        vis: &'a syn::Visibility,
        attrs: &'a [syn::Attribute],
        eager: bool,
        helper_style: HelperStyle,
        provider_origin: String,
    ) -> Self {
        let singleton_name = format_ident!(
            "__PROVIDER_SINGLETON_{}",
            struct_name.to_string().to_uppercase()
        );

        let clone_derive = if has_clone_derive(attrs) {
            quote! {}
        } else {
            quote! { #[derive(Clone)] }
        };

        let type_tokens = quote! { #struct_name };
        let framework_init_fn = quote! {
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#struct_name::default()))
        };
        let managed_init_fn = quote! {
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#struct_name::default()))
        };

        let provided_impl = generate_provided_impl(ProvidedImplConfig {
            type_tokens: &type_tokens,
            singleton_name: &singleton_name,
            item_attrs: &[],
            user_span: struct_name.span(),
            param_entries: &[],
            eager,
            cache_scope: quote! { service_daemon::__private::ProviderCacheScope::Inherited },
            framework_init_fn: &framework_init_fn,
            managed_init_fn: &managed_init_fn,
            helper_style,
            provider_origin,
        });

        Self {
            struct_name,
            vis,
            attrs,
            clone_derive,
            provided_impl,
        }
    }
}
