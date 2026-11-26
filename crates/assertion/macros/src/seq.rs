//! `seq!`: a sequence's elements, chained by `->`, parsed into a tree:
//! booleans, `let`s, `delay`, `wait`, `happen`, `repeat`, `throughout` and
//! `until`; `repeat` and `throughout` over a sequence of their own (one
//! element, or a `seq!`).

use proc_macro2::{Spacing, Span, TokenStream as TokenStream2, TokenTree};
use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Ident, Lit, RangeLimits, Token, Type};

use crate::named::Named;

/// An element of a sequence. (Sizes do not matter: these live only while
/// a macro expands.)
#[allow(clippy::large_enum_variant)]
pub(crate) enum Node {
    /// A boolean: the branch goes on where it holds.
    Bool(Expr),
    /// `let name: ty = expr`: a value kept with the branch.
    Let { name: Ident, ty: Type, expr: Expr },
    /// `delay(n)`, `delay(m..=n)`, `delay(m..)`.
    Delay(Count),
    /// `wait(b)`: until `b` holds, this cycle included.
    Wait(Expr),
    /// `happen(n, b)`: until `b` has held `n` times.
    Happen(Count, Expr),
    /// `repeat(n, s)`: `s` `n` times, a cycle between.
    Repeat(Count, Vec<Node>),
    /// `throughout(a, s)`: `s`, with `a` at each of its cycles.
    Throughout(Expr, Vec<Node>),
    /// `until(a, b)`: until `b` holds, this cycle included, with `a` at
    /// each cycle before.
    Until(Expr, Expr),
    /// A named sequence's body, expanded: its elements in place.
    Group(Vec<Node>),
}

/// A count or a range of them: `n`, `m..=n`, `m..` (`max` is `None`).
#[derive(Clone, Copy)]
pub(crate) struct Count {
    pub min: usize,
    pub max: Option<usize>,
}

impl Count {
    /// The NFA states a delay takes: one per cycle counted, the last one
    /// staying for an unbounded delay.
    pub fn delay_states(&self) -> usize {
        match self.max {
            Some(max) => max,
            None => self.min.max(1),
        }
    }
}

/// Properties ending a body, not elements of a sequence.
const STATEMENTS: &[&str] = &["eventually", "never"];

/// Parses `seq!`'s tokens, expanding the named sequences of `named`.
pub(crate) fn parse(tokens: TokenStream2, named: &Named) -> syn::Result<Vec<Node>> {
    let elements = split_arrows(tokens);
    if elements.len() == 1 && elements[0].is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "`seq!` takes elements chained by `->`",
        ));
    }
    elements
        .into_iter()
        .map(|element| {
            if element.is_empty() {
                return Err(syn::Error::new(
                    Span::call_site(),
                    "an element is missing around `->`",
                ));
            }
            element_node(syn::parse2::<Element>(element)?, named)
        })
        .collect()
}

/// Splits at the top-level `->`s (a `-` joined to a `>`).
fn split_arrows(tokens: TokenStream2) -> Vec<TokenStream2> {
    let mut parts = vec![Vec::new()];
    let mut tokens = tokens.into_iter().peekable();
    while let Some(tt) = tokens.next() {
        if let TokenTree::Punct(p) = &tt
            && p.as_char() == '-'
            && p.spacing() == Spacing::Joint
            && matches!(tokens.peek(), Some(TokenTree::Punct(q)) if q.as_char() == '>')
        {
            tokens.next();
            parts.push(Vec::new());
            continue;
        }
        parts.last_mut().expect("a part").push(tt);
    }
    parts.into_iter().map(|p| p.into_iter().collect()).collect()
}

/// An element as written: a `let`, or an expression.
#[allow(clippy::large_enum_variant)]
enum Element {
    Let { name: Ident, ty: Type, expr: Expr },
    Expr(Expr),
}

fn element_node(element: Element, named: &Named) -> syn::Result<Node> {
    match element {
        Element::Let { name, ty, expr } => Ok(Node::Let { name, ty, expr }),
        Element::Expr(expr) => node(expr, named),
    }
}

impl Parse for Element {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Token![let]) {
            input.parse::<Token![let]>()?;
            let name: Ident = input.parse()?;
            if !input.peek(Token![:]) {
                return Err(syn::Error::new(
                    name.span(),
                    format!(
                        "a `let` in `seq!` has its type written: `let {name}: u32 = ..`; \
                         its value is kept across cycles"
                    ),
                ));
            }
            input.parse::<Token![:]>()?;
            let ty: Type = input.parse()?;
            input.parse::<Token![=]>()?;
            let expr: Expr = input.parse()?;
            return Ok(Element::Let { name, ty, expr });
        }
        Ok(Element::Expr(input.parse()?))
    }
}

