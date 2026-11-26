//! The expansions of `#[assertions]` on a struct and on an impl.

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{FnArg, Ident, ImplItem, ImplItemFn, Item, ItemImpl, ItemStruct, Token, Type};
use verald_macros_codegen::fields::{FieldSpec, Role, take_tagged_fields};

use crate::assertion_crate;
use crate::named::{Item as NamedItem, Named, takes_time};
use crate::property;
use crate::rewrite;

/// The name of the signals struct of the block `name`.
fn signals_struct(name: &Ident) -> Ident {
    format_ident!("__VeraldAssertionSignals{}", name)
}

/// The name of the macro carrying the block's signal names.
fn names_macro(name: &Ident) -> Ident {
    format_ident!("__verald_assertions_{}", name)
}

/// The parameter giving a method the sampled signals.
pub(crate) fn signals_param() -> Ident {
    Ident::new("__verald_s", Span::call_site())
}

/// The parameter giving a method the samples, current and earlier.
pub(crate) fn history_param() -> Ident {
    Ident::new("__verald_h", Span::call_site())
}

/// The parameter giving a hook the report.
pub(crate) fn report_param() -> Ident {
    Ident::new("__verald_r", Span::call_site())
}

pub(crate) fn assertions(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            attr.span(),
            "`#[assertions]` takes no arguments: the clock and the reset are \
             `#[clock]` and `#[reset]` fields",
        ));
    }
    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => block_struct(item),
        Item::Impl(item) => {
            let Type::Path(ty) = &*item.self_ty else {
                return Err(syn::Error::new(
                    item.self_ty.span(),
                    "expected the assertion block's type",
                ));
            };
            let name = &ty.path.segments.last().expect("a path").ident;
            let mac = names_macro(name);
            Ok(quote!(#mac! { #item }))
        }
        other => Err(syn::Error::new(
            other.span(),
            "`#[assertions]` goes on an assertion block's struct and its impl",
        )),
    }
}

/// Checks that a clock or reset field is declared with its type
/// (`Clock`, `Reset`), which the expansion replaces with a port.
fn check_port_type(field: &FieldSpec, expected: &str) -> syn::Result<()> {
    let ok = match &field.ty {
        Type::Path(ty) => ty.path.segments.last().is_some_and(|s| s.ident == expected),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(syn::Error::new(
            field.ty.span(),
            format!("a `#[{}]` field is a `{expected}`", expected.to_lowercase()),
        ))
    }
}

