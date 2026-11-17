//! `#[component(..)]` and `#[sequence(..)]`.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Field, Fields, Ident, ImplItem, Item, ItemImpl, ItemStruct, Meta, Token, Type};

use crate::core;

const KINDS: &[(&str, &str)] = &[
    ("component", "Component"),
    ("test", "Test"),
    ("env", "Env"),
    ("agent", "Agent"),
    ("driver", "Driver"),
    ("monitor", "Monitor"),
    ("sequencer", "Sequencer"),
    ("scoreboard", "Scoreboard"),
    ("subscriber", "Subscriber"),
];

fn kind_variant(name: &Ident) -> syn::Result<Ident> {
    KINDS
        .iter()
        .find(|(k, _)| name == k)
        .map(|(_, v)| Ident::new(v, name.span()))
        .ok_or_else(|| {
            let kinds: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
            syn::Error::new(
                name.span(),
                format!("unknown kind `{name}`; one of {}", kinds.join(", ")),
            )
        })
}

/// The attribute's arguments: a kind, `passive`, or a generator
/// (`sequencer(item = ..)`).
struct Args {
    kind: Option<Ident>,
    passive: bool,
    sequencer: Option<SequencerArgs>,
    agent: Option<AgentArgs>,
    test: Option<TestArgs>,
}

/// `test(base = B, pre_run = P, run = R, post_run = Q)`: a test made from a
/// base test and scenarios.
struct TestArgs {
    base: Type,
    pre_run: Option<Type>,
    run: Option<Type>,
    post_run: Option<Type>,
}

struct SequencerArgs {
    item: Type,
    response: Option<Type>,
    request: Option<Type>,
    /// The configuration's field holding a `DefaultSequence`.
    default_sequence: Option<Ident>,
}

