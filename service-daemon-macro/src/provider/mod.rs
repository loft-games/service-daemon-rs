//! `#[provider]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `parser`: Attribute parsing and configuration.
//! - `templates`: Template generators for Notify, Queue.
//! - `struct_gen`: Struct provider generation with field injection.

mod impls;
mod parser;
mod struct_gen;
mod templates;

use proc_macro::TokenStream;
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Item, ItemFn, ItemStruct, Token, parse_macro_input};

use crate::common::{WrapperKind, decompose_type, extract_sync_handler_flag};
use impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
pub use parser::ProviderArgs;
use struct_gen::generate_struct_provider;

struct ProviderImplArgs {
    priority: u8,
}

#[derive(Default)]
struct ProviderContractArgs {
    eager: bool,
}

impl Parse for ProviderContractArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self::default());
        }

        let mut args = Self::default();
        let mut eager_seen = false;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "eager" => {
                    if eager_seen {
                        return Err(syn::Error::new(
                            key.span(),
                            "duplicate provider_contract attribute `eager`",
                        ));
                    }
                    eager_seen = true;
                    let value: syn::LitBool = input.parse()?;
                    args.eager = value.value;
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "Unknown provider_contract attribute. Supported: eager",
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        Ok(args)
    }
}

impl Default for ProviderImplArgs {
    fn default() -> Self {
        Self { priority: 50 }
    }
}

impl Parse for ProviderImplArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self::default());
        }

        let mut args = Self::default();
        let mut priority_seen = false;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "priority" => {
                    if priority_seen {
                        return Err(syn::Error::new(
                            key.span(),
                            "duplicate provider_impl attribute `priority`",
                        ));
                    }
                    priority_seen = true;
                    let lit: syn::LitInt = input.parse()?;
                    args.priority = lit.base10_parse::<u8>().map_err(|_| {
                        syn::Error::new(lit.span(), "provider_impl priority must be in 0..=255")
                    })?;
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "Unknown provider_impl attribute. Supported: priority",
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        Ok(args)
    }
}

struct FallibleProviderReturn {
    ok_ty: syn::Type,
    err_ty: syn::Type,
}

fn extract_result_provider_return(ty: &syn::Type) -> Option<FallibleProviderReturn> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };

    let last = tp.path.segments.last()?;
    if last.ident != "Result" {
        return None;
    }

    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };

    let mut iter = args.args.iter();
    let ok = iter.next()?;
    let err = iter.next()?;

    let ok_ty = match ok {
        syn::GenericArgument::Type(t) => t.clone(),
        _ => return None,
    };
    let err_ty = match err {
        syn::GenericArgument::Type(t) => t.clone(),
        _ => return None,
    };

    Some(FallibleProviderReturn { ok_ty, err_ty })
}

fn is_provider_error_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };

    tp.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "ProviderError")
}

fn provider_error_type_assertion(err_ty: &syn::Type) -> proc_macro2::TokenStream {
    let span = err_ty.span();
    quote_spanned! { span =>
        const _: () = {
            fn __service_daemon_assert_provider_error_type(value: #err_ty) {
                let _: service_daemon::ProviderError = value;
            }
        };
    }
}

fn provider_help_error(
    span: impl ToTokens,
    message: &'static str,
    help: &'static str,
) -> syn::Error {
    syn::Error::new_spanned(span, format!("{message}\n\n  = help: {help}\n"))
}

fn provider_help_note_error(
    span: impl ToTokens,
    message: &'static str,
    help: &'static str,
    note: &'static str,
) -> syn::Error {
    syn::Error::new_spanned(
        span,
        format!("{message}\n\n  = help: {help}\n  = note: {note}\n"),
    )
}

fn block_uses_service_handle_macro(block: &syn::Block) -> bool {
    token_stream_uses_service_handle_macro(block.to_token_stream())
}

fn token_stream_uses_service_handle_macro(tokens: proc_macro2::TokenStream) -> bool {
    let mut service_handle_ident_seen = false;

    for token in tokens {
        match token {
            proc_macro2::TokenTree::Ident(ident) => {
                service_handle_ident_seen = ident == "service_handle";
            }
            proc_macro2::TokenTree::Punct(punct)
                if service_handle_ident_seen && punct.as_char() == '!' =>
            {
                return true;
            }
            proc_macro2::TokenTree::Group(group) => {
                if token_stream_uses_service_handle_macro(group.stream()) {
                    return true;
                }
                service_handle_ident_seen = false;
            }
            _ => {
                service_handle_ident_seen = false;
            }
        }
    }

    false
}

