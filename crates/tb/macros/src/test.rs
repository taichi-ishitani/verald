//! `register_test!` and `#[testcase]`.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Ident, ItemFn, Type};

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

/// `#[testcase(base = B, name = N, spec = S)]`: `base` needed.
struct TestcaseArgs {
    base: Type,
    name: Option<Ident>,
    spec: Option<Expr>,
}

fn testcase_args(attr: TokenStream2) -> syn::Result<TestcaseArgs> {
    let (mut base, mut name, mut spec) = (None, None, None);
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("base") {
            base = Some(meta.value()?.parse::<Type>()?);
        } else if meta.path.is_ident("name") {
            name = Some(meta.value()?.parse::<Ident>()?);
        } else if meta.path.is_ident("spec") {
            spec = Some(meta.value()?.parse::<Expr>()?);
        } else {
            return Err(meta.error("expected `base = ..`, `name = ..` or `spec = ..`"));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, attr)?;
    let base =
        base.ok_or_else(|| syn::Error::new(Span::call_site(), "`#[testcase]` needs `base = ..`"))?;
    Ok(TestcaseArgs { base, name, spec })
}

/// `basic_write` as `BasicWrite`.
fn camel_case(ident: &Ident) -> Ident {
    let camel: String = ident
        .to_string()
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    Ident::new(&camel, ident.span())
}

/// A test made from a function: a scenario calling it, a test made from the
/// base with that scenario as its `run`, and its registration.
pub(crate) fn testcase(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let TestcaseArgs { base, name, spec } = testcase_args(attr)?;
    let function: ItemFn = syn::parse2(item)?;
    if function.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            function.sig.fn_token.span(),
            "`#[testcase]` goes on an `async fn`",
        ));
    }
    let core = core();
    let ident = &function.sig.ident;
    let name = name.unwrap_or_else(|| camel_case(ident));
    let vis = &function.vis;
    let docs = function.attrs.iter().filter(|a| a.path().is_ident("doc"));
    let scenario = format_ident!("__VeraldTestcase{}", name);
    let test = crate::component::component(
        quote!(test(base = #base, run = #scenario)),
        quote! {
            #(#docs)*
            #vis struct #name {}
        },
    )?;
    let register = match spec {
        Some(spec) => register_test(quote!(#name, #name, #spec))?,
        None => register_test(quote!(#name))?,
    };
    Ok(quote! {
        #function

        #[doc(hidden)]
        #[derive(Default)]
        struct #scenario {}

        impl #core::Scenario for #scenario {
            type Test = #base;

            async fn body(
                &mut self,
                test: &mut #base,
                run_context: &mut #core::RunContext,
            ) -> ::core::result::Result<(), #core::__private::TestError> {
                #ident(test, run_context).await
            }
        }

        #test
        #register
    })
}
