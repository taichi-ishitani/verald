//! `#[modport]`, `#[clocking]` and the wait macros.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Expr, Field, Fields, Ident, ItemStruct, Type};

use crate::runtime;

/// The role a field attribute gives a field.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    Output,
    Modport,
}

struct FieldSpec {
    ident: Ident,
    ty: Type,
    role: Role,
    /// The member name the field binds to.
    name: String,
    /// An output's initial value (`#[output(init = ..)]`), driven by `init`.
    init: Option<Expr>,
}

const HIDDEN: &str = "__tbf";

/// Removes the role attribute of each field and returns the fields' specs.
fn take_fields(item: &mut ItemStruct, roles: &[Role]) -> syn::Result<Vec<FieldSpec>> {
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "expected a struct with named fields",
        ));
    };
    let mut specs = Vec::new();
    for field in fields.named.iter_mut() {
        let (role, init) = take_role(field, roles)?;
        let ident = field.ident.clone().expect("named field");
        specs.push(FieldSpec {
            name: ident.unraw().to_string(),
            ident,
            ty: field.ty.clone(),
            role,
            init,
        });
    }
    Ok(specs)
}

/// The initial value of `#[output(init = ..)]`.
struct InitArg {
    expr: Expr,
}

impl Parse for InitArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "init" {
            return Err(syn::Error::new(key.span(), "expected `init = <value>`"));
        }
        input.parse::<syn::Token![=]>()?;
        Ok(InitArg {
            expr: input.parse()?,
        })
    }
}

fn take_role(field: &mut Field, roles: &[Role]) -> syn::Result<(Role, Option<Expr>)> {
    let mut role = None;
    let mut init = None;
    let mut rest = Vec::new();
    for attr in field.attrs.drain(..) {
        let this = [
            ("input", Role::Input),
            ("output", Role::Output),
            ("modport", Role::Modport),
        ]
        .into_iter()
        .find(|(name, role)| attr.path().is_ident(name) && roles.contains(role))
        .map(|(_, role)| role);
        match this {
            Some(this) => {
                if this == Role::Output && matches!(attr.meta, syn::Meta::List(_)) {
                    init = Some(attr.parse_args::<InitArg>()?.expr);
                } else {
                    attr.meta.require_path_only()?;
                }
                if role.replace(this).is_some() {
                    return Err(syn::Error::new(attr.span(), "a field takes one role"));
                }
            }
            None => rest.push(attr),
        }
    }
    field.attrs = rest;
    let role = role.ok_or_else(|| {
        let expected = if roles.contains(&Role::Modport) {
            "`#[modport]`"
        } else {
            "`#[input]` or `#[output]`"
        };
        syn::Error::new(field.span(), format!("the field needs {expected}"))
    })?;
    Ok((role, init))
}

/// Binds the fields' values: an output's initial value where it has one
/// (taken as the value last driven, so it is not driven until `init` or a
/// change), else zeros. `members` holds the bound members.
fn initial_values(specs: &[&FieldSpec]) -> Vec<TokenStream2> {
    specs
        .iter()
        .map(|f| {
            let (ident, ty) = (&f.ident, &f.ty);
            let local = format_ident!("__tbf_v_{}", ident.unraw());
            match &f.init {
                Some(expr) => quote! {
                    let #local: #ty = #expr;
                    members.#ident.assume(&#local);
                },
                None => quote!(let #local = members.#ident.initial();),
            }
        })
        .collect()
}

/// Assigns and drives the initial values of the outputs that have one.
fn init_body(specs: &[&FieldSpec], hidden: &Ident) -> Vec<TokenStream2> {
    specs
        .iter()
        .filter_map(|f| {
            let (ident, ty) = (&f.ident, &f.ty);
            let expr = f.init.as_ref()?;
            Some(quote! {
                self.#ident = { let v: #ty = #expr; v };
                self.#hidden.#ident.force(&self.#ident);
            })
        })
        .collect()
}

fn check_plain(attr: &TokenStream2, item: &ItemStruct, what: &str) -> syn::Result<()> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            attr.span(),
            format!("`#[{what}]` takes no arguments"),
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.span(),
            format!("a {what} struct cannot be generic"),
        ));
    }
    Ok(())
}