struct AgentArgs {
    monitor: Type,
    sequencer: Type,
    driver: Type,
    reactive: bool,
    post_build: bool,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        let mut args = Args {
            kind: None,
            passive: false,
            sequencer: None,
            agent: None,
            test: None,
        };
        for meta in metas {
            match meta {
                Meta::Path(path) if path.is_ident("passive") => args.passive = true,
                Meta::Path(path) => {
                    let ident = path
                        .get_ident()
                        .cloned()
                        .ok_or_else(|| syn::Error::new(path.span(), "expected a kind"))?;
                    kind_variant(&ident)?;
                    if args.kind.replace(ident).is_some() {
                        return Err(syn::Error::new(path.span(), "one kind only"));
                    }
                }
                Meta::List(list) if list.path.is_ident("sequencer") => {
                    let mut item = None;
                    let mut response = None;
                    let mut request = None;
                    let mut default_sequence = None;
                    list.parse_nested_meta(|nested| {
                        if nested.path.is_ident("item") {
                            item = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("response") {
                            response = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("request") {
                            request = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("default_sequence") {
                            default_sequence = Some(nested.value()?.parse::<Ident>()?);
                        } else {
                            return Err(nested.error(
                                "expected `item = ..`, `response = ..`, `request = ..` \
                                 or `default_sequence = ..`",
                            ));
                        }
                        Ok(())
                    })?;
                    let item =
                        item.ok_or_else(|| syn::Error::new(list.span(), "`item = ..` is needed"))?;
                    args.kind = Some(Ident::new("sequencer", list.path.span()));
                    args.sequencer = Some(SequencerArgs {
                        item,
                        response,
                        request,
                        default_sequence,
                    });
                }
                Meta::List(list) if list.path.is_ident("agent") => {
                    let (mut monitor, mut sequencer, mut driver) = (None, None, None);
                    let mut reactive = false;
                    let mut post_build = false;
                    list.parse_nested_meta(|nested| {
                        if nested.path.is_ident("monitor") {
                            monitor = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("sequencer") {
                            sequencer = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("driver") {
                            driver = Some(nested.value()?.parse::<Type>()?);
                        } else if nested.path.is_ident("reactive") {
                            reactive = true;
                        } else if nested.path.is_ident("post_build") {
                            post_build = true;
                        } else {
                            return Err(nested.error(
                                "expected `monitor = ..`, `sequencer = ..`, `driver = ..`, \
                                 `reactive` or `post_build`",
                            ));
                        }
                        Ok(())
                    })?;
                    let need = |t: Option<Type>, what: &str| {
                        t.ok_or_else(|| {
                            syn::Error::new(list.span(), format!("`{what} = ..` is needed"))
                        })
                    };
                    args.kind = Some(Ident::new("agent", list.path.span()));
                    args.agent = Some(AgentArgs {
                        monitor: need(monitor, "monitor")?,
                        sequencer: need(sequencer, "sequencer")?,
                        driver: need(driver, "driver")?,
                        reactive,
                        post_build,
                    });
                }
                Meta::List(list) if list.path.is_ident("test") => {
                    let (mut base, mut pre_run, mut run, mut post_run) = (None, None, None, None);
                    list.parse_nested_meta(|nested| {
                        let slot = if nested.path.is_ident("base") {
                            &mut base
                        } else if nested.path.is_ident("pre_run") {
                            &mut pre_run
                        } else if nested.path.is_ident("run") {
                            &mut run
                        } else if nested.path.is_ident("post_run") {
                            &mut post_run
                        } else {
                            return Err(nested.error(
                                "expected `base = ..`, `pre_run = ..`, `run = ..` or `post_run = ..`",
                            ));
                        };
                        *slot = Some(nested.value()?.parse::<Type>()?);
                        Ok(())
                    })?;
                    let base =
                        base.ok_or_else(|| syn::Error::new(list.span(), "`base = ..` is needed"))?;
                    args.kind = Some(Ident::new("test", list.path.span()));
                    args.test = Some(TestArgs {
                        base,
                        pre_run,
                        run,
                        post_run,
                    });
                }
                other => return Err(syn::Error::new(other.span(), "unexpected argument")),
            }
        }
        Ok(args)
    }
}

pub(crate) fn component(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let args: Args = syn::parse2(attr)?;
    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => on_struct(args, item),
        Item::Impl(item) => on_impl(args, item),
        other => Err(syn::Error::new(
            other.span(),
            "`#[component]` goes on a component's struct and on its `impl Component`",
        )),
    }
}

/// Removes `#[context]` from the fields; returns the field it marked.
fn take_context(item: &mut ItemStruct) -> syn::Result<Option<(Ident, Type)>> {
    let mut found = None;
    if let Fields::Named(fields) = &mut item.fields {
        for field in fields.named.iter_mut() {
            let before = field.attrs.len();
            field.attrs.retain(|a| !a.path().is_ident("context"));
            if field.attrs.len() != before {
                let ident = field.ident.clone().expect("named");
                if found.replace((ident, field.ty.clone())).is_some() {
                    return Err(syn::Error::new(field.span(), "one `#[context]` field only"));
                }
            }
        }
    }
    Ok(found)
}

/// `HasContext` for the `#[context]` field, an `InstanceContext<C, S>` (or an
/// alias of one).
fn has_context(name: &Ident, context: &Option<(Ident, Type)>) -> syn::Result<TokenStream2> {
    let Some((field, ty)) = context else {
        return Ok(quote!());
    };
    let core = core();
    let parts = quote!(<#ty as #core::__private::ContextParts>);
    Ok(quote! {
        impl #core::HasContext for #name {
            type Config = #parts::Config;
            type Status = #parts::Status;
            fn context(&self) -> &#core::InstanceContext<Self::Config, Self::Status> {
                &self.#field
            }
        }
    })
}

fn on_struct(args: Args, mut item: ItemStruct) -> syn::Result<TokenStream2> {
    if args.passive {
        return Err(syn::Error::new(
            Span::call_site(),
            "`passive` goes on the `impl Component`, where `run` is",
        ));
    }
    let kind = args
        .kind
        .clone()
        .ok_or_else(|| syn::Error::new(item.ident.span(), "`#[component(..)]` needs a kind"))?;
    let variant = kind_variant(&kind)?;
    let core = core();
    let name = item.ident.clone();
    let context = take_context(&mut item)?;
    let has_context = has_context(&name, &context)?;
    let ports = port_impls(&name, &mut item)?;
    let declared = quote! {
        impl #core::__private::DeclaredKind for #name {
            const KIND: #core::Kind = #core::Kind::#variant;
        }
    };
    if let Some(agent) = args.agent {
        return agent_struct(item, agent, declared, quote!(#has_context #ports));
    }
    if let Some(test) = args.test {
        return test_struct(item, test, declared);
    }
    // A test is the target of its own specs.
    let spec_target = (kind == "test").then(|| {
        quote! {
            impl #core::__private::SpecTarget for #name {
                type Target = Self;
                fn __target(&mut self) -> &mut Self {
                    self
                }
                fn __config(
                    config: &mut <Self as #core::Component>::Context,
                ) -> &mut <Self as #core::Component>::Context {
                    config
                }
            }
        }
    });
    match args.sequencer {
        None => Ok(quote! {
            #item
            #declared
            #has_context
            #ports
            #spec_target
        }),
        Some(sequencer) => sequencer_struct(item, sequencer, declared, quote!(#has_context #ports)),
    }
}

/// The type arguments of a field's type: `[T]` of `AnalysisPort<T>`.
fn type_args(ty: &Type) -> Vec<Type> {
    let Type::Path(p) = ty else { return Vec::new() };
    let Some(seg) = p.path.segments.last() else {
        return Vec::new();
    };
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a
            .args
            .iter()
            .filter_map(|a| match a {
                syn::GenericArgument::Type(t) => Some(t.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Removes the marker `attr` from the fields; returns the field it marked.
fn take_marked(item: &mut ItemStruct, attr: &str) -> syn::Result<Option<(Ident, Type)>> {
    let mut found = None;
    if let Fields::Named(fields) = &mut item.fields {
        for field in fields.named.iter_mut() {
            let before = field.attrs.len();
            field.attrs.retain(|a| !a.path().is_ident(attr));
            if field.attrs.len() != before {
                let ident = field.ident.clone().expect("named");
                if found.replace((ident, field.ty.clone())).is_some() {
                    return Err(syn::Error::new(
                        field.span(),
                        format!("one `#[{attr}]` field only"),
                    ));
                }
            }
        }
    }
    Ok(found)
}

/// `MonitorPort`, `RequestPort` and `DriverPort` from the fields marked
/// `#[analysis_port]`, `#[request_port]` and `#[seq_item_port]`.
fn port_impls(name: &Ident, item: &mut ItemStruct) -> syn::Result<TokenStream2> {
    let core = core();
    let mut out = TokenStream2::new();
    if let Some((field, ty)) = take_marked(item, "analysis_port")? {
        let t = type_args(&ty).into_iter().next().ok_or_else(|| {
            syn::Error::new(ty.span(), "an `#[analysis_port]` is an `AnalysisPort<T>`")
        })?;
        out.extend(quote! {
            impl #core::MonitorPort for #name {
                type Item = #t;
                fn analysis_port(&self) -> #core::AnalysisPort<#t> {
                    self.#field.clone()
                }
            }
        });
    }
    if let Some((field, ty)) = take_marked(item, "request_port")? {
        let t = type_args(&ty).into_iter().next().ok_or_else(|| {
            syn::Error::new(ty.span(), "a `#[request_port]` is an `AnalysisPort<T>`")
        })?;
        out.extend(quote! {
            impl #core::RequestPort for #name {
                type Request = #t;
                fn request_port(&self) -> #core::AnalysisPort<#t> {
                    self.#field.clone()
                }
            }
        });
    }
    if let Some((field, ty)) = take_marked(item, "seq_item_port")? {
        let mut args = type_args(&ty).into_iter();
        let req = args.next().ok_or_else(|| {
            syn::Error::new(
                ty.span(),
                "a `#[seq_item_port]` is a `SeqItemPort<Req, Rsp>`",
            )
        })?;
        let rsp = args.next().unwrap_or_else(|| req.clone());
        out.extend(quote! {
            impl #core::DriverPort for #name {
                type Item = #req;
                type Response = #rsp;
                fn seq_item_port(&mut self) -> &mut #core::SeqItemPort<#req, #rsp> {
                    &mut self.#field
                }
            }
        });
    }
    Ok(out)
}

/// An agent, whole: its parts, its `Component` (making the driver when
/// active, the sequencer when active or asked for when passive, and the
/// connections), and accessors. The struct's own fields start at their
/// defaults; `post_build` calls the agent's `post_build(&mut self, scope)`
/// to make more.
fn agent_struct(
    mut item: ItemStruct,
    args: AgentArgs,
    declared: TokenStream2,
    extra: TokenStream2,
) -> syn::Result<TokenStream2> {
    let core = core();
    let name = item.ident.clone();
    let vis = item.vis.clone();
    let AgentArgs {
        monitor,
        sequencer,
        driver,
        reactive,
        post_build,
    } = args;
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "an agent is a struct with named fields",
        ));
    };
    let user: Vec<Ident> = fields
        .named
        .iter()
        .map(|f| f.ident.clone().expect("named"))
        .collect();
    fields.named.push(syn::parse_quote! {
        /// The monitor, always there.
        pub monitor: #core::Instance<#monitor>
    });
    fields.named.push(syn::parse_quote! {
        /// The sequencer, when active (or kept when passive).
        pub sequencer: #core::OptInstance<#sequencer>
    });
    fields.named.push(syn::parse_quote! {
        /// The driver, when active.
        pub driver: #core::OptInstance<#driver>
    });
    fields.named.push(syn::parse_quote! {
        #[doc(hidden)]
        __tbf_context: <#monitor as #core::Component>::Context
    });
    let reactive_connect = reactive.then(|| {
        quote! {
            // Only when active, as tue: a passive agent's sequencer has no
            // driver to answer through.
            if let (true, ::core::option::Option::Some(sequencer)) = (active, &sequencer) {
                let fifo = #core::ReactiveSequencer::__requests(&**sequencer).clone();
                #core::RequestPort::request_port(&*monitor).connect_export(&fifo.export());
            }
        }
    });
    let post_build = post_build.then(|| quote!(agent.post_build(scope)?;));
    Ok(quote! {
        #item
        #declared
        #extra

        impl #core::Component for #name {
            type Context = <#monitor as #core::Component>::Context;
            const KIND: #core::Kind = #core::Kind::Agent;
            const PASSIVE: bool = true;

            fn build(
                scope: &#core::Scope,
                context: Self::Context,
            ) -> ::core::result::Result<Self, #core::__private::TestError> {
                use #core::__private::AgentContext as _;
                let active = context.active();
                let monitor = scope.create_instance::<#monitor>("monitor", context.clone())?;
                let sequencer = scope.create_instance_if::<#sequencer>(
                    active || context.passive_sequencer(),
                    "sequencer",
                    context.clone(),
                )?;
                let mut driver =
                    scope.create_instance_if::<#driver>(active, "driver", context.clone())?;
                if let (
                    ::core::option::Option::Some(driver),
                    ::core::option::Option::Some(sequencer),
                ) = (&mut driver, &sequencer)
                {
                    #core::DriverPort::seq_item_port(&mut **driver).connect(&sequencer.handle());
                }
                #reactive_connect
                #[allow(unused_mut)]
                let mut agent = #name {
                    #(#user: ::core::default::Default::default(),)*
                    monitor,
                    sequencer,
                    driver,
                    __tbf_context: context,
                };
                #post_build
                ::core::result::Result::Ok(agent)
            }
        }

        impl #name {
            /// The sequencer's handle, for sequences to start on. Panics on a
            /// passive agent with no sequencer.
            #vis fn sequencer_handle(&self) -> #core::SequencerHandle<#sequencer> {
                self.sequencer
                    .as_ref()
                    .expect("a passive agent has no sequencer")
                    .handle()
            }

            /// The monitor's analysis port.
            #vis fn analysis_port(
                &self,
            ) -> #core::AnalysisPort<<#monitor as #core::MonitorPort>::Item> {
                #core::MonitorPort::analysis_port(&*self.monitor)
            }

            /// The agent's context, which its parts share.
            #vis fn context(&self) -> &<#monitor as #core::Component>::Context {
                &self.__tbf_context
            }
        }
    })
}

