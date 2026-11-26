//! Lowering a property (`#[assert]`) into methods stepped at each edge.
//!
//! A property's body is a continuation: same-cycle conditions (`if`),
//! `let`s and expectations, evaluated at an edge, ending with at most one
//! statement that holds a `seq!`:
//!
//! - `if seq!(s) { body }`: a condition sequence (antecedent). A thread
//!   starts at the edge; at each of its matches, `body` is evaluated as a
//!   continuation of its own, at that edge.
//! - `seq!(s)`: an expectation (consequent). An attempt starts at the edge;
//!   it succeeds at its first match and fails when no branch is left.
//!
//! Each `seq!` is a stage, numbered in the order met; a continuation only
//! starts later stages, so at an edge the stages are stepped from the last
//! to the first, then the body itself is evaluated: what a stage starts at
//! an edge is not stepped again at that edge.
//!
//! A `let` kept across cycles (within `seq!`, or followed by a statement
//! holding one) is a field of the property's lets struct, reached through
//! `__verald_l`; it is rebound as a modified copy.
//!
//! Each `seq!` is built into an NFA (`nfa.rs`), and a method is generated
//! per state (`_v<i>`, visiting it at an edge) and one per stage (`_step`,
//! visiting the states a branch occupies). `ended(s)` is a matcher of its
//! own (`_e<j>`), stepped first at each edge; its result is passed to every
//! generated method in `__verald_e`.

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Block, Expr, Ident, Macro, Pat, Stmt, Type};

use crate::assertion_crate;
use crate::named::Named;
use crate::nfa::{Act, Chain, Cond, Nfa, State, Trans};
use crate::rewrite::{is_macro, location};
use crate::seq::{self, Count, Node};

/// What a property lowers to.
pub(crate) struct Lowered {
    /// The property's body: steps its stages and evaluates the body. Its
    /// method takes the history, its state and the report.
    pub block: Block,
    /// The methods of its stages and continuations.
    pub methods: Vec<TokenStream2>,
    /// The structs of its `let`s and its state.
    pub items: TokenStream2,
    /// The type of its state.
    pub state: Ident,
    /// Its method reporting at the end of the test, taking its state and
    /// the report.
    pub finish: Ident,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// A condition: a branch that fails is dropped.
    Antecedent,
    /// An expectation: an attempt with no branch left fails.
    Consequent,
    /// `never(s)`: a match fails the property; a branch that fails is
    /// dropped.
    Never,
}

/// A stage: its side, its bit set's width, whether it is strong (an
/// attempt still running at the end fails: `eventually`), and the failure
/// it reports as a whole (`never`, `eventually`).
struct Stage {
    side: Side,
    words: usize,
    strong: bool,
    failure: Option<TokenStream2>,
}

/// A name in scope: a `let` kept in the lets struct, or a plain local.
#[derive(Clone)]
enum Binding {
    Stored(Ident),
    Local,
    /// Out of reach: a name of the property's body, within `ended`'s
    /// sequence, which runs on its own.
    Forbidden,
}

#[derive(Clone, Default)]
struct Env(Vec<HashMap<String, Binding>>);

impl Env {
    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.0.iter().rev().find_map(|s| s.get(name))
    }

    fn insert(&mut self, name: String, binding: Binding) {
        if self.0.is_empty() {
            self.0.push(HashMap::new());
        }
        self.0.last_mut().expect("a scope").insert(name, binding);
    }

    fn push(&mut self) {
        self.0.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.0.pop();
    }

    /// The names in scope, all out of reach (for `ended`'s sequence).
    fn forbidden(&self) -> Env {
        let mut scope = HashMap::new();
        for s in &self.0 {
            for name in s.keys() {
                scope.insert(name.clone(), Binding::Forbidden);
            }
        }
        Env(vec![scope])
    }
}

struct Lowerer<'a> {
    /// The property's name, as reported.
    name: String,
    /// The prefix of its generated methods.
    prefix: String,
    sig: &'a Ident,
    /// The impl's named sequences and properties.
    named: &'a Named,
    /// The `self` the user wrote, for the generated methods to reach the
    /// same `self` as the user's code (hygiene).
    sf: syn::token::SelfValue,
    lets_ty: Ident,
    state_ty: Ident,
    /// The lets struct's fields.
    lets: Vec<(Ident, Type)>,
    /// The stages' sides and widths in words.
    stages: Vec<Stage>,
    /// The `ended` matchers' widths in words and entry methods.
    ended: Vec<(usize, Ident)>,
    /// Within an `ended` matcher's sequence.
    in_ended: bool,
    methods: Vec<TokenStream2>,
}

/// The hidden names of the generated code.
mod names {
    use proc_macro2::{Ident, Span};

    pub fn h() -> Ident {
        crate::block::history_param()
    }
    pub fn st() -> Ident {
        Ident::new("__verald_st", Span::call_site())
    }
    pub fn l() -> Ident {
        Ident::new("__verald_l", Span::call_site())
    }
    pub fn start() -> Ident {
        Ident::new("__verald_start", Span::call_site())
    }
    pub fn r() -> Ident {
        crate::block::report_param()
    }
    pub fn out() -> Ident {
        Ident::new("__verald_out", Span::call_site())
    }
    pub fn states() -> Ident {
        Ident::new("__verald_states", Span::call_site())
    }
    pub fn e() -> Ident {
        Ident::new("__verald_e", Span::call_site())
    }
    pub fn now() -> Ident {
        Ident::new("__verald_now", Span::call_site())
    }
}

