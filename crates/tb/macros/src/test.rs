//! `register_test!`.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, Type};

use crate::core;

/// `Name` or `Name, Type, spec`.
struct Args {
    name: Ident,
    test: Option<(Type, Expr)>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        if input.is_empty() {
            return Ok(Args { name, test: None });
        }
        input.parse::<syn::Token![,]>()?;
        let ty: Type = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let spec: Expr = input.parse()?;
        let _ = input.parse::<Option<syn::Token![,]>>()?;
        if !input.is_empty() {
            return Err(input
                .error("`register_test!` takes a test type, or a name, a test type and a spec"));
        }
        Ok(Args {
            name,
            test: Some((ty, spec)),
        })
    }
}

pub(crate) fn register_test(input: TokenStream2) -> syn::Result<TokenStream2> {
    let Args { name, test } = syn::parse2(input)?;
    let core = core();
    let text = name.to_string();
    let (ty, spec) = match test {
        Some((ty, spec)) => (quote!(#ty), quote!(#spec)),
        None => (quote!(#name), quote!(())),
    };
    let marker = format_ident!("__VERALD_TEST_{}", name);
    let param = Ident::new("__verald_start", Span::mixed_site());
    let registered = Ident::new("__verald_name", Span::mixed_site());
    Ok(quote! {
        /// Registering a name twice in one module does not compile.
        #[allow(non_upper_case_globals, dead_code)]
        const #marker: () = ();

        #core::__private::inventory::submit! {
            #core::__private::TestEntry {
                name: #text,
                start: |#param, #registered| {
                    #core::__private::start_test::<#ty, _>(#param, #registered, #spec)
                },
            }
        }
    })
}