/// A sequencer, whole: its core, `Sequencer`, and its `Component`, built
/// from its fields (one field: that field; several: a tuple, in order).
fn sequencer_struct(
    mut item: ItemStruct,
    args: SequencerArgs,
    declared: TokenStream2,
    has_context: TokenStream2,
) -> syn::Result<TokenStream2> {
    let core = core();
    let name = item.ident.clone();
    let SequencerArgs {
        item: req,
        response,
        request,
        default_sequence,
    } = args;
    // The default sequence its configuration carries, read through its
    // `#[context]`.
    let default = match &default_sequence {
        Some(field) => quote! {
            ::core::option::Option::Some(
                #core::HasContext::context(&*this.borrow()).config().#field.clone(),
            )
        },
        None => quote!(::core::option::Option::None),
    };
    let rsp = response.map_or(quote!(#req), |r| quote!(#r));
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "a sequencer is a struct with named fields",
        ));
    };
    let user: Vec<Field> = fields.named.iter().cloned().collect();
    fields.named.push(syn::parse_quote! {
        #[doc(hidden)]
        __tbf_sequencer: #core::__private::SequencerCore<#req, #rsp>
    });
    let (request_init, reactive) = match &request {
        Some(request) => {
            fields.named.push(syn::parse_quote! {
                #[doc(hidden)]
                __tbf_requests: #core::AnalysisFifo<#request>
            });
            (
                quote!(__tbf_requests: #core::AnalysisFifo::new(),),
                quote! {
                    impl #core::ReactiveSequencer for #name {
                        type Request = #request;
                        fn __requests(&self) -> &#core::AnalysisFifo<#request> {
                            &self.__tbf_requests
                        }
                    }
                },
            )
        }
        None => (quote!(), quote!()),
    };
    let idents: Vec<&Ident> = user.iter().map(|f| f.ident.as_ref().unwrap()).collect();
    let types: Vec<&Type> = user.iter().map(|f| &f.ty).collect();
    let (context_ty, pattern) = match idents.len() {
        0 => (quote!(()), quote!(())),
        1 => {
            let (t, i) = (types[0], idents[0]);
            (quote!(#t), quote!(#i))
        }
        _ => (quote!((#(#types,)*)), quote!((#(#idents,)*))),
    };
    let unbound = user.iter().filter_map(|f| {
        let ident = f.ident.as_ref().unwrap();
        let text = ident.to_string();
        let last = match &f.ty {
            Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        (last.as_deref() == Some("SequencerHandle")).then(|| {
            quote! {
                if !self.#ident.is_bound() {
                    unbound.push(#text);
                }
            }
        })
    });
    Ok(quote! {
        #item
        #declared
        #has_context

        #reactive

        impl #core::Sequencer for #name {
            type Item = #req;
            type Response = #rsp;
            fn __core(&self) -> &#core::__private::SequencerCore<#req, #rsp> {
                &self.__tbf_sequencer
            }
        }

        impl #core::Component for #name {
            type Context = #context_ty;
            const KIND: #core::Kind = #core::Kind::Sequencer;
            const PASSIVE: bool = true;

            fn build(
                _scope: &#core::Scope,
                #pattern: #context_ty,
            ) -> ::core::result::Result<Self, #core::__private::TestError> {
                ::core::result::Result::Ok(#name {
                    #(#idents,)*
                    __tbf_sequencer: ::core::default::Default::default(),
                    #request_init
                })
            }

            fn __attach(
                this: &::std::rc::Rc<::core::cell::RefCell<Self>>,
                path: &::std::rc::Rc<str>,
            ) {
                #core::__private::attach_sequencer(this, path, #default);
            }

            fn __default_task(&mut self) -> ::core::option::Option<#core::__private::TaskFuture> {
                self.__tbf_sequencer.take_default()
            }

            fn __unbound(&self) -> ::std::vec::Vec<&'static str> {
                #[allow(unused_mut)]
                let mut unbound = ::std::vec::Vec::new();
                #(#unbound)*
                unbound
            }
        }
    })
}

