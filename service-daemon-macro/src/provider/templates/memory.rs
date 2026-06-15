//! In-memory provider templates.

use proc_macro::TokenStream;
use quote::quote;

use super::super::impls::HelperStyle;
use super::context::TemplateContext;

/// Generates a Signal provider using `tokio::sync::Notify`.
pub(in crate::provider) fn generate_notify_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    eager: bool,
) -> TokenStream {
    let ctx = TemplateContext::new(
        struct_name,
        vis,
        attrs,
        eager,
        HelperStyle::Infallible,
        format!("#[provider(Notify)] struct {struct_name}"),
    );
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
        #vis struct #struct_name(pub std::sync::Arc<::service_daemon::TrackedNotify>);

        impl Default for #struct_name {
            fn default() -> Self {
                Self(std::sync::Arc::new(::service_daemon::TrackedNotify::new()))
            }
        }

        impl ::std::ops::Deref for #struct_name {
            type Target = ::service_daemon::TrackedNotify;
            fn deref(&self) -> &::service_daemon::TrackedNotify {
                &*self.0
            }
        }

        #provided_impl

        impl #struct_name {
            /// Trigger this signal and wake subscribed triggers.
            /// The tracked signal records a UUID v7 message ID for causal tracing.
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
pub(in crate::provider) fn generate_broadcast_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type: &syn::Type,
    capacity: std::num::NonZeroUsize,
    eager: bool,
) -> TokenStream {
    let capacity = capacity.get();
    let ctx = TemplateContext::new(
        struct_name,
        vis,
        attrs,
        eager,
        HelperStyle::Infallible,
        format!("#[provider(Queue)] struct {struct_name}"),
    );
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
            pub tx: service_daemon::TrackedSender<#item_type>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                const CAPACITY: std::num::NonZeroUsize = match std::num::NonZeroUsize::new(#capacity) {
                    Some(capacity) => capacity,
                    None => std::num::NonZeroUsize::MIN,
                };

                Self {
                    tx: service_daemon::TrackedSender::new(CAPACITY),
                }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = service_daemon::TrackedSender<#item_type>;
            fn deref(&self) -> &service_daemon::TrackedSender<#item_type> {
                &self.tx
            }
        }

        #provided_impl

        impl #struct_name {
            /// Push an item to this queue.
            /// The tracked sender records a UUID v7 message ID for causal tracing.
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