/// Adds the hidden field holding the bound members.
fn push_hidden_field(item: &mut ItemStruct, ty: &Ident) {
    let Fields::Named(fields) = &mut item.fields else {
        unreachable!("checked by take_fields");
    };
    let hidden = Ident::new(HIDDEN, Span::call_site());
    fields.named.push(syn::parse_quote! {
        #[doc(hidden)]
        #hidden: #ty
    });
}

/// The name of the method that reads one field of a modport.
fn load_method(field: &str) -> Ident {
    format_ident!("__tbf_load_{}", field)
}

pub(crate) fn modport(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let mut item: ItemStruct = syn::parse2(item)?;
    check_plain(&attr, &item, "modport")?;
    let specs = take_fields(&mut item, &[Role::Input, Role::Output])?;
    let name = item.ident.clone();
    let members = format_ident!("__TbfModport{}", name);
    push_hidden_field(&mut item, &members);
    let hidden = Ident::new(HIDDEN, Span::call_site());

    let rt = runtime();
    let ck = quote!(#rt::clocking);
    let member_fields = specs.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        quote!(#ident: #ck::Member<#ty>)
    });
    let binds = specs.iter().map(|f| {
        let (ident, ty, path) = (&f.ident, &f.ty, &f.name);
        let ctor = if f.role == Role::Input {
            quote!(input)
        } else {
            quote!(output)
        };
        quote!(#ident: #ck::Member::<#ty>::#ctor(bus, #path)?)
    });
    let all: Vec<&FieldSpec> = specs.iter().collect();
    let initials = initial_values(&all);
    let values = specs.iter().map(|f| {
        let ident = &f.ident;
        let local = format_ident!("__tbf_v_{}", ident.unraw());
        quote!(#ident: #local)
    });
    let inits = init_body(&all, &hidden);
    let inputs: Vec<&FieldSpec> = specs.iter().filter(|f| f.role == Role::Input).collect();
    let loads = inputs.iter().map(|f| {
        let ident = &f.ident;
        quote!(self.#ident = self.#hidden.#ident.load();)
    });
    let stores = specs.iter().filter(|f| f.role == Role::Output).map(|f| {
        let ident = &f.ident;
        quote!(self.#hidden.#ident.store(&self.#ident);)
    });
    let load_methods = specs.iter().map(|f| {
        let ident = &f.ident;
        let method = load_method(&f.name);
        let body = if f.role == Role::Input {
            quote!(self.#ident = self.#hidden.#ident.load();)
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

    Ok(quote! {
        #item

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(Debug)]
        struct #members {
            #(#member_fields,)*
        }

        impl #ck::Modport for #name {
            fn __bind(
                bus: &#ck::__private::ModportRef,
            ) -> ::core::result::Result<Self, #ck::__private::TestError> {
                #[allow(unused_mut)]
                let mut members = #members { #(#binds,)* };
                #(#initials)*
                ::core::result::Result::Ok(Self {
                    #(#values,)*
                    #hidden: members,
                })
            }

            fn __init(&mut self) {
                #(#inits)*
            }

            fn __load(&mut self) {
                #(#loads)*
            }

            fn __store(&mut self) {
                #(#stores)*
            }
        }

        impl #name {
            #(#load_methods)*
        }
    })
}

pub(crate) fn clocking(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let mut item: ItemStruct = syn::parse2(item)?;
    check_plain(&attr, &item, "clocking")?;
    let specs = take_fields(&mut item, &[Role::Input, Role::Output, Role::Modport])?;
    let name = item.ident.clone();
    let vis = item.vis.clone();
    let state = format_ident!("__TbfClocking{}", name);
    push_hidden_field(&mut item, &state);
    let hidden = Ident::new(HIDDEN, Span::call_site());

    let rt = runtime();
    let ck = quote!(#rt::clocking);
    let signals: Vec<&FieldSpec> = specs.iter().filter(|f| f.role != Role::Modport).collect();
    let state_fields = signals.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        quote!(#ident: #ck::Member<#ty>)
    });
    let paths = format_ident!("{}Paths", name);
    let path_fields = specs.iter().map(|f| {
        let ident = &f.ident;
        let doc = format!("Path of `{}`; defaults to `\"{}\"`.", f.name, f.name);
        quote!(#[doc = #doc] pub #ident: ::core::option::Option<&'a str>)
    });
    let path_of = |f: &FieldSpec| {
        let (ident, default) = (&f.ident, &f.name);
        quote!(paths.#ident.unwrap_or(#default))
    };
    let member_binds = signals.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        let path = path_of(f);
        let (ctor, lookup) = if f.role == Role::Input {
            (quote!(input_port), quote!(input))
        } else {
            (quote!(output_port), quote!(output))
        };
        quote! {
            #ident: {
                let path = #path;
                #ck::Member::<#ty>::#ctor(
                    #ck::__private::Signals::#lookup(signals, path)?,
                    path,
                )?
            }
        }
    });
    let signal_initials = initial_values(&signals);
    let signal_inits = init_body(&signals, &hidden);
    let modport_inits = specs.iter().filter(|f| f.role == Role::Modport).map(|f| {
        let ident = &f.ident;
        quote!(#ck::Modport::__init(&mut self.#ident);)
    });
    let binds = specs.iter().map(|f| {
        let (ident, ty) = (&f.ident, &f.ty);
        if f.role == Role::Modport {
            let path = path_of(f);
            quote! {
                #ident: <#ty as #ck::Modport>::__bind(
                    &#ck::__private::Signals::interface(signals, #path)?,
                )?
            }
        } else {
            let local = format_ident!("__tbf_v_{}", ident.unraw());
            quote!(#ident: #local)
        }
    });
    let paths_doc = format!(
        "Paths the fields of [`{name}`] bind to, among the interfaces and signals \
         the test module registered; a field left `None` binds to its own name. A \
         signal's path may name an interface's member (`\"bus.done\"`)."
    );
    let loads = specs.iter().filter_map(|f| {
        let ident = &f.ident;
        match f.role {
            Role::Modport => Some(quote!(#ck::Modport::__load(&mut self.#ident);)),
            Role::Input => Some(quote!(self.#ident = self.#hidden.#ident.load();)),
            Role::Output => None,
        }
    });
    let stores = specs.iter().filter_map(|f| {
        let ident = &f.ident;
        match f.role {
            Role::Modport => Some(quote!(#ck::Modport::__store(&mut self.#ident);)),
            Role::Output => Some(quote!(self.#hidden.#ident.store(&self.#ident);)),
            Role::Input => None,
        }
    });
    let load_methods = specs.iter().map(|f| {
        let ident = &f.ident;
        let method = load_method(&f.name);
        let body = match f.role {
            Role::Modport => quote!(#ck::Modport::__load(&mut self.#ident);),
            Role::Input => quote!(self.#ident = self.#hidden.#ident.load();),
            Role::Output => quote!(),
        };
        quote! {
            #[doc(hidden)]
            #[inline]
            pub fn #method(&mut self) {
                #body
            }
        }
    });

    Ok(quote! {
        #item

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(Debug)]
        struct #state {
            clock: #ck::__private::Clock,
            #(#state_fields,)*
        }

        #[doc = #paths_doc]
        #[derive(Clone, Copy, Debug, Default)]
        #vis struct #paths<'a> {
            #(#path_fields,)*
            #[doc(hidden)]
            pub __marker: ::core::marker::PhantomData<&'a ()>,
        }

        impl #name {
            /// Binds the fields among the interfaces and signals `signals`
            /// holds, at the paths `paths` gives or their own names, to be
            /// synchronized with the edges of `clock`. Fails if a signal is
            /// missing, has the wrong direction or does not fit its field's
            /// type.
            #vis fn new(
                signals: &(impl #ck::__private::Signals + ?::core::marker::Sized),
                clock: #ck::__private::Clock,
                paths: #paths<'_>,
            ) -> ::core::result::Result<Self, #ck::__private::TestError> {
                #[allow(unused_mut)]
                let mut members = #state { clock, #(#member_binds,)* };
                #(#signal_initials)*
                ::core::result::Result::Ok(Self {
                    #(#binds,)*
                    #hidden: members,
                })
            }

            #(#load_methods)*

            /// Assigns the outputs that declare an initial value
            /// (`#[output(init = ..)]`) their value and drives them now,
            /// whatever was last driven: at a reset, to bring the bus to
            /// idle.
            #vis fn init(&mut self) {
                #(#signal_inits)*
                #(#modport_inits)*
            }

            /// The clock the block is synchronized with.
            #vis fn clock(&self) -> #ck::__private::Clock {
                self.#hidden.clock
            }

            /// Drives the outputs changed since the last tick, waits for the
            /// clock's next edge, and samples every input.
            #vis async fn tick(&mut self) {
                #ck::tick(self).await
            }

            /// Drives the outputs changed since the last tick now, without
            /// waiting for an edge: in a hook they reach the design when it
            /// ends (at a reset, to bring a bus to idle).
            #vis fn drive(&mut self) {
                #ck::drive(self)
            }

            /// Waits until `f` goes from false to true at a clock edge.
            #vis fn posedge<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::posedge(self, f)
            }

            /// Waits until `f` goes from true to false at a clock edge.
            #vis fn negedge<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::negedge(self, f)
            }

            /// Waits until `f` changes at a clock edge, either way.
            #vis fn edge<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::edge(self, f)
            }

            /// Waits until the value of `f` changes at a clock edge.
            #vis fn changed<'a, T: ::core::cmp::PartialEq + 'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> T + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::changed(self, f)
            }

            /// Waits until `f` holds; returns at once if it already does.
            #vis fn wait<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::wait(self, f)
            }

            /// Ticks, and goes on ticking until `f` holds at an edge
            /// (`@(cb iff f)`): at least one tick.
            #vis fn tick_until<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::tick_until(self, f)
            }

            /// Ticks, and goes on ticking while `f` holds at an edge
            /// (`do @(cb); while (f)`): at least one tick.
            #vis fn tick_while<'a>(
                &'a mut self,
                f: impl FnMut(&Self) -> bool + 'a,
            ) -> #ck::Wait<'a, Self> {
                #ck::waits::tick_while(self, f)
            }
        }

        impl #ck::Clocking for #name {
            type Paths<'a> = #paths<'a>;

            fn bind<S: #ck::__private::Signals + ?::core::marker::Sized>(
                signals: &S,
                clock: #ck::__private::Clock,
                paths: #paths<'_>,
            ) -> ::core::result::Result<Self, #ck::__private::TestError> {
                Self::new(signals, clock, paths)
            }
        }

        impl<'a> #ck::ClockingPaths for #paths<'a> {
            type Block = #name;

            fn bind<S: #ck::__private::Signals + ?::core::marker::Sized>(
                self,
                signals: &S,
                clock: #ck::__private::Clock,
            ) -> ::core::result::Result<#name, #ck::__private::TestError> {
                #name::new(signals, clock, self)
            }
        }

        impl #ck::ClockingBlock for #name {
            fn __clock(&self) -> #ck::__private::Clock {
                self.#hidden.clock
            }

            fn __load(&mut self) {
                #(#loads)*
            }

            fn __store(&mut self) {
                #(#stores)*
            }
        }
    })
}

