//! Template generators for special provider types.
//!
//! This module contains generators for:
//! - Notify (Signal) template
//! - Broadcast Queue template
//! - Load-Balancing Queue template

use proc_macro::TokenStream;
use quote::{format_ident, quote};

/// Generates a Signal provider using `tokio::sync::Notify`.
pub fn generate_notify_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());

    let expanded = quote! {
        #(#attrs)*
        #[derive(Clone)]
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

        static #singleton_name: service_daemon::core::managed_state::StateManager<#struct_name> = service_daemon::core::managed_state::StateManager::new();

        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                #singleton_name.resolve_mutex(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn changed() {
                // Signals are not watchable state. Wait indefinitely.
                std::future::pending::<()>().await;
            }
        }

        impl #struct_name {
            /// Trigger this signal from anywhere in the application.
            pub async fn notify() {
                <Self as service_daemon::Provided>::resolve().await.notify_one();
            }

            /// Wait for a notification on this signal.
            pub async fn wait() {
                <Self as service_daemon::Provided>::resolve().await.notified().await;
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Load-Balancing Queue provider using `tokio::sync::mpsc`.
pub fn generate_lb_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type_str: &str,
    capacity: usize,
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());
    let item_type: proc_macro2::TokenStream =
        item_type_str.parse().unwrap_or_else(|_| quote!(String));

    let expanded = quote! {
        #(#attrs)*
        #[derive(Clone)]
        #vis struct #struct_name {
            pub tx: tokio::sync::mpsc::Sender<#item_type>,
            pub rx: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<#item_type>>>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                let (tx, rx) = tokio::sync::mpsc::channel(#capacity);
                Self {
                    tx,
                    rx: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
                }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<#item_type>>>;
            fn deref(&self) -> &Self::Target {
                &self.rx
            }
        }

        static #singleton_name: service_daemon::core::managed_state::StateManager<#struct_name> = service_daemon::core::managed_state::StateManager::new();

        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                #singleton_name.resolve_mutex(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn changed() {
                // Queues are not watchable state. Wait indefinitely.
                std::future::pending::<()>().await;
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            pub async fn push(item: #item_type) -> Result<(), tokio::sync::mpsc::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().await.tx.send(item).await
            }
        }

        impl service_daemon::core::triggers::LBQueueTarget for #struct_name {
            type Item = #item_type;

            fn receiver(&self) -> &std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<#item_type>>> {
                &self.rx
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
    item_type_str: &str,
    capacity: usize,
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());
    let item_type: proc_macro2::TokenStream =
        item_type_str.parse().unwrap_or_else(|_| quote!(String));

    let expanded = quote! {
        #(#attrs)*
        #[derive(Clone)]
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

        static #singleton_name: service_daemon::core::managed_state::StateManager<#struct_name> = service_daemon::core::managed_state::StateManager::new();

        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                #singleton_name.resolve_mutex(|| async { std::sync::Arc::new(Self::default()) }).await
            }

            async fn changed() {
                // Queues are not watchable state. Wait indefinitely.
                std::future::pending::<()>().await;
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            pub async fn push(item: #item_type) -> Result<usize, tokio::sync::broadcast::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().await.tx.send(item)
            }

            /// Subscribe to this queue to receive broadcast messages.
            pub async fn subscribe() -> tokio::sync::broadcast::Receiver<#item_type> {
                <Self as service_daemon::Provided>::resolve().await.tx.subscribe()
            }
        }
    };

    TokenStream::from(expanded)
}
