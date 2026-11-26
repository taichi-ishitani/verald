//! Parsing the fields of a signal struct (`#[input]`, `#[output]`,
//! `#[modport]`, `#[clock]`, `#[reset]`) and the code shared by the structs generated over them:
//! `#[modport]` here, `#[clocking]` in the testbench's macros.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Field, Fields, Ident, ItemStruct, Type};

/// The role a field attribute gives a field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Input,
    Output,
    Modport,
    /// The block's clock (`#[clock] clk: Clock`).
    Clock,
    /// The block's reset (`#[reset] rst: Reset`).
    Reset,
}

pub struct FieldSpec {
    pub ident: Ident,
    pub ty: Type,
    pub role: Role,
    /// The member name the field binds to.
    pub name: String,
    /// An output's initial value (`#[output(init = ..)]`), driven by `init`.
    pub init: Option<Expr>,
}

/// The name of the hidden field generated code keeps its state in.
pub const HIDDEN: &str = "__verald";

/// Removes the role attribute of each field and returns the fields' specs.
pub fn take_fields(item: &mut ItemStruct, roles: &[Role]) -> syn::Result<Vec<FieldSpec>> {
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

/// Removes the tagged fields of a struct and returns their specs, leaving
/// the fields without a role attribute (an assertion block's auxiliary
/// state) in the struct.
pub fn take_tagged_fields(item: &mut ItemStruct, roles: &[Role]) -> syn::Result<Vec<FieldSpec>> {
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "expected a struct with named fields",
        ));
    };
    let mut specs = Vec::new();
    let mut rest = syn::punctuated::Punctuated::new();
    for mut field in std::mem::take(&mut fields.named) {
        match field_role(&mut field, roles)? {
            Some((role, init)) => {
                let ident = field.ident.clone().expect("named field");
                specs.push(FieldSpec {
                    name: ident.unraw().to_string(),
                    ident,
                    ty: field.ty.clone(),
                    role,
                    init,
                });
            }
            None => rest.push(field),
        }
    }
    fields.named = rest;
    Ok(specs)
}

fn take_role(field: &mut Field, roles: &[Role]) -> syn::Result<(Role, Option<Expr>)> {
    field_role(field, roles)?.ok_or_else(|| {
        let names: Vec<&str> = roles
            .iter()
            .map(|role| match role {
                Role::Input => "`#[input]`",
                Role::Output => "`#[output]`",
                Role::Modport => "`#[modport]`",
                Role::Clock => "`#[clock]`",
                Role::Reset => "`#[reset]`",
            })
            .collect();
        let expected = match names.split_last() {
            Some((last, [])) => last.to_string(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        };
        syn::Error::new(field.span(), format!("the field needs {expected}"))
    })
}

/// Removes the field's role attribute and returns the role (and an
/// output's initial value); `None` for a field with none.
fn field_role(field: &mut Field, roles: &[Role]) -> syn::Result<Option<(Role, Option<Expr>)>> {
    let mut role = None;
    let mut init = None;
    let mut rest = Vec::new();
    for attr in field.attrs.drain(..) {
        let this = [
            ("input", Role::Input),
            ("output", Role::Output),
            ("modport", Role::Modport),
            ("clock", Role::Clock),
            ("reset", Role::Reset),
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
    Ok(role.map(|role| (role, init)))
}

/// Binds the fields' values: an output's initial value where it has one
/// (taken as the value last driven, so it is not driven until `init` or a
/// change), else zeros. `members` holds the bound members.
pub fn initial_values(specs: &[&FieldSpec]) -> Vec<TokenStream2> {
    specs
        .iter()
        .map(|f| {
            let (ident, ty) = (&f.ident, &f.ty);
            let local = format_ident!("__verald_v_{}", ident.unraw());
            match &f.init {
                Some(expr) => quote! {
                    let #local: #ty = #expr;
                    members.#ident.assume(&#local);
                },
                None if f.role == Role::Input => quote!(let #local = members.#ident.zero();),
                None => quote!(let #local = members.#ident.initial();),
            }
        })
        .collect()
}

/// Assigns and drives the initial values of the outputs that have one.
pub fn init_body(specs: &[&FieldSpec], hidden: &Ident) -> Vec<TokenStream2> {
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

pub fn check_plain(attr: &TokenStream2, item: &ItemStruct, what: &str) -> syn::Result<()> {
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
pub fn push_hidden_field(item: &mut ItemStruct, ty: &Ident) {
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
pub fn load_method(field: &str) -> Ident {
    format_ident!("__verald_load_{}", field)
}