/// The argument of a wait macro: an expression rooted at the clocking block
/// variable, then, for the `_within` forms, the edge limit.
struct WaitArgs {
    expr: Expr,
    edges: Option<Expr>,
}

impl Parse for WaitArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let expr: Expr = input.parse()?;
        let edges = if input.is_empty() {
            None
        } else {
            input.parse::<syn::Token![,]>()?;
            Some(input.parse()?)
        };
        if !input.is_empty() {
            return Err(input.error("expected `(expression)` or `(expression, edges)`"));
        }
        Ok(WaitArgs { expr, edges })
    }
}

/// The variable an expression over a clocking block starts from: `cb` in
/// `cb.bus.done`, `cb.bus.cnt % 4 == 0` or `(cb.bus.a, cb.bus.b)`.
fn root_ident(expr: &Expr) -> Option<Ident> {
    match expr {
        Expr::Path(p) => p.path.get_ident().cloned(),
        Expr::Field(x) => root_ident(&x.base),
        Expr::MethodCall(x) => root_ident(&x.receiver),
        Expr::Index(x) => root_ident(&x.expr),
        Expr::Binary(x) => root_ident(&x.left),
        Expr::Unary(x) => root_ident(&x.expr),
        Expr::Paren(x) => root_ident(&x.expr),
        Expr::Cast(x) => root_ident(&x.expr),
        Expr::Tuple(x) => x.elems.first().and_then(root_ident),
        _ => None,
    }
}