/// An element written as an expression.
fn node(expr: Expr, named: &Named) -> syn::Result<Node> {
    if let Expr::Macro(m) = &expr
        && m.mac.path.is_ident("seq")
    {
        return Err(syn::Error::new(
            m.span(),
            "a `seq!` is not nested in another's chain; its elements chain directly",
        ));
    }
    let Expr::Call(call) = &expr else {
        return Ok(Node::Bool(expr));
    };
    let Expr::Path(func) = &*call.func else {
        return Ok(Node::Bool(expr));
    };
    let Some(ident) = func.path.get_ident() else {
        return Ok(Node::Bool(expr));
    };
    let name = ident.to_string();
    let args: Vec<&Expr> = call.args.iter().collect();
    let usage = |what: &str| syn::Error::new(call.span(), what.to_string());
    if let Some(item) = named.sequences.get(&name) {
        // A named sequence: its body in place, after the check of the
        // arguments' types.
        let _expanding = named.enter(item, call.span())?;
        let tokens = named.expand(item, &args, call.span())?;
        let owned: Vec<Expr> = args.iter().map(|a| (*a).clone()).collect();
        let check = named.check(item, &owned);
        let mut nodes = vec![Node::Bool(syn::parse_quote!({ #check true }))];
        nodes.extend(parse(tokens, named)?);
        return Ok(Node::Group(nodes));
    }
    match name.as_str() {
        "delay" => match args.as_slice() {
            [n] => Ok(Node::Delay(count(n, 0, "a delay")?)),
            _ => Err(usage(
                "`delay` takes a count or a range: `delay(1)`, `delay(1..=3)`, `delay(1..)`",
            )),
        },
        "wait" => match args.as_slice() {
            [b] => Ok(Node::Wait((*b).clone())),
            _ => Err(usage("`wait` takes a boolean: `wait(ack)`")),
        },
        "happen" => match args.as_slice() {
            [n, b] => Ok(Node::Happen(repetitions(n, "`happen`")?, (*b).clone())),
            _ => Err(usage(
                "`happen` takes a count or a range, and a boolean: `happen(3, ack)`",
            )),
        },
        "repeat" => match args.as_slice() {
            [n, s] => Ok(Node::Repeat(
                repetitions(n, "`repeat`")?,
                sequence(s, named)?,
            )),
            _ => Err(usage(
                "`repeat` takes a count or a range, and an element or a `seq!`: \
                 `repeat(1.., busy)`",
            )),
        },
        "throughout" => match args.as_slice() {
            [a, s] => Ok(Node::Throughout((*a).clone(), sequence(s, named)?)),
            _ => Err(usage(
                "`throughout` takes a boolean, and an element or a `seq!`: \
                 `throughout(busy, wait(done))`",
            )),
        },
        "until" => match args.as_slice() {
            [a, b] => Ok(Node::Until((*a).clone(), (*b).clone())),
            _ => Err(usage("`until` takes two booleans: `until(busy, done)`")),
        },
        _ if STATEMENTS.contains(&name.as_str()) => Err(syn::Error::new(
            ident.span(),
            format!("`{name}(..)` ends a property's body; it is not an element of `seq!`"),
        )),
        _ => Ok(Node::Bool(expr)),
    }
}

/// A sequence argument: one element, or a `seq!`.
pub(crate) fn sequence(arg: &Expr, named: &Named) -> syn::Result<Vec<Node>> {
    if let Expr::Macro(m) = arg
        && m.mac.path.is_ident("seq")
    {
        return parse(m.mac.tokens.clone(), named);
    }
    let element = syn::parse2::<Element>(arg.to_token_stream())?;
    Ok(vec![element_node(element, named)?])
}

fn literal(expr: &Expr) -> syn::Result<usize> {
    if let Expr::Lit(lit) = expr
        && let Lit::Int(int) = &lit.lit
    {
        return int.base10_parse();
    }
    Err(syn::Error::new(
        expr.span(),
        "a count is an integer literal",
    ))
}

/// `n`, `m..=n` or `m..`, each at least `least`.
fn count(arg: &Expr, least: usize, what: &str) -> syn::Result<Count> {
    let count = match arg {
        Expr::Range(range) => {
            let min = match &range.start {
                Some(start) => literal(start)?,
                None => 0,
            };
            let max = match (&range.limits, &range.end) {
                (RangeLimits::Closed(_), Some(end)) => Some(literal(end)?),
                (RangeLimits::HalfOpen(_), None) => None,
                _ => {
                    return Err(syn::Error::new(range.span(), "a range is `m..=n` or `m..`"));
                }
            };
            if let Some(max) = max
                && max < min
            {
                return Err(syn::Error::new(range.span(), "an empty range"));
            }
            Count { min, max }
        }
        other => {
            let n = literal(other)?;
            Count {
                min: n,
                max: Some(n),
            }
        }
    };
    if count.min < least {
        return Err(syn::Error::new(
            arg.span(),
            format!("{what}'s count is {least} or more"),
        ));
    }
    Ok(count)
}

/// A repetition's count: a range may start at 0 (none at all: the element
/// is skipped, and what follows goes on at the same cycle), but a count is
/// 1 or more.
fn repetitions(arg: &Expr, what: &str) -> syn::Result<Count> {
    let count = count(arg, 0, what)?;
    if count.max == Some(0) {
        return Err(syn::Error::new(
            arg.span(),
            format!("{what}'s count is 1 or more; a range may start at 0 (`0..=2`)"),
        ));
    }
    Ok(count)
}