pub(crate) fn lower(
    block_name: &Ident,
    sig: &Ident,
    named: &Named,
    sf: syn::token::SelfValue,
    property: &Ident,
    body: &Block,
) -> syn::Result<Lowered> {
    TEMPORAL.with(|t| *t.borrow_mut() = named.temporal.clone());
    let a = assertion_crate();
    let p = quote!(#a::__private);
    let mut lowerer = Lowerer {
        name: property.to_string(),
        prefix: format!("__verald_{property}"),
        sig,
        named,
        sf,
        lets_ty: format_ident!("__VeraldLets_{}_{}", block_name, property),
        state_ty: format_ident!("__VeraldState_{}_{}", block_name, property),
        lets: Vec::new(),
        stages: Vec::new(),
        ended: Vec::new(),
        in_ended: false,
        methods: Vec::new(),
    };
    let mut env = Env::default();
    env.push();
    let body = lowerer.block(&body.stmts, &mut env)?;

    let (h, st, l, start, r, e, now) = (
        names::h(),
        names::st(),
        names::l(),
        names::start(),
        names::r(),
        names::e(),
        names::now(),
    );
    let (lets_ty, state_ty) = (&lowerer.lets_ty, &lowerer.state_ty);
    let name = &lowerer.name;
    let sf = lowerer.sf;
    // The `ended` matchers: stepped, then started at this edge.
    let ended = lowerer.ended.iter().enumerate().map(|(j, (_, entry))| {
        let field = format_ident!("e{}", j);
        let step = format_ident!("{}_e{}_step", lowerer.prefix, j);
        quote! {{
            let __verald_m = #st.#field.step(|__verald_x, __verald_y, __verald_z| {
                #sf.#step(#h, &[], __verald_x, __verald_y, __verald_z)
            });
            let mut __verald_hit = !__verald_m.is_empty();
            #st.#field.give_back(__verald_m);
            let __verald_m = #st.#field.start(#now, |__verald_z| {
                #sf.#entry(#h, &[], &(), __verald_z)
            });
            __verald_hit |= !__verald_m.is_empty();
            #st.#field.give_back(__verald_m);
            __verald_hit
        }}
    });
    let steps = (0..lowerer.stages.len()).rev().map(|k| {
        let field = format_ident!("s{}", k);
        let step = lowerer.method_name(k, "step");
        match lowerer.stages[k].side {
            Side::Antecedent => {
                let cont = lowerer.cont_name(k);
                quote! {
                    let mut __verald_m = #st.#field.step(|__verald_x, __verald_y, __verald_z| {
                        #sf.#step(#h, #e, __verald_x, __verald_y, __verald_z)
                    });
                    for (__verald_ml, __verald_ms) in __verald_m.drain(..) {
                        #sf.#cont(#h, #e, #st, &__verald_ml, __verald_ms, #r);
                    }
                    #st.#field.give_back(__verald_m);
                }
            }
            Side::Never => {
                let failure = lowerer.stages[k].failure.as_ref().expect("never's failure");
                quote! {
                    let mut __verald_m = #st.#field.step(|__verald_x, __verald_y, __verald_z| {
                        #sf.#step(#h, #e, __verald_x, __verald_y, __verald_z)
                    });
                    for (_, __verald_ms) in __verald_m.drain(..) {
                        #r.property_failed(#name, &#failure, __verald_ms, 0);
                    }
                    #st.#field.give_back(__verald_m);
                }
            }
            Side::Consequent => quote! {
                #st.#field.step(
                    |__verald_x, __verald_y, __verald_z| {
                        #sf.#step(#h, #e, __verald_x, __verald_y, __verald_z)
                    },
                    |__verald_f| {
                        #r.property_failed(#name, &__verald_f.failure, __verald_f.start, __verald_f.merged)
                    },
                );
            },
        }
    });
    let block = syn::parse_quote! {{
        let #now = #p::time();
        let #e: &[bool] = &[#(#ended),*];
        #(#steps)*
        let #l = &<#lets_ty as ::core::default::Default>::default();
        let #start = #now;
        #body
    }};
    // At the end: an expectation's attempts still running are incomplete,
    // or failures for a strong one (unless the test failed already, ending
    // it early).
    let finish = format_ident!("{}_finish", lowerer.prefix);
    let ends = lowerer
        .stages
        .iter()
        .enumerate()
        .filter(|(_, s)| s.side == Side::Consequent)
        .map(|(k, stage)| {
            let field = format_ident!("s{}", k);
            let fail = match (&stage.failure, stage.strong) {
                (Some(failure), true) => quote! {
                    if !#p::failed() {
                        #r.property_failed(#name, &#failure, __verald_s, __verald_n);
                        continue;
                    }
                },
                _ => quote!(),
            };
            quote! {{
                let mut __verald_count = 0u64;
                let mut __verald_earliest = u64::MAX;
                for (__verald_s, __verald_n) in #st.#field.attempts() {
                    #fail
                    __verald_count += 1 + u64::from(__verald_n);
                    __verald_earliest = __verald_earliest.min(__verald_s);
                }
                if __verald_count > 0 {
                    #r.incomplete(#name, __verald_count, __verald_earliest);
                }
            }}
        });
    let finish_method = quote! {
        #[doc(hidden)]
        #[allow(unused_variables, unused_parens)]
        fn #finish(&#sf, #st: &mut #state_ty, #r: &#a::Report<'_>) {
            #(#ends)*
        }
    };
    lowerer.methods.push(finish_method);
    let let_fields = lowerer.lets.iter().map(|(f, ty)| quote!(#f: #ty));
    let stage_fields = lowerer
        .stages
        .iter()
        .enumerate()
        .map(|(k, stage)| {
            let field = format_ident!("s{}", k);
            let words = stage.words;
            match stage.side {
                Side::Antecedent | Side::Never => {
                    quote!(#field: #p::Antecedent<#lets_ty, #words>)
                }
                Side::Consequent => quote!(#field: #p::Consequent<#lets_ty, #words>),
            }
        })
        .chain(lowerer.ended.iter().enumerate().map(|(j, (words, _))| {
            let field = format_ident!("e{}", j);
            quote!(#field: #p::Antecedent<(), #words>)
        }));
    let items = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types, dead_code)]
        #[derive(::core::clone::Clone, ::core::cmp::PartialEq, ::core::default::Default)]
        pub struct #lets_ty {
            #(#let_fields,)*
        }

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(::core::default::Default)]
        pub struct #state_ty {
            #(#stage_fields,)*
        }
    };
    Ok(Lowered {
        block,
        methods: lowerer.methods,
        items,
        state: lowerer.state_ty,
        finish,
    })
}

