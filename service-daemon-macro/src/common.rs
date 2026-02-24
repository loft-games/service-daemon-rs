use proc_macro_error2::abort;
use quote::{quote, quote_spanned};
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
}

/// Helper to check if `#[allow_sync]` is present on the function's attributes.
pub fn has_allow_sync(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "allow_sync")
    })
}

/// Represents the detected intent of a function parameter.
#[derive(Debug, Clone)]
pub enum ParamIntent {
    /// An event payload (optionally wrapped in Arc).
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

impl WrapperKind {
    /// Returns a formatted string representation of the wrapper type.
    fn format_with_inner(&self, inner_type_str: &str) -> String {
        match self {
            WrapperKind::Arc(_) => format!("Arc<{}>", inner_type_str),
            WrapperKind::ArcRwLock(_, _) => format!("Arc<RwLock<{}>>", inner_type_str),
            WrapperKind::ArcMutex(_, _) => format!("Arc<Mutex<{}>>", inner_type_str),
        }
    }
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

        // Implicit payload (non-wrapped type)
        return Some((arg_name, ParamIntent::Payload { is_arc: false }));
    }
    None
}

/// Decomposes a type to find the inner type and the wrapper kind.
/// Supports Arc<T>, Arc<RwLock<T>>, Arc<Mutex<T>>.
/// Now supports qualified paths (e.g., std::sync::Arc) and captures spans.
fn decompose_type(ty: &Type) -> (&Type, Option<WrapperKind>) {
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
            payload_arg_name: None,
        }
    }

    /// Processes a payload parameter.
    fn process_payload(&mut self, arg: &FnArg, arg_name: syn::Ident, is_arc: bool) {
        if !self.allow_payload {
            abort!(
                arg,
                "Services do not support event payloads. Only Arc<T> dependencies are allowed.";
                help = "Remove the payload parameter or use #[trigger] instead."
            );
        }

        if self.payload_arg_name.is_some() {
            abort!(
                arg,
                "Multiple payload parameters detected. Only one parameter can be the event payload."
            );
        }
        self.payload_arg_name = Some(arg_name);

        let mut clean_arg = arg.clone();
        if let syn::FnArg::Typed(syn::PatType { attrs, .. }) = &mut clean_arg {
            attrs.retain(|a| !a.path().is_ident("payload"));
        }
        self.clean_inputs.push(clean_arg);

        if is_arc {
            self.call_args.push(quote! { std::sync::Arc::new(payload) });
        } else {
            self.call_args.push(quote! { payload });
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
        let arg_type_wrapper_str = wrapper.format_with_inner(&type_str);

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
                    let #arg_name = #inner_type::rwlock().await;
                });
                let rw_path = quote_spanned! { rwlock_span => service_daemon::utils::managed_state::RwLock<#inner_type> };
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
                    let #arg_name = #inner_type::mutex().await;
                });
                let mutex_path = quote_spanned! { mutex_span => service_daemon::utils::managed_state::Mutex<#inner_type> };
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
        let key_str = format!("{}_{}", arg_name_str, arg_type_wrapper_str);
        self.param_entries.push(quote! {
            service_daemon::ServiceParam { name: #arg_name_str, type_name: #type_str, key: #key_str }
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
            "Unsupported parameter type. Service parameters must be Arc wrappers.";
            help = "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>."
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
        }
    }
}

/// Extracts and categorizes parameters from the function signature.
/// Supported by both `#[service]` and `#[trigger]`.
pub fn extract_params(sig: &syn::Signature, allow_payload: bool) -> ExtractedParams {
    let mut processor = ParamProcessor::new(allow_payload);
    for arg in &sig.inputs {
        processor.process_param(arg);
    }
    processor.finish()
}
