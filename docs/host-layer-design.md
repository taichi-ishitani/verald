# Host layer: design

> **Status:** implemented as the crate `verald-host`, with the runtime
> as its one subscriber; what the assertions will need of it (several
> subscribers in `export!`, the registrations and the start in the layer,
> failures recorded, later probes) is not yet. It records how the code that
> talks to Veryl's host API was separated from the runtime, so that layers
> other than the testbench (assertions first) share it.

## Contents

- [Goals and constraints](#goals-and-constraints)
- [Overview](#overview)
- [Hooks](#hooks)
- [Services](#services)
- [Types](#types)
- [Registrations and the start](#registrations-and-the-start)
- [Exporting a component](#exporting-a-component)
- [What stays in the layers above](#what-stays-in-the-layers-above)
- [Migration](#migration)
- [Deferred items](#deferred-items)
- [Decisions and rejected alternatives](#decisions-and-rejected-alternatives)

## Goals and constraints

- Layers other than the testbench use the host API (veryl-component):
  assertions first (see [assertion-design.md](assertion-design.md)),
  coverage likely later. **The types and the APIs the host API deals in are
  shared** among them, not reimplemented by each.
- One component may hold several layers (a testbench and assertions); a
  component has one entry for its hooks, so the layers are dispatched to
  from one place. A component may also hold the assertions without the
  testbench.
- **Used within this project only.** The layer is a crate of Verald, not
  published for other components nor proposed to the SDK.
- A light run comes first: static dispatch, no allocation or host call on
  the per-edge path where it can be avoided.
- The separation keeps behavior: the existing tests and examples pass
  unchanged at every step.

## Overview

```text
veryl-component             the SDK: the host API
`-- verald-host             the hook entry and dispatch, the hook's context,
    |                       services, value conversion, typed signals and
    |                       modports, Signals / MemberSource, HostError,
    |                       registrations and the start
    +-- verald-tb-runtime   executor, Clock / Reset, clocking, lifecycle,
    |   |                   VERALD_ arguments
    |   `-- verald-tb-core  methodology, report
    +-- verald-assert       property engine, probes, its lifecycle
    `-- (later layers)
```

**What goes in the layer**: what

- (a) receives a hook from the host API,
- (b) calls a service of the host API, or
- (c) represents a type of the host API, or converts it to Rust types.

**What does not**: what decides when and why to call it: the control of
time (tasks, waits), lifecycles, the meaning of names, and policies
(writing only changed values, reporting). These belong to the layers above.

## Hooks

### The entry and dispatch

The layer implements veryl-component's `Component` hooks once per
component and passes each to the layers above, its subscribers:

```rust
pub trait HookSubscriber: Default {
    /// At `create`: ports and parameters are resolved, probes declared.
    fn on_build(&mut self, ctx: &mut BuildCtx) -> Result<(), HostError> { Ok(()) }
    fn on_init(&mut self) -> Result<(), HostError> { Ok(()) }
    /// At the test module's `start`: the registrations are complete.
    fn on_start(&mut self, test: Option<&str>) -> Result<(), HostError> { Ok(()) }
    fn on_clock(&mut self) -> Result<(), HostError> { Ok(()) }
    fn on_reset(&mut self) -> Result<(), HostError> { Ok(()) }
    fn on_finish(&mut self) -> Result<(), HostError> { Ok(()) }
}
```

- **Static dispatch.** The component struct holds its subscribers as fields
  of their own types, and each hook calls them in turn; no trait objects.
- **Building takes the context as an argument.** At `create` there is no
  `SimCtx` and nothing is published; a subscriber resolves what it needs
  from the `BuildCtx` (parameters, ports, later probes) in `on_build`. It
  is made with `Default` first, as veryl-component makes a component's
  plain state before calling the component's `on_build`. The layer takes
  the seed there first (see [Services](#services)).
- **The other hooks take no context**: the layer publishes it for the
  hook's duration (below), and subscribers use the services.
- **Which clock or reset fired** is a service (`fired(port)`), not an
  argument: a subscriber asks about the ports it knows.
- **Order**: the order the subscribers are given in. Assertions are put
  before the testbench: both read pre-edge values, so the order changes no
  result, but a failure is then reported before what the testbench does at
  that edge.
- **Errors**: a subscriber's `Err` is returned from the hook at once; the
  remaining subscribers are not called for that hook. The layer records
  it as a failure (`failed()`, below).
- **The entry** (`entry::on_build`, `on_init`, `event` for `on_clock` and
  `on_reset`, `on_finish`): what the generated component's hooks call, with
  a closure calling the subscribers in order. It frames the hook (below)
  and converts a `HostError` to the host API's error.

### The hook's context

The `SimCtx` of the hook being run is published in a thread-local for the
hook's duration (`HookGuard`), and the services read it.
There is one entry per component, so one guard per hook, entered once
before the subscribers are called; calls never nest. Methods the test
module calls (`set_clock`, `start`, ..) enter it the same way.

### Panic detection

A hook that panicked leaves the component in an unknown state. The layer
marks a hook as running on entry and clears the mark on a normal return;
finding the mark already set on entry means the previous hook unwound (a
panic, reported by Veryl), and every later hook returns at once, except
`on_finish`: it is still dispatched, without the context, so that the
subscribers let go of what they hold (the runtime drops its tasks); the
service `poisoned()` tells them. The flags are the layer's, not a
subscriber's, since a panic concerns the whole component; `on_init`
clears them for a new simulation on the thread.

## Services

Free functions over the published context; outside a hook, those that
need it panic (reads and writes) or do nothing (`time` gives 0, `log`
prints).

**Signals are two-state for now.** Reads and writes go through the direct
paths; the SDK's `read` / `write`, which build a `Value` with its X/Z
mask, are not used for signals in a two-state simulation. In a four-state
one X and Z read as 0, and nothing drives X or Z. A port's buffer holds a
Z as a 1 and only the mask tells it, and an input port carries no pointer
to its mask, so in a four-state simulation reads go through the SDK's
`read` and drop the bits the mask marks; the layer takes whether the run is
four-state at `on_init`, so the choice is a branch that always goes the
same way. Four-state signals are deferred (see
[Two-state signals first](#two-state-signals-first)).

| Service | Notes |
|---|---|
| `read_u64(port)`, `read_words(port, out)` | direct reads through the pointer the port keeps to the staged buffer, without a host call; a copy where memory cannot be shared (the wasm transport). X and Z read as 0 (through the SDK's `read` in a four-state run, above) |
| `write_u64(port, word)`, `write_words(port, words)` | direct writes into the port's buffer, setting its dirty flag, without building a `Value`; a host call where memory cannot be shared. Reach the design at the edge. X and Z are not driven |
| `time()` | the current simulation time |
| `log(line)` | a line of the test's output |
| `fail(message)`, `finish()` | the end of the simulation |
| `failed() -> bool` | whether the simulation has failed: `fail` called, or a subscriber's `Err` returned from a hook. The runtime does not report the finish a failure caused as an unexpected one |
| `fired(port) -> bool` | whether the clock or reset `port` fired the current hook |
| `seed() -> u64` | the instance's seed, taken at `create` |
| `poisoned() -> bool` | whether a hook has unwound (see [Panic detection](#panic-detection)) |
| `runtime_args() -> Vec<RuntimeArg>`, `runtime_arg(name)`, `runtime_arg_value::<T>(name)` | the arguments of `--runtime-arg`, raw, taken at `create`: the last one given wins, values converted by `ArgValue`'s conversions. Prefixes (`VERALD_`) are the layers' |
| `track_reset(port)`, `reset_asserted(port) -> bool` | the state of a reset, below |

Direct reads need no change to the simulator or the SDK: the host stages
each input into a buffer of the component's port before the flip-flops
commit (`stage_inputs`), `port_direct` gives pointers to these buffers, and
the SDK's `InputPort` keeps the pointer from when it is made (`words_ptr`).
The buffers are a pre-edge snapshot; the design's own storage is not
shared, since hooks run after the flip-flops commit and it holds the values
after the edge. The ports a modport passed by reference carries have the
pointer too: the SDK gives its members the pointer when it converts the
reference (`member_port` in `reference.rs`).

### The state of a reset

The host API calls `on_reset` at every edge while a reset is in force, and
`on_clock` otherwise; the two are exclusive. A layer that needs a reset's
level asks the layer to track the port (`track_reset`), and the layer
updates it before dispatching: asserted when `on_reset` fires for the port,
released on `on_clock`. This is what the runtime does today; waiting for a
reset (`Reset`, its wakers) stays in the runtime.

## Types

### The host API's types

`InputPort`, `OutputPort`, `ClockPort`, `ResetPort`, `ModportRef`,
`MemberPort`, `Value`, `BuildCtx`, `RuntimeArg`, `ArgValue` and `ArgError`
are re-exported by the layer; the layers above name them through it, not
through veryl-component.

### Values

A field type reads and writes itself: each type takes the shortest path
for it, rather than every type going through a slice of words.

```rust
pub trait SignalValue: Clone + PartialEq + Default {
    /// Whether a port `width` bits wide fits the type; checked when bound.
    fn fits(width: u32) -> bool;
    /// The port's value before the edge.
    fn read(port: InputPort) -> Self;
    /// Drives the port; the value reaches the design at the edge.
    fn write(&self, port: OutputPort);
}

impl SignalValue for bool {
    fn fits(width: u32) -> bool { width == 1 }
    fn read(port: InputPort) -> Self { read_u64(port) & 1 != 0 }
    fn write(&self, port: OutputPort) { write_u64(port, u64::from(*self)) }
}
```

| Type | Read and written with |
|---|---|
| `bool`, `u8` to `u64` | `read_u64` / `write_u64`, converted with `as`; no buffer |
| `u128` | `read_words` / `write_words` on two words on the stack |
| `BigUint` | `read_words` into words it is then built from; the one type that allocates |

- `InputSignal<T>` and `OutputSignal<T>` are generic over `T`, so
  `T::read(port)` is monomorphized: whether a port goes through `read_u64`
  or `read_words` is decided at compile time, not by a branch at every
  access.
- `Default` gives the value a `Driven<T>` holds before its first write
  (zero), in place of today's `zero(width)`.
- A type added later reads into its own storage: a two-state bit vector
  with `read_words` into its words, a four-state type with the mask as
  well (see [Deferred items](#deferred-items)).
- `Value` is no longer a field type: it stays the host API's type for
  settings, options and method arguments.

The direct reads and writes are the speed-up: the SDK's `read` calls the
host, copies the words and the mask and builds a `Value` (allocating for
wide signals) at every read, and `write` builds one at every write. They
need no change to the simulator or the SDK, and are independent of the
split of `Member<T>` below. The writes concern clocking's outputs (what
drivers drive); the assertions only read.

### Typed signals

- `InputSignal<T>`: an input port and its width, checked against `T` when
  bound; `read() -> T`.
- `OutputSignal<T>`: the same for an output; `write(&T)`.

They read and write only. Comparing with the last value written, to write
only what changed, is clocking's (`Driven<T>` in the runtime).

They replace the runtime's `Member<T>`, which holds either port in an enum
(`MemberPort::Input` / `Output`) and rejects the other side in `load` and
`store` with `unreachable!`. The split is to organize the types, not for
speed (the enum's branch costs little):

- they follow the SDK, whose `InputPort` and `OutputPort` are types of
  their own (an output cannot be read);
- a read of an output or a write of an input is a compile error, not an
  `unreachable!` at run time;
- the assertions, which only read, get a read-only type;
- an input does not carry `last`, the value last written, which only
  outputs need;
- writing only what changed moves to clocking, where the policy belongs.

### Where signals come from

```rust
/// What supplies a modport's members.
pub trait MemberSource {
    fn path(&self) -> &str;
    fn input(&self, name: &str) -> Result<InputPort, HostError>;
    fn output(&self, name: &str) -> Result<OutputPort, HostError>;
    fn clock(&self, name: &str) -> Result<ClockPort, HostError>;
    fn reset(&self, name: &str) -> Result<ResetPort, HostError>;
}

/// Where names bind: resolution to ports only.
pub trait Signals {
    type Members: MemberSource;
    fn interface(&self, name: &str) -> Result<Self::Members, HostError>;
    fn input(&self, path: &str) -> Result<InputPort, HostError>;
    fn output(&self, path: &str) -> Result<OutputPort, HostError>;
    fn clock_port(&self, name: &str) -> Result<ClockPort, HostError>;
    fn reset_port(&self, name: &str) -> Result<ResetPort, HostError>;
}
```

- `ModportRef` implements `MemberSource`; its members are looked up in the
  list it carries, without a host call. Later the probes of an instance's
  modport port implement it too, since such a port does not arrive as a
  `ModportRef`.
- `Signals` is a vocabulary, not a service: which names exist is decided by
  whoever implements it. The layer implements it over the test module's
  registrations (`Registered`, below), with `type Members = ModportRef`;
  `Start` and the methodology's `Scope` forward to it; the assertions
  implement it over an instance's ports and observed signals, later. Code generated over `S: Signals` (an assertion
  block's binding) then binds the same way to either.
- What a name means beyond a port (a `Clock` that tasks wait on) is the
  runtime's: see `Clocks` [below](#what-stays-in-the-layers-above).

### Typed modports

`#[modport]` generates `ModportRead` and, for a struct with no
`#[output]` field, `InputOnly` over the types above; the testbench's
runtime adds `ModportDrive`. See
[modport-clocking-design.md](modport-clocking-design.md#typed-modports).

### Errors

`HostError`: a binding failure (a missing name, a member of the wrong
direction, a width that does not fit) or an error from the host API. The
runtime converts it to `TestError`; the assertions to their own reports.

## Registrations and the start

The test module names the signals the layers above bind to, then starts
the component. Both are the layer's, so that a component without the
testbench has them too:

- **The registrations**: `set_clock`, `set_reset`, `set_interface`,
  `set_input`, `set_output`, a table of names to ports. A name is
  registered once; an interface's clock and reset members are registered
  as `<name>.<member>`; a registered reset is tracked (`track_reset`).
  `Registered` implements `Signals` over the table.
- **The start**: the test module's `start` ends the registrations (one
  after it is an error) and calls `on_start` of every subscriber, in
  order: the assertions bind their blocks, the runtime starts its test.
  With the testbench `start` takes the test's name; without it, none.

What a name means beyond a port stays above: the runtime builds its
`Clock` and `Reset` over the registered ports at `on_start`, and
`set_master_clock` is its own method, checked against the table.

## Exporting a component

`component!` / `export!` are wired to the runtime's `host` today. They are
to generate the layer's entry over a list of subscribers:

```rust
verald::export!("tb", testbench = Methodology, assertions = [BusSva, FifoSva]);
verald::export!("sva", assertions = [BusSva, FifoSva]);
```

- The component struct holds one field per subscriber; each hook enters the
  context and calls them in order, the assertions before the testbench.
- The registration methods and `start` are the layer's and generated for
  every component. The testbench's own methods (`set_master_clock`,
  `option`, `set`) are generated only when the testbench is among the
  subscribers; they forward to it.
- An assertions-only component (a DUT with `$tb::clock_gen` and no
  testbench) is exported without the runtime.

## What stays in the layers above

**The runtime**:

- the executor, tasks and their concurrency, the shared parts (`sync`);
- `Clock` and `Reset`: handles over the layer's ports that count edges and
  let tasks wait; the runtime asks `fired(port)` and `reset_asserted(port)`;
- over the layer's registrations (`Registered`, which `Start` and the
  methodology's `Scope` forward to), a runtime trait giving the names'
  `Clock` and `Reset`, and clocking:

  ```rust
  pub trait Clocks: Signals {
      fn clock(&self, name: &str) -> Result<Clock, TestError>;
      fn reset(&self, name: &str) -> Result<Reset, TestError>;
      fn clocking<C: Clocking>(&self, clock: &str) -> Result<C, TestError> { .. }
      fn clocking_with<P: ClockingPaths>(&self, clock: &str, paths: P)
          -> Result<P::Block, TestError> { .. }
  }
  ```

  The prelude exports it, so `scope.clocking::<C>("clk")` reads as today;
- clocking: `Driven<T>` (writing only what changed), `tick`, the waits,
  `drive`;
- the lifecycle (setup, running, ended), the timeout, unexpected finishes
  (a finish after a failure, `failed()`, is not one);
- the master clock, the `VERALD_` arguments and the options.

**The assertions** (see [assertion-design.md](assertion-design.md)): the
property engine, the probes (`Signals` and `MemberSource` for an instance),
a subscriber evaluating every bound instance synchronously, their own
lifecycle and reporting trait.

**The methodology** (core) is not affected beyond implementing the traits
again for `Scope`.

## Migration

Done as a refactoring that keeps behavior, in steps, each checked by the
existing tests and examples:

1. The context and the services of `sim` moved to a new module of the
   runtime (`host`, say), which uses nothing of `crate::state`: the
   boundary is kept by that rule.
2. The panic detection and the hook frame moved to it.
3. `HookSubscriber` and the dispatch introduced; the runtime made a
   subscriber; `component!` going through the dispatch.
4. The types: `SignalValue` moved; `Member<T>` split into the typed signals
   and `Driven<T>`; `MemberSource`, `Signals` (ports only) and `Clocks`;
   `ModportRead` and `InputOnly`; `HostError`. The reset state and the raw
   runtime arguments as services.
5. The direct reads and writes, with tests that they give the same values
   as the SDK's `read` and `write` in two-state simulations, and X and Z
   as 0 in four-state ones; `SignalValue` made to read and write itself
   (`fits`, `read`, `write`) and its `Value` implementation dropped (no example or test uses it as a field
   type).
6. The module moved out to its crate.

Done before the assertions' proof of concept, which is built on the layer.
The runtime re-exports the crate as `host`, so the paths the macros
generate (through the runtime) and the runtime's own did not change.
Changes the steps made to the public API: `Signals` returns ports only and
`Clocks` (in the prelude) gives `clock`, `reset`, `clocking` and
`clocking_with`; `host::Registered` stands for the registrations outside
`Start` and `Scope`; the `sim` module (its `log` and `time` are the
layer's) and `Value` fields are gone; the runtime arguments can be read
from `create` on, not only after `start`.

Still to do before the proof of concept, in the same way:

7. The registrations' table and `Registered` moved to the layer, the
   runtime's `Clock` and `Reset` built over it; `start` and `on_start`.
8. `failed()`, and the runtime's unexpected finish skipped after a failure.
9. `export!` over a list of subscribers, the runtime one of them; a
   component without it.

## Deferred items

- Probes and enumerating instances (changes to Veryl; see
  [assertion-design.md](assertion-design.md#changes-to-veryl)).
- Staging only the inputs bound to the event that fired: today every input
  of a component is copied before every hook of the events it listens to
  (`stage_components` in Veryl's simulator); a change to Veryl.
- Traces and other services as layers need them.
- Modport structs generated by Veryl (veryl-lang/veryl#3712): the SDK
  cannot implement `ModportDrive`, a testbench's trait, so the contract
  above is to be split then, the SDK's types carrying the ports and the
  packing, and the testbench wrapping them to drive what changed.
- Four-state signals: a field type keeping X and Z, checks for unknown
  values, driving X or Z. To come as field types whose `read` and `write`
  carry the mask too (the SDK's `read` / `write`, or the mask's pointer),
  so that two-state types keep their direct paths.
- A two-state bit vector with its width (inline up to 128 bits, one
  allocation beyond), as the field type for wide signals in place of
  `BigUint`, and for fields whose width is known only at run time (structs
  generated from metadata). `BigUint`, read with `read_words`, serves for
  now.

## Decisions and rejected alternatives

### A layer at the host API, not at "signals"

Considered first: a layer of the types interfacing Rust with Veryl inside
the runtime, `Clock` and `Reset` included, shared by clocking and the
assertions. `Clock` and `Reset` live on the runtime's state and wake its
tasks, so such a layer stays tied to the runtime, and a layer using it
does too. The cut is drawn at what talks to the host API instead.

### Registrations and the start in the layer

The registrations were the runtime's, so assertions could bind only in a
component with the testbench. In the layer, a component holding the
assertions alone has them too, and one with both registers each signal
once. Only names to ports move: `Clock` and `Reset` wake the runtime's
tasks and stay with it. The start moves with them, since it ends the
registrations and every subscriber needs to know it (see
[assertion-design.md](assertion-design.md#one-component-with-or-without-the-testbench)).

### `Signals` in the layer, not in the runtime

Name resolution is not the host API's, and could stay in the runtime. In
the layer, an assertion block's binding is generated once over
`S: Signals` and binds both to the test module's registrations (the proof
of concept) and to an instance's probes (later), without the assertions
depending on the runtime. It returns ports only, so it brings no meaning
of the runtime's with it.

### Writing only changed values stays with clocking

It is a policy of the testbench's drivers, not the host API's; the
simulator compares outputs before writing them back (`apply_outputs`)
anyway.

### Within the project, not proposed to the SDK

Some of the layer would serve any component (typed reads above all), but
its shape follows Verald's needs and is still changing. It can be
proposed to veryl-component later, which would make the layer thinner.

### Two-state signals first

Testbenches and assertions so far assume two-state values, and X and Z
already read as 0 into every field type but `Value`, which nothing uses as
a field. Keeping `Value` would leave the one path that builds a `Value` at
every access. Four-state support is deferred until a use needs it; since
each field type reads and writes itself, it comes as types of its own
without changing the two-state ones.

### Field types read and write themselves

Considered: `SignalValue` converting to and from a slice of words
(`from_words` / `to_words`), with the typed signals choosing `read_u64` or
`read_words` by width. Every type then went through a slice, and the
choice was a branch at run time or a buffer for every type. Chosen:
`read` and `write` as functions of the type, so `bool` and the integers
convert a `u64` directly, `u128` uses two words on the stack, and the
choice is made by monomorphization.

### Static dispatch to subscribers

Hooks are on the per-edge path; subscribers are known when the component
is exported, so a list of types does without trait objects.