struct FindSeq(bool);

impl<'ast> Visit<'ast> for FindSeq {
    fn visit_macro(&mut self, mac: &'ast Macro) {
        if is_macro(mac, "seq") {
            self.0 = true;
        }
    }

    /// `ended(seq!(..))` is a boolean: its `seq!` does not count.
    /// `eventually`, `never` and `until` take time as `seq!` does.
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if is_ended(call) {
            return;
        }
        if property_call(call).is_some() || named_temporal(call) {
            self.0 = true;
            return;
        }
        syn::visit::visit_expr_call(self, call);
    }
}

thread_local! {
    /// The impl's named properties that take time, for `FindSeq`.
    static TEMPORAL: std::cell::RefCell<std::collections::HashSet<String>> =
        Default::default();
}

/// Whether `call` is of a named property that takes time.
fn named_temporal(call: &syn::ExprCall) -> bool {
    let Expr::Path(p) = &*call.func else {
        return false;
    };
    let Some(ident) = p.path.get_ident() else {
        return false;
    };
    TEMPORAL.with(|t| t.borrow().contains(&ident.to_string()))
}

/// The properties written as calls ending a body.
const PROPERTY_CALLS: &[&str] = &["eventually", "never", "until"];

/// `eventually(..)`, `never(..)` or `until(..)`: its name.
fn property_call(call: &syn::ExprCall) -> Option<String> {
    let Expr::Path(p) = &*call.func else {
        return None;
    };
    let ident = p.path.get_ident()?;
    PROPERTY_CALLS
        .iter()
        .any(|n| ident == n)
        .then(|| ident.to_string())
}

/// Whether `call` is `ended(..)`.
fn is_ended(call: &syn::ExprCall) -> bool {
    matches!(&*call.func, Expr::Path(p) if p.path.is_ident("ended"))
}

/// Whether a statement holds a `seq!` anywhere.
fn stmt_holds_seq(stmt: &Stmt) -> bool {
    let mut find = FindSeq(false);
    find.visit_stmt(stmt);
    find.0
}

/// Whether an expression holds a `seq!` anywhere.
fn holds_seq(expr: &Expr) -> bool {
    let mut find = FindSeq(false);
    find.visit_expr(expr);
    find.0
}

fn misplaced_seq(span: Span) -> syn::Error {
    syn::Error::new(
        span,
        "a `seq!` is an `if`'s whole condition, or the last statement of a body",
    )
}

/// The `seq!` a statement or an expression is, if it is one.
fn as_seq_stmt(stmt: &Stmt) -> Option<&Macro> {
    match stmt {
        Stmt::Macro(m) if is_macro(&m.mac, "seq") => Some(&m.mac),
        Stmt::Expr(Expr::Macro(m), _) if is_macro(&m.mac, "seq") => Some(&m.mac),
        _ => None,
    }
}

