//! The paths generated code names Verald's crates by, as the calling crate
//! reaches them: through the entry point `verald`, or the crates used on
//! their own.

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::Ident;

/// The path of `krate` as the calling crate names it, if it depends on it.
/// `None` for the calling crate itself (its own tests reach it by name).
fn found(krate: &str) -> Option<Ident> {
    match crate_name(krate).ok()? {
        FoundCrate::Name(name) => Some(Ident::new(&name, Span::call_site())),
        FoundCrate::Itself => None,
    }
}

/// Whether the calling crate is `krate`, or depends on it.
fn reaches(krate: &str) -> bool {
    crate_name(krate).is_ok()
}

/// A crate reached through the entry point (`verald::<module>`), or on its
/// own (`krate`), or `fallback` when neither is found (the crate's own
/// tests and examples).
pub fn path(module: TokenStream2, krate: &str, fallback: &str) -> TokenStream2 {
    if let Some(verald) = found("verald") {
        return quote!(::#verald::#module);
    }
    match found(krate) {
        Some(name) => quote!(::#name),
        None => {
            let name = Ident::new(fallback, Span::call_site());
            quote!(::#name)
        }
    }
}

/// The host layer: `verald::host`, `verald_host`, or the testbench's
/// runtime's re-export (`verald_tb_runtime::host`).
pub fn host() -> TokenStream2 {
    if let Some(verald) = found("verald") {
        return quote!(::#verald::host);
    }
    if let Some(name) = found("verald-host") {
        return quote!(::#name);
    }
    if reaches("verald-tb-runtime") {
        let runtime = tb_runtime().expect("the runtime is reached");
        return quote!(#runtime::host);
    }
    quote!(::verald_host)
}

/// The testbench's runtime, if the calling crate reaches it.
pub fn tb_runtime() -> Option<TokenStream2> {
    if let Some(verald) = found("verald") {
        return Some(quote!(::#verald::tb::runtime));
    }
    if reaches("verald-tb-runtime") {
        return Some(path(
            quote!(tb::runtime),
            "verald-tb-runtime",
            "verald_tb_runtime",
        ));
    }
    None
}
