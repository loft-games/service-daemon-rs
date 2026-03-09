use proc_macro_error2::abort;
use quote::{format_ident, quote, quote_spanned};
use syn::{Attribute, FnArg, GenericArgument, Pat, PathArguments, Type};

/// Result of extracting and categorizing function parameters.
///
/// Used by `#[service]` and `#[trigger]` macros to collect:
/// - Cleaned function inputs (without macro attributes).
/// - Dependency resolution tokens.
/// - Call arguments for the user function.
/// - Registry parameter entries.
/// - Watcher select arms for reactive dependency updates.
pub struct ExtractedParams {
    /// Function inputs with macro attributes stripped.
    pub clean_inputs: syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>,
    /// Tokens that resolve dependencies via DI.
    pub resolve_tokens: Vec<proc_macro2::TokenStream>,
    /// Arguments to pass when calling the user function.
    pub call_args: Vec<proc_macro2::TokenStream>,
    /// ServiceParam entries for the static registry.
    pub param_entries: Vec<proc_macro2::TokenStream>,
    /// Select arms for the watcher function (reactive updates).
    pub watcher_arms: Vec<proc_macro2::TokenStream>,
    /// Variable identifiers for DI-resolved dependencies.
    ///
    /// Used by trigger codegen to generate `let x = x.clone();` shadow
    /// bindings inside the per-event `Fn` closure.
    pub di_idents: Vec<syn::Ident>,
}

/// Extracts the `sync_handler` flag from function attributes and returns
/// cleaned attributes with `sync_handler` stripped from any `#[allow(...)]`.
///
/// This function scans the attribute list for `#[allow(sync_handler)]`.
/// When found, it:
/// - Returns `true` to indicate sync is explicitly allowed.
/// - Strips `sync_handler` from the `#[allow(...)]` list so the compiler
///   never sees the unknown lint name.
/// - If `#[allow(sync_handler)]` was the only item, the entire attribute
///   is removed. If mixed (e.g., `#[allow(dead_code, sync_handler)]`),
///   only `sync_handler` is removed and `#[allow(dead_code)]` is preserved.
///
/// # Returns
/// `(is_sync_allowed, cleaned_attrs)`
pub fn extract_sync_handler_flag(attrs: &[Attribute]) -> (bool, Vec<Attribute>) {
    let mut found = false;
    let mut cleaned = Vec::with_capacity(attrs.len());

    for attr in attrs {
        // Only inspect `#[allow(...)]` attributes
        if attr.path().is_ident("allow")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            // Parse the token stream inside allow(...) to find sync_handler
            let tokens = &meta_list.tokens;
            let mut has_sync_handler = false;
            let mut other_idents: Vec<proc_macro2::TokenStream> = Vec::new();

            // Walk tokens: expect comma-separated identifiers
            for token in tokens.clone().into_iter() {
                match &token {
                    proc_macro2::TokenTree::Ident(ident) if ident == "sync_handler" => {
                        has_sync_handler = true;
                    }
                    proc_macro2::TokenTree::Punct(p) if p.as_char() == ',' => {
                        // Skip commas — we rebuild them below
                    }
                    other => {
                        other_idents.push(other.clone().into());
                    }
                }
            }

            if has_sync_handler {
                found = true;
                // If there are remaining lints, rebuild the #[allow(...)]
                if !other_idents.is_empty() {
                    let rebuilt: proc_macro2::TokenStream = other_idents
                        .into_iter()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .enumerate()
                        .flat_map(|(i, ts)| {
                            if i > 0 {
                                vec![
                                    proc_macro2::TokenTree::Punct(proc_macro2::Punct::new(
                                        ',',
                                        proc_macro2::Spacing::Alone,
                                    ))
                                    .into(),
                                    ts,
                                ]
                            } else {
                                vec![ts]
                            }
                        })
                        .collect();

                    let new_attr: Attribute = syn::parse_quote!(#[allow(#rebuilt)]);
                    cleaned.push(new_attr);
                }
                // If sync_handler was the only item, drop the entire attribute
                continue;
            }
        }

        // Keep all other attributes unchanged
        cleaned.push(attr.clone());
    }

    (found, cleaned)
}

/// Represents the shared parser classification for a function parameter.
#[derive(Debug, Clone)]
pub enum ParamIntent {
    /// A parameter routed through the shared payload lane.
    ///
    /// For `#[trigger]`, this is the actual trigger payload (optionally wrapped
    /// in `Arc`). For `#[service]`, the same lane is only used to reject
    /// unsupported bare or `#[payload]` parameters with a service-specific error.
    Payload { is_arc: bool },
    /// A DI dependency.
    Dependency {
        inner_type: Box<Type>,
        wrapper: WrapperKind,
    },
}

