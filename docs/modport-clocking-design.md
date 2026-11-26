# Modports and clocking blocks: design

> **Status:** implemented, except where marked *to change*. Typed modports
> (`#[modport]`, shared by the testbench and the assertions) and clocking
> blocks (`#[clocking]`, the testbench's), gathered from
> [design.md](design.md), [host-layer-design.md](host-layer-design.md) and
> [assertion-design.md](assertion-design.md) so that what both rest on is in
> one place.

## Contents

- [Typed modports](#typed-modports)
- [Clocking blocks](#clocking-blocks)
- [Clock and reset as fields](#clock-and-reset-as-fields)
- [Reading only what a wait needs](#reading-only-what-a-wait-needs)

## Typed modports

A `#[modport]` struct declares the members of an interface's modport as
value fields: `#[input]` fields are sampled, `#[output]` fields are driven.
Each field binds to the modport member of its name. It is used as a
`#[modport]` field of a `#[clocking]` struct, and of an assertion block
(see [assertion-design.md](assertion-design.md#reusing-users-modport-structs)).

`#[modport]` generates, for its struct:

- `ModportRead`: binding from a `MemberSource` (`__bind`, widths checked)
  and loading every input field (`__load`);
- `InputOnly`, a marker, when the struct has no `#[output]` field. An
  assertion block requires it of its `#[modport]` fields, so a modport
  for driving is rejected at compile time.

Storing the output fields, only those that changed, is clocking's: the
runtime's `ModportDrive` (`__store`, `__init`), over the `Driven<T>` the
macro generates for each output.
`#[modport]` is `verald-macros`', its generation `verald-macros-codegen`'s
(see [design.md](design.md#crate-layout)). It names the layer's crate
through `proc-macro-crate` (`verald::host`, `verald_host`, or the
runtime's re-export), and generates the parts driving outputs
(`Driven<T>`, `ModportDrive`) only where the testbench's runtime is
reached; without it, an `#[output]` field is an error.

What `#[clocking]` and the wait macros use of a modport struct, and so
what a struct generated otherwise (by Veryl, veryl-lang/veryl#3712) has
to give: `ModportRead` (`__bind`, `__load`), `ModportDrive` (`__store`,
`__init`), and a method per field reading that field alone
(`__verald_load_<field>`), with which a wait reads only what its
condition names.

The types it builds on (`InputSignal<T>`, `MemberSource`, `Signals`,
`SignalValue`) are the host layer's: see
[host-layer-design.md](host-layer-design.md#types).

## Clocking blocks

Decided (implemented in the runtime, the macros in `verald-tb-macros`); what
users see follows rustdv-veryl's clocking blocks:

- A `#[clocking]` struct is a clocking block, as SystemVerilog's
  `clocking cb @(posedge clk)`. Its fields are `#[input]` (sampled) and
  `#[output]` (driven) signals, and `#[modport]` fields, structs declaring a
  modport's members the same way. A signal need not belong to an interface
  (an interrupt, for example).
- `Cb::new(signals, clock, CbPaths::default())` binds each field by its name
  among what the test module registered: an interface (`set_interface`) for
  a modport field, whose members bind by their names; a signal registered
  with `set_input` / `set_output`, or an interface's member by a dotted path
  (`"bus.done"`), for a signal field. The generated `<Name>Paths` struct
  overrides the path of any field. `signals` is anything implementing
  `Signals`: `Start` now, the methodology's `Scope` later. `Signals` also
  gives the short forms `scope.clocking::<Cb>("clk")?` (every field at its
  own name, the clock by its registered name) and
  `scope.clocking_with("clk", CbPaths { .. })?` (the block's type following
  from its paths' type); `new` remains for a clock given as a `Clock`.
  Widths are checked
  against the fields' types at construction. Clocking stays on the
  component's side, not in Veryl (see
  [veryl-proposals.md](veryl-proposals.md#clocking-blocks-as-a-language-feature-a-study)),
  so the clock is given here.
- **To change**: the clock becomes a field (see
  [Clock and reset as fields](#clock-and-reset-as-fields)).
- Field types implement `SignalValue`: `bool`, the unsigned integers up to
  `u128` and `BigUint` (any width). Signals are two-state: X and Z read as
  0, as SystemVerilog's conversion to two-state types, and each type reads
  and writes itself through the host layer's direct paths (see
  [host-layer-design.md](host-layer-design.md#two-state-signals-first)).
- Fields start at zero. An output is written when it differs from the value
  last written (zero at first), so an output never assigned is never
  driven and the design's value stands; `tick` writes the changed outputs
  before waiting, `drive` writes them at once.
- An output may declare an initial value, its idle value:
  `#[output(init = false)] start: bool`, in a clocking block or a modport.
  The field starts at it, taken as already driven; `cb.init()` assigns
  every such output its value and drives it at once, whatever was last
  driven, so a reset brings the bus to idle in one call
  (`run_with_reset(rst, &mut cb, |cb| cb.init(), ..)`). An output
  declares it where it is declared: in its modport, or, for a signal of its
  own, in the clocking block. An output with no initial value has none
  (it is uninitialized, as an undriven signal is undefined), so `init`
  leaves it alone.
- `tick!(cb)`, and the waits `posedge`, `negedge`, `edge`, `changed`,
  `wait`, `tick_until`, `tick_while`, as methods taking a closure (reading
  every input at each edge) and as macros (reading only what the condition
  names while waiting, see
  [below](#reading-only-what-a-wait-needs)); the `_within` forms of the
  macros give up after a number of edges with a `ClockingTimeout`, which
  `?` turns into a fatal error naming the condition.
- The macros' generated code names the runtime through the crate the user
  depends on, `verald` (as `verald::tb::runtime`) or `verald-tb-runtime`.
- `run_with_reset(reset, state, on_reset, body)` is in the runtime: it runs
  `on_reset`, waits for the release, runs `body` until the next assertion
  (dropping it, with what it forked), and starts over.

## Clock and reset as fields

> **To change** for clocking blocks (decided with the assertions, not
> implemented); assertion blocks are designed this way from the start.

The clock and the reset are fields marked `#[clock]` and `#[reset]`, in
clocking blocks and assertion blocks alike:

```rust
#[clocking]
struct AdderCb {
    #[clock]   clk: Clock,
    #[modport] bus: AdderBus,
}

#[assertions]
struct FifoSva {
    #[clock] i_clk: Clock,
    #[reset] i_rst: Reset,
    #[input] i_push: bool,
}
```

- One clock, and at most one reset, of the prelude's `Clock` and `Reset`.
- They bind by name as the other fields do, and are overridden as they are
  (in `<Name>Paths`, or a bind's map). A clocking block is then made
  without a clock argument: `scope.clocking::<Cb>()` and
  `scope.clocking_with(CbPaths { clk: Some("clk2"), ..Default::default() })`.
- A clock or reset inside a `#[modport]` struct is not supported: an
  interface's clock is reached by mapping the field to it (`"bus.clk"`).

What should drive both is the clock the observed module or interface
receives. The definition names it as the observed thing does (`i_clk`),
and binding resolves the name: among the test module's registrations now,
where any clock signal it sees may be registered (the net on a DUT's
clock port, a clock the DUT outputs), and in the design once Veryl gives
probes, without changing the definition. Clocks only inside the DUT wait
for the probes.

Rejected:

- the clock's registered name given when a block is made or bound
  (`clocking::<Cb>("clk")`, an argument of `bind_to_instance`):
  a bind at every instance of a module could not give it;
- the clock named in an attribute (`#[assertions(clock = "clk")]`): a
  member outside the struct's fields, with a rule of its own.

## Reading only what a wait needs

Part of keeping the per-edge path light (see
[design.md](design.md#executor)): a clocking block keeps its members as
plain fields, and `tick` reads the inputs it declares, nothing else. A
conditional wait (`tick_until!`, `tick_while!`, `posedge!`, ..) reads,
while it waits, only the members its condition refers to, found in the
condition by the macro, and all inputs at the edge it ends: a monitor idle
until `psel` reads one signal per edge. Fields not read meanwhile are
stale, but only the condition runs while waiting. A condition the macro
cannot see into (a method on the block) reads all inputs at each edge. The
gain is the reading and conversion on the Rust side: Veryl's simulator
copies a component's inputs at every edge it hears regardless.
