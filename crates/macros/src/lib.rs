//! Verald's procedural macros shared by its parts: `#[modport]`. The code
//! generation is `verald-macros-codegen`'s.

use proc_macro::TokenStream;

/// Declares the members of an interface's modport as value fields:
/// `#[input]` fields are sampled, `#[output]` fields are driven. Each field
/// binds to the modport member of its name. Use it as a `#[modport]` field
/// of a `#[clocking]` struct. A struct with no `#[output]` field is
/// `InputOnly`.
#[proc_macro_attribute]
pub fn modport(attr: TokenStream, item: TokenStream) -> TokenStream {
    verald_macros_codegen::modport::modport(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
