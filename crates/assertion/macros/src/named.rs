//! Named sequences (`#[sequence]`) and properties (`#[property]`) of an
//! impl, expanded where they are used, as SVA's declarations are:
//!
//! - an argument is substituted by name, as an expression evaluated at
//!   each cycle it is referred to (SVA's formal arguments), not a value
//!   taken at the call;
//! - the body's `let`s are renamed, so that they do not capture the
//!   caller's names;
//! - recursion is rejected;
//! - the arguments' types are checked against the signature, by bindings
//!   to variables of the parameters' types in `if false`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use proc_macro2::{Delimiter, Group, Span, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::{Expr, Ident, Type};

/// A named sequence or property: its parameters, and its body as written
/// (a sequence's `seq!` tokens, a property's block).
pub(crate) struct Item {
    pub name: Ident,
    pub params: Vec<(Ident, Type)>,
    pub body: TokenStream2,
}

#[derive(Default)]
pub(crate) struct Named {
    pub sequences: HashMap<String, Item>,
    pub properties: HashMap<String, Item>,
    /// The properties that take time (hold a `seq!`, a property call such
    /// as `eventually`, or a call of one that does).
    pub temporal: HashSet<String>,
    /// The items being expanded, innermost last.
    stack: RefCell<Vec<String>>,
    /// Numbers the expansions, for the renamed `let`s.
    count: Cell<usize>,
}

/// Leaves an item's expansion when dropped.
pub(crate) struct Expanding<'a>(&'a Named);

impl Drop for Expanding<'_> {
    fn drop(&mut self) {
        self.0.stack.borrow_mut().pop();
    }
}