pub fn provider_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed_item = parse_macro_input!(item as Item);
    let args = parse_macro_input!(attr as ProviderArgs);

    let expanded = match parsed_item {
        Item::Struct(item_struct) => generate_struct_provider(item_struct, args),
        Item::Fn(item_fn) => generate_async_fn_provider(item_fn, args.named.eager),
        other => Err(provider_help_note_error(
            other,
            "#[provider] can only be applied to struct or function items",
            "Use #[provider] on a struct definition or a function returning the provider type",
            "Example: #[provider(8080)] pub struct Port(pub i32);",
        )),
    };

    match expanded {
        Ok(tokens) => tokens,
        Err(err) => TokenStream::from(err.to_compile_error()),
    }
}

pub fn provider_contract_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ProviderContractArgs);
    let parsed_item = parse_macro_input!(item as Item);
    let expanded = match parsed_item {
        Item::Struct(item_struct) => generate_provider_contract(item_struct, args),
        other => Err(provider_help_note_error(
            other,
            "#[provider_contract] can only be applied to struct items",
            "Mark the shared output struct with #[provider_contract], then register local functions with #[provider_impl]",
            "Example: #[provider_contract] pub struct SharedSettings { ... }",
        )),
    };

    match expanded {
        Ok(tokens) => tokens,
        Err(err) => TokenStream::from(err.to_compile_error()),
    }
}

pub fn provider_candidate_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ProviderImplArgs);
    let parsed_item = parse_macro_input!(item as Item);
    let expanded = match parsed_item {
        Item::Fn(item_fn) => generate_provider_impl_candidate(item_fn, args),
        other => Err(provider_help_note_error(
            other,
            "#[provider_impl] can only be applied to function items",
            "Use #[provider_impl] on a function returning a #[provider_contract] type",
            "Example: #[provider_impl(priority = 80)] async fn primary_settings() -> Result<SharedSettings, ProviderError> { ... }",
        )),
    };

    match expanded {
        Ok(tokens) => tokens,
        Err(err) => TokenStream::from(err.to_compile_error()),
    }
}

fn generate_provider_contract(
    item_struct: ItemStruct,
    args: ProviderContractArgs,
) -> syn::Result<TokenStream> {
    let struct_name = &item_struct.ident;
    if !item_struct.generics.params.is_empty() {
        return Err(provider_help_error(
            &item_struct.generics,
            "#[provider_contract] does not support generic structs in this version",
            "Define a concrete shared contract type, then put generic behavior behind fields or trait objects",
        ));
    }

    let struct_def = quote! { #item_struct };
    let singleton_name = format_ident!(
        "__PROVIDER_CONTRACT_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };
    let type_name_str = quote!(#struct_name).to_string().replace(' ', "");
    let framework_init_fn = quote! {
        service_daemon::__private::resolve_provider_contract::<#type_tokens>(
            #type_name_str,
            policy,
            cancel,
        )
        .await
    };
    let managed_init_fn = quote! {
        service_daemon::__private::resolve_provider_contract_managed::<#type_tokens>(
            #type_name_str,
            policy,
            cancel,
        )
        .await
    };
    let provider_origin = format!("#[provider_contract] struct {struct_name}");
    let cache_scope = quote! {
        service_daemon::__private::provider_contract_cache_scope(
            std::any::TypeId::of::<#type_tokens>(),
        )
    };
    let provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &[],
        user_span: struct_name.span(),
        param_entries: &[],
        eager: args.eager,
        cache_scope,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin,
    });

    Ok(TokenStream::from(quote! {
        #struct_def

        impl service_daemon::ProviderContract for #struct_name {}

        #provided_impl
    }))
}