fn block_struct(mut item: ItemStruct) -> syn::Result<TokenStream2> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.span(),
            "an assertion block cannot be generic",
        ));
    }
    let specs = take_tagged_fields(
        &mut item,
        &[Role::Clock, Role::Reset, Role::Input, Role::Modport],
    )?;
    let name = item.ident.clone();
    let vis = item.vis.clone();
    let a = assertion_crate();
    let p = quote!(#a::__private);

    let clocks: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Clock).collect();
    let clock = match clocks.as_slice() {
        [clock] => *clock,
        [] => {
            return Err(syn::Error::new(
                name.span(),
                "an assertion block needs a `#[clock]` field: `#[clock] clk: Clock`",
            ));
        }
        [_, second, ..] => {
            return Err(syn::Error::new(
                second.ident.span(),
                "an assertion block has one `#[clock]` field; another clock's \
                 properties go in another block",
            ));
        }
    };
    check_port_type(clock, "Clock")?;
    let resets: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Reset).collect();
    let reset = match resets.as_slice() {
        [] => None,
        [reset] => Some(*reset),
        [_, second, ..] => {
            return Err(syn::Error::new(
                second.ident.span(),
                "an assertion block has at most one `#[reset]` field",
            ));
        }
    };
    if let Some(reset) = reset {
        check_port_type(reset, "Reset")?;
    }
    let inputs: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Input).collect();
    let modports: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Modport).collect();

    let sig = signals_struct(&name);
    let bind = format_ident!("__VeraldAssertionBind{}", name);
    let mac = names_macro(&name);
    let block_name = name.to_string();

    let input_fields = inputs.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        quote!(#ident: #ty)
    });
    let modport_fields = modports.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        quote!(#ident: #ty)
    });
    let bind_fields = inputs.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        quote!(#ident: #p::InputSignal<#ty>)
    });
    let clock_name = &clock.name;
    let reset_bind = match reset {
        Some(reset) => {
            let name = &reset.name;
            quote!(::core::option::Option::Some(#p::Signals::reset_port(signals, &at(#name))?))
        }
        None => quote!(::core::option::Option::None),
    };
    let input_binds = inputs.iter().map(|f| {
        let (ident, ty, name) = (&f.ident, &f.ty, &f.name);
        quote! {
            #ident: {
                let path = at(#name);
                #p::InputSignal::<#ty>::bind(#p::Signals::input(signals, &path)?, &path)?
            }
        }
    });
    let input_zeros = inputs.iter().map(|f| {
        let ident = &f.ident;
        quote!(#ident: members.#ident.zero())
    });
    let modport_binds = modports.iter().map(|f| {
        let (ident, ty, name) = (&f.ident, &f.ty, &f.name);
        quote! {
            #ident: <#ty as #p::ModportRead>::__bind(&#p::Signals::modport(signals, &at(#name))?)?
        }
    });
    let samples = inputs
        .iter()
        .map(|f| {
            let ident = &f.ident;
            quote!(self.#ident = self.__verald.#ident.read();)
        })
        .chain(modports.iter().map(|f| {
            let ident = &f.ident;
            quote!(#p::ModportRead::__load(&mut self.#ident);)
        }));
    let input_only = modports.iter().map(|f| {
        let ty = &f.ty;
        quote! {
            const _: () = {
                fn __verald_input_only<T: #p::InputOnly>() {}
                let _ = __verald_input_only::<#ty>;
            };
        }
    });
    let names = inputs
        .iter()
        .chain(modports.iter())
        .map(|f| &f.ident)
        .collect::<Vec<_>>();
    let sig_doc = format!("The signals of the assertion block [`{name}`], bound and sampled.");

    Ok(quote! {
        #item

        #[doc = #sig_doc]
        #[doc(hidden)]
        #vis struct #sig {
            #(#input_fields,)*
            #(#modport_fields,)*
            __verald: #bind,
        }

        #[doc(hidden)]
        struct #bind {
            clock: #p::ClockPort,
            reset: ::core::option::Option<#p::ResetPort>,
            #(#bind_fields,)*
        }

        #(#input_only)*

        impl #a::AssertionSignals for #sig {
            const NAME: &'static str = #block_name;

            fn bind<S: #p::Signals + ?::core::marker::Sized>(
                signals: &S,
                path: &str,
            ) -> ::core::result::Result<Self, #p::HostError> {
                let at = |member: &str| -> ::std::string::String {
                    if path.is_empty() {
                        member.to_string()
                    } else {
                        ::std::format!("{path}.{member}")
                    }
                };
                let members = #bind {
                    clock: #p::Signals::clock_port(signals, &at(#clock_name))?,
                    reset: #reset_bind,
                    #(#input_binds,)*
                };
                ::core::result::Result::Ok(Self {
                    #(#input_zeros,)*
                    #(#modport_binds,)*
                    __verald: members,
                })
            }

            fn clock(&self) -> #p::ClockPort {
                self.__verald.clock
            }

            fn reset(&self) -> ::core::option::Option<#p::ResetPort> {
                self.__verald.reset
            }

            #[inline]
            fn sample(&mut self) {
                #(#samples)*
            }
        }

        #[doc(hidden)]
        #[allow(unused_macros)]
        macro_rules! #mac {
            ($($t:tt)*) => {
                #[#p::__assertions_impl(#(#names),*)]
                $($t)*
            };
        }
    })
}

