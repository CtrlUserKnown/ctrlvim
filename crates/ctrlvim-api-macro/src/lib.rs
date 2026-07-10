//! The `#[ctrlvim_api]` proc-macro — the replacement for Neovim's LPeg-based
//! `c_grammar.lua` + `gen_api_dispatch.lua` codegen pipeline.
//!
//! In C, a public API function is a plain C function whose signature is parsed
//! out of the source text by a hand-rolled LPeg C grammar, and two dispatch
//! wrappers (msgpack-RPC and Lua) are generated for it. Rust has real attribute
//! macros, so we do the same job at the type level with no text parsing: annotate
//! an ordinary Rust function and the macro emits a type-erased dispatch shim plus
//! an `inventory` registration so the runtime can find it by name and expose it
//! to both the Lua `vim.api` table and the RPC handler map.
//!
//! ```ignore
//! #[ctrlvim_api(since = 1, fast)]
//! fn ctrlvim_get_current_line(cx: &mut ApiContext) -> Result<Object> { ... }
//! ```

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, LitInt, Meta, ReturnType};

/// Attribute macro applied to API functions. Accepts optional `since = N`,
/// `fast`, and `textlock` metadata mirroring Neovim's `FUNC_API_*` annotations.
#[proc_macro_attribute]
pub fn ctrlvim_api(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let attrs = parse_attr(attr);

    let name = func.sig.ident.clone();
    let name_str = name.to_string();
    let dispatch_ident = format_ident!("__ctrlvim_api_dispatch_{}", name);

    // The first parameter is the `&mut ApiContext`; the rest are Object args
    // converted positionally from the incoming argument slice.
    let param_count = func.sig.inputs.len();
    if param_count == 0 {
        return syn::Error::new_spanned(
            &func.sig,
            "#[ctrlvim_api] functions must take `cx: &mut ApiContext` as the first parameter",
        )
        .to_compile_error()
        .into();
    }

    // Build `let a{i} = FromObject::from_object(args.get(i))?;` for each arg
    // beyond the context parameter.
    let arg_conversions = (1..param_count).map(|i| {
        let ident = format_ident!("__arg{}", i - 1);
        let idx = i - 1;
        let argname = format!("{}#{}", name_str, idx + 1);
        quote! {
            let #ident = crate::convert::FromObject::from_object(
                args.get(#idx).unwrap_or(&::ctrlvim_types::Object::Nil),
                #argname,
            )?;
        }
    });
    let arg_idents: Vec<_> = (1..param_count)
        .map(|i| format_ident!("__arg{}", i - 1))
        .collect();

    // Validate the return type is a Result (we wrap its Ok into an Object).
    if let ReturnType::Type(_, _) = &func.sig.output {
        // fine — assume Result<T> where T: IntoObject
    }

    // Confirm the first arg looks like a receiver-style context, not `self`.
    if let Some(FnArg::Receiver(_)) = func.sig.inputs.first() {
        return syn::Error::new_spanned(
            &func.sig,
            "#[ctrlvim_api] functions must be free functions, not methods",
        )
        .to_compile_error()
        .into();
    }

    let since = attrs.since;
    let fast = attrs.fast;

    let expanded = quote! {
        #func

        #[doc(hidden)]
        fn #dispatch_ident(
            cx: &mut crate::ApiContext,
            args: &[::ctrlvim_types::Object],
        ) -> ::ctrlvim_types::Result<::ctrlvim_types::Object> {
            #(#arg_conversions)*
            let __ret = #name(cx #(, #arg_idents)*)?;
            Ok(crate::convert::IntoObject::into_object(__ret))
        }

        ::inventory::submit! {
            crate::registry::ApiFunction {
                name: #name_str,
                since: #since,
                fast: #fast,
                dispatch: #dispatch_ident,
            }
        }
    };

    expanded.into()
}

struct ApiAttrs {
    since: u32,
    fast: bool,
}

fn parse_attr(attr: TokenStream) -> ApiAttrs {
    let mut out = ApiAttrs { since: 0, fast: false };
    if attr.is_empty() {
        return out;
    }
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let metas = match syn::parse::Parser::parse(parser, attr) {
        Ok(m) => m,
        Err(_) => return out,
    };
    for meta in metas {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("since") => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }) = nv.value
                {
                    out.since = i.base10_parse::<u32>().unwrap_or(0);
                }
            }
            Meta::Path(p) if p.is_ident("fast") => out.fast = true,
            // `textlock` and other markers are accepted and currently ignored.
            _ => {}
        }
    }
    out
}

// Silence unused import warning if LitInt ends up unreferenced across edits.
#[allow(dead_code)]
fn _use_litint(_: LitInt) {}
