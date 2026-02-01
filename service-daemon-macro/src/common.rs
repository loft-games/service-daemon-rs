use syn::{Attribute, FnArg, GenericArgument, Pat, PathArguments, Type};

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
    Payload {
        is_arc: bool,
        span: Option<proc_macro2::Span>,
    },
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
            let span = match wrapper {
                Some(WrapperKind::Arc(s)) => Some(s),
                Some(WrapperKind::ArcRwLock(s, _)) => Some(s),
                Some(WrapperKind::ArcMutex(s, _)) => Some(s),
                None => None,
            };
            return Some((
                arg_name,
                ParamIntent::Payload {
                    is_arc: wrapper.is_some(),
                    span,
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
        return Some((
            arg_name,
            ParamIntent::Payload {
                is_arc: false,
                span: None,
            },
        ));
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