/// What a method of the impl is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Helper,
    Assert,
    OnClock,
    OnReset,
    /// `#[sequence]`: expanded where used, not a method.
    Sequence,
    /// `#[property]`: expanded where used, not a method.
    Property,
}

/// Removes the method's marker attribute and returns its kind.
fn take_kind(method: &mut ImplItemFn) -> syn::Result<Kind> {
    let mut kind = Kind::Helper;
    let mut rest = Vec::new();
    for attr in method.attrs.drain(..) {
        let this = if attr.path().is_ident("assert") {
            Some(Kind::Assert)
        } else if attr.path().is_ident("on_clock") {
            Some(Kind::OnClock)
        } else if attr.path().is_ident("on_reset") {
            Some(Kind::OnReset)
        } else if attr.path().is_ident("sequence") {
            Some(Kind::Sequence)
        } else if attr.path().is_ident("property") {
            Some(Kind::Property)
        } else if attr.path().is_ident("cover") {
            return Err(syn::Error::new(
                attr.span(),
                "`#[cover]` is not supported yet",
            ));
        } else {
            None
        };
        match this {
            Some(this) => {
                attr.meta.require_path_only()?;
                if kind != Kind::Helper {
                    return Err(syn::Error::new(attr.span(), "a method takes one marker"));
                }
                kind = this;
            }
            None => rest.push(attr),
        }
    }
    method.attrs = rest;
    Ok(kind)
}

/// Checks that a property or hook takes `&self` (`&mut self` for a hook)
/// and nothing else.
fn check_signature(method: &ImplItemFn, kind: Kind) -> syn::Result<()> {
    let (what, mutable) = match kind {
        Kind::Assert => ("a property takes `&self`", false),
        Kind::OnClock | Kind::OnReset => ("a hook takes `&mut self`", true),
        Kind::Helper => return Ok(()),
        Kind::Sequence | Kind::Property => return check_named(method, kind),
    };
    let sig = &method.sig;
    let receiver_ok = matches!(
        sig.inputs.first(),
        Some(FnArg::Receiver(r)) if r.reference.is_some() && r.mutability.is_some() == mutable
    );
    if !receiver_ok || sig.inputs.len() != 1 {
        return Err(syn::Error::new(
            sig.span(),
            format!("{what} and nothing else"),
        ));
    }
    if !matches!(sig.output, syn::ReturnType::Default) {
        return Err(syn::Error::new(sig.output.span(), "it returns nothing"));
    }
    if sig.asyncness.is_some() || !sig.generics.params.is_empty() {
        return Err(syn::Error::new(sig.span(), "it is a plain method"));
    }
    Ok(())
}

/// Checks that a named sequence or property takes named parameters (no
/// `self`) and returns nothing, and that a sequence's body is one `seq!`.
fn check_named(method: &ImplItemFn, kind: Kind) -> syn::Result<()> {
    let sig = &method.sig;
    let what = if kind == Kind::Sequence {
        "a `#[sequence]`"
    } else {
        "a `#[property]`"
    };
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(r) => {
                return Err(syn::Error::new(
                    r.span(),
                    format!("{what} takes no `self`: it is expanded where it is used"),
                ));
            }
            FnArg::Typed(t) => {
                if !matches!(&*t.pat, syn::Pat::Ident(i) if i.by_ref.is_none() && i.subpat.is_none())
                {
                    return Err(syn::Error::new(
                        t.pat.span(),
                        format!("{what}'s parameter is a name: `valid: bool`"),
                    ));
                }
            }
        }
    }
    if !matches!(sig.output, syn::ReturnType::Default) {
        return Err(syn::Error::new(sig.output.span(), "it returns nothing"));
    }
    if sig.asyncness.is_some() || !sig.generics.params.is_empty() {
        return Err(syn::Error::new(sig.span(), "it is a plain function"));
    }
    if kind == Kind::Sequence && sequence_body(method).is_none() {
        return Err(syn::Error::new(
            method.block.span(),
            "a `#[sequence]`'s body is one `seq!(..)`",
        ));
    }
    Ok(())
}

