//! Procedural macros of `verald_random`: `#[randomize]`, `#[constraints]`,
//! `#[constraint_fn]`, `RandEnum`, `randomize_with!`, `std_randomize!` and
//! `randcase!`.
//!
//! The generated code names the crate through the one the user depends on:
//! `verald` (as `verald::random`) or `verald-random`.

mod random;

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::Ident;

/// The path of the constrained random crate as the calling crate reaches
/// it: `verald::random`, or `verald_random` used on its own.
pub(crate) fn random() -> TokenStream2 {
    let named = |krate: &str| match crate_name(krate).ok()? {
        FoundCrate::Name(name) => Some(Ident::new(&name, Span::call_site())),
        FoundCrate::Itself => None,
    };
    if let Some(verald) = named("verald") {
        return quote!(::#verald::random);
    }
    match named("verald-random") {
        Some(name) => quote!(::#name),
        None => quote!(::verald_random),
    }
}

fn expand(r: syn::Result<TokenStream2>) -> TokenStream {
    r.unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Makes a struct randomizable: `#[rand]` fields are random variables,
/// the others are state. See `verald_random`.
#[proc_macro_attribute]
pub fn randomize(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::randomize(attr.into(), item.into()))
}

/// The constraint blocks of a random struct (`#[constraints] impl T`), or
/// of a constraint set (`#[constraints(for T)]`).
#[proc_macro_attribute]
pub fn constraints(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::constraints(attr.into(), item.into()))
}

/// A function written in constraint syntax, expanded where it is called.
#[proc_macro_attribute]
pub fn constraint_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::constraint_fn(attr.into(), item.into()))
}

/// Lets a fieldless enum be a random variable: it takes only its variants.
#[proc_macro_derive(RandEnum)]
pub fn rand_enum(item: TokenStream) -> TokenStream {
    expand(random::rand_enum(item.into()))
}

/// `randomize_with!(obj, { constraints })`: SV `randomize() with`.
#[proc_macro]
pub fn randomize_with(input: TokenStream) -> TokenStream {
    expand(random::randomize_with(input.into()))
}

/// `std_randomize!([x, y], { constraints })`: randomizes local variables.
#[proc_macro]
pub fn std_randomize(input: TokenStream) -> TokenStream {
    expand(random::std_randomize(input.into()))
}

/// `randcase! { 3 => a(), 1 => b() }`: runs one branch, picked by weight.
#[proc_macro]
pub fn randcase(input: TokenStream) -> TokenStream {
    expand(random::randcase(input.into()))
}
