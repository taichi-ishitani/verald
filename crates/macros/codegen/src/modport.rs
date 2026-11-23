//! `#[modport]`: a struct declaring the members of an interface's modport
//! as value fields, bound through the host layer. Its outputs are driven
//! by the testbench's clocking, so the parts that drive them are generated
//! only where the testbench's runtime is reachable.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::{Ident, ItemStruct};

use crate::fields::{
    FieldSpec, HIDDEN, Role, check_plain, init_body, initial_values, load_method,
    push_hidden_field, take_fields,
};
use crate::paths;

pub fn modport(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let mut item: ItemStruct = syn::parse2(item)?;
    check_plain(&attr, &item, "modport")?;
    let specs = take_fields(&mut item, &[Role::Input, Role::Output])?;
    let name = item.ident.clone();
    let members = format_ident!("__VeraldModport{}", name);
    push_hidden_field(&mut item, &members);
    let hidden = Ident::new(HIDDEN, Span::call_site());

    let host = paths::host();
    let tb = paths::tb_runtime();
    let outputs = specs.iter().any(|f| f.role == Role::Output);
    let ck = match (&tb, outputs) {
        (Some(tb), _) => quote!(#tb::clocking),
        (None, true) => {
            return Err(syn::Error::new(
                name.span(),
                "an `#[output]` field is driven by the testbench's clocking: \
                 it needs `verald` or `verald-tb-runtime`",
            ));
        }
        (None, false) => quote!(),
    };
    let member_fields = specs.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        if f.role == Role::Input {
            quote!(#ident: #host::InputSignal<#ty>)
        } else {
            quote!(#ident: #ck::Driven<#ty>)
        }
    });
    let binds = specs.iter().map(|f| {
        let (ident, ty, path) = (&f.ident, &f.ty, &f.name);
        if f.role == Role::Input {
            quote!(#ident: #host::InputSignal::<#ty>::member(source, #path)?)
        } else {
            quote!(#ident: #ck::Driven::<#ty>::member(source, #path)?)
        }
    });
    let input_only = if specs.iter().all(|f| f.role == Role::Input) {
        quote!(impl #host::InputOnly for #name {})
    } else {
        quote!()
    };
    let all: Vec<&FieldSpec> = specs.iter().collect();
    let initials = initial_values(&all);
    let values = specs.iter().map(|f| {
        let ident = &f.ident;
        let local = format_ident!("__verald_v_{}", ident.unraw());
        quote!(#ident: #local)
    });
    let inits = init_body(&all, &hidden);
    let inputs: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Input).collect();
    let loads = inputs.iter().map(|f| {
        let ident = &f.ident;
        quote!(self.#ident = self.#hidden.#ident.read();)
    });
    let stores = specs.iter().filter(|f| f.role == Role::Output).map(|f| {
        let ident = &f.ident;
        quote!(self.#hidden.#ident.store(&self.#ident);)
    });
    let load_methods = specs.iter().map(|f| {
        let ident = &f.ident;
        let method = load_method(&f.name);
        let body = if f.role == Role::Input {
            quote!(self.#ident = self.#hidden.#ident.read();)
        } else {
            // An output is not read.
            quote!()
        };
        quote! {
            #[doc(hidden)]
            #[inline]
            pub fn #method(&mut self) {
                #body
            }
        }
    });

    let drive = if tb.is_some() {
        quote! {
            impl #ck::ModportDrive for #name {
                fn __init(&mut self) {
                    #(#inits)*
                }

                fn __store(&mut self) {
                    #(#stores)*
                }
            }
        }
    } else {
        quote!()
    };

    Ok(quote! {
        #item

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(Debug)]
        struct #members {
            #(#member_fields,)*
        }

        impl #host::ModportRead for #name {
            fn __bind<M: #host::MemberSource + ?::core::marker::Sized>(
                source: &M,
            ) -> ::core::result::Result<Self, #host::HostError> {
                #[allow(unused_mut)]
                let mut members = #members { #(#binds,)* };
                #(#initials)*
                ::core::result::Result::Ok(Self {
                    #(#values,)*
                    #hidden: members,
                })
            }

            fn __load(&mut self) {
                #(#loads)*
            }
        }

        #drive

        #input_only

        impl #name {
            #(#load_methods)*
        }
    })
}