/// What a condition reads of the clocking block: the members it names
/// (`cb.bus.done`: modport `bus`, field `done`), whole modports
/// (`cb.bus.is_idle()`), or, when it uses the block itself, everything.
#[derive(Default)]
struct Reads {
    cb: Option<Ident>,
    fields: Vec<(Ident, Ident)>,
    modports: Vec<Ident>,
    everything: bool,
}

impl Reads {
    /// The member chain of a field access rooted at the block: `[bus, done]`
    /// for `cb.bus.done`.
    fn chain(&self, expr: &Expr) -> Option<Vec<Ident>> {
        match expr {
            Expr::Path(p) => {
                let ident = p.path.get_ident()?;
                (Some(ident) == self.cb.as_ref()).then(Vec::new)
            }
            Expr::Field(x) => {
                let mut chain = self.chain(&x.base)?;
                match &x.member {
                    syn::Member::Named(ident) => chain.push(ident.clone()),
                    syn::Member::Unnamed(_) => return None,
                }
                Some(chain)
            }
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for Reads {
    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        match self.chain(&Expr::Field(node.clone())) {
            Some(chain) if chain.len() >= 2 => {
                let pair = (chain[0].clone(), chain[1].clone());
                if !self.fields.contains(&pair) {
                    self.fields.push(pair);
                }
            }
            Some(chain) => {
                if !self.modports.contains(&chain[0]) {
                    self.modports.push(chain[0].clone());
                }
            }
            None => syn::visit::visit_expr_field(self, node),
        }
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if node.qself.is_none() && node.path.get_ident() == self.cb.as_ref() {
            self.everything = true;
        }
    }
}

/// Renames the clocking block variable to the closure argument.
struct Rename {
    from: Ident,
    to: Ident,
}

impl VisitMut for Rename {
    fn visit_expr_path_mut(&mut self, p: &mut syn::ExprPath) {
        if p.qself.is_none() && p.path.is_ident(&self.from) {
            p.path = self.to.clone().into();
        }
    }
}

/// The expression as written, for messages: `cb.bus.done`, not the
/// token-spaced `cb . bus . done`.
fn expr_text(expr: &Expr) -> String {
    let mut text = quote!(#expr).to_string();
    for (spaced, tight) in [
        (" . ", "."),
        (" (", "("),
        ("( ", "("),
        (" )", ")"),
        (" [", "["),
        ("[ ", "["),
        (" ]", "]"),
        (" ,", ","),
    ] {
        text = text.replace(spaced, tight);
    }
    text
}

pub(crate) fn wait(method: &str, input: TokenStream2, within: bool) -> syn::Result<TokenStream2> {
    let WaitArgs { mut expr, edges } = syn::parse2(input)?;
    match (&edges, within) {
        (None, true) => {
            return Err(syn::Error::new(
                expr.span(),
                "a `_within` wait macro takes `(expression, edges)`",
            ));
        }
        (Some(edges), false) => {
            return Err(syn::Error::new(
                edges.span(),
                "a wait macro takes one expression; to limit the wait, use the `_within` \
                 form (`posedge_within!(cb.bus.done, 8)`)",
            ));
        }
        _ => {}
    }
    let cb = root_ident(&expr).ok_or_else(|| {
        syn::Error::new(
            expr.span(),
            "the expression must start from the clocking block variable, e.g. `cb.bus.done`",
        )
    })?;
    let label = expr_text(&expr);

    let mut reads = Reads {
        cb: Some(cb.clone()),
        ..Reads::default()
    };
    reads.visit_expr(&expr);

    let param = Ident::new("__tbf_cb", Span::mixed_site());
    Rename {
        from: cb.clone(),
        to: param.clone(),
    }
    .visit_expr_mut(&mut expr);

    let sampling = if reads.everything {
        quote!()
    } else {
        let fields = reads
            .fields
            .iter()
            .filter(|(modport, _)| !reads.modports.contains(modport))
            .map(|(modport, field)| {
                let method = load_method(&field.unraw().to_string());
                quote!(#param.#modport.#method();)
            });
        let modports = reads.modports.iter().map(|field| {
            let method = load_method(&field.unraw().to_string());
            quote!(#param.#method();)
        });
        quote!(.sampling(move |#param| { #(#fields)* #(#modports)* }))
    };
    let method = Ident::new(method, Span::call_site());
    let limit = match edges {
        Some(edges) => quote!(.within(#edges).label(#label)),
        None => quote!(),
    };
    Ok(quote! {
        #cb.#method(move |#param| #expr)#sampling #limit.await
    })
}
