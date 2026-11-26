//! Rewriting the methods of an assertion block's impl: `check!` in hooks
//! into a report, bare signal names into reads of the sampled values, and
//! `past` and the functions built on it into reads of earlier samples.

use std::collections::{HashMap, HashSet};

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Block, Expr, FnArg, Ident, ImplItemFn, Macro, Pat, Stmt, Token};

use crate::assertion_crate;
use crate::block::{Kind, history_param, signals_param};

/// Where `span` is in the source, as verald-assertion's `Location`: the
/// crate's name and the file's path from the crate's directory
/// (`regblock_assertions/src/lib.rs`), and the line, fixed at compile time.
/// `file!()` alone is relative to wherever rustc runs (a workspace's root),
/// so the path is made here from the span's file and the crate's directory.
pub(crate) fn location(span: Span) -> TokenStream2 {
    let a = assertion_crate();
    let file = crate_relative_file(span)
        .map(|file| quote!(#file))
        .unwrap_or_else(|| quote_spanned!(span=> ::core::file!()));
    let line = quote_spanned!(span=> ::core::line!());
    quote!(#a::Location { file: #file, line: #line })
}

/// `<crate>/<path from the crate's directory>` of the file `span` is in,
/// when the compiler knows it.
fn crate_relative_file(span: Span) -> Option<String> {
    let file = span.unwrap().local_file()?;
    // Relative to where rustc runs, as `file!()` is.
    let file = match file.is_absolute() {
        true => file,
        false => std::env::current_dir().ok()?.join(file),
    };
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let krate = std::env::var("CARGO_PKG_NAME").ok()?;
    let path = file.strip_prefix(&dir).unwrap_or(&file);
    Some(format!("{krate}/{}", path.display()))
}

/// A span in the macro's context (as hygiene and lints see it), located
/// at `span` (as errors point): for code the macro writes in place of the
/// user's, so that rustc points an error at what the user wrote, while the
/// macro's own names resolve and its parentheses are not linted.
pub(crate) fn located(span: Span) -> Span {
    Span::call_site().located_at(span)
}

/// `tokens` with every token's span `span`.
pub(crate) fn respan(tokens: TokenStream2, span: Span) -> TokenStream2 {
    tokens
        .into_iter()
        .map(|tt| match tt {
            proc_macro2::TokenTree::Group(g) => {
                let mut group = proc_macro2::Group::new(g.delimiter(), respan(g.stream(), span));
                group.set_span(span);
                proc_macro2::TokenTree::Group(group)
            }
            mut other => {
                other.set_span(span);
                other
            }
        })
        .collect()
}

/// Whether `mac` is the macro `name`, however its path is written
/// (`check!`, `::core::stringify!`).
pub(crate) fn is_macro(mac: &Macro, name: &str) -> bool {
    mac.path.segments.last().is_some_and(|s| s.ident == name)
}

/// Rewrites `check!(cond)` and `check!(cond, "format", args..)` in a hook
/// into a report through `report`.
pub(crate) fn lower_checks(block: &mut Block, report: &Ident) -> syn::Result<()> {
    let mut lower = LowerChecks {
        report,
        error: None,
    };
    lower.visit_block_mut(block);
    match lower.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct LowerChecks<'a> {
    report: &'a Ident,
    error: Option<syn::Error>,
}

impl LowerChecks<'_> {
    fn lower(&mut self, mac: &Macro) -> Option<Expr> {
        let args = match Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone())
        {
            Ok(args) => args,
            Err(error) => {
                self.error.get_or_insert(error);
                return None;
            }
        };
        let mut args = args.into_iter();
        let Some(cond) = args.next() else {
            self.error
                .get_or_insert(syn::Error::new(mac.span(), "`check!` takes a condition"));
            return None;
        };
        let rest: Vec<Expr> = args.collect();
        let report = self.report;
        let location = location(mac.path.span());
        let message = if rest.is_empty() {
            quote!(::std::string::String::new())
        } else {
            quote!(::std::format!(#(#rest),*))
        };
        Some(syn::parse_quote! {
            if !(#cond) {
                #report.check_failed(::core::stringify!(#cond), &#message, #location);
            }
        })
    }
}

impl VisitMut for LowerChecks<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Macro(m) = expr
            && is_macro(&m.mac, "check")
        {
            if let Some(lowered) = self.lower(&m.mac) {
                *expr = lowered;
            }
            return;
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_stmt_mut(&mut self, stmt: &mut Stmt) {
        if let Stmt::Macro(m) = stmt
            && is_macro(&m.mac, "check")
        {
            if let Some(lowered) = self.lower(&m.mac) {
                *stmt = Stmt::Expr(lowered, None);
            }
            return;
        }
        syn::visit_mut::visit_stmt_mut(self, stmt);
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

/// Rewrites a method's bare signal names into reads of the sampled values
/// (`valid` into `__verald_s.valid`), `past` and the functions built on it
/// into reads of earlier samples, and calls of the impl's methods to pass
/// the samples (and a hook's report) on. Returns how many edges back
/// `past` reads.
pub(crate) fn rewrite_names(
    method: &mut ImplItemFn,
    signals: &[String],
    kinds: &HashMap<String, Kind>,
) -> syn::Result<usize> {
    let mut params = HashSet::new();
    for input in &method.sig.inputs {
        if let FnArg::Typed(t) = input {
            params.extend(pat_names(&t.pat));
        }
    }
    let mut rewrite = Names {
        signals,
        kinds,
        scopes: vec![params],
        past: None,
        depth: 0,
        error: None,
    };
    rewrite.visit_block_mut(&mut method.block);
    match rewrite.error {
        Some(error) => Err(error),
        None => Ok(rewrite.depth),
    }
}

struct Names<'a> {
    signals: &'a [String],
    kinds: &'a HashMap<String, Kind>,
    scopes: Vec<HashSet<String>>,
    /// Within `past(x, n)`'s `x`: `n`, the samples names are read from.
    past: Option<usize>,
    /// The most edges back a `past` reads.
    depth: usize,
    error: Option<syn::Error>,
}

/// The functions on earlier samples.
const PAST_FNS: &[&str] = &["past", "rose", "fell", "stable", "changed"];

impl Names<'_> {
    fn shadowed(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn bind(&mut self, pat: &Pat) {
        let names = pat_names(pat);
        self.scopes.last_mut().expect("a scope").extend(names);
    }

    fn with_scope(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(HashSet::new());
        f(self);
        self.scopes.pop();
    }

    fn error(&mut self, error: syn::Error) {
        self.error.get_or_insert(error);
    }

    /// The extra arguments a call of the impl's method `name` takes.
    fn extra_args(&self, name: &str) -> Option<Vec<Expr>> {
        let h = history_param();
        let r = crate::block::report_param();
        match self.kinds.get(name)? {
            Kind::OnClock | Kind::OnReset => {
                Some(vec![syn::parse_quote!(#h), syn::parse_quote!(#r)])
            }
            Kind::Helper => Some(vec![syn::parse_quote!(#h)]),
            Kind::Assert | Kind::Sequence | Kind::Property => None,
        }
    }

    /// `past(x)`, `past(x, n)`, `rose(x)`, `fell(x)`, `stable(x)` and
    /// `changed(x)`, rewritten; `None` if `call` is not one of them.
    fn past_call(&mut self, call: &syn::ExprCall) -> Option<Expr> {
        let Expr::Path(func) = &*call.func else {
            return None;
        };
        let ident = func.path.get_ident()?;
        let name = ident.to_string();
        if !PAST_FNS.contains(&name.as_str()) || self.shadowed(&name) {
            return None;
        }
        if self.past.is_some() {
            self.error(syn::Error::new(
                ident.span(),
                format!("`{name}` within `past`'s argument: write `past(x, n)` for earlier edges"),
            ));
            return Some(Expr::Call(call.clone()));
        }
        let args: Vec<&Expr> = call.args.iter().collect();
        let (arg, n) = match (name.as_str(), args.as_slice()) {
            ("past", [x]) => (*x, 1),
            ("past", [x, n]) => {
                let n = match n {
                    Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Int(int),
                        ..
                    }) => int.base10_parse::<usize>().ok().filter(|n| *n >= 1),
                    _ => None,
                };
                let Some(n) = n else {
                    self.error(syn::Error::new(
                        args[1].span(),
                        "`past`'s count is an integer literal, 1 or more",
                    ));
                    return Some(Expr::Call(call.clone()));
                };
                (*x, n)
            }
            (_, [x]) if name != "past" => (*x, 1),
            _ => {
                self.error(syn::Error::new(
                    call.span(),
                    if name == "past" {
                        "`past` takes a value and a count: `past(x)`, `past(x, 2)`".to_string()
                    } else {
                        format!("`{name}` takes a value: `{name}(x)`")
                    },
                ));
                return Some(Expr::Call(call.clone()));
            }
        };
        if let Some(error) = not_signals(arg) {
            self.error(error);
            return Some(Expr::Call(call.clone()));
        }
        self.depth = self.depth.max(n);
        let mut now = arg.clone();
        self.visit_expr_mut(&mut now);
        let mut before = arg.clone();
        self.past = Some(n);
        self.visit_expr_mut(&mut before);
        self.past = None;
        let a = assertion_crate();
        let lsb = quote!(#a::__private::lsb);
        Some(match name.as_str() {
            "past" => syn::parse_quote!((#before)),
            "rose" => syn::parse_quote!((#lsb(&(#now)) && !#lsb(&(#before)))),
            "fell" => syn::parse_quote!((!#lsb(&(#now)) && #lsb(&(#before)))),
            "stable" => syn::parse_quote!(((#now) == (#before))),
            _ => syn::parse_quote!(((#now) != (#before))),
        })
    }
}

/// An error if `past`'s argument reads anything but signals: auxiliary
/// state (`self`) or a `let` kept across cycles.
fn not_signals(arg: &Expr) -> Option<syn::Error> {
    struct Find(Option<syn::Error>);
    impl<'ast> Visit<'ast> for Find {
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            if let Some(ident) = p.path.get_ident() {
                if ident == "self" {
                    self.0.get_or_insert(syn::Error::new(
                        ident.span(),
                        "`past` reads signals only, not the block's state",
                    ));
                } else if ident == "__verald_l" {
                    self.0.get_or_insert(syn::Error::new(
                        ident.span(),
                        "`past` reads signals only, not a `let`",
                    ));
                }
            }
        }
    }
    let mut find = Find(None);
    find.visit_expr(arg);
    find.0
}

fn is_self(expr: &Expr) -> bool {
    matches!(expr, Expr::Path(p) if p.path.is_ident("self"))
}

/// Visits the arguments of a macro that parse as expressions (`format!`,
/// `assert!`); `stringify!` is left as written, for reports.
pub(crate) fn visit_macro_args(mac: &mut Macro, mut f: impl FnMut(&mut Expr)) {
    if is_macro(mac, "stringify") {
        return;
    }
    let Ok(mut args) = Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone())
    else {
        return;
    };
    for arg in args.iter_mut() {
        f(arg);
    }
    mac.tokens = quote!(#args);
}

impl VisitMut for Names<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Path(p) if p.qself.is_none() => {
                if let Some(ident) = p.path.get_ident() {
                    let name = ident.to_string();
                    if self.signals.contains(&name) && !self.shadowed(&name) {
                        // Located at the name, in the macro's context.
                        let read = match self.past {
                            None => {
                                let s = signals_param();
                                quote!((#s.#ident))
                            }
                            Some(n) => {
                                let a = assertion_crate();
                                let h = history_param();
                                quote!((#a::History::past(#h, #n).#ident))
                            }
                        };
                        *expr =
                            syn::parse2(respan(read, located(ident.span()))).expect("a field read");
                    }
                }
            }
            Expr::MethodCall(call) if is_self(&call.receiver) => {
                syn::visit_mut::visit_expr_method_call_mut(self, call);
                if let Some(extra) = self.extra_args(&call.method.to_string()) {
                    call.args.extend(extra);
                }
            }
            Expr::Call(call) => {
                // A property's `ended(..)` is rewritten before; one left is
                // elsewhere.
                if let Expr::Path(func) = &*call.func
                    && func.path.is_ident("ended")
                    && !self.shadowed("ended")
                {
                    self.error(syn::Error::new(
                        func.span(),
                        "`ended` is only allowed in a property (`#[assert]`)",
                    ));
                    return;
                }
                // A property's named sequences and properties are expanded
                // before; one left is elsewhere.
                if let Expr::Path(func) = &*call.func
                    && let Some(ident) = func.path.get_ident()
                    && matches!(
                        self.kinds.get(&ident.to_string()),
                        Some(Kind::Sequence | Kind::Property)
                    )
                    && !self.shadowed(&ident.to_string())
                {
                    self.error(syn::Error::new(
                        ident.span(),
                        format!(
                            "`{ident}` is a named sequence or property: it is used in a property"
                        ),
                    ));
                    return;
                }
                if let Some(lowered) = self.past_call(call) {
                    *expr = lowered;
                    return;
                }
                syn::visit_mut::visit_expr_call_mut(self, call);
                if let Expr::Path(func) = &*call.func
                    && func.path.segments.len() == 2
                    && func.path.segments[0].ident == "Self"
                    && let Some(extra) = self.extra_args(&func.path.segments[1].ident.to_string())
                {
                    call.args.extend(extra);
                }
            }
            Expr::Closure(closure) => {
                let inputs = closure.inputs.clone();
                self.with_scope(|this| {
                    for pat in &inputs {
                        this.bind(pat);
                    }
                    this.visit_expr_mut(&mut closure.body);
                });
            }
            Expr::ForLoop(f) => {
                self.visit_expr_mut(&mut f.expr);
                let pat = (*f.pat).clone();
                self.with_scope(|this| {
                    this.bind(&pat);
                    this.visit_block_mut(&mut f.body);
                });
            }
            Expr::Let(l) => {
                self.visit_expr_mut(&mut l.expr);
                let pat = (*l.pat).clone();
                self.bind(&pat);
            }
            Expr::Match(m) => {
                self.visit_expr_mut(&mut m.expr);
                for arm in &mut m.arms {
                    let pat = arm.pat.clone();
                    self.with_scope(|this| {
                        this.bind(&pat);
                        if let Some((_, guard)) = &mut arm.guard {
                            this.visit_expr_mut(guard);
                        }
                        this.visit_expr_mut(&mut arm.body);
                    });
                }
            }
            _ => syn::visit_mut::visit_expr_mut(self, expr),
        }
    }

    fn visit_block_mut(&mut self, block: &mut Block) {
        self.with_scope(|this| {
            for stmt in &mut block.stmts {
                this.visit_stmt_mut(stmt);
            }
        });
    }

    fn visit_local_mut(&mut self, local: &mut syn::Local) {
        if let Some(init) = &mut local.init {
            self.visit_expr_mut(&mut init.expr);
            if let Some((_, diverge)) = &mut init.diverge {
                self.visit_expr_mut(diverge);
            }
        }
        let pat = local.pat.clone();
        self.bind(&pat);
    }

    fn visit_macro_mut(&mut self, mac: &mut Macro) {
        visit_macro_args(mac, |arg| self.visit_expr_mut(arg));
    }
}