fn generate_provider_impl_candidate(
    item_fn: ItemFn,
    args: ProviderImplArgs,
) -> syn::Result<TokenStream> {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_sig = &item_fn.sig;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;
    let fn_inputs = &item_fn.sig.inputs;

    if let syn::Safety::Unsafe(unsafety) = &item_fn.sig.safety {
        return Err(provider_help_error(
            unsafety,
            "#[provider_impl] fn cannot be unsafe",
            "Move unsafe operations behind a safe provider implementation boundary",
        ));
    }

    let return_type = match &item_fn.sig.output {
        syn::ReturnType::Type(_, ty) => ty.clone(),
        syn::ReturnType::Default => {
            return Err(provider_help_error(
                &item_fn.sig,
                "#[provider_impl] fn must have a return type",
                "Add a return type, e.g., `async fn settings() -> SharedSettings { ... }`",
            ));
        }
    };

    let result_return = extract_result_provider_return(&return_type);
    if let Some(result_return) = &result_return
        && !is_provider_error_type(&result_return.err_ty)
    {
        return Err(provider_help_error(
            &result_return.err_ty,
            "#[provider_impl] function Result error type must be service_daemon::ProviderError",
            "Use Result<T, service_daemon::ProviderError> so Unavailable, Retryable, and Fatal have framework semantics",
        ));
    }

    let (provided_type, is_fallible) = match &result_return {
        Some(fallible) => (fallible.ok_ty.clone(), true),
        None => ((*return_type).clone(), false),
    };
    let provider_error_assertion = result_return
        .as_ref()
        .map(|fallible| provider_error_type_assertion(&fallible.err_ty));

    let contract_assertion = quote_spanned! { provided_type.span() =>
        const _: () = {
            fn __service_daemon_assert_provider_contract<T: service_daemon::ProviderContract>() {}
            fn __check() { __service_daemon_assert_provider_contract::<#provided_type>(); }
        };
    };

    let fn_name_str = fn_name.to_string();
    let return_type_str = quote!(#provided_type).to_string().replace(' ', "");
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&item_fn.attrs);

    let mut resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();

    for arg in fn_inputs {
        if let syn::FnArg::Receiver(_) = arg {
            return Err(provider_help_error(
                arg,
                "#[provider_impl] fn must be a free function, not a method",
                "Remove the `self` parameter",
            ));
        }

        if let syn::FnArg::Typed(syn::PatType { attrs, pat, ty, .. }) = arg
            && let syn::Pat::Ident(pat_ident) = &**pat
        {
            if attrs.iter().any(|attr| attr.path().is_ident("input")) {
                return Err(provider_help_error(
                    arg,
                    "#[input] is only supported by #[service]",
                    "Provider implementation parameters must be Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>> dependencies",
                ));
            }
            let arg_name = &pat_ident.ident;
            let (inner_type, wrapper) = decompose_type(ty);
            let dependency_kind = match wrapper {
                Some(WrapperKind::ArcRwLock(_, _)) => quote! { RwLock },
                Some(WrapperKind::ArcMutex(_, _)) => quote! { Mutex },
                Some(WrapperKind::Arc(_)) => quote! { Snapshot },
                None => quote! { Snapshot },
            };

            match wrapper {
                Some(WrapperKind::ArcRwLock(_, _)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_rwlock()
                            .map_err(|error| service_daemon::__private::ProviderCandidateInitError::Failed(
                                Box::new(service_daemon::__private::ProviderInitFailure::new(
                                    service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                    error,
                                )),
                            ))?;
                    });
                }
                Some(WrapperKind::ArcMutex(_, _)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_mutex()
                            .map_err(|error| service_daemon::__private::ProviderCandidateInitError::Failed(
                                Box::new(service_daemon::__private::ProviderInitFailure::new(
                                    service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                    error,
                                )),
                            ))?;
                    });
                }
                Some(WrapperKind::Arc(_)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_snapshot()
                            .map_err(|error| service_daemon::__private::ProviderCandidateInitError::Failed(
                                Box::new(service_daemon::__private::ProviderInitFailure::new(
                                    service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                    error,
                                )),
                            ))?;
                    });
                }
                None => {
                    return Err(provider_help_error(
                        arg,
                        "Provider implementation parameters must be Arc-wrapped dependencies",
                        "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>",
                    ));
                }
            }

            let arg_name_str = arg_name.to_string();
            let type_str = quote!(#inner_type).to_string().replace(' ', "");
            param_entries.push(quote! {
                service_daemon::__private::ServiceParam {
                    name: #arg_name_str,
                    type_name: #type_str,
                    type_id: std::any::TypeId::of::<#inner_type>(),
                    kind: service_daemon::__private::ProviderDependencyKind::#dependency_kind,
                }
            });

            call_args.push(quote! { #arg_name });
        }
    }

    let fn_call_with_args = if fn_asyncness.is_some() {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Provider implementation function '{}' for contract '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str, #return_type_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    let init_body = if is_fallible {
        quote! {
            #(#resolve_tokens)*
            service_daemon::__private::init_provider_candidate(
                #fn_name_str,
                policy,
                cancel,
                move || {
                    #(let #call_args = #call_args.clone();)*
                    async move { #fn_call_with_args }
                },
            )
            .await
        }
    } else {
        quote! {
            #(#resolve_tokens)*
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#fn_call_with_args))
        }
    };

    let identity_name = format_ident!(
        "__PROVIDER_IMPL_IDENTITY_{}",
        fn_name.to_string().to_uppercase()
    );
    let init_fn_name = format_ident!(
        "__PROVIDER_IMPL_INIT_{}",
        fn_name.to_string().to_uppercase()
    );
    let entry_name = format_ident!(
        "__PROVIDER_IMPL_ENTRY_{}",
        fn_name.to_string().to_uppercase()
    );
    let priority = args.priority;
    let cache_scope = if block_uses_service_handle_macro(fn_block) {
        quote! { service_daemon::__private::ProviderCacheScope::DaemonLocal }
    } else {
        quote! { service_daemon::__private::ProviderCacheScope::Inherited }
    };

    Ok(TokenStream::from(quote! {
        #(#cleaned_attrs)*
        #fn_vis #fn_sig #fn_block

        #provider_error_assertion
        #contract_assertion

        #[allow(non_camel_case_types)]
        struct #identity_name;

        fn #init_fn_name(
            policy: service_daemon::RestartPolicy,
            cancel: service_daemon::__private::tokio_util::sync::CancellationToken,
        ) -> service_daemon::__private::futures::future::BoxFuture<
            'static,
            std::result::Result<
                std::sync::Arc<dyn std::any::Any + Send + Sync>,
                service_daemon::__private::ProviderCandidateInitError,
            >,
        > {
            Box::pin(async move {
                match service_daemon::__private::catch_init_panic(
                    #fn_name_str,
                    async move { #init_body },
                )
                .await
                {
                    Ok(Ok(value)) => Ok(value as std::sync::Arc<dyn std::any::Any + Send + Sync>),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(service_daemon::__private::ProviderCandidateInitError::Failed(
                        Box::new(service_daemon::__private::ProviderInitFailure::new(
                            service_daemon::__private::ProviderInitSourceKind::Panic,
                            error,
                        )),
                    )),
                }
            })
        }

        #[allow(unsafe_code)] // linkme uses #[link_section] internally
        #[service_daemon::__private::linkme::distributed_slice(service_daemon::__private::PROVIDER_CANDIDATE_REGISTRY)]
        #[linkme(crate = service_daemon::__private::linkme)]
        static #entry_name: service_daemon::__private::ProviderCandidateEntry = service_daemon::__private::ProviderCandidateEntry {
            name: #fn_name_str,
            module: module_path!(),
            output_type_id: std::any::TypeId::of::<#provided_type>(),
            output_type_name: #return_type_str,
            provider_type_id: std::any::TypeId::of::<#identity_name>(),
            priority: #priority,
            params: &[#(#param_entries),*],
            cache_scope: #cache_scope,
            init: #init_fn_name,
        };
    }))
}

fn generate_async_fn_provider(item_fn: ItemFn, eager: bool) -> syn::Result<TokenStream> {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_sig = &item_fn.sig;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;
    let fn_inputs = &item_fn.sig.inputs;

    if let syn::Safety::Unsafe(unsafety) = &item_fn.sig.safety {
        return Err(provider_help_error(
            unsafety,
            "#[provider] fn cannot be unsafe",
            "Move unsafe operations behind a safe provider function boundary",
        ));
    }

    let return_type = match &item_fn.sig.output {
        syn::ReturnType::Type(_, ty) => ty.clone(),
        syn::ReturnType::Default => {
            return Err(provider_help_error(
                &item_fn.sig,
                "#[provider] fn must have a return type",
                "Add a return type, e.g., `async fn config() -> MyConfig { ... }`",
            ));
        }
    };

    let result_return = extract_result_provider_return(&return_type);
    if let Some(result_return) = &result_return
        && !is_provider_error_type(&result_return.err_ty)
    {
        return Err(provider_help_error(
            &result_return.err_ty,
            "#[provider] function Result error type must be service_daemon::ProviderError",
            "Use Result<T, service_daemon::ProviderError> for provider-init retry/fatal semantics, or wrap non-framework Result values in a local provider type",
        ));
    }

    let fallible_return = result_return;
    let provider_error_assertion = fallible_return
        .as_ref()
        .map(|fallible| provider_error_type_assertion(&fallible.err_ty));
    let (provided_type, is_fallible) = match &fallible_return {
        Some(fallible) => (fallible.ok_ty.clone(), true),
        None => ((*return_type).clone(), false),
    };

    let fn_name_str = fn_name.to_string();
    let return_type_str = quote!(#provided_type).to_string().replace(" ", "");
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&item_fn.attrs);

    let mut framework_resolve_tokens = Vec::new();
    let mut managed_resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();

    for arg in fn_inputs {
        if let syn::FnArg::Receiver(_) = arg {
            return Err(provider_help_error(
                arg,
                "#[provider] fn must be a free function, not a method",
                "Remove the `self` parameter",
            ));
        }

        if let syn::FnArg::Typed(syn::PatType { attrs, pat, ty, .. }) = arg
            && let syn::Pat::Ident(pat_ident) = &**pat
        {
            if attrs.iter().any(|attr| attr.path().is_ident("input")) {
                return Err(provider_help_error(
                    arg,
                    "#[input] is only supported by #[service]",
                    "Provider function parameters must be Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>> dependencies",
                ));
            }
            let arg_name = &pat_ident.ident;
            let (inner_type, wrapper) = decompose_type(ty);
            let dependency_kind = match wrapper {
                Some(WrapperKind::ArcRwLock(_, _)) => quote! { RwLock },
                Some(WrapperKind::ArcMutex(_, _)) => quote! { Mutex },
                Some(WrapperKind::Arc(_)) => quote! { Snapshot },
                None => quote! { Snapshot },
            };

            match wrapper {
                Some(WrapperKind::ArcRwLock(_, _)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_rwlock()
                            .map_err(service_daemon::__private::ProviderInitFailure::from)?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_rwlock()
                            .map_err(|e| service_daemon::ProviderError::Fatal(e.to_string()))?;
                    });
                }
                Some(WrapperKind::ArcMutex(_, _)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_mutex()
                            .map_err(service_daemon::__private::ProviderInitFailure::from)?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_mutex()
                            .map_err(|e| service_daemon::ProviderError::Fatal(e.to_string()))?;
                    });
                }
                Some(WrapperKind::Arc(_)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_snapshot()
                            .map_err(service_daemon::__private::ProviderInitFailure::from)?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::__private::ProviderDefinition>::ready_snapshot()
                            .map_err(|e| service_daemon::ProviderError::Fatal(e.to_string()))?;
                    });
                }
                None => {
                    return Err(provider_help_error(
                        arg,
                        "Provider function parameters must be Arc-wrapped dependencies",
                        "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>",
                    ));
                }
            }

            let arg_name_str = arg_name.to_string();
            let type_str = quote!(#inner_type).to_string().replace(' ', "");
            param_entries.push(quote! {
                service_daemon::__private::ServiceParam {
                    name: #arg_name_str,
                    type_name: #type_str,
                    type_id: std::any::TypeId::of::<#inner_type>(),
                    kind: service_daemon::__private::ProviderDependencyKind::#dependency_kind,
                }
            });

            call_args.push(quote! { #arg_name });
        }
    }

    let fn_call_with_args = if fn_asyncness.is_some() {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Provider function '{}' for type '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str, #return_type_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        fn_name.to_string().to_uppercase()
    );

    let framework_init_fn = if is_fallible {
        quote! {
            #(#framework_resolve_tokens)*
            service_daemon::__private::init_fallible(
                #fn_name_str,
                policy,
                cancel,
                move || {
                    #(let #call_args = #call_args.clone();)*
                    async move { #fn_call_with_args }
                },
            )
            .await
        }
    } else {
        quote! {
            #(#framework_resolve_tokens)*
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#fn_call_with_args))
        }
    };

    let managed_init_fn = if is_fallible {
        quote! {
            #(#managed_resolve_tokens)*
            let value = #fn_call_with_args?;
            Ok(std::sync::Arc::new(value))
        }
    } else {
        quote! {
            #(#managed_resolve_tokens)*
            Ok(std::sync::Arc::new(#fn_call_with_args))
        }
    };

    let type_tokens = quote! { #provided_type };
    let helper_style = if is_fallible || !param_entries.is_empty() {
        HelperStyle::Fallible
    } else {
        HelperStyle::Infallible
    };
    let provider_origin = format!("#[provider] function {fn_name_str}");
    let cache_scope = if block_uses_service_handle_macro(fn_block) {
        quote! { service_daemon::__private::ProviderCacheScope::DaemonLocal }
    } else {
        quote! { service_daemon::__private::ProviderCacheScope::Inherited }
    };
    let provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &[],
        user_span: return_type.span(),
        param_entries: &param_entries,
        eager,
        cache_scope,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style,
        provider_origin,
    });

    let expanded = quote! {
        #(#cleaned_attrs)*
        #fn_vis #fn_sig #fn_block

        #provider_error_assertion

        #provided_impl
    };

    Ok(TokenStream::from(expanded))
}

#[cfg(test)]
mod tests {
    use super::{ProviderContractArgs, ProviderImplArgs, block_uses_service_handle_macro};

    #[test]
    fn detects_service_handle_macro_in_provider_block() {
        let block: syn::Block =
            syn::parse_quote!({ service_daemon::service_handle!(worker).map(WorkerHandle) });

        assert!(block_uses_service_handle_macro(&block));
    }

    #[test]
    fn ignores_service_handle_identifier_without_macro_call() {
        let block: syn::Block = syn::parse_quote!({
            let service_handle = WorkerHandle::default();
            service_handle
        });

        assert!(!block_uses_service_handle_macro(&block));
    }

    #[test]
    fn provider_contract_args_default_to_lazy() {
        let args = syn::parse2::<ProviderContractArgs>(quote::quote!()).unwrap();
        assert!(!args.eager);
    }

    #[test]
    fn provider_contract_args_accept_explicit_eager_values() {
        let eager = syn::parse2::<ProviderContractArgs>(quote::quote!(eager = true)).unwrap();
        let lazy = syn::parse2::<ProviderContractArgs>(quote::quote!(eager = false)).unwrap();

        assert!(eager.eager);
        assert!(!lazy.eager);
    }

    #[test]
    fn provider_contract_args_reject_duplicate_unknown_and_malformed_values() {
        assert!(
            syn::parse2::<ProviderContractArgs>(quote::quote!(eager = true, eager = false))
                .is_err()
        );
        assert!(syn::parse2::<ProviderContractArgs>(quote::quote!(capacity = 4)).is_err());
        assert!(syn::parse2::<ProviderContractArgs>(quote::quote!(eager = "true")).is_err());
    }

    #[test]
    fn provider_impl_args_default_to_priority_50() {
        let args = syn::parse2::<ProviderImplArgs>(quote::quote!()).unwrap();

        assert_eq!(args.priority, 50);
    }

    #[test]
    fn provider_impl_args_accept_priority_boundaries() {
        let minimum = syn::parse2::<ProviderImplArgs>(quote::quote!(priority = 0)).unwrap();
        let maximum = syn::parse2::<ProviderImplArgs>(quote::quote!(priority = 255)).unwrap();

        assert_eq!(minimum.priority, 0);
        assert_eq!(maximum.priority, 255);
    }

    #[test]
    fn provider_impl_args_reject_duplicate_priority() {
        assert!(
            syn::parse2::<ProviderImplArgs>(quote::quote!(priority = 10, priority = 20)).is_err()
        );
    }

    #[test]
    fn provider_impl_args_reject_unknown_attribute() {
        assert!(syn::parse2::<ProviderImplArgs>(quote::quote!(fallback = true)).is_err());
    }

    #[test]
    fn provider_impl_args_reject_non_integer_priority() {
        assert!(syn::parse2::<ProviderImplArgs>(quote::quote!(priority = "high")).is_err());
    }

    #[test]
    fn provider_impl_args_reject_priority_above_u8() {
        assert!(syn::parse2::<ProviderImplArgs>(quote::quote!(priority = 256)).is_err());
    }
}
