# Proposals to Veryl

What Verald would want of Veryl and its component SDK (`veryl-component`):
what it cannot do, or can do only by duplicating in Rust what Veryl already
knows. Each proposal records the need, the shape proposed, and the
alternatives set aside, with their reasons. A section marked as a study
records an idea studied and not proposed, kept because other components
may want it. The framework's own design is in
[design.md](design.md); until a proposal is taken up, Verald works around
it as the proposal says.

The proposals refer to Veryl's master, with
[veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)
(signal and modport references passed to component methods). A proposal
taken up and implemented is removed from here; the design documents refer
to what Veryl does instead.

## Contents

- [Clocking blocks as a language feature (a study)](#clocking-blocks-as-a-language-feature-a-study)
- [A hook on reset release](#a-hook-on-reset-release)
- [`%m` as a format specifier](#m-as-a-format-specifier)

## Clocking blocks as a language feature (a study)

**Not proposed.** Veryl keeps to its features as an HDL and keeps its
specification small, so clocking stays on the components' side, as
Verald implements it. What the simulator holding it would give over
that is small:

- Fewer per-signal costs: little, as a component already reads and writes
  its signals through direct pointers, and drives only what changed.
- Definitions checked at compile time: given as well by "Rust types
  generated from Veryl packages", which can describe an interface's
  modports.
- A component not called at edges it waits through: the largest gain, on
  buses idle for long, but wake conditions in the SDK give it without a
  clocking in the language; worth asking for once measured to matter.
- Only these need the language: one sampling and driving semantics
  guaranteed by the simulator across components (today it rests on how
  the hooks are called), and skews (`input #1step`, `output #2`), which
  need the simulator's time. Verald needs neither today.

The study below is kept for components that may come to want it.

Clocking is as useful to tests written in Veryl, and to other components,
as to Verald, which was the case for Veryl and its simulator holding it
rather than each testbench. Verald implements it on the component's
side: at every edge of a clock bound to it, Veryl calls the component's
hook, and the component reads each input it needs and writes each output
it changed, a host call (or a direct pointer access) per signal. The idea
moves the sampling and the driving into the simulator, and lets a
component be woken only when it has something to do.

### The declaration

A syntax of its own, shaped as a modport is (the form would be the Veryl
authors' to settle; this is the one studied):

```veryl
interface bus_vip_if {             // a test interface (see below)
    var i_clk: clock;              // its edge is the clock type's (`clock_posedge`, ..)
    var valid: logic;
    var ready: logic;
    var data : logic<32>;

    modport master { valid: output, ready: input, data: output }

    clocking master_cb (i_clk) {
        valid: output,
        ready: input ,
        data : output,
    }
    clocking slave_cb   (i_clk) { ..converse(master_cb) }
    clocking monitor_cb (i_clk) { ..input }
    clocking driver_cb  (i_clk) { ..same(master) }   // from a modport too
}

// a test module: a clocking of its own may hold any signals, by path
clocking tb_cb (clk) {
    bus.valid: output,
    irq      : input ,
}

tb.set_clocking("bus", bus.master_cb);   // passed by reference, as a modport is
```

- Members are written as a modport's (`name: direction`), and the defaults
  are a modport's: `..same(x)` and `..converse(x)`, where `x` is a
  clocking or a modport, so a clocking made from a modport repeats no
  direction; `..input` for a monitor's. Directions are `input` and
  `output` only.
- The clock is the argument; the edge comes from its type, so no
  SystemVerilog `@(posedge ..)` is carried over.
- In an interface, members are its names; in a test module, paths, so a
  clocking may hold an interface's members and a loose signal (an
  interrupt) together.
- The timing is the semantics: inputs are sampled before the edge,
  outputs reach the design at it; skews mean nothing in a cycle-based
  simulator and are left out.
- Usable in Veryl test modules too, not only by components, with as
  little as `$tb::clock_gen` offers: its members (`cb.member`) and
  `cb.next()` / `cb.next(n)` (drive the outputs, wait for the edge or `n`
  of them, sample the inputs). A wait on a condition is a loop
  (`while !cb.ready { cb.next(); }`). Verald's richer waits
  (`tick_until!`, `posedge!`, `changed!`, with or without a limit) rest on
  what Rust has (`async`, closures, macros reading the condition, `Result`
  for a timeout) and stay in Rust, built on the wake conditions the SDK
  gives a component (below), which are no methods of a Veryl test's
  clocking.

A clocking describes a testbench, not a design, so it is allowed only in
test modules and test interfaces, never in what is synthesized. Veryl has
test modules (`#[test]`); a test interface would need a mark of its own,
also the authors' to settle. A design's interface (one a DUT uses) then
holds no clocking: a VIP has an interface of its own, and the test module
connects the design's signals to it. That is also how a VIP's interface
avoids parameters (see below), and how SystemVerilog VIPs are built.

The other shapes considered each fail one of the needs above:

- An attribute on a modport giving the clock and a name
  (`#[clocking(clk, master_cb)]`): no syntax, the directions taken from
  the modport, and an interface ships it; but the clocking is no entity a
  Veryl test can use (`cb.member`), and it holds one modport's signals
  only.
- A built-in test component (`inst cb: $tb::clocking (clk, bus.master)`,
  in a test module or, if instances were allowed there, in an interface),
  or a built-in struct value holding the references
  (`$tb::clocking'{ clk: clk, modport: master_mp }`): a component's ports,
  and a struct's fields, are fixed, while a clocking holds any number of
  signals; and a value holding references to signals is a notion Veryl's
  values do not have. (A type argument, `$tb::clocking::<clk, master_mp>`,
  would misuse generics, which are for types that vary.)

### What the SDK gives a component

- **A clocking reference**: the clock and edge, and each signal's name,
  direction and width; no separate `set_clock`.
- **Signals, one at a time, as bits**: the sampled value of an input, and
  the value to drive an output with. The simulator samples a clocking's
  inputs once per edge into a buffer shared by all its users, and applies
  the outputs written at the edge; the component reads and writes the
  buffers at fixed offsets (direct pointers), with no host call per
  signal.
- **Wake conditions**: a component registers what it waits for: so many
  edges, a signal changing, a signal taking a value. The simulator calls
  the component's hook only when one holds, and the hook says which
  clocking and which condition.

The SDK takes no position on structs: a signal is the unit, as a port is
today. How signals map to a testbench's types is the testbench's
(below).

### A clocking passed to a component has no parameters

A clocking passed to a component belongs to an interface marked `#[export]`,
as boundary types belong to `#[export(rust)]` packages
([veryl-lang/veryl#3772](https://github.com/veryl-lang/veryl/pull/3772)):
such an interface, and the types of its signals,
have no parameters and no generics, so every width is fixed by the
definition and the layout is the same for every instance. Then:

- Veryl describes the marked interface (its clockings: their clock and
  edge, their signals' names, directions and widths; and its modports) in
  the same `veryl.types.json` as the packages;
- `import!(bus_if)` expands the interface as a public module holding a
  struct per clocking (and per modport), so they are defined once, in
  Veryl, with nothing generated for the user to manage; a struct-typed
  signal (an AXI channel) is a type of its `#[export]` package, whose
  expansion `use(..)` names (as #3772 does for packages and
  interfaces).

With that, Verald's own `#[modport]` / `#[clocking]` structs, which a
user writes by hand today, go away.

How the generation works:

- Veryl writes the descriptions as JSON, `veryl.types.json`, into the
  component crate's directory, after the analysis and before it builds
  the components; the file is committed, as `veryl.manifest.json` is, so a
  dependency's interfaces are described without running Veryl on it, and
  kept current by a hash of the Veryl sources. It is a file of its own,
  not part of `veryl.manifest.json`: that one is made from the crate
  after it is built, this one from the Veryl sources before; in one file,
  each build would rewrite what the next build depends on.
- `import!` is a proc macro: at compile time it reads the description and
  expands the package or interface it names as a module, named after it,
  where the line is written.

  ```json
  { "bus_vip_if::master_cb": {
      "clock": "i_clk", "edge": "posedge",
      "signals": [
        { "name": "valid", "dir": "output", "width": 1 },
        { "name": "ready", "dir": "input",  "width": 1 },
        { "name": "data",  "dir": "output", "width": 32 }
      ] } }
  ```

- The macro reads the file next to the crate (`CARGO_MANIFEST_DIR`): no
  search and no setting, and the same file when cargo runs alone. With
  none, it fails asking for `veryl build` first.
- Cargo does not track files a proc macro reads, so the expansion
  includes the description
  (`include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/veryl.types.json"))`),
  which makes rustc rebuild the crate when the Veryl definition changes.
- A `build.rs` writing the structs into `OUT_DIR` would work as well, but
  needs one in every crate; a macro is one line where the module is
  wanted.

A design whose widths come from parameters connects to a fixed-width
interface in the test module, as SystemVerilog VIPs do (an interface
parameterized for a virtual interface is awkward there, so a VIP declares
each signal at the widest it handles): the test module assigns the
design's signals to the interface's, and the VIP is told the widths in
use through its configuration. A clocking used only within Veryl test
modules, not passed to a component, needs no such restriction.

Bounds per signal (an attribute giving a parameterized signal's widest
width) were set aside: they would need further rules (a struct-typed
signal whose fields' widths vary would still have no fixed layout), where
one rule, no parameters, covers every case.

### Performance

| At each edge | Today (component's side) | Veryl's clocking |
|---|---|---|
| inputs | Veryl prepares every bound port; the component reads each it needs | sampled once per clocking, in the simulator, shared |
| reading | a host call or a pointer access per signal, then conversion | a fixed offset in a buffer |
| outputs | a write and a dirty flag per signal | written into a buffer; applied by the simulator together |
| the hook | called at every edge, waiting or not | called when a wake condition holds |

The first rows save per-signal work, which counts on wide buses. The
last saves the most: today a testbench waiting (for an interrupt, a long
delay, an idle bus) is still called at every edge, runs its executor and
returns having done nothing; with wake conditions those edges cost
nothing. Where every cycle carries a transfer, the testbench's own work
dominates and the gain is smaller. How much each saves is to be measured.

### Verald with it

- The clockings' structs come from `import!` (its own `#[modport]` /
  `#[clocking]` go), and `tick` knows its clock from the clocking; the
  wait macros stay.
- The executor collects what its waiting tasks wait for (the nearest edge
  count, the signals and values) and registers it as wake conditions:
  from "called at every edge and looking" to "called when needed".
- Without it, the Rust-side `#[clocking]` stays, its binding (where the
  members and the clock come from) kept apart from its sampling and
  driving.

## A hook on reset release

`on_reset_release`, the pair of `on_reset`. Today a release calls no hook
and is seen only at the next `on_clock`. With the hook, a release would be
known when it happens, and values to drive at the release could be
written then, rather than at the first edge after it.

## `%m` as a format specifier

Filed as [veryl-lang/veryl#3713](https://github.com/veryl-lang/veryl/issues/3713).

`%m`, the hierarchical name of the current scope, as in SystemVerilog. A
setting given per instance is best keyed by the HDL hierarchy (each module
of the harness sets its own: `u_apb[0].data_width`). With `%m` among the
format specifiers (`$display` and the like), a key can be written as
`"%m.data_width"`. For it to reach a component method as a string, either
the string argument of a method call is formatted before the call
(`tb.set("%m.data_width", W)`), or a formatted string (an `$sformatf`
equivalent) can be passed as an argument (today only string literals
arrive as strings; other expressions arrive as bits).
