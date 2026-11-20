//! Procedural macros of veryl-tbf: modports, clocking blocks and their wait
//! macros (see `veryl_tbf_runtime::clocking`), components, sequences and
//! tests (see `veryl_tbf_core`), and constrained random (see
//! `veryl_tbf_random`).
//!
//! The generated code names the runtime through the crate the user depends
//! on: `veryl-tbf` (as `veryl_tbf::runtime`) or `veryl-tbf-runtime`.

mod clocking;
mod component;
mod random;
mod test;

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::Ident;

/// The path of the runtime crate as the calling crate reaches it.
pub(crate) fn runtime() -> TokenStream2 {
    if let Ok(found) = crate_name("veryl-tbf") {
        let name = match found {
            FoundCrate::Itself => Ident::new("veryl_tbf", Span::call_site()),
            FoundCrate::Name(name) => Ident::new(&name, Span::call_site()),
        };
        return quote!(::#name::runtime);
    }
    match crate_name("veryl-tbf-runtime") {
        Ok(FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote!(::#name)
        }
        // The runtime's own tests and examples.
        _ => quote!(::veryl_tbf_runtime),
    }
}

/// The path of the methodology crate as the calling crate reaches it.
pub(crate) fn core() -> TokenStream2 {
    if let Ok(found) = crate_name("veryl-tbf") {
        let name = match found {
            FoundCrate::Itself => Ident::new("veryl_tbf", Span::call_site()),
            FoundCrate::Name(name) => Ident::new(&name, Span::call_site()),
        };
        return quote!(::#name::core);
    }
    match crate_name("veryl-tbf-core") {
        Ok(FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote!(::#name)
        }
        _ => quote!(::veryl_tbf_core),
    }
}

/// The path of the constrained random crate as the calling crate reaches
/// it: `veryl_tbf::random`, or `veryl_tbf_random` used on its own.
pub(crate) fn random() -> TokenStream2 {
    if let Ok(found) = crate_name("veryl-tbf") {
        let name = match found {
            FoundCrate::Itself => Ident::new("veryl_tbf", Span::call_site()),
            FoundCrate::Name(name) => Ident::new(&name, Span::call_site()),
        };
        return quote!(::#name::random);
    }
    match crate_name("veryl-tbf-random") {
        Ok(FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote!(::#name)
        }
        _ => quote!(::veryl_tbf_random),
    }
}

/// Marks a component's struct and its `impl Component` with the component's
/// kind: `#[component(driver)]` on both (they must agree). On the struct it
/// also implements `HasContext` for a field marked `#[context]`; on the impl
/// it sets the kind, checks that a test has a `run`, and with `passive`
/// (`#[component(scoreboard, passive)]`) that the component has none.
/// `#[component(sequencer(item = T))]` on a struct generates a whole
/// sequencer (`response = R` for a response of another type).
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(component::component(attr.into(), item.into()))
}

/// Makes a struct a sequence running on `sequencer`, with the protocol's
/// fields `base` (`#[sequence(sequencer = ApbSequencer, base = ApbSeqBase)]`):
/// inserts the core's fields and the base, implements `SequenceCore` and
/// `HasBase`, and generates `new` over the struct's own fields. The struct's
/// `impl Sequence` gives the body.
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(component::sequence(attr.into(), item.into()))
}

/// Marks a sequence item (an item, or a monitor's transaction, shared as an
/// `ItemHandle`) and defines its handle's alias: `#[sequence_item] struct
/// BusItem` adds `type BusItemHandle = ItemHandle<BusItem>`, as visible as
/// the item; `#[sequence_item(handle = Name)]` names it otherwise. The struct
/// is left as it is.
#[proc_macro_attribute]
pub fn sequence_item(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(component::sequence_item(attr.into(), item.into()))
}

/// Registers a test, which the test module starts by name
/// (`tb.start("SlowWriteTest")`): `register_test!(RegTest)` registers the
/// test type under its own name with no spec; `register_test!(SlowWriteTest,
/// RegTest, SlowWrites)` registers a test type with a spec (one spec, or a
/// `BundledSpecs`) under a name of its own.
#[proc_macro]
pub fn register_test(input: TokenStream) -> TokenStream {
    expand(test::register_test(input.into()))
}

fn expand(r: syn::Result<TokenStream2>) -> TokenStream {
    r.unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Declares the members of an interface's modport as value fields:
/// `#[input]` fields are sampled, `#[output]` fields are driven. Each field
/// binds to the modport member of its name. Use it as a `#[modport]` field
/// of a `#[clocking]` struct.
#[proc_macro_attribute]
pub fn modport(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(clocking::modport(attr.into(), item.into()))
}

/// Makes the struct a clocking block over its `#[modport]` fields.
/// Generates `new(clock, <one ModportRef per modport field>)`, `tick`,
/// `drive` and the waits `posedge`, `negedge`, `edge`, `changed`, `wait`,
/// `tick_until` and `tick_while`.
#[proc_macro_attribute]
pub fn clocking(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(clocking::clocking(attr.into(), item.into()))
}

/// Waits until a `bool` over a clocking block goes from false to true at a
/// clock edge, like SystemVerilog's `@(posedge cb.done)`:
/// `posedge!(cb.bus.done)`. Stands for `cb.posedge(|c| c.bus.done).await`,
/// reading only `bus.done` while it waits. Never times out;
/// `posedge_within!` limits the wait.
#[proc_macro]
pub fn posedge(input: TokenStream) -> TokenStream {
    expand(clocking::wait("posedge", input.into(), false))
}

/// Waits until a `bool` over a clocking block goes from true to false at a
/// clock edge, like `@(negedge cb.done)`. See `posedge!`.
#[proc_macro]
pub fn negedge(input: TokenStream) -> TokenStream {
    expand(clocking::wait("negedge", input.into(), false))
}

/// Waits until a `bool` over a clocking block changes either way at a clock
/// edge, like `@(edge cb.done)`. See `posedge!`.
#[proc_macro]
pub fn edge(input: TokenStream) -> TokenStream {
    expand(clocking::wait("edge", input.into(), false))
}

/// Waits until a value over a clocking block changes at a clock edge, like
/// `@(cb.state)`: `changed!(cb.state)`, or `changed!((cb.a, cb.b))`. See
/// `posedge!`.
#[proc_macro]
pub fn changed(input: TokenStream) -> TokenStream {
    expand(clocking::wait("changed", input.into(), false))
}

/// Ticks, and goes on ticking until a `bool` over a clocking block holds at
/// an edge, like `@(cb iff (cb.bus.ready))`: at least one tick. See
/// `posedge!`.
#[proc_macro]
pub fn tick_until(input: TokenStream) -> TokenStream {
    expand(clocking::wait("tick_until", input.into(), false))
}

/// Ticks, and goes on ticking while a `bool` over a clocking block holds at
/// an edge, like `do @(cb); while (cb.bus.busy)`. See `posedge!`.
#[proc_macro]
pub fn tick_while(input: TokenStream) -> TokenStream {
    expand(clocking::wait("tick_while", input.into(), false))
}

/// `posedge!` limited to a number of clock edges:
/// `posedge_within!(cb.bus.done, 8)?`. Returns
/// `Result<(), ClockingTimeout>`, naming the wait by its expression.
#[proc_macro]
pub fn posedge_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("posedge", input.into(), true))
}

/// `negedge!` limited to a number of clock edges. See `posedge_within!`.
#[proc_macro]
pub fn negedge_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("negedge", input.into(), true))
}

/// `edge!` limited to a number of clock edges. See `posedge_within!`.
#[proc_macro]
pub fn edge_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("edge", input.into(), true))
}