/// The type of wrapper used for a dependency, including spans for documentation hints.
#[derive(Debug, Clone, Copy)]
pub enum WrapperKind {
    /// Arc<T>
    Arc(proc_macro2::Span),
    /// Arc<RwLock<T>>
    ArcRwLock(proc_macro2::Span, proc_macro2::Span),
    /// Arc<Mutex<T>>
    ArcMutex(proc_macro2::Span, proc_macro2::Span),
}

/// Analyzes a function argument to determine its DI intent.
pub fn analyze_param(arg: &FnArg) -> Option<(syn::Ident, ParamIntent)> {
    if let FnArg::Typed(syn::PatType {
        pat,
        ty,
        attrs: arg_attrs,
        ..
    }) = arg
        && let Pat::Ident(pat_ident) = &**pat
    {
        let arg_name = pat_ident.ident.clone();

        // 1. Check for explicit #[payload]
        let is_explicit_payload = arg_attrs.iter().any(|a| a.path().is_ident("payload"));

        // 2. Analyze the type structure
        let (inner_type, wrapper) = decompose_type(ty);

        if is_explicit_payload {
            return Some((
                arg_name,
                ParamIntent::Payload {
                    is_arc: wrapper.is_some(),
                },
            ));
        }

        if let Some(wrapper) = wrapper {
            return Some((
                arg_name,
                ParamIntent::Dependency {
                    inner_type: Box::new(inner_type.clone()),
                    wrapper,
                },
            ));
        }

        // This parser is shared by `#[service]` and `#[trigger]`, so bare
        // non-wrapper parameters flow through one common classification branch.
        // For triggers that branch represents the real payload parameter. For
        // services it is only the internal rejection path for unsupported
        // signatures; services do not conceptually have payload parameters.
        return Some((arg_name, ParamIntent::Payload { is_arc: false }));
    }
    None
}

/// Decomposes a type to find the inner type and the wrapper kind.
/// Supports Arc<T>, Arc<RwLock<T>>, Arc<Mutex<T>>.
/// Now supports qualified paths (e.g., std::sync::Arc) and captures spans.
pub(crate) fn decompose_type(ty: &Type) -> (&Type, Option<WrapperKind>) {
    if let Type::Path(syn::TypePath { path, .. }) = ty
        && let Some(segment) = path.segments.last()
        && segment.ident == "Arc"
        && let PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        let arc_span = segment.ident.span();

        // Check for nested RwLock or Mutex
        if let Type::Path(syn::TypePath {
            path: inner_path, ..
        }) = inner
            && let Some(inner_segment) = inner_path.segments.last()
        {
            if inner_segment.ident == "RwLock"
                && let PathArguments::AngleBracketed(inner_args) = &inner_segment.arguments
                && let Some(GenericArgument::Type(actual_inner)) = inner_args.args.first()
            {
                return (
                    actual_inner,
                    Some(WrapperKind::ArcRwLock(arc_span, inner_segment.ident.span())),
                );
            }
            if inner_segment.ident == "Mutex"
                && let PathArguments::AngleBracketed(inner_args) = &inner_segment.arguments
                && let Some(GenericArgument::Type(actual_inner)) = inner_args.args.first()
            {
                return (
                    actual_inner,
                    Some(WrapperKind::ArcMutex(arc_span, inner_segment.ident.span())),
                );
            }
        }
        return (inner, Some(WrapperKind::Arc(arc_span)));
    }
    (ty, None)
}

/// Processes function parameters and generates the necessary tokens.
struct ParamProcessor {
    allow_payload: bool,
    clean_inputs: syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>,
    resolve_tokens: Vec<proc_macro2::TokenStream>,
    call_args: Vec<proc_macro2::TokenStream>,
    param_entries: Vec<proc_macro2::TokenStream>,
    watcher_arms: Vec<proc_macro2::TokenStream>,
    di_idents: Vec<syn::Ident>,
    payload_arg_name: Option<syn::Ident>,
}

impl ParamProcessor {
    fn new(allow_payload: bool) -> Self {
        Self {
            allow_payload,
            clean_inputs: syn::punctuated::Punctuated::new(),
            resolve_tokens: Vec::new(),
            call_args: Vec::new(),
            param_entries: Vec::new(),
            watcher_arms: Vec::new(),
            di_idents: Vec::new(),
            payload_arg_name: None,
        }
    }