impl Lowerer<'_> {
    fn method_name(&self, stage: usize, what: &str) -> Ident {
        format_ident!("{}_s{}_{}", self.prefix, stage, what)
    }

    fn cont_name(&self, stage: usize) -> Ident {
        format_ident!("{}_c{}", self.prefix, stage)
    }

    fn store(&mut self, ty: Type) -> Ident {
        let field = format_ident!("f{}", self.lets.len());
        self.lets.push((field.clone(), ty));
        field
    }

    /// The statements of a continuation's block.
    fn block(&mut self, stmts: &[Stmt], env: &mut Env) -> syn::Result<TokenStream2> {
        let mut out = TokenStream2::new();
        let last = stmts.len().saturating_sub(1);
        let temporal: Vec<bool> = stmts.iter().map(stmt_holds_seq).collect();
        for (i, stmt) in stmts.iter().enumerate() {
            if temporal[i] && i != last {
                return Err(syn::Error::new(
                    stmts[i + 1].span(),
                    "nothing follows a statement holding a `seq!`: time advances within it",
                ));
            }
            let followed_by_time = i < last && temporal[last];
            out.extend(self.stmt(stmt, followed_by_time, env)?);
        }
        Ok(out)
    }

    fn stmt(
        &mut self,
        stmt: &Stmt,
        followed_by_time: bool,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        if let Some(mac) = as_seq_stmt(stmt) {
            let nodes = seq::parse(mac.tokens.clone(), self.named)?;
            return self.consequent(nodes, false, None, env);
        }
        if let Stmt::Expr(Expr::Call(call), _) = stmt
            && let Some(name) = property_call(call)
        {
            return self.property_call(&name, call, env);
        }
        if let Stmt::Expr(Expr::Call(call), _) = stmt
            && let Expr::Path(func) = &*call.func
            && let Some(ident) = func.path.get_ident()
            && self.named.properties.contains_key(&ident.to_string())
        {
            return self.named_property(&ident.to_string(), call, env);
        }
        match stmt {
            Stmt::Local(local) => self.local(local, followed_by_time, env),
            Stmt::Expr(expr, _) => self.expr(expr, env),
            Stmt::Macro(mac) => Err(syn::Error::new(
                mac.span(),
                "a property holds conditions (`if`), `let`s, expectations and `seq!`",
            )),
            Stmt::Item(item) => Err(syn::Error::new(
                item.span(),
                "a property holds conditions (`if`), `let`s, expectations and `seq!`",
            )),
        }
    }

    fn local(
        &mut self,
        local: &syn::Local,
        followed_by_time: bool,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let Some(init) = &local.init else {
            return Err(syn::Error::new(
                local.span(),
                "a `let` in a property has a value",
            ));
        };
        if init.diverge.is_some() {
            return Err(syn::Error::new(
                local.span(),
                "a `let` in a property has no `else`",
            ));
        }
        if holds_seq(&init.expr) {
            return Err(misplaced_seq(init.expr.span()));
        }
        let value = self.value(&init.expr, env)?;
        if !followed_by_time {
            let pat = &local.pat;
            let attrs = &local.attrs;
            for name in pat_names(pat) {
                env.insert(name, Binding::Local);
            }
            return Ok(quote!(#(#attrs)* let #pat = #value;));
        }
        // Kept across cycles: a field of the lets struct.
        let (ident, ty) = match &local.pat {
            Pat::Type(t) => match &*t.pat {
                Pat::Ident(i) if i.by_ref.is_none() && i.subpat.is_none() => {
                    (i.ident.clone(), (*t.ty).clone())
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "a `let` kept across cycles binds one name",
                    ));
                }
            },
            Pat::Ident(i) => {
                return Err(syn::Error::new(
                    i.span(),
                    format!(
                        "a `let` followed by a `seq!` has its type written: `let {}: u32 = ..`; \
                         its value is kept across cycles",
                        i.ident
                    ),
                ));
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "a `let` kept across cycles binds one name",
                ));
            }
        };
        Ok(self.stored_let(ident, ty, &value, env))
    }

    /// `let name: ty = value` kept in the lets struct: `__verald_l` is
    /// rebound to a copy with the field set.
    fn stored_let(&mut self, name: Ident, ty: Type, value: &Expr, env: &mut Env) -> TokenStream2 {
        let field = self.store(ty);
        env.insert(name.to_string(), Binding::Stored(field.clone()));
        let l = names::l();
        quote! {
            let #l = &{
                let mut __verald_n = ::core::clone::Clone::clone(#l);
                __verald_n.#field = #value;
                __verald_n
            };
        }
    }

    fn expr(&mut self, expr: &Expr, env: &mut Env) -> syn::Result<TokenStream2> {
        match expr {
            Expr::If(e) => {
                if let Some((_, other)) = &e.else_branch {
                    return Err(syn::Error::new(
                        other.span(),
                        "a condition has no `else`: the expectation goes in its body",
                    ));
                }
                if let Expr::Macro(m) = &*e.cond
                    && is_macro(&m.mac, "seq")
                {
                    return self.antecedent(&m.mac, &e.then_branch, env);
                }
                if holds_seq(&e.cond) {
                    return Err(misplaced_seq(e.cond.span()));
                }
                let cond = self.value(&e.cond, env)?;
                env.push();
                let body = self.block(&e.then_branch.stmts, env);
                env.pop();
                let body = body?;
                Ok(quote!(if #cond { #body }))
            }
            Expr::Block(b) if b.label.is_none() => {
                env.push();
                let body = self.block(&b.block.stmts, env);
                env.pop();
                let body = body?;
                Ok(quote!({ #body }))
            }
            expr => {
                if holds_seq(expr) {
                    return Err(misplaced_seq(expr.span()));
                }
                let a = assertion_crate();
                let location = location(expr.span());
                let value = self.value(expr, env)?;
                let (name, r, start) = (&self.name, names::r(), names::start());
                Ok(quote! {
                    if !(#value) {
                        #r.property_failed(
                            #name,
                            &#a::Failure {
                                expr: ::core::stringify!(#expr),
                                location: #location,
                            },
                            #start,
                            0,
                        );
                        return;
                    }
                })
            }
        }
    }

    /// `if seq!(s) { body }`: starts a thread of the stage at this edge;
    /// each match evaluates `body`, a continuation of its own.
    fn antecedent(
        &mut self,
        mac: &Macro,
        body: &Block,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let nodes = seq::parse(mac.tokens.clone(), self.named)?;
        let k = self.stages.len();
        self.stages.push(Stage {
            side: Side::Antecedent,
            words: 0,
            strong: false,
            failure: None,
        });
        let mut seq_env = env.clone();
        seq_env.push();
        let entry = self.stage(k, Side::Antecedent, &nodes, &mut seq_env)?;
        // The continuation, evaluated at each match.
        seq_env.push();
        let cont_body = self.block(&body.stmts, &mut seq_env)?;
        let a = assertion_crate();
        let (h, st, l, start, r, e) = (
            names::h(),
            names::st(),
            names::l(),
            names::start(),
            names::r(),
            names::e(),
        );
        let (sig, lets_ty, state_ty) = (self.sig, &self.lets_ty, &self.state_ty);
        let sf = self.sf;
        let cont = self.cont_name(k);
        self.methods.push(quote! {
            #[doc(hidden)]
            #[allow(unused_variables, unused_parens)]
            fn #cont(
                &#sf,
                #h: &#a::History<#sig>,
                #e: &[bool],
                #st: &mut #state_ty,
                #l: &#lets_ty,
                #start: u64,
                #r: &#a::Report<'_>,
            ) {
                #cont_body
            }
        });
        let field = format_ident!("s{}", k);
        Ok(quote! {
            {
                let mut __verald_m = #st.#field.start(#start, |__verald_z| {
                    #sf.#entry(#h, #e, #l, __verald_z)
                });
                for (__verald_ml, __verald_ms) in __verald_m.drain(..) {
                    #sf.#cont(#h, #e, #st, &__verald_ml, __verald_ms, #r);
                }
                #st.#field.give_back(__verald_m);
            }
        })
    }

    /// A named property's call: its body in place, as a block, after the
    /// check of the arguments' types.
    fn named_property(
        &mut self,
        name: &str,
        call: &syn::ExprCall,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let named = self.named;
        let item = &named.properties[name];
        let args: Vec<&Expr> = call.args.iter().collect();
        let _expanding = named.enter(item, call.span())?;
        let tokens = named.expand(item, &args, call.span())?;
        let block: Block = syn::parse2(tokens)?;
        let values = args
            .iter()
            .map(|a| self.value(a, env))
            .collect::<syn::Result<Vec<_>>>()?;
        let check = named.check(item, &values);
        env.push();
        let body = self.block(&block.stmts, env);
        env.pop();
        let body = body?;
        Ok(quote! {
            #check
            { #body }
        })
    }

    /// `eventually(x)`, `never(s)` or `until(a, b)` ending a body.
    fn property_call(
        &mut self,
        name: &str,
        call: &syn::ExprCall,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let args: Vec<&Expr> = call.args.iter().collect();
        let a = assertion_crate();
        let location = location(call.span());
        let failure = quote! {
            #a::Failure {
                expr: ::core::stringify!(#call),
                location: #location,
            }
        };
        match (name, args.as_slice()) {
            ("eventually", [x]) => {
                // Strong: `delay(0..) -> x`, failing at the end if no match.
                let mut nodes = vec![Node::Delay(Count { min: 0, max: None })];
                nodes.extend(seq::sequence(x, self.named)?);
                self.consequent(nodes, true, Some(failure), env)
            }
            ("never", [s]) => {
                let nodes = seq::sequence(s, self.named)?;
                self.never(nodes, failure, env)
            }
            ("until", [a, b]) => {
                let nodes = vec![Node::Until((*a).clone(), (*b).clone())];
                self.consequent(nodes, false, None, env)
            }
            ("eventually", _) => Err(syn::Error::new(
                call.span(),
                "`eventually` takes an element or a `seq!`: `eventually(ack)`",
            )),
            ("never", _) => Err(syn::Error::new(
                call.span(),
                "`never` takes an element or a `seq!`: `never(seq!(grant -> delay(1) -> grant))`",
            )),
            _ => Err(syn::Error::new(
                call.span(),
                "`until` takes two booleans: `until(busy, done)`",
            )),
        }
    }

    /// `never(s)` ending a body: a thread of the stage starts at this edge,
    /// and its match fails the property.
    fn never(
        &mut self,
        nodes: Vec<Node>,
        failure: TokenStream2,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let k = self.stages.len();
        self.stages.push(Stage {
            side: Side::Never,
            words: 0,
            strong: false,
            failure: Some(failure.clone()),
        });
        let mut seq_env = env.clone();
        seq_env.push();
        let entry = self.stage(k, Side::Never, &nodes, &mut seq_env)?;
        let (h, st, l, start, r, e) = (
            names::h(),
            names::st(),
            names::l(),
            names::start(),
            names::r(),
            names::e(),
        );
        let field = format_ident!("s{}", k);
        let name = &self.name;
        let sf = self.sf;
        Ok(quote! {
            {
                let mut __verald_m = #st.#field.start(#start, |__verald_z| {
                    #sf.#entry(#h, #e, #l, __verald_z)
                });
                for (_, __verald_ms) in __verald_m.drain(..) {
                    #r.property_failed(#name, &#failure, __verald_ms, 0);
                }
                #st.#field.give_back(__verald_m);
            }
        })
    }

    /// An expectation ending a body (`seq!(s)`, `eventually`, `until`):
    /// starts an attempt of the stage at this edge.
    fn consequent(
        &mut self,
        nodes: Vec<Node>,
        strong: bool,
        failure: Option<TokenStream2>,
        env: &mut Env,
    ) -> syn::Result<TokenStream2> {
        let k = self.stages.len();
        self.stages.push(Stage {
            side: Side::Consequent,
            words: 0,
            strong,
            failure,
        });
        let mut seq_env = env.clone();
        seq_env.push();
        let entry = self.stage(k, Side::Consequent, &nodes, &mut seq_env)?;
        let (h, st, l, start, r, e) = (
            names::h(),
            names::st(),
            names::l(),
            names::start(),
            names::r(),
            names::e(),
        );
        let field = format_ident!("s{}", k);
        let name = &self.name;
        let sf = self.sf;
        Ok(quote! {
            if let ::core::result::Result::Err(__verald_f) = #st.#field.start(#start, |__verald_z| {
                #sf.#entry(#h, #e, #l, __verald_z)
            }) {
                #r.property_failed(#name, &__verald_f, #start, 0);
            }
        })
    }

    /// Stage `k`: builds its NFA and generates its methods. Returns the
    /// entry's method.
    fn stage(&mut self, k: usize, side: Side, nodes: &[Node], env: &mut Env) -> syn::Result<Ident> {
        let nfa = Build::new(self, true).nfa(nodes, env)?;
        let prefix = format!("{}_s{}", self.prefix, k);
        let lets_ty = self.lets_ty.clone();
        let words = self.emit(&prefix, side, quote!(#lets_ty), &nfa);
        self.stages[k].words = words;
        Ok(format_ident!("{}_v{}", prefix, nfa.entry))
    }

    /// `ended(s)`: a matcher of its own, run at every edge. Returns its
    /// index in `__verald_e`.
    fn ended(&mut self, arg: &Expr, env: &Env) -> syn::Result<usize> {
        if self.in_ended {
            return Err(syn::Error::new(
                arg.span(),
                "`ended` is not nested in another's sequence",
            ));
        }
        let nodes = seq::sequence(arg, self.named)?;
        let j = self.ended.len();
        let prefix = format!("{}_e{}", self.prefix, j);
        self.ended.push((0, format_ident!("{}_v0", prefix)));
        let mut ended_env = env.forbidden();
        self.in_ended = true;
        let nfa = Build::new(self, false).nfa(&nodes, &mut ended_env);
        self.in_ended = false;
        let nfa = nfa?;
        let words = self.emit(&prefix, Side::Antecedent, quote!(()), &nfa);
        self.ended[j] = (words, format_ident!("{}_v{}", prefix, nfa.entry));
        Ok(j)
    }

    /// `expr` as the generated code evaluates it: the `let`s kept across
    /// cycles read from the lets struct, and each `ended(s)` from
    /// `__verald_e`.
    fn value(&mut self, expr: &Expr, env: &Env) -> syn::Result<Expr> {
        let mut expr = expr.clone();
        let mut values = Values {
            lowerer: self,
            env,
            error: None,
        };
        values.visit_expr_mut(&mut expr);
        match values.error {
            Some(error) => Err(error),
            None => Ok(expr),
        }
    }

    /// The code reporting that a branch died at `cond` (an expectation's
    /// side), or nothing (a condition's).
    fn fail(&self, side: Side, cond: &Cond) -> TokenStream2 {
        match side {
            Side::Antecedent | Side::Never => quote!(),
            Side::Consequent => {
                let a = assertion_crate();
                let out = names::out();
                let text = &cond.text;
                let location = location(text.span());
                quote! {
                    #out.fail(#a::Failure {
                        expr: ::core::stringify!(#text),
                        location: #location,
                    });
                }
            }
        }
    }

    /// The methods of an NFA, named from `prefix`: `_v<i>` per state and
    /// `_step`. Returns the bit set's width in words.
    fn emit(&mut self, prefix: &str, side: Side, lets_ty: TokenStream2, nfa: &Nfa) -> usize {
        let (bits, chain_lo, total) = nfa.bits();
        let words = total.div_ceil(64).max(1);
        let a = assertion_crate();
        let p = quote!(#a::__private);
        let (h, l, out, states, e) = (
            names::h(),
            names::l(),
            names::out(),
            names::states(),
            names::e(),
        );
        let sig = self.sig;
        let sf = self.sf;
        let out_ty = quote!(#p::Out<#lets_ty, #words>);
        let visit = |i: usize| -> TokenStream2 {
            let m = format_ident!("{}_v{}", prefix, i);
            quote!(#sf.#m(#h, #e, #l, #out);)
        };
        let bit_of = |s: usize| -> usize {
            bits.iter()
                .find(|(state, _)| *state == s)
                .map(|(_, b)| *b)
                .expect("a ticked state has a bit")
        };
        let act = |act: &Act, cond: &Cond| -> TokenStream2 {
            match act {
                Act::Visit(t) => visit(*t),
                Act::Tick(t) => {
                    let b = bit_of(*t);
                    quote!(#out.set(#l, #b);)
                }
                Act::Fail => self.fail(side, cond),
            }
        };
        let guards = |guards: &[Cond], exit: TokenStream2| -> TokenStream2 {
            guards
                .iter()
                .map(|g| {
                    let value = &g.value;
                    let fail = self.fail(side, g);
                    quote! {
                        if !(#value) {
                            #fail
                            #exit
                        }
                    }
                })
                .collect()
        };

        let mut methods = Vec::new();
        for (i, state) in nfa.states.iter().enumerate() {
            let mut body = guards(&state.guards, quote!(return;));
            for t in &state.trans {
                body.extend(match t {
                    Trans::Branch {
                        cond,
                        then,
                        otherwise,
                    } => {
                        let value = &cond.value;
                        let then = act(then, cond);
                        let otherwise = act(otherwise, cond);
                        quote! {
                            if #value {
                                #then
                            } else {
                                #otherwise
                            }
                        }
                    }
                    Trans::Let {
                        field,
                        value,
                        target,
                    } => {
                        let next = visit(*target);
                        quote! {{
                            let #l = &{
                                let mut __verald_n = ::core::clone::Clone::clone(#l);
                                __verald_n.#field = #value;
                                __verald_n
                            };
                            #next
                        }}
                    }
                    Trans::Eps(t) => visit(*t),
                    Trans::Tick(t) => {
                        let b = bit_of(*t);
                        quote!(#out.set(#l, #b);)
                    }
                    Trans::Chain(c) => {
                        let lo = chain_lo[*c];
                        let chain = &nfa.chains[*c];
                        let now = if chain.count.min == 0 {
                            visit(chain.exit)
                        } else {
                            quote!()
                        };
                        quote! {
                            #out.set(#l, #lo);
                            #now
                        }
                    }
                    Trans::Accept => quote!(#out.matched(#l);),
                });
            }
            let m = format_ident!("{}_v{}", prefix, i);
            methods.push(quote! {
                #[doc(hidden)]
                #[allow(unused_variables, unreachable_code, unused_parens)]
                fn #m(&#sf, #h: &#a::History<#sig>, #e: &[bool], #l: &#lets_ty, #out: &mut #out_ty) {
                    #body
                }
            });
        }

        // At an edge: each occupied state is visited; each chain arrives
        // where its count is reached, and moves on.
        let mut moves: Vec<TokenStream2> = bits
            .iter()
            .map(|(s, b)| {
                let v = visit(*s);
                quote! {
                    if #states.get(#b) {
                        #v
                    }
                }
            })
            .collect();
        let label = syn::Lifetime::new("'__verald_chain", Span::call_site());
        for (c, chain) in nfa.chains.iter().enumerate() {
            let lo = chain_lo[c];
            let hi = lo + chain.count.delay_states() - 1;
            let arrive_lo = lo + chain.count.min.max(1) - 1;
            let sticky = chain.count.max.is_none();
            let exit = visit(chain.exit);
            let checks = guards(&chain.guards, quote!(break #label;));
            moves.push(quote! {
                #label: {
                    const ALL: #p::States<#words> = #p::States::<#words>::range(#lo, #hi);
                    const ARRIVE: #p::States<#words> = #p::States::<#words>::range(#arrive_lo, #hi);
                    if !#states.intersects(&ALL) {
                        break #label;
                    }
                    #checks
                    if #states.intersects(&ARRIVE) {
                        #exit
                    }
                    #out.or(#l, &#states.advance(#lo, #hi, #sticky));
                }
            });
        }
        let step = format_ident!("{}_step", prefix);
        methods.push(quote! {
            #[doc(hidden)]
            #[allow(unused_variables, unused_parens)]
            fn #step(
                &#sf,
                #h: &#a::History<#sig>,
                #e: &[bool],
                #states: &#p::States<#words>,
                #l: &#lets_ty,
                #out: &mut #out_ty,
            ) {
                #(#moves)*
            }
        });
        self.methods.extend(methods);
        words
    }
}

/// Building a sequence's NFA from its tree.
struct Build<'a, 'b> {
    lowerer: &'a mut Lowerer<'b>,
    nfa: Nfa,
    /// The conditions of the `throughout`s around what is built.
    guards: Vec<Cond>,
    /// The field of each `let` (by node: a repetition's copies share it).
    fields: HashMap<*const Node, Ident>,
    /// Whether `let` is allowed (not in `ended`'s sequence).
    lets: bool,
}

impl<'a, 'b> Build<'a, 'b> {
    fn new(lowerer: &'a mut Lowerer<'b>, lets: bool) -> Self {
        Build {
            lowerer,
            nfa: Nfa::default(),
            guards: Vec::new(),
            fields: HashMap::new(),
            lets,
        }
    }

    fn nfa(mut self, nodes: &[Node], env: &mut Env) -> syn::Result<Nfa> {
        let entry = self.state();
        self.nfa.entry = entry;
        let end = self.seq(nodes, entry, env)?;
        self.push(end, Trans::Accept);
        Ok(self.nfa)
    }

    fn state(&mut self) -> usize {
        self.nfa.states.push(State {
            guards: self.guards.clone(),
            trans: Vec::new(),
        });
        self.nfa.states.len() - 1
    }

    fn push(&mut self, state: usize, t: Trans) {
        self.nfa.states[state].trans.push(t);
    }

    fn cond(&mut self, expr: &Expr, env: &Env) -> syn::Result<Cond> {
        if holds_seq(expr) {
            return Err(misplaced_seq(expr.span()));
        }
        Ok(Cond {
            value: self.lowerer.value(expr, env)?,
            text: expr.clone(),
        })
    }

    /// The elements from `cur`; returns where the last one ends.
    fn seq(&mut self, nodes: &[Node], mut cur: usize, env: &mut Env) -> syn::Result<usize> {
        for node in nodes {
            cur = self.node(node, cur, env)?;
        }
        Ok(cur)
    }

    fn node(&mut self, node: &Node, cur: usize, env: &mut Env) -> syn::Result<usize> {
        match node {
            Node::Bool(b) => {
                let cond = self.cond(b, env)?;
                let next = self.state();
                self.push(
                    cur,
                    Trans::Branch {
                        cond,
                        then: Act::Visit(next),
                        otherwise: Act::Fail,
                    },
                );
                Ok(next)
            }
            Node::Let { name, ty, expr } => {
                if !self.lets {
                    return Err(syn::Error::new(
                        name.span(),
                        "`ended`'s sequence runs on its own: it has no `let`",
                    ));
                }
                let value = self.lowerer.value(expr, env)?;
                let key = node as *const Node;
                let field = match self.fields.get(&key) {
                    Some(f) => f.clone(),
                    None => {
                        let f = self.lowerer.store(ty.clone());
                        self.fields.insert(key, f.clone());
                        f
                    }
                };
                env.insert(name.to_string(), Binding::Stored(field.clone()));
                let next = self.state();
                self.push(
                    cur,
                    Trans::Let {
                        field,
                        value,
                        target: next,
                    },
                );
                Ok(next)
            }
            Node::Delay(count) => {
                if count.max == Some(0) {
                    return Ok(cur);
                }
                let exit = self.state();
                self.nfa.chains.push(Chain {
                    count: *count,
                    guards: self.guards.clone(),
                    exit,
                });
                let c = self.nfa.chains.len() - 1;
                self.push(cur, Trans::Chain(c));
                Ok(exit)
            }
            Node::Wait(b) => {
                let w = self.state();
                self.push(cur, Trans::Eps(w));
                self.wait_from(b, w, env)
            }
            Node::Happen(count, b) => self.repeated(*count, cur, env, |this, entry, env| {
                this.wait_from(b, entry, env)
            }),
            Node::Repeat(count, body) => self.repeated(*count, cur, env, |this, entry, env| {
                this.seq(body, entry, env)
            }),
            Node::Group(nodes) => self.seq(nodes, cur, env),
            Node::Until(a, b) => {
                // At `w`: where `b` holds, on; otherwise `a` holds and `w`
                // again at the next edge.
                let cond_b = self.cond(b, env)?;
                let cond_a = self.cond(a, env)?;
                let w = self.state();
                self.push(cur, Trans::Eps(w));
                let next = self.state();
                let holding = self.state();
                self.push(
                    w,
                    Trans::Branch {
                        cond: cond_b,
                        then: Act::Visit(next),
                        otherwise: Act::Visit(holding),
                    },
                );
                self.push(
                    holding,
                    Trans::Branch {
                        cond: cond_a,
                        then: Act::Tick(w),
                        otherwise: Act::Fail,
                    },
                );
                Ok(next)
            }
            Node::Throughout(a, body) => {
                let cond = self.cond(a, env)?;
                self.guards.push(cond);
                let entry = self.state();
                self.push(cur, Trans::Eps(entry));
                let end = self.seq(body, entry, env);
                self.guards.pop();
                end
            }
        }
    }

    /// `wait(b)` from state `w`, a state of its own: where `b` holds, on;
    /// otherwise, `w` again at the next edge.
    fn wait_from(&mut self, b: &Expr, w: usize, env: &mut Env) -> syn::Result<usize> {
        let cond = self.cond(b, env)?;
        let next = self.state();
        self.push(
            w,
            Trans::Branch {
                cond,
                then: Act::Visit(next),
                otherwise: Act::Tick(w),
            },
        );
        Ok(next)
    }

    /// `body` `count` times from `cur`, the next time from the edge after
    /// one ends; past `count.min` times, each end is an end of the whole.
    /// An unbounded count repeats its last copy; a count from 0 may skip
    /// `body` altogether.
    fn repeated(
        &mut self,
        count: Count,
        cur: usize,
        env: &mut Env,
        mut body: impl FnMut(&mut Self, usize, &mut Env) -> syn::Result<usize>,
    ) -> syn::Result<usize> {
        let end = self.state();
        let copies = count.max.unwrap_or(count.min.max(1));
        let mut entry = self.state();
        self.push(cur, Trans::Eps(entry));
        if count.min == 0 {
            // None at all: skipped, what follows goes on at this cycle.
            self.push(cur, Trans::Eps(end));
        }
        for j in 1..=copies {
            let x = body(self, entry, env)?;
            if j >= count.min {
                self.push(x, Trans::Eps(end));
            }
            if j < copies {
                let next = self.state();
                self.push(x, Trans::Tick(next));
                entry = next;
            } else if count.max.is_none() {
                self.push(x, Trans::Tick(entry));
            }
        }
        Ok(end)
    }
}

/// Rewriting an expression for the generated code: `let`s kept across
/// cycles, and `ended(s)`.
struct Values<'a, 'b> {
    lowerer: &'a mut Lowerer<'b>,
    env: &'a Env,
    error: Option<syn::Error>,
}

impl VisitMut for Values<'_, '_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Path(p) = expr
            && p.qself.is_none()
            && let Some(ident) = p.path.get_ident()
        {
            match self.env.lookup(&ident.to_string()) {
                Some(Binding::Stored(field)) => {
                    let l = names::l();
                    let read = quote!((#l.#field));
                    *expr = syn::parse2(crate::rewrite::respan(
                        read,
                        crate::rewrite::located(ident.span()),
                    ))
                    .expect("a field read");
                }
                Some(Binding::Forbidden) => {
                    self.error.get_or_insert(syn::Error::new(
                        ident.span(),
                        format!("`ended`'s sequence runs on its own: it cannot read `{ident}`"),
                    ));
                }
                _ => {}
            }
            return;
        }
        if let Expr::Call(call) = expr
            && is_ended(call)
        {
            if call.args.len() != 1 {
                self.error.get_or_insert(syn::Error::new(
                    call.span(),
                    "`ended` takes an element or a `seq!`: `ended(seq!(req -> delay(1) -> ack))`",
                ));
                return;
            }
            match self.lowerer.ended(&call.args[0], self.env) {
                Ok(j) => {
                    let e = names::e();
                    *expr = syn::parse_quote!((#e[#j]));
                }
                Err(error) => {
                    self.error.get_or_insert(error);
                }
            }
            return;
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_macro_mut(&mut self, mac: &mut Macro) {
        crate::rewrite::visit_macro_args(mac, |e| self.visit_expr_mut(e));
    }
}

/// The names a pattern binds.
fn pat_names(pat: &Pat) -> Vec<String> {
    struct Names(Vec<String>);
    impl<'ast> Visit<'ast> for Names {
        fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
            self.0.push(p.ident.to_string());
            syn::visit::visit_pat_ident(self, p);
        }
    }
    let mut names = Names(Vec::new());
    names.visit_pat(pat);
    names.0
}