/// A test made from a base test and scenarios: the base in a field, and a
/// `Component` and `Test` forwarding to it, except for the steps given a
/// scenario. The struct's own fields start at their defaults.
fn test_struct(
    mut item: ItemStruct,
    args: TestArgs,
    declared: TokenStream2,
) -> syn::Result<TokenStream2> {
    let core = core();
    let name = item.ident.clone();
    let TestArgs {
        base,
        pre_run,
        run,
        post_run,
    } = args;
    let user: Vec<Ident> = match &mut item.fields {
        Fields::Named(fields) => {
            let user = fields
                .named
                .iter()
                .map(|f| f.ident.clone().expect("named"))
                .collect();
            fields.named.push(syn::parse_quote! {
                /// The base test, which the steps without a scenario run.
                pub base: #base
            });
            user
        }
        _ => {
            return Err(syn::Error::new(
                item.span(),
                "a test made from a base is a struct with named fields (`{}` when none)",
            ));
        }
    };
    let step = |scenario: Option<Type>, fallback: TokenStream2| match scenario {
        Some(scenario) => quote! {
            #core::Scenario::body(
                &mut <#scenario as ::core::default::Default>::default(),
                &mut self.base,
                run_context,
            )
            .await
        },
        None => fallback,
    };
    let pre_run = step(
        pre_run,
        quote!(#core::Test::pre_run(&mut self.base, run_context).await),
    );
    let run = step(
        run,
        quote!(#core::Component::run(&mut self.base, run_context).await),
    );
    let post_run = step(
        post_run,
        quote!(#core::Test::post_run(&mut self.base, run_context).await),
    );
    Ok(quote! {
        #item
        #declared

        impl #core::Component for #name {
            type Context = <#base as #core::Component>::Context;
            const KIND: #core::Kind = #core::Kind::Test;

            fn build(
                scope: &#core::Scope,
                context: Self::Context,
            ) -> ::core::result::Result<Self, #core::__private::TestError> {
                ::core::result::Result::Ok(#name {
                    #(#user: ::core::default::Default::default(),)*
                    base: <#base as #core::Component>::build(scope, context)?,
                })
            }

            async fn run(
                &mut self,
                run_context: &mut #core::RunContext,
            ) -> ::core::result::Result<(), #core::__private::TestError> {
                #run
            }

            fn check(&mut self, check_context: &mut #core::CheckContext) {
                #core::Component::check(&mut self.base, check_context)
            }

            fn report(&self, report_context: &#core::ReportContext) {
                #core::Component::report(&self.base, report_context)
            }

            fn __default_task(
                &mut self,
            ) -> ::core::option::Option<#core::__private::TaskFuture> {
                #core::Component::__default_task(&mut self.base)
            }

            fn __unbound(&self) -> ::std::vec::Vec<&'static str> {
                #core::Component::__unbound(&self.base)
            }
        }

        impl #core::Test for #name {
            fn setup(
                setup_context: &mut #core::SetupContext,
            ) -> ::core::result::Result<Self::Context, #core::__private::TestError> {
                <#base as #core::Test>::setup(setup_context)
            }

            async fn pre_run(
                &mut self,
                run_context: &mut #core::RunContext,
            ) -> ::core::result::Result<(), #core::__private::TestError> {
                #pre_run
            }

            async fn post_run(
                &mut self,
                run_context: &mut #core::RunContext,
            ) -> ::core::result::Result<(), #core::__private::TestError> {
                #post_run
            }
        }

        /// Its specs are written for the base.
        impl #core::__private::SpecTarget for #name {
            type Target = <#base as #core::__private::SpecTarget>::Target;
            fn __target(&mut self) -> &mut Self::Target {
                #core::__private::SpecTarget::__target(&mut self.base)
            }
            fn __config(
                config: &mut <Self as #core::Component>::Context,
            ) -> &mut <Self::Target as #core::Component>::Context {
                <#base as #core::__private::SpecTarget>::__config(config)
            }
        }

        impl ::core::ops::Deref for #name {
            type Target = #base;
            fn deref(&self) -> &#base {
                &self.base
            }
        }

        impl ::core::ops::DerefMut for #name {
            fn deref_mut(&mut self) -> &mut #base {
                &mut self.base
            }
        }
    })
}