    /// Processes a payload parameter.
    ///
    /// The framework now wraps every payload in `Arc<P>` internally.
    /// This method generates the correct extraction code based on
    /// whether the user's handler expects `Arc<T>` or bare `T`:
    ///
    /// - **`is_arc == true`**: user declared `Arc<T>` — pass the
    ///   framework's `Arc` directly (zero-copy, no `Clone` needed).
    /// - **`is_arc == false`**: user declared `T` — dereference the
    ///   `Arc` and clone the data. Uses a descriptive trait call
    ///   to produce a friendly compiler error if `T: Clone` is missing.
    fn process_payload(&mut self, arg: &FnArg, arg_name: syn::Ident, is_arc: bool) {
        if !self.allow_payload {
            // `#[service]` and `#[trigger]` share the same parameter processor.
            // Bare or `#[payload]` parameters arrive here because they use the
            // shared payload classification lane. For services, that lane exists
            // only so we can reject unsupported signatures with accurate wording;
            // it does not mean services semantically support payloads.
            abort!(
                arg,
                "#[service] parameters must be framework-managed dependencies wrapped as Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>. Payload parameters are only supported by #[trigger].";
                help = "Wrap service dependencies in Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>. If you intended to handle an event payload, use #[trigger] instead."
            );
        }

        if self.payload_arg_name.is_some() {
            abort!(
                arg,
                "Multiple payload parameters detected. A trigger can accept only one payload parameter.";
                help = "Keep one bare or #[payload] parameter and convert the others to Arc<T> dependencies."
            );
        }
        self.payload_arg_name = Some(arg_name);

        let mut clean_arg = arg.clone();
        if let syn::FnArg::Typed(syn::PatType { attrs, .. }) = &mut clean_arg {
            attrs.retain(|a| !a.path().is_ident("payload"));
        }
        self.clean_inputs.push(clean_arg);

        if is_arc {
            // User wants Arc<T> — pass the framework's Arc pointer
            // directly. This is a zero-copy path and does NOT require
            // the inner type to implement Clone.
            self.call_args.push(quote! { payload });
        } else {
            // User wants bare T — must clone out of the Arc.
            // Uses a descriptive helper to produce a clear compiler
            // error when T does not implement Clone.
            self.call_args
                .push(quote! { service_daemon::trigger_clone_payload(&*payload) });
        }
    }