/// The tokens of a `#[sequence]`'s `seq!`.
fn sequence_body(method: &ImplItemFn) -> Option<TokenStream2> {
    let [stmt] = method.block.stmts.as_slice() else {
        return None;
    };
    let mac = match stmt {
        syn::Stmt::Macro(m) => &m.mac,
        syn::Stmt::Expr(syn::Expr::Macro(m), _) => &m.mac,
        _ => return None,
    };
    mac.path.is_ident("seq").then(|| mac.tokens.clone())
}

/// The impl's named sequences and properties, taken out of it.
fn take_named(item: &mut ItemImpl, kinds: &HashMap<String, Kind>) -> Named {
    let mut named = Named::default();
    item.items.retain(|impl_item| {
        let ImplItem::Fn(method) = impl_item else {
            return true;
        };
        let kind = kinds[&method.sig.ident.to_string()];
        if !matches!(kind, Kind::Sequence | Kind::Property) {
            return true;
        }
        let params: Vec<(Ident, Type)> = method
            .sig
            .inputs
            .iter()
            .filter_map(|input| match input {
                FnArg::Typed(t) => match &*t.pat {
                    syn::Pat::Ident(i) => Some((i.ident.clone(), (*t.ty).clone())),
                    _ => None,
                },
                FnArg::Receiver(_) => None,
            })
            .collect();
        let (body, map) = if kind == Kind::Sequence {
            (
                sequence_body(method).expect("checked"),
                &mut named.sequences,
            )
        } else {
            let block = &method.block;
            (quote!(#block), &mut named.properties)
        };
        let named_item = NamedItem {
            name: method.sig.ident.clone(),
            params,
            body,
        };
        map.insert(method.sig.ident.to_string(), named_item);
        false
    });
    // The properties taking time, to a fixed point (a call of one that
    // does takes time too).
    loop {
        let before = named.temporal.len();
        let found: Vec<String> = named
            .properties
            .iter()
            .filter(|(n, it)| {
                !named.temporal.contains(*n) && takes_time(it.body.clone(), &named.temporal)
            })
            .map(|(n, _)| n.clone())
            .collect();
        named.temporal.extend(found);
        if named.temporal.len() == before {
            break;
        }
    }
    named
}

pub(crate) fn assertions_impl(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let names: Punctuated<Ident, Token![,]> = Punctuated::parse_terminated.parse2(attr)?;
    let signals: Vec<String> = names.iter().map(Ident::to_string).collect();
    let mut item: ItemImpl = syn::parse2(item)?;
    if let Some((_, path, _)) = &item.trait_ {
        return Err(syn::Error::new(
            path.span(),
            "`#[assertions]` goes on the block's own impl, not a trait's",
        ));
    }
    let Type::Path(ty) = &*item.self_ty else {
        return Err(syn::Error::new(
            item.self_ty.span(),
            "expected the block's type",
        ));
    };
    let name = ty.path.segments.last().expect("a path").ident.clone();
    let self_ty = item.self_ty.clone();
    let sig = signals_struct(&name);
    let a = assertion_crate();
    let s = signals_param();
    let h = history_param();
    let r = report_param();
    let state_ty = format_ident!("__VeraldState_{}", name);

    // The methods and their kinds, for calls between them.
    let mut kinds: HashMap<String, Kind> = HashMap::new();
    let mut methods: Vec<(Ident, Kind)> = Vec::new();
    for impl_item in &mut item.items {
        if let ImplItem::Fn(method) = impl_item {
            let kind = take_kind(method)?;
            check_signature(method, kind)?;
            kinds.insert(method.sig.ident.to_string(), kind);
            methods.push((method.sig.ident.clone(), kind));
        }
    }
    let named = take_named(&mut item, &kinds);

    let mut generated: Vec<ImplItemFn> = Vec::new();
    let mut items = TokenStream2::new();
    let mut states: Vec<(Ident, Ident, Ident)> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let kind = kinds[&method.sig.ident.to_string()];
        match kind {
            Kind::Assert => {
                let Some(FnArg::Receiver(receiver)) = method.sig.inputs.first() else {
                    unreachable!("checked: a property takes `&self`");
                };
                let sf = receiver.self_token;
                let lowered =
                    property::lower(&name, &sig, &named, sf, &method.sig.ident, &method.block)?;
                let st = &lowered.state;
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(#h: &#a::History<#sig>));
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(__verald_st: &mut #st));
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(#r: &#a::Report<'_>));
                method.block = lowered.block;
                // Arguments of named items are put in parentheses.
                method
                    .attrs
                    .push(syn::parse_quote!(#[allow(unused_parens)]));
                for m in lowered.methods {
                    generated.push(syn::parse2(m)?);
                }
                items.extend(lowered.items);
                states.push((method.sig.ident.clone(), lowered.state, lowered.finish));
            }
            Kind::OnClock | Kind::OnReset => {
                rewrite::lower_checks(&mut method.block, &r)?;
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(#h: &#a::History<#sig>));
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(#r: &#a::Report<'_>));
            }
            Kind::Sequence | Kind::Property => unreachable!("taken out of the impl"),
            Kind::Helper => {
                method
                    .sig
                    .inputs
                    .push(syn::parse_quote!(#h: &#a::History<#sig>));
            }
        }
    }
    let mut depth = 0;
    let all = item
        .items
        .iter_mut()
        .filter_map(|i| match i {
            ImplItem::Fn(m) => Some(m),
            _ => None,
        })
        .chain(generated.iter_mut());
    for method in all {
        depth = depth.max(rewrite::rewrite_names(method, &signals, &kinds)?);
        // The current samples, as the names were rewritten to read them,
        // in a method given them.
        let takes_samples = method.sig.inputs.iter().any(|input| {
            matches!(input, FnArg::Typed(t) if matches!(&*t.pat, syn::Pat::Ident(i) if i.ident == h))
        });
        if takes_samples {
            method
                .block
                .stmts
                .insert(0, syn::parse_quote!(let #s = #a::History::now(#h);));
        }
    }
    item.items.extend(generated.into_iter().map(ImplItem::Fn));

    let checks = states
        .iter()
        .map(|(m, _, _)| quote!(self.#m(#h, &mut __verald_st.#m, #r);));
    let finishes = states
        .iter()
        .map(|(m, _, f)| quote!(self.#f(&mut __verald_st.#m, #r);));
    let state_fields = states.iter().map(|(m, ty, _)| quote!(#m: #ty));
    let on_clocks = methods
        .iter()
        .filter(|(_, k)| *k == Kind::OnClock)
        .map(|(m, _)| quote!(self.#m(#h, #r);));
    let on_resets = methods
        .iter()
        .filter(|(_, k)| *k == Kind::OnReset)
        .map(|(m, _)| quote!(self.#m(#h, #r);));

    Ok(quote! {
        #item

        #items

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(::core::default::Default)]
        pub struct #state_ty {
            #(#state_fields,)*
        }

        impl #a::AssertionBlock for #self_ty {
            type Signals = #sig;
            type State = #state_ty;
            const PAST: usize = #depth;

            #[inline]
            fn check(
                &self,
                __verald_st: &mut #state_ty,
                #h: &#a::History<#sig>,
                #r: &#a::Report<'_>,
            ) {
                #(#checks)*
            }

            #[inline]
            fn on_clock(&mut self, #h: &#a::History<#sig>, #r: &#a::Report<'_>) {
                #(#on_clocks)*
            }

            fn on_reset(&mut self, #h: &#a::History<#sig>, #r: &#a::Report<'_>) {
                #(#on_resets)*
            }

            fn finish(&self, __verald_st: &mut #state_ty, #r: &#a::Report<'_>) {
                #(#finishes)*
            }
        }
    })
}