fn has_fn(item: &ItemImpl, name: &str) -> bool {
    item.items
        .iter()
        .any(|i| matches!(i, ImplItem::Fn(f) if f.sig.ident == name))
}

fn on_impl(args: Args, mut item: ItemImpl) -> syn::Result<TokenStream2> {
    if args.test.is_some() {
        return Err(syn::Error::new(
            Span::call_site(),
            "a test made from a base is generated whole: \
             `#[component(test(base = ..))]` goes on its struct only",
        ));
    }
    if args.sequencer.is_some() {
        return Err(syn::Error::new(
            Span::call_site(),
            "a sequencer is generated whole: `#[component(sequencer(..))]` goes on its struct only",
        ));
    }
    let kind = args
        .kind
        .ok_or_else(|| syn::Error::new(Span::call_site(), "`#[component(..)]` needs a kind"))?;
    let variant = kind_variant(&kind)?;
    let core = core();
    let self_ty = item.self_ty.clone();
    let has_run = has_fn(&item, "run");
    if args.passive && has_run {
        return Err(syn::Error::new(
            item.span(),
            "a passive component has no `run`: ports write to it during the run phase",
        ));
    }
    if kind == "test" && !has_run {
        return Err(syn::Error::new(
            item.self_ty.span(),
            "a test needs a `run`: the run phase ends when it returns",
        ));
    }
    if !args.passive && !has_run {
        return Err(syn::Error::new(
            item.self_ty.span(),
            format!("a component with no `run` is passive: write `#[component({kind}, passive)]`"),
        ));
    }
    let passive = args.passive;
    item.items.push(syn::parse_quote! {
        const KIND: #core::Kind = #core::Kind::#variant;
    });
    item.items.push(syn::parse_quote! {
        const PASSIVE: bool = #passive;
    });
    let message = format!("the kind on the impl (`{kind}`) differs from the one on the struct");
    let check = format_ident!("__TBF_KIND_CHECK");
    Ok(quote! {
        #item

        const _: () = {
            #[allow(non_upper_case_globals, dead_code)]
            const #check: () = ::core::assert!(
                <#self_ty as #core::__private::DeclaredKind>::KIND as u8
                    == #core::Kind::#variant as u8,
                #message
            );
        };
    })
}

