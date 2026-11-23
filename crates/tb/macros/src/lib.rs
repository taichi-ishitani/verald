//! Procedural macros of verald: modports, clocking blocks and their wait
//! macros (see `verald_tb_runtime::clocking`), components, sequences and
//! tests (see `verald_tb_core`), and constrained random (see
//! `verald_random`).
//!
//! The generated code names the runtime through the crate the user depends
//! on: `verald` (as `verald::tb::runtime`) or `verald-tb-runtime`.

mod clocking;
mod component;
mod test;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use verald_macros_codegen::paths;

/// The path of the testbench's runtime as the calling crate reaches it:
/// `verald::tb::runtime`, or `verald_tb_runtime` used on its own.
pub(crate) fn runtime() -> TokenStream2 {
    paths::path(
        quote!(tb::runtime),
        "verald-tb-runtime",
        "verald_tb_runtime",
    )
}

/// The path of the methodology crate as the calling crate reaches it:
/// `verald::tb::core`, or `verald_tb_core` used on its own.
pub(crate) fn core() -> TokenStream2 {
    paths::path(quote!(tb::core), "verald-tb-core", "verald_tb_core")
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
