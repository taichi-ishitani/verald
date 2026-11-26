//! The procedural macros of verald-assertion: `#[assertions]` on an assertion
//! block's struct and on its impl.
//!
//! The struct's signal fields (`#[clock]`, `#[reset]`, `#[input]`,
//! `#[modport]`) are moved into a generated signals struct; the fields left
//! are the block's auxiliary state. The struct's expansion also defines a
//! `macro_rules!` carrying the signals' names, through which the impl's
//! expansion goes, so that the impl (a separate macro call, which cannot see
//! the struct) knows which bare names are signals. The struct must
//! therefore come before its impl, in the same module.

mod block;
mod named;
mod nfa;
mod property;
mod rewrite;
mod seq;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use verald_macros_codegen::paths;

/// The path of verald-assertion as the calling crate reaches it:
/// `verald::assertion`, or `verald_assertion` used on its own.
pub(crate) fn assertion_crate() -> TokenStream2 {
    paths::path(quote!(assertion), "verald-assertion", "verald_assertion")
}

/// Makes a struct an assertion block's signals and state, or an impl its
/// properties and hooks. See `verald_assertion`.
#[proc_macro_attribute]
pub fn assertions(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(block::assertions(attr.into(), item.into()))
}

/// The impl's expansion, called back with the signals' names by the macro
/// the struct's expansion defines.
#[doc(hidden)]
#[proc_macro_attribute]
pub fn __assertions_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(block::assertions_impl(attr.into(), item.into()))
}

fn expand(r: syn::Result<TokenStream2>) -> TokenStream {
    r.unwrap_or_else(syn::Error::into_compile_error).into()
}