impl Named {
    /// Enters `item`'s expansion; an error if it is already being expanded
    /// (recursion).
    pub fn enter(&self, item: &Item, span: Span) -> syn::Result<Expanding<'_>> {
        let name = item.name.to_string();
        let mut stack = self.stack.borrow_mut();
        if stack.contains(&name) {
            let chain = stack
                .iter()
                .chain(std::iter::once(&name))
                .cloned()
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(syn::Error::new(
                span,
                format!("`{name}` is used within itself ({chain}): recursion is not allowed"),
            ));
        }
        stack.push(name);
        Ok(Expanding(self))
    }

    /// `item`'s body with its `let`s renamed and `args` in place of its
    /// parameters.
    pub fn expand(&self, item: &Item, args: &[&Expr], span: Span) -> syn::Result<TokenStream2> {
        if args.len() != item.params.len() {
            return Err(syn::Error::new(
                span,
                format!(
                    "`{}` takes {} argument{}",
                    item.name,
                    item.params.len(),
                    if item.params.len() == 1 { "" } else { "s" }
                ),
            ));
        }
        let n = self.count.get();
        self.count.set(n + 1);
        let mut lets = HashSet::new();
        let_names(item.body.clone(), &mut lets);
        let mut map: HashMap<String, (TokenStream2, bool)> = lets
            .into_iter()
            .map(|l| {
                let fresh = format_ident!("__verald_{}_{}_{}", item.name, n, l);
                (l, (quote!(#fresh), false))
            })
            .collect();
        let renamed = substitute(item.body.clone(), &map);
        map.clear();
        for ((param, _), arg) in item.params.iter().zip(args) {
            map.insert(param.to_string(), (quote!(#arg), !atomic(arg)));
        }
        Ok(substitute(renamed, &map))
    }

    /// The check of `args` against `item`'s parameter types, bound to
    /// variables of those types (by reference: a value may not be
    /// copied), in `if false`: checked by rustc, never evaluated. A type
    /// error notes the parameter's type as written.
    pub fn check(&self, item: &Item, args: &[Expr]) -> TokenStream2 {
        // The reference to the type carries the type's span, so that the
        // annotation is all the user's (rustc notes it then).
        let tys = item.params.iter().map(|(_, ty)| {
            let span = syn::spanned::Spanned::span(ty);
            quote::quote_spanned!(span=> &#ty)
        });
        // The argument's reference, located at the argument.
        let args = args.iter().map(|arg| {
            let span = crate::rewrite::located(syn::spanned::Spanned::span(arg));
            quote::quote_spanned!(span=> &(#arg))
        });
        quote! {
            if false {
                #(let _: #tys = #args;)*
            }
        }
    }
}

/// The names `let` binds in `tokens` (`let name`, `let mut name`).
fn let_names(tokens: TokenStream2, out: &mut HashSet<String>) {
    let mut tokens = tokens.into_iter().peekable();
    while let Some(tt) = tokens.next() {
        match &tt {
            TokenTree::Ident(i) if i == "let" => {
                if matches!(tokens.peek(), Some(TokenTree::Ident(m)) if m == "mut") {
                    tokens.next();
                }
                if let Some(TokenTree::Ident(name)) = tokens.peek() {
                    out.insert(name.to_string());
                }
            }
            TokenTree::Group(g) => let_names(g.stream(), out),
            _ => {}
        }
    }
}

/// Whether `expr` stands as one operand wherever it is put, needing no
/// parentheses (a name, a field, a call, a literal, ..).
fn atomic(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Path(_)
            | Expr::Lit(_)
            | Expr::Field(_)
            | Expr::MethodCall(_)
            | Expr::Call(_)
            | Expr::Paren(_)
            | Expr::Index(_)
            | Expr::Macro(_)
            | Expr::Tuple(_)
            | Expr::Array(_)
            | Expr::Block(_)
    )
}

/// `tokens` with each name in `map` replaced, in parentheses where its
/// flag says so, except where it is a field or a method (`x.name`) or a
/// path's segment (`a::name`).
fn substitute(tokens: TokenStream2, map: &HashMap<String, (TokenStream2, bool)>) -> TokenStream2 {
    let mut out = Vec::new();
    // The last two tokens, to tell `x.name` from `a..name`.
    let mut prev: [Option<char>; 2] = [None, None];
    for tt in tokens {
        let replaced = match &tt {
            TokenTree::Ident(i) => {
                let field = prev[1] == Some('.') && prev[0] != Some('.');
                let path = prev[1] == Some(':');
                match map.get(&i.to_string()) {
                    Some((with, parens)) if !field && !path => {
                        let mut with: Vec<TokenTree> = with.clone().into_iter().collect();
                        Some(if *parens {
                            let mut g =
                                Group::new(Delimiter::Parenthesis, with.into_iter().collect());
                            g.set_span(i.span());
                            TokenTree::Group(g)
                        } else if with.len() == 1 {
                            // A renamed `let`, or a name.
                            let mut one = with.pop().expect("one token");
                            if let TokenTree::Ident(fresh) = &mut one
                                && fresh.to_string().starts_with("__verald_")
                            {
                                fresh.set_span(i.span());
                            }
                            one
                        } else {
                            // An operand of its own, kept one token.
                            TokenTree::Group(Group::new(
                                Delimiter::None,
                                with.into_iter().collect(),
                            ))
                        })
                    }
                    _ => None,
                }
            }
            TokenTree::Group(g) => {
                let mut inner = Group::new(g.delimiter(), substitute(g.stream(), map));
                inner.set_span(g.span());
                Some(TokenTree::Group(inner))
            }
            _ => None,
        };
        prev = [
            prev[1],
            match &tt {
                TokenTree::Punct(p) => Some(p.as_char()),
                _ => None,
            },
        ];
        out.push(replaced.unwrap_or(tt));
    }
    out.into_iter().collect()
}

/// Whether `tokens` take time: a `seq!`, `eventually(..)`, `never(..)`,
/// `until(..)`, or a call of a property in `temporal`.
pub(crate) fn takes_time(tokens: TokenStream2, temporal: &HashSet<String>) -> bool {
    let mut tokens = tokens.into_iter().peekable();
    while let Some(tt) = tokens.next() {
        match &tt {
            TokenTree::Ident(i) => {
                let name = i.to_string();
                let next = tokens.peek();
                if name == "seq" && matches!(next, Some(TokenTree::Punct(p)) if p.as_char() == '!')
                {
                    return true;
                }
                let call = matches!(next, Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
                if call
                    && (["eventually", "never", "until"].contains(&name.as_str())
                        || temporal.contains(&name))
                {
                    return true;
                }
            }
            TokenTree::Group(g) if takes_time(g.stream(), temporal) => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(params: &[&str], body: TokenStream2) -> Item {
        Item {
            name: format_ident!("item"),
            params: params
                .iter()
                .map(|p| (format_ident!("{}", p), syn::parse_quote!(bool)))
                .collect(),
            body,
        }
    }

    #[test]
    fn arguments_replace_parameters_but_not_fields_or_paths() {
        let named = Named::default();
        let it = item(&["v", "r"], quote!(v -> wait(r) -> x.v && a::v && 0..r));
        let a: Expr = syn::parse_quote!(bus.valid);
        let b: Expr = syn::parse_quote!(x || y);
        let out = named.expand(&it, &[&a, &b], Span::call_site()).unwrap();
        // An operand of its own as it is; another in parentheses.
        assert_eq!(
            out.to_string(),
            quote!(bus.valid -> wait((x || y)) -> x.v && a::v && 0..(x || y)).to_string()
        );
    }

    #[test]
    fn lets_are_renamed() {
        let named = Named::default();
        let it = item(&["v"], quote!(let d: u8 = v -> delay(1) -> out == d));
        let a: Expr = syn::parse_quote!(d);
        let out = named.expand(&it, &[&a], Span::call_site()).unwrap();
        // The caller's `d` is not captured by the body's.
        assert_eq!(
            out.to_string(),
            quote!(let __verald_item_0_d: u8 = d -> delay(1) -> out == __verald_item_0_d)
                .to_string()
        );
    }

    #[test]
    fn what_takes_time() {
        let none = HashSet::new();
        assert!(takes_time(
            quote!({
                if a {
                    seq!(b);
                }
            }),
            &none
        ));
        assert!(takes_time(
            quote!({
                eventually(b);
            }),
            &none
        ));
        assert!(!takes_time(
            quote!({
                a && b;
            }),
            &none
        ));
        let named: HashSet<String> = ["p".to_string()].into();
        assert!(takes_time(
            quote!({
                p(a);
            }),
            &named
        ));
    }
}