/// `#[sequence(sequencer = S, base = B)]`.
struct SequenceArgs {
    sequencer: Type,
    base: Option<Type>,
}

impl Parse for SequenceArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        let mut sequencer = None;
        let mut base = None;
        for meta in metas {
            match meta {
                Meta::NameValue(nv) if nv.path.is_ident("sequencer") => {
                    let expr = nv.value;
                    sequencer = Some(syn::parse2::<Type>(quote!(#expr))?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("base") => {
                    let expr = nv.value;
                    base = Some(syn::parse2::<Type>(quote!(#expr))?);
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "expected `sequencer = ..` or `base = ..`",
                    ));
                }
            }
        }
        let sequencer = sequencer.ok_or_else(|| {
            syn::Error::new(Span::call_site(), "`#[sequence]` needs `sequencer = ..`")
        })?;
        Ok(SequenceArgs { sequencer, base })
    }
}

pub(crate) fn sequence(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let SequenceArgs { sequencer, base } = syn::parse2(attr)?;
    let mut item: ItemStruct = syn::parse2(item)?;
    let core = core();
    let name = item.ident.clone();
    let text = name.to_string();
    let vis = item.vis.clone();
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "a sequence is a struct with named fields",
        ));
    };
    let user: Vec<Field> = fields.named.iter().cloned().collect();
    fields.named.push(syn::parse_quote! {
        #[doc(hidden)]
        __tbf_sequence: #core::__private::SequenceFields<#sequencer>
    });
    if let Some(base) = &base {
        fields.named.push(syn::parse_quote! {
            #[doc(hidden)]
            __tbf_base: #base
        });
    }
    let idents: Vec<&Ident> = user.iter().map(|f| f.ident.as_ref().unwrap()).collect();
    let types: Vec<&Type> = user.iter().map(|f| &f.ty).collect();
    let base_init = base
        .as_ref()
        .map(|_| quote!(__tbf_base: ::core::default::Default::default(),));
    let has_base = base.as_ref().map(|base| {
        quote! {
            impl #core::HasBase<#base> for #name {
                fn base(&self) -> &#base {
                    &self.__tbf_base
                }
                fn base_mut(&mut self) -> &mut #base {
                    &mut self.__tbf_base
                }
            }
        }
    });
    Ok(quote! {
        #item

        impl #name {
            /// Makes the sequence from its fields; the core's and the base's
            /// start at their defaults.
            #[allow(clippy::too_many_arguments)]
            #vis fn new(#(#idents: #types),*) -> Self {
                #name {
                    #(#idents,)*
                    __tbf_sequence: ::core::default::Default::default(),
                    #base_init
                }
            }
        }

        impl #core::SequenceCore for #name {
            type Sequencer = #sequencer;
            const NAME: &'static str = #text;

            fn __fields(&self) -> &#core::__private::SequenceFields<#sequencer> {
                &self.__tbf_sequence
            }

            fn __fields_mut(&mut self) -> &mut #core::__private::SequenceFields<#sequencer> {
                &mut self.__tbf_sequence
            }
        }

        #has_base
    })
}
