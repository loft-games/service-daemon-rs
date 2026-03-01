//! Template generators for special provider types.
//!
//! This module contains generators for:
//! - Notify (Signal) template
//! - Broadcast Queue template
//!
//! Both templates share common initialization logic via [`TemplateContext`].

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::struct_gen::generate_provided_impl;

/// Checks whether any attribute in the list already contains `derive(Clone)`.
///
/// Uses proper AST-based parsing via `syn::punctuated::Punctuated<Path, Token![,]>`
/// to correctly handle complex derive lists (e.g., `derive(MyMacro<A, B>, Clone)`).
fn has_clone_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        // Parse the derive arguments as a comma-separated list of paths.
        // This correctly handles generics with commas (e.g., `MyMacro<A, B>`)
        // unlike the previous string-split approach.
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
/// Encapsulates the common boilerplate (singleton name generation, Clone derive
/// detection, constructor, and `Provided` impl) that every template needs.
/// Individual templates only supply their struct body and convenience methods.
struct TemplateContext<'a> {
    struct_name: &'a syn::Ident,
    vis: &'a syn::Visibility,
    attrs: &'a [syn::Attribute],
    clone_derive: proc_macro2::TokenStream,
    provided_impl: proc_macro2::TokenStream,
}

impl<'a> TemplateContext<'a> {
    /// Creates a new template context with all common boilerplate pre-computed.
    ///
    /// All templates use `Self::default()` as the constructor and mark their
    /// `changed()` as `pending()` since templates are not watchable state.
    fn new(
        struct_name: &'a syn::Ident,
        vis: &'a syn::Visibility,
        attrs: &'a [syn::Attribute],
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

        let constructor = quote! { std::sync::Arc::new(Self::default()) };
        let changed_body = Some(quote! {
            // Templates are not watchable state. Wait indefinitely.
            std::future::pending::<()>().await;
        });

        let type_tokens = quote! { #struct_name };
        let provided_impl = generate_provided_impl(
            &type_tokens,
            &singleton_name,
            &constructor,
            changed_body,
            struct_name.span(),
        );

        Self {
            struct_name,
            vis,
            attrs,
            clone_derive,
            provided_impl,
        }
    }
}

/// Generates a Signal provider using `tokio::sync::Notify`.
pub fn generate_notify_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
) -> TokenStream {
    let ctx = TemplateContext::new(struct_name, vis, attrs);
    let TemplateContext {
        struct_name,
        vis,
        attrs,
        clone_derive,
        provided_impl,
        ..
    } = &ctx;

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name(pub std::sync::Arc<tokio::sync::Notify>);

        impl Default for #struct_name {
            fn default() -> Self {
                Self(std::sync::Arc::new(tokio::sync::Notify::new()))
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = tokio::sync::Notify;
            fn deref(&self) -> &tokio::sync::Notify {
                &self.0
            }
        }

        #provided_impl

        impl #struct_name {
            /// Trigger this signal, waking all subscribed triggers.
            pub fn notify(&self) {
                self.0.notify_waiters();
            }

            /// Wait for a notification on this signal.
            pub async fn wait(&self) {
                self.0.notified().await;
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Broadcast Queue provider using `tokio::sync::broadcast`.
pub fn generate_broadcast_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type: &syn::Type,
    capacity: usize,
) -> TokenStream {
    let ctx = TemplateContext::new(struct_name, vis, attrs);
    let TemplateContext {
        struct_name,
        vis,
        attrs,
        clone_derive,
        provided_impl,
        ..
    } = &ctx;

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            pub tx: tokio::sync::broadcast::Sender<#item_type>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                let (tx, _) = tokio::sync::broadcast::channel(#capacity);
                Self { tx }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = tokio::sync::broadcast::Sender<#item_type>;
            fn deref(&self) -> &tokio::sync::broadcast::Sender<#item_type> {
                &self.tx
            }
        }

        #provided_impl

        impl #struct_name {
            /// Push an item to this queue.
            pub fn push(&self, item: #item_type) -> Result<usize, tokio::sync::broadcast::error::SendError<#item_type>> {
                self.tx.send(item)
            }

            /// Subscribe to this queue to receive broadcast messages.
            pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<#item_type> {
                self.tx.subscribe()
            }
        }
    };

    TokenStream::from(expanded)
}