    /// Processes a dependency parameter.
    fn process_dependency(
        &mut self,
        arg_name: syn::Ident,
        inner_type: Box<Type>,
        wrapper: WrapperKind,
    ) {
        let arg_name_str = arg_name.to_string();
        let type_str = quote!(#inner_type).to_string().replace(' ', "");

        match wrapper {
            WrapperKind::Arc(arc_span) => {
                self.resolve_tokens.push(quote! {
                    let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                });
                self.clean_inputs.push(
                    syn::parse2(
                        quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#inner_type> },
                    )
                    .unwrap_or_else(|e| {
                        abort!(
                            arg_name,
                            format!("Internal macro error parsing Arc dependency: {}", e)
                        )
                    }),
                );
            }
            WrapperKind::ArcRwLock(arc_span, rwlock_span) => {
                self.resolve_tokens.push(quote! {
                    let #arg_name = <#inner_type as service_daemon::Provided>::resolve_rwlock().await;
                });
                let rw_path = quote_spanned! { rwlock_span => service_daemon::core::managed_state::RwLock<#inner_type> };
                self.clean_inputs.push(
                    syn::parse2(
                        quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#rw_path> },
                    )
                    .unwrap_or_else(|e| {
                        abort!(
                            arg_name,
                            format!("Internal macro error parsing Arc<RwLock> dependency: {}", e)
                        )
                    }),
                );
            }
            WrapperKind::ArcMutex(arc_span, mutex_span) => {
                self.resolve_tokens.push(quote! {
                    let #arg_name = <#inner_type as service_daemon::Provided>::resolve_mutex().await;
                });
                let mutex_path = quote_spanned! { mutex_span => service_daemon::core::managed_state::Mutex<#inner_type> };
                self.clean_inputs.push(
                    syn::parse2(
                        quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#mutex_path> },
                    )
                    .unwrap_or_else(|e| {
                        abort!(
                            arg_name,
                            format!("Internal macro error parsing Arc<Mutex> dependency: {}", e)
                        )
                    }),
                );
            }
        }

        self.call_args.push(quote! { #arg_name });
        self.di_idents.push(arg_name.clone());
        self.param_entries.push(quote! {
            service_daemon::ServiceParam {
                name: #arg_name_str,
                type_name: #type_str,
                type_id: std::any::TypeId::of::<#inner_type>(),
            }
        });

        self.watcher_arms.push(quote! {
            _ = <#inner_type as service_daemon::Provided>::changed() => {}
        });
    }

    /// Processes a single parameter.
    fn process_param(&mut self, arg: &FnArg) {
        if let Some((arg_name, intent)) = analyze_param(arg) {
            match intent {
                ParamIntent::Payload { is_arc } => {
                    self.process_payload(arg, arg_name, is_arc);
                }
                ParamIntent::Dependency {
                    inner_type,
                    wrapper,
                } => {
                    self.process_dependency(arg_name, inner_type, wrapper);
                }
            }
            return;
        }

        abort!(
            arg,
            "Unsupported parameter type. Framework-managed dependencies must use Arc wrappers.";
            help = "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>> for dependencies. Use one bare parameter only when defining a trigger payload."
        );
    }

    /// Consumes the processor and returns the extracted parameters.
    fn finish(self) -> ExtractedParams {
        ExtractedParams {
            clean_inputs: self.clean_inputs,
            resolve_tokens: self.resolve_tokens,
            call_args: self.call_args,
            param_entries: self.param_entries,
            watcher_arms: self.watcher_arms,
            di_idents: self.di_idents,
        }
    }
}

/// Extracts parameters from the function signature using the shared parser.
///
/// Both `#[service]` and `#[trigger]` use this function. The `allow_payload`
/// flag determines whether the shared payload lane is accepted as real trigger
/// payload semantics or reused as the rejection path for unsupported service
/// signatures.
pub fn extract_params(sig: &syn::Signature, allow_payload: bool) -> ExtractedParams {
    let mut processor = ParamProcessor::new(allow_payload);
    for arg in &sig.inputs {
        processor.process_param(arg);
    }
    processor.finish()
}

// -----------------------------------------------------------------------------
// Shared codegen helpers (used by both #[service] and #[trigger])
// -----------------------------------------------------------------------------

/// Generates the call expression for the user's function.
///
/// Shared by `#[service]` and `#[trigger]`. The `kind` parameter controls
/// the sync warning message (e.g., `"Service"` or `"Trigger"`).
pub fn generate_call_expr(
    fn_name: &syn::Ident,
    fn_name_str: &str,
    call_args: &[proc_macro2::TokenStream],
    is_async: bool,
    allow_sync_present: bool,
    kind: &str,
) -> proc_macro2::TokenStream {
    if is_async {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        // User explicitly allowed sync, no warning
        quote! { #fn_name(#(#call_args),*) }
    } else {
        let msg = format!(
            "{} '{{}}' is synchronous. Consider switching to 'async fn'.",
            kind
        );
        quote! {
            {
                tracing::warn!(#msg, #fn_name_str);
                #fn_name(#(#call_args),*)
            }
        }
    }
}

/// Generates the watcher function and pointer for dependency change monitoring.
///
/// Shared by `#[service]` and `#[trigger]`. Both pass their watcher arms
/// (collected by `extract_params`); triggers should push the target's
/// `changed()` arm to the list before calling this function.
///
/// # Returns
/// A tuple of `(watcher_fn_tokens, watcher_ptr_tokens)`.
pub fn generate_watcher(
    fn_name: &syn::Ident,
    watcher_select_arms: &[proc_macro2::TokenStream],
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let watcher_name = format_ident!("{}_watcher", fn_name);

    if !watcher_select_arms.is_empty() {
        (
            quote! {
                /// Auto-generated watcher -- notifies when dependencies change
                pub fn #watcher_name() -> service_daemon::futures::future::BoxFuture<'static, ()> {
                    Box::pin(async move {
                        service_daemon::tokio::select! {
                            #(#watcher_select_arms),*
                        }
                    })
                }
            },
            quote! { Some(#watcher_name) },
        )
    } else {
        (quote! {}, quote! { None })
    }
}

// -----------------------------------------------------------------------------
// Tags parsing (shared by #[service] and #[trigger])
// -----------------------------------------------------------------------------

/// A parsed list of static tag strings from `tags = ["a", "b"]` syntax.
///
/// Implements `syn::Parse` so it can be used inline by both macro parsers.
pub struct TagsList {
    pub tags: Vec<syn::LitStr>,
}

impl syn::parse::Parse for TagsList {
    /// Parses the bracket-delimited list: `["tag_a", "tag_b"]`
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let content;
        syn::bracketed!(content in input);
        let punctuated =
            content.parse_terminated(|input| input.parse::<syn::LitStr>(), syn::Token![,])?;
        Ok(Self {
            tags: punctuated.into_iter().collect(),
        })
    }
}

impl TagsList {
    /// Generates the `tags: &[...]` expression for the `ServiceEntry` codegen.
    pub fn to_tokens(&self) -> proc_macro2::TokenStream {
        let tag_strs: Vec<_> = self
            .tags
            .iter()
            .map(|lit| {
                let s = lit.value();
                quote::quote! { #s }
            })
            .collect();
        if tag_strs.is_empty() {
            quote::quote! { &[] }
        } else {
            quote::quote! { &[#(#tag_strs),*] }
        }
    }
}
