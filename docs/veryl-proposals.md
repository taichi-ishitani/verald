# Proposals to Veryl

What veryl-tbf would want of Veryl and its component SDK (`veryl-component`):
what it cannot do, or can do only by duplicating in Rust what Veryl already
knows. Each proposal records the need, the shape proposed, and the
alternatives set aside, with their reasons. The framework's own design is in
[design.md](design.md); until a proposal is taken up, veryl-tbf works around
it as the proposal says.

The proposals refer to Veryl as of
[veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)
(signal and modport references passed to component methods).

## Contents

- [Arguments into a simulation](#arguments-into-a-simulation)
- [Clocking blocks as a language feature](#clocking-blocks-as-a-language-feature)
- [Types defined by a component's crate, usable in Veryl](#types-defined-by-a-components-crate-usable-in-veryl)
- [A hook on reset release](#a-hook-on-reset-release)
- [`%m` as a format specifier](#m-as-a-format-specifier)

## Arguments into a simulation

So that a test can be chosen at run time, like UVM's `+UVM_TESTNAME`, and
one `#[test]` module can run any test. Veryl would need a `veryl test`
option for them (or per-test arguments in `Veryl.toml`) and an API for a
component to read them.

A file the testbench reads is no substitute today: reads resolve against
the project root, shared by every test (only writes go to a per-test
directory, `target/veryl-components/out/<test>`), and a component cannot
learn its test's name. Until then each test has a `#[test]` module of its
own, which starts it by name.

## Clocking blocks as a language feature

The timing is there already (hooks read values before the edge and writes
reach the design at it); what is missing is the declaration. What
veryl-tbf would need of it:

- the clock and edge a clocking block follows (the interface's clock, as
  the reference-arguments work allows);
- the direction of each member, sampled or driven (possibly from the
  modport);
- passing one to a component method by reference
  (`tb.set_interface("apb", bus.cb)`), the component getting its members'
  names, widths and directions and its clock, as a `ModportRef` grown;
- the timing written down as the semantics: inputs before the edge,
  outputs at it; SystemVerilog's skews mean nothing in a cycle-based
  simulator and can be left out;
- usable in Veryl test modules too (`cb.next()`), not only by components.

With it, veryl-tbf would no longer have users write `#[modport]` /
`#[clocking]` structs by hand (they would be generated from, or checked
against, the declaration), and `tick` would know its clock; the value
conversions and the wait macros stay in Rust. Until then the Rust-side
`#[clocking]` is used, its binding (where the members and the clock come
from) kept apart so that it can be replaced.

## Types defined by a component's crate, usable in Veryl

Filed as [veryl-lang/veryl#3712](https://github.com/veryl-lang/veryl/issues/3712).

A component takes structured values (a configuration) at a fixed width it
defines; the design's own types often take their widths from parameters
or generics, fixed only per instance. So the boundary types are defined by
the component's side, in Rust, where widths are fixed, and Veryl lets test
modules use them as packages. A test module copies the design's values
into them, where the parameters are known.

### Shape

```rust
// a VIP's crate: a module published to Veryl as the package `bus_vip_pkg`
#[veryl(package = bus_vip_pkg)]
pub mod config {
    pub const REGS: u32 = 8;
    pub enum Mode { Master, Slave }
    pub struct BusConfig { pub mode: Mode, pub regs: u8, pub timeout: u16 }
}
// the testbench reads it: setup_context.get::<config::BusConfig>("bus")
```

```veryl
// a test module of a project depending on the VIP's project `vip_bus`
var cfg: vip_bus::bus_vip_pkg::BusConfig;
assign cfg.regs    = REGS;      // a parameter's value, into a fixed width
assign cfg.timeout = 1000;
tb.set("bus", cfg);
```

### Flow

The one component manifests already follow: `veryl test` builds a
component's crate and takes its manifest into a sidecar the analyzer
reads, and `veryl publish` commits the manifest (`veryl.manifest.json`),
so a project depending on it is analyzable without building. The
attribute puts the module in the manifest (its constants, enums and
structs) under the package name it gives; Veryl makes a package of each
such module for name resolution and simulation. Nothing generated is left for the
user to manage, no `Cargo.toml` is edited, and the crate stays an
ordinary one (`cargo test` and `cargo clippy` run on it alone).

The cost: a type changed in Rust reaches Veryl (and its language server)
when the component is built again. `veryl test` builds it each time; a
published dependency carries its committed manifest.

### Pack and unpack

Converting a value between Veryl's packing and the Rust type (unpack, and
pack the other way) belongs with it, in the component SDK, not in
veryl-tbf: the packing is Veryl's rule, any component may take or return a
struct (a model, a file reader), and the conversion is made from the same
type information as the manifest entry, so it should be read in one place.

With it, a component method could take or return a published type directly
(`fn set_config(&mut self, ctx: &mut SimCtx, cfg: BusConfig)`), the macro
converting the argument, and the analyzer could check the call's argument
against the type (today a value argument's width is taken from the call
site and not checked).

The SDK's conversion would cover the primitives as well as the published
types: `bool`, the integers, `BigUint` (with a width), `Value` and
`String`. A component then has one conversion layer, the SDK's, rather
than one of its own on top. Two layers do not combine in Rust: a
component's trait implemented for every type the SDK converts conflicts
with the component's own impls for the primitives (`String`, say), since
the SDK might add an impl for them later.

veryl-tbf would use it in two places, and drop its own conversions for it
(`FromSetting` for settings, `SignalValue` for signals):

- Settings: `setup_context.get::<T>(key)` unpacks the `Value` a test module
  passed with `tb.set`, a scalar or a published struct alike; a VIP's
  configuration is read as the VIP's type and put in the test's own.
  `tb.set` stays a key and a `Value`.
- Clocking blocks: a modport member of a published struct type (an AXI
  channel) is a field of that type in the `#[modport]` struct, unpacked
  when sampled and packed when driven
  (`cb.bus.aw = AwChannel { addr, len, .. }`).

Sequence items stay Rust's own: they never cross to Veryl.

### Rules

- A crate's manifest carries the modules it defines, not its
  dependencies' (the attribute records the defining crate, and a manifest
  keeps its own crate's), so a type arrives through one manifest only.
- The package's name is given in the attribute
  (`#[veryl(package = bus_vip_pkg)]`, with no default) and carried by the
  manifest, not made from the module's: the Rust module is named as Rust
  wants it, renaming or moving it leaves Veryl code alone, and which name
  Veryl sees is written where the module is.
- The types are in the namespace of the Veryl project the crate belongs
  to, as a Veryl-only dependency's packages are: a project's own as
  `bus_vip_pkg::BusConfig`, a dependency's as
  `vip_bus::bus_vip_pkg::BusConfig`. One module is one package; a package
  name given twice in a project (by two modules, in one crate or two, or
  by a module and a package written in Veryl) is an error, found by Veryl
  when it makes the packages, since Rust sees none of these as a clash.
- A crate defining types belongs to a project's `[[components]]`, which
  Veryl would allow for a crate that exports types and no component (a
  VIP's types alone).
- A field is `bool`, an integer (`u8` .. `u128`, `i8` .. `i128`), a
  `BigUint` with `#[width(N)]`, an array, or a type of a module marked
  `#[veryl]` (shared enums, a VIP's configuration in a testbench's); a
  marked module refers to no other kind of type. An integer may be
  narrower than its Rust type (`#[width(3)] u8`). An enum takes the width
  of its values, or `#[width(N)]`.
- The layout is a packed struct, fields from the most significant bit in
  declaration order, as Veryl's own; the manifest gives each field's
  width, so the two sides agree. A struct's width has no bound. X and Z
  bits read as 0, as clocking blocks read them; a value no variant of an
  enum has fails the unpacking, naming the field.

### What already works

A struct variable passed to a method as a value arrives as one `Value` of
the struct's whole width, fields from the most significant bit in
declaration order (tried with 24 and 600 bits; only a method's return
value is limited, to 512 bits). Until the proposal, a testbench wanting a
struct may define it on both sides and write a `FromSetting` that slices
the `Value`; veryl-tbf adds no interim derive, which the SDK's would
replace. Settings are otherwise set one value at a time.

### Modports

Modports stay as they are: they are signals, not values, and an
interface's widths usually come from its parameters. The Rust-side
`#[modport]` structs are checked against the modport's widths when bound.

### Set aside

- Taking the design's types into Rust as they are: a width from a
  parameter or a generic would need a bound to choose a Rust type (an
  attribute on the parameter), and a packed struct the field offsets of
  each instance at run time. A boundary type of fixed widths needs
  neither; the copy into it has to be written either way, and a change to
  the design's struct breaks that copy at compile time.
- Rust generated from Veryl packages marked `#[rust]` (non-generic
  packages referring only to marked ones): it gives the types the same
  identity, but the generated Rust has to be written out as a crate, added
  to the component's `Cargo.toml`, kept in step before cargo builds it,
  and placed in some crate when the package belongs to another project.
  Generating Veryl from Rust keeps all of that inside Veryl's own manifest
  flow.
- Merging the types of every manifest by their origin under a namespace of
  Veryl's own: unneeded once each crate publishes only its own types, in
  its project's namespace.

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