/// `changed!` limited to a number of clock edges. See `posedge_within!`.
#[proc_macro]
pub fn changed_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("changed", input.into(), true))
}

/// `tick_until!` limited to a number of clock edges. See `posedge_within!`.
#[proc_macro]
pub fn tick_until_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("tick_until", input.into(), true))
}

/// `tick_while!` limited to a number of clock edges. See `posedge_within!`.
#[proc_macro]
pub fn tick_while_within(input: TokenStream) -> TokenStream {
    expand(clocking::wait("tick_while", input.into(), true))
}

/// Makes a struct randomizable: `#[rand]` fields are random variables,
/// the others are state. See `veryl_tbf_random`.
#[proc_macro_attribute]
pub fn randomize(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::randomize(attr.into(), item.into()))
}

/// The constraint blocks of a random struct (`#[constraints] impl T`), or
/// of a constraint set (`#[constraints(for T)]`).
#[proc_macro_attribute]
pub fn constraints(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::constraints(attr.into(), item.into()))
}

/// A function written in constraint syntax, expanded where it is called.
#[proc_macro_attribute]
pub fn constraint_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(random::constraint_fn(attr.into(), item.into()))
}

/// Lets a fieldless enum be a random variable: it takes only its variants.
#[proc_macro_derive(RandEnum)]
pub fn rand_enum(item: TokenStream) -> TokenStream {
    expand(random::rand_enum(item.into()))
}

/// `randomize_with!(obj, { constraints })`: SV `randomize() with`.
#[proc_macro]
pub fn randomize_with(input: TokenStream) -> TokenStream {
    expand(random::randomize_with(input.into()))
}

/// `std_randomize!([x, y], { constraints })`: randomizes local variables.
#[proc_macro]
pub fn std_randomize(input: TokenStream) -> TokenStream {
    expand(random::std_randomize(input.into()))
}

/// `randcase! { 3 => a(), 1 => b() }`: runs one branch, picked by weight.
#[proc_macro]
pub fn randcase(input: TokenStream) -> TokenStream {
    expand(random::randcase(input.into()))
}
