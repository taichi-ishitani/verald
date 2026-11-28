# Assertions: design study

> **Status:** a design, implemented in part (see
> [Proof of concept](#proof-of-concept)): assertion blocks with
> properties over cycles (`seq!` with `delay`, `let`, `wait`, `happen`,
> `repeat`, `throughout`, `until` and `ended`, implication by `if`,
> `eventually`, `never`, `past` and the functions on values), the reports
> at the end, auxiliary state and `check!`, bound with `bind_to_instance`,
> in `verald-assertion`, with named sequences and properties within an impl.
> `#[cover]` waits for coverage (see [Deferred items](#deferred-items)).
> It records what has been decided about SVA-like assertions for Veryl's
> simulator, and why, so that the implementation and later changes can
> refer back to it.

## Contents

- [Goals and constraints](#goals-and-constraints)
- [Overview](#overview)
- [Writing assertions](#writing-assertions)
- [Temporal syntax](#temporal-syntax)
- [Semantics](#semantics)
- [Auxiliary logic](#auxiliary-logic)
- [Binding to the design](#binding-to-the-design)
- [Sharing the host layer](#sharing-the-host-layer)
- [Evaluation in the simulator](#evaluation-in-the-simulator)
- [Failures and reporting](#failures-and-reporting)
- [Changes to Veryl](#changes-to-veryl)
- [Proof of concept](#proof-of-concept)
- [Formal verification: a later study](#formal-verification-a-later-study)
- [Deferred items](#deferred-items)
- [Decisions and rejected alternatives](#decisions-and-rejected-alternatives)

## Goals and constraints

- Assertions equivalent to SystemVerilog's concurrent assertions (SVA),
  usable in simulation with Veryl's built-in simulator.
- **Veryl itself stays an RTL language.** Its grammar and features are not
  extended for verification; assertions live outside the language.
- **Assertions are written against the Veryl source**, not against the
  generated SystemVerilog. The generated names differ from Veryl's (project
  prefixes, mangled generic instances, clock and reset prefixes and
  suffixes), so assertions on the SV would depend on the emitter's rules.
- **Evaluated by a component**, as the rest of Verald is. Veryl does not
  instantiate or bind checkers; the component reads the design's signals and
  evaluates the properties itself.
- Run time comes first: properties are compiled into state machines, not
  interpreted.
- Emitting SVA for other simulators and formal tools is out of scope for
  now. The semantics follow SVA, so it remains possible later.

## Overview

Assertions are a crate of Verald, paired with constrained random
(`verald-random`, `crates/random`): `verald-assertion` (`crates/assertion`,
its macros in `verald-assertion-macros`), re-exported by `verald` as
`verald::assertion` under the feature `assertion`.

```text
#[assertions] struct / impl                (compile time, proc macros)
  -> property IR
  -> a state machine per property (Rust code)
test module's initial                      (run time)
  -> signals registered (tb.set_clock, tb.set_modport, ..)
  -> the assertion struct bound to them by name, widths checked
each clock edge (component hook)
  1. sampled values read
  2. properties stepped: running attempts advanced, new attempts started
  3. #[on_clock] hooks run (auxiliary state updated)
each reset
  -> attempts dropped, auxiliary state re-initialized
end of simulation
  -> incomplete attempts and cover counts reported
```

The macros go through a property IR rather than generating the state
machines directly, so that the notation can be replaced (see
[the notations compared](#notation-rust-functions-with-sequences-in-seq))
and SVA can be emitted later from the same IR.

## Writing assertions

An assertion block is a struct, as a clocking block is: its fields declare
the signals it reads, with their Rust types, and its other fields are
auxiliary state. The impl holds the properties and the hooks.

```rust
use verald::prelude::*;

#[assertions]
#[derive(Default)]
struct BridgeAssertions {
    #[clock] clk: Clock,
    #[reset] rst: Reset,
    #[input] i_valid: bool,
    #[input] i_ready: bool,
    #[input] i_data: u32,
    #[input] o_valid: bool,
    #[input] o_ready: bool,
    #[input] o_data: u32,
    #[input] req: bool,
    #[input] ack: bool,
    #[input] full: bool,
    #[input] empty: bool,

    // Auxiliary state (no attribute)
    outstanding: u32,
    expected: VecDeque<u32>,
}

#[assertions]
impl BridgeAssertions {
    // A combinational helper: a pure method
    fn req_fire(&self) -> bool {
        i_valid && i_ready
    }

    // req |-> ##[1:4] ack
    #[assert]
    fn req_ack(&self) {
        if req {
            seq!(delay(1..=4) -> ack);
        }
    }

    // (i_valid && i_ready, d = i_data) |-> (o_valid && o_ready)[->1] ##0 o_data == d
    #[assert]
    fn echo_data(&self) {
        if self.req_fire() {
            let d: u32 = i_data;
            seq!(wait(o_valid && o_ready) -> o_data == d);
        }
    }

    // Auxiliary state update, at each edge of the block's clock
    #[on_clock]
    fn update(&mut self) {
        if o_valid && o_ready {
            let exp = self.expected.pop_front();
            check!(exp == Some(o_data), "data mismatch: expected {:?}, got {:#x}", exp, o_data);
            self.outstanding -= 1;
        }
        if self.req_fire() {
            self.expected.push_back(i_data);
            self.outstanding += 1;
        }
    }

    #[assert]
    fn no_overrun(&self) {
        self.outstanding <= 4;
    }

    // full ##[1:$] empty
    #[cover]
    fn fill_drain(&self) {
        seq!(full -> delay(1..) -> empty);
    }
}
```

- `#[assert]` and `#[cover]` mark properties; SVA's `assume` is for formal
  tools and is not provided.
- A property takes `&self` and a hook `&mut self`; the macros reject a
  property that changes state.
- The clock and the reset are fields too, marked `#[clock]` and
  `#[reset]`, of the prelude's `Clock` and `Reset`: one clock, and at most
  one reset. They bind as the other members do (see
  [Binding to the design](#binding-to-the-design)), and clocking blocks
  declare theirs the same way (see
  [modport-clocking-design.md](modport-clocking-design.md#clock-and-reset-as-fields)).
- Signal fields are named in property bodies by their bare names; auxiliary
  state through `self`.
- Field types are those of clocking blocks (`SignalValue`): `bool`, the
  unsigned integers up to `u128` and `BigUint`. Signals are two-state for
  now (see
  [host-layer-design.md](host-layer-design.md#two-state-signals-first)).

## Temporal syntax

Properties are written in Rust functions, so that rustfmt and rust-analyzer
work on everything around them, and with keywords rather than SVA's symbols
(`|->`, `##`, `[*]`, `[->]`), which are hard to read. A sequence is written
in `seq!(..)`, its elements chained by `->`, so that it reads as one
continuing expression.

### The time model

- **`->` is `&&` extended over time**: the next element starts at the
  cycle where the previous one ended (SVA's `##0`, fusion). It advances no
  time itself. Between two booleans it is exactly `&&`; over elements
  that take time it reads the same way:

  | here | read as | SVA |
  |---|---|---|
  | `a -> b` | a and b | `a ##0 b`, that is `a && b` |
  | `a -> delay(1..=3) -> b` | a, and 1 to 3 cycles pass, and b | `a ##[1:3] b` |
  | `wait(x) -> y` | wait for x, and y (at that cycle) | `x[->1] ##0 y` |
  | `repeat(1.., busy) -> done` | busy on, and done (at its last cycle) | `busy[*1:$] ##0 done` |

  So `a -> b` is allowed and means `a && b`; between plain booleans,
  `a && b` is the usual way to write it.
- **`->` is not SystemVerilog's implication.** In SV, `a -> b` is "a
  implies b"; within `seq!` it is "a and b". Implication is written with
  `if` (see below), which keeps the two apart.
- **Time is taken by the elements `delay`, `wait` and `repeat`** only.
- **Time advances only within `seq!`.** A property body is same-cycle
  conditions and `let`s, ending with at most one `seq!`; nothing follows it.

`seq!` is not expanded on its own: `#[assert]`, `#[cover]` and the other
attributes read it as part of the function they rewrite. A placeholder
definition only reports its use elsewhere. Its contents are a token stream
to Rust, so `->` compiles there (it does not in a function body, which rustc
parses before the attribute sees it), and rustfmt leaves them as written
while formatting the rest.

### Implication

The condition (antecedent) and the expectation (consequent) are kept apart
syntactically: the condition goes in the `if` header, the expectation in
the body.

| SVA | here |
|---|---|
| `a \|-> p` | `if a { p }` |
| `a \|=> s` | `if a { seq!(delay(1) -> s) }` |
| `s \|-> p`, `s` a sequence | `if seq!(s) { p }`, e.g. `if seq!(a -> delay(1..=3) -> b) { c; }` |

The two sides differ in how multiple matches count. Every match of the
condition is checked (universal); the expectation holds if one of its
matches does (existential). `a ##[1:3] b |-> c` requires `c` at every cycle
where `b` matches, so it is written with the whole sequence in the header:

```rust
if seq!(a -> delay(1..=3) -> b) {
    c;
}
```

A nested condition after a delay in the body, as
`if a { delay(1..=3); if b { c; } }` would be, would fall on the
expectation's side, where one match suffices: the property would hold
whenever `b` is false at any cycle of the window, which is almost always.
Since time advances only within `seq!` and nothing follows it, such a
condition cannot be written.

### Elements and operators

| SVA | here | notes |
|---|---|---|
| `a ##0 b` (booleans) | `a && b` | the same cycle |
| `##n`, `##[m:n]`, `##[m:$]` | `delay(n)`, `delay(m..=n)`, `delay(m..)` | Veryl's range notation |
| `s[*n]`, `s[*m:$]` | `repeat(n, s)`, `repeat(m.., s)` | consecutive: one cycle between repetitions, as in SVA |
| `b[->n]`, `b[->m:n]` | `happen(n, b)`, `happen(m..=n, b)` | intermittent: ends where `b` holds the n-th time, gaps allowed; counts cycles where `b` holds, not rises |
| `s.triggered` | `ended(s)` | a boolean: true at a cycle where a match of `s` ends, whenever it started |
| `s.triggered[->n]` | `happen(n, ended(s))` | a sequence happening n times, counted at its end points |
| `b[->1]` | `wait(b)` | the same as `happen(1, b)`, kept for the most common case; matches at the current cycle if `b` holds |
| `a throughout s` | `throughout(a, s)` | |
| `a until b` | `until(a, b)` | weak |
| `s_eventually p` | `eventually(p)` | strong |
| `not s` | `never(s)` | |
| `(.., d = x)` | `let d: T = x` | an element of `seq!`; in a body, a plain `let`. Its type is written: see below |
| `$past`, `$rose`, `$fell`, `$stable`, `$changed` | `past(x)`, `past(x, n)`, `rose(x)`, `fell(x)`, `stable(x)`, `changed(x)` | |
| `$onehot`, `$onehot0`, `$countones` | `onehot(x)`, `onehot0(x)`, `countones(x)` | |

A sequence argument (`s` above) is one element as it is (`repeat(1.., busy)`,
`throughout(busy, wait(done))`, `eventually(ack)`), or a chain in `seq!`
(`never(seq!(grant -> delay(1) -> grant && !release))`).

**A `let` kept across cycles has its type written**: a `let` within
`seq!`, and a `let` in a body followed in its block by a statement holding
a `seq!` (`let d: u32 = i_data;`). Its value is kept with the attempt, in
a struct the macro generates, and a proc macro does not know the type of
an expression. A `let` used within its cycle only is a plain Rust `let`,
its type inferred. The type is `Clone + PartialEq + Default` (attempts
with equal values merge).

**`past` and the functions built on it read signals only**: `past(x, n)`
evaluates `x` over the samples of `n` edges before (`n` a literal), so `x`
names the block's signals, not its auxiliary state or `let`s. Before `n`
edges have passed, the samples are zeros, as SVA's initial values. The
samples are taken at every edge of the clock, during the reset too, as
SVA's sampled values are whatever `disable iff` says: the simulator calls
`on_reset` instead of `on_clock` at each edge while the reset is asserted,
and both move the samples on.

### Examples

Each with the SVA it stands for and how it reads:

```rust
#[assertions]
impl BridgeAssertions {
    // SVA:  o_valid && !i_ready |=> o_valid && $stable(o_data)
    // Read: if o_valid && !i_ready: 1 cycle passes, and o_valid && stable(o_data)
    #[assert]
    fn hold_while_stalled(&self) {
        if o_valid && !i_ready {
            seq!(delay(1) -> o_valid && stable(o_data));
        }
    }

    // SVA:  req |-> ##[1:4] ack
    // Read: if req: 1 to 4 cycles pass, and ack
    #[assert]
    fn req_ack(&self) {
        if req {
            seq!(delay(1..=4) -> ack);
        }
    }

    // SVA:  a ##[1:3] b |-> c
    // Read: if a, and 1 to 3 cycles pass, and b: c (at every match)
    #[assert]
    fn b_then_c(&self) {
        if seq!(a -> delay(1..=3) -> b) {
            c;
        }
    }

    // SVA:  (i_valid && i_ready, d = i_data) |-> (o_valid && o_ready)[->1] ##0 o_data == d
    // Read: if i_valid && i_ready, keep i_data as d: wait for o_valid && o_ready, and o_data == d
    #[assert]
    fn echo_data(&self) {
        if i_valid && i_ready {
            let d: u32 = i_data;
            seq!(wait(o_valid && o_ready) -> o_data == d);
        }
    }

    // SVA:  (a, d = data) ##1 b |-> out == d
    // Read: if a, and keep data as d, and 1 cycle passes, and b: out == d
    #[assert]
    fn keep_mid_sequence(&self) {
        if seq!(a -> let d: u32 = data -> delay(1) -> b) {
            out == d;
        }
    }

    // SVA:  full |-> !i_push
    // Read: if full, !i_push (same cycle)
    #[assert]
    fn no_overflow(&self) {
        if full {
            !i_push;
        }
    }

    // SVA:  i_push && !i_pop && !full |=> count == $past(count) + 1
    // Read: if i_push && !i_pop && !full: 1 cycle passes, and count == past(count) + 1
    #[assert]
    fn count_inc(&self) {
        if i_push && !i_pop && !full {
            seq!(delay(1) -> count == past(count) + 1);
        }
    }

    // SVA:  $onehot0(grant)
    // Read: onehot0(grant) at every cycle
    #[assert]
    fn grant_onehot(&self) {
        onehot0(grant);
    }

    // SVA:  start |=> busy[*1:$] ##1 done
    // Read: if start: 1 cycle passes, and busy for 1 or more cycles, and 1 cycle passes, and done
    #[assert]
    fn burst(&self) {
        if start {
            seq!(delay(1) -> repeat(1.., busy) -> delay(1) -> done);
        }
    }

    // SVA:  start |=> busy throughout done[->1]
    // Read: if start: 1 cycle passes, and busy throughout waiting for done
    #[assert]
    fn busy_until_done(&self) {
        if start {
            seq!(delay(1) -> throughout(busy, wait(done)));
        }
    }

    // SVA:  not (grant ##1 (grant && !release))
    // Read: never grant, and 1 cycle passes, and grant && !release
    #[assert]
    fn no_double_grant(&self) {
        never(seq!(grant -> delay(1) -> grant && !release));
    }

    // SVA:  req |-> s_eventually ack
    // Read: if req, eventually ack
    #[assert]
    fn req_eventually_ack(&self) {
        if req {
            eventually(ack);
        }
    }

    // SVA:  full ##[1:$] empty
    // Read: full, and 1 or more cycles pass, and empty
    #[cover]
    fn fill_drain(&self) {
        seq!(full -> delay(1..) -> empty);
    }
}
```

All but `keep_mid_sequence` were checked to be valid Rust and left
unchanged by rustfmt; `let` within `seq!` is the macro's own syntax.

### Named sequences and properties

In the first version, as SVA's `sequence` and `property` declarations: a
sequence or a property is named, with formal arguments, and used by the
asserts and covers of the same impl, so that one property serves an assert
and a cover (and later an assume):

```rust
#[assertions]
impl BridgeAssertions {
    // SVA's `sequence handshake(valid, ready)`
    #[sequence]
    fn handshake(valid: bool, ready: bool) {
        seq!(valid -> wait(ready));
    }

    // SVA's `property stable_until_ready(valid, ready, data)`
    #[property]
    fn stable_until_ready(valid: bool, ready: bool, data: u32) {
        if valid && !ready {
            seq!(delay(1) -> valid && stable(data));
        }
    }

    #[assert]
    fn req_hold(&self) {
        stable_until_ready(i_valid, i_ready, i_data);
    }

    #[assert]
    fn rsp_hold(&self) {
        stable_until_ready(o_valid, o_ready, o_data);
    }

    #[cover]
    fn req_done(&self) {
        seq!(handshake(i_valid, i_ready) -> delay(1..) -> o_valid);
    }
}
```

`#[assertions]` receives the whole impl, so it sees the named ones and
expands each use in place before lowering to the property IR; the engine
is not involved. The expansion:

- **substitutes arguments by name**, as SVA's formal arguments: an argument
  is an expression evaluated at each cycle it is referred to, not a value
  taken at the call (`data` in `stable_until_ready` is the data of the
  cycle after the condition);
- **renames local `let`s** so that they do not collide where expanded;
- **rejects recursion**;
- **checks the arguments' types** against the signature, by bindings
  generated only for the check; the signature is plain Rust, so
  rust-analyzer sees it.

Only the same impl's are visible: a proc macro sees its own tokens alone.
Reuse beyond it is by assertion blocks as types (see
[Deferred items](#deferred-items)).

### Not in the first version

Property `and` / `or` (two asserts do for `and`; `or` is rare), SVA's
`if`/`else` property, `intersect`, `within`, `first_match`, non-consecutive
repetition (`[=n]`). The subset is to be
confirmed. Sequences written as `;`-separated statements, beside `seq!`,
are left out until long sequences show the need: adding a second form
later breaks nothing, while removing one would.

## Semantics

The semantics are SVA's (IEEE 1800, chapter 16) except where noted.

- **Sampling**: properties read the values before the clock edge, as SVA's
  sampled values and as clocking blocks do.
- **Attempts**: every property starts an attempt at every edge of its
  clock; attempts run concurrently, each with its own copies of `let`
  variables.
- **Clock and reset**: given per assertion block (its `#[clock]` and
  `#[reset]` fields). Properties are evaluated at `on_clock` and attempts
  dropped at `on_reset`, as the simulator calls them; no edge or polarity
  is written or looked at. A block without a reset is never disabled. Properties of another clock
  domain go in another block; a property cannot override its block's clock.
- **Reset** (`disable iff`): while the reset is asserted, attempts are
  dropped and no attempts start. Whether a synchronous reset reaches the
  component at its assertion as an asynchronous one does (`on_reset`) is to
  be checked.
- **Strength**: sequences in an expectation are weak, as in SVA:
  in `if req { seq!(wait(ack)); }`, an attempt whose `ack` has not come is
  left incomplete. At the end of the simulation, attempts still running are
  reported as incomplete, not as failures, as commercial simulators do.
  `eventually` is strong: an attempt still running at the end fails.
- **`wait`** is level-sensitive, as `b[->1]` is (`!b[*0:$] ##1 b`): it
  matches at the current cycle if its condition holds there, and does not
  wait for the condition to fall first; "from the next cycle" is
  `seq!(delay(1) -> wait(b))`.
- **`happen`** is too (`b[->n]`, defined as `(!b[*0:$] ##1 b)[*n]`): it
  counts the cycles where `b` holds, so a `b` held for three cycles makes
  `happen(3, b)` match at the third, after one pulse. Counting pulses is
  `happen(n, rose(b))`; whether a `b` already high at the start counts then
  depends on the sample before it.
- **`happen` takes a boolean**, as SVA's `[->n]` does (`!s` is not
  defined for a sequence spanning cycles and matching several times). A
  sequence happening n times is `happen(n, ended(s))`, as SVA's
  `s.triggered[->n]`: counted at the cycles where a match of `s` ends,
  once per cycle however many end there, **whenever the match started**,
  so after `req` a handshake that started before `req` and ends after it
  counts too.
- **`ended(s)`** is true at a cycle where a match of `s` ends, whenever
  that match started, as SVA's `s.triggered` (`.ended` in its earlier
  versions).
- **`repeat` is consecutive, `happen` intermittent.** `repeat(3, rose(a))`
  asks for three rises in three consecutive cycles, which a one-bit signal
  cannot do (a rise needs the cycle before at 0), so it never matches;
  three rises with gaps are `happen(3, rose(a))`.
- **An empty repetition is skipped** (deviation from SV): with a range
  from 0, none at all leaves what follows at the same cycle, so
  `repeat(0..=3, a) -> delay(1) -> b` with no `a` is `delay(1) -> b`,
  where SVA's `a[*0:3] ##1 b` is `b`. See
  [An empty repetition is skipped](#an-empty-repetition-is-skipped).
- **`rose` / `fell`** look at the least significant bit, as `$rose` and
  `$fell` do.
- **X and Z read as 0** (deviation from SV). Fields are converted as
  clocking blocks convert them, so `a == b` with an X in `a` may be true
  where SV's would be X (false as a condition). Checks for unknown values
  (`!data.is_unknown()`) wait for four-state fields, deferred with the
  host layer's.

## Auxiliary logic

SVA checkers often carry auxiliary logic. Its uses fall into three kinds,
each expressed differently:

| use | SVA | here |
|---|---|---|
| naming a condition | `wire`, `let` | a pure method taking `&self` |
| keeping a value per attempt (data at the request, compared at the response) | local variables, or auxiliary registers | `let` in the property |
| state across attempts (outstanding count, expected-data queue) | `always_ff` auxiliary logic | struct fields updated by `#[on_clock]` |

- **Order at each edge**: sample, evaluate the properties, then run the
  `#[on_clock]` hooks. Properties see the auxiliary state before the
  update: the state behaves as registers, as auxiliary `always_ff` logic
  sampled by SVA does.
- **Reset**: the auxiliary state is re-initialized from `Default`;
  `#[on_reset]` is written only for another initialization.
- **One instance of the state per bound target** (when assertions bind to
  module instances, see [Changes to Veryl](#changes-to-veryl)).
- **Immediate checks** in hooks use `check!(cond, "format", args..)`, as
  `$assert` in an `always_ff`. It reports through the same path as a
  property failure. Rust's `assert!` is not used: it panics, a panicked hook
  poisons the runtime, and its message carries no simulation time.

## Binding to the design

A block's definition does not say where it binds. Its members, its
clock and reset among them (`#[clock] i_clk: Clock` names a member of the
block, not a registration), are names of its own; where the block is bound, and to
which signals, is given when it is bound. The same block is bound in
several places, each with its own state.

Blocks are bound in two ways, which may be used together:

- **from the test module**, through the component's methods (below);
- **from the configuration file**, `Verald.toml`, which also gives a bind
  a name and a map of its members to other names. See
  [config-design.md](config-design.md).

Members bind by name, as clocking blocks' fields do, with the widths
checked against the field types when bound; a map, where given, overrides
some of the names. Binds of the same name at the same instance are an
error (the name is the block's unless given); reports name a bind by its
name and instance path (`FifoAssertions@dut0`). The proof of concept binds
through the test module's methods only, by member names; the
configuration file comes later.

### The test module's signals

The names the test module registers are read as instance paths: a
registered name `a.b.c` is the member `c` of the instance `a.b` (see
[config-design.md](config-design.md#instance-paths)). A block bound at an
instance path `p` binds each member to the registered name `p.<member>`;
at the empty path, to the member's own name.

```veryl
#[test(fifo_test)]
module fifo_test {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    inst bus: fifo_if;
    inst dut: fifo (i_clk: clk, i_rst: rst, bus: bus.slave);
    inst tb : $comp::verald_fifo;      // testbench and assertions in one component

    initial {
        tb.set_clock("dut.i_clk", clk);
        tb.set_reset("dut.i_rst", rst);
        tb.set_modport("dut.bus", bus.monitor);
        tb.bind_to_instance("dut", "FifoAssertions");
        tb.start("FifoTest");
        clk.start();
        rst.assert(clk);
    }
}
```

| Method | Does |
|---|---|
| `bind_to_instance(path, block)` | binds `block` at the instance `path` (wildcards allowed), by member names |
| `bind_to_module(module, block)` | binds `block` at every instance of `module` (reserved: needs Veryl's metadata) |

- The component is instantiated explicitly in the test module. A
  testbench that already registers its signals needs no extra wiring for
  its assertions beyond the bind.
- Without a testbench, the component exports the assertions alone and is
  used the same way: the test module registers the signals and starts it
  (a DUT driven by `$tb::clock_gen` and the test module's own `initial`).
  See [One component](#one-component-with-or-without-the-testbench).
- Signals reached this way are the test module's, which connect to the
  DUT's ports: enough for assertions on a DUT's inputs and outputs.
- It relies on references passed to component methods
  ([veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)),
  so it runs on Veryl's master with `veryl +local`.
- Blocks are bound when the component starts, once the registrations and
  the configuration are complete. `export!` lists the blocks a component
  has; where they bind is not in it.

### Later: module instances

The aim is assertions written for a module and applied to all its instances,
wherever they are in the hierarchy, bound with `bind_to_module`
or `[[modules]]` in the configuration, and with instance paths resolved in
the design rather than among the registered names:

```rust
#[assertions]
struct FifoAssertions {
    #[clock]   i_clk: Clock,
    #[reset]   i_rst: Reset,
    #[input]   i_push: bool,
    #[input]   i_pop: bool,
    #[input]   full: bool,
    #[observe] count: u8,
}
```

- **Ports are visible implicitly** (`#[input]`): they are the module's
  specification.
- **Internal signals are listed explicitly** (`#[observe]`): a dependency
  on the implementation is visible where it is declared, and breaks there
  when the RTL is refactored.
- Only signals at the module's top-level scope can be observed: `var` and
  `let`, and members of interface instances. Signals in generate blocks and
  in child instances cannot. A signal in a `for` generate block can be
  declared as an array at the top level instead.
- `proto` modules cannot be targets: they do not exist in a simulation.
- Signals are first bound at run time, by name. Later the struct is
  generated by a build script from metadata Veryl emits, so that names and
  widths are checked by rustc and completed by rust-analyzer.

## Sharing the host layer

Clocking blocks and assertions both read the design's signals. What they
share is not clocking's mechanism (`tick`, the waits, driving outputs) but
what talks to Veryl's host API: the host layer, a crate of its own below
the runtime and the assertions, specified in
[host-layer-design.md](host-layer-design.md). It is separated from the
runtime before the proof of concept, which is built on it.

What the assertions take from it:

- `Signals` and `MemberSource`: an assertion block's binding is generated
  over `S: Signals`, so it binds to the test module's registrations (the
  proof of concept) and later to an instance's probes, unchanged;
- `InputSignal<T>`, `SignalValue` and the direct reads: assertions read
  every field at every edge, through the staged buffers' pointers, without
  a host call;
- `ModportRead` and `InputOnly`, generated by `#[modport]`;
- the services (`fail`, `log`, `time`, `fired`, the reset state);
- `HookSubscriber`, from the proof of concept on: the assertions are a
  subscriber of the hooks, evaluating synchronously, without the runtime.

What the layer is to gain before the proof of concept:

- **`export!` over a list of subscribers**
  ([host-layer-design.md](host-layer-design.md#exporting-a-component)):
  one component holding the testbench, the assertions, or both, the hooks
  dispatched to them in order (assertions first).
- **The registrations in the layer**: the table of names to ports
  (`set_clock`, `set_reset`, `set_modport`, `set_input`, `set_output`)
  and its `Signals`, moved from the runtime, so that a component without
  the testbench has them too. What a name means beyond a port (`Clock` and
  `Reset`, which tasks wait on) stays in the runtime, over the layer's
  table.
- **The start, as an event of the layer**: the test module's `start`
  ends the registrations; the subscribers are told (to bind, and the
  runtime to run its test). Without the testbench, `start` takes no test
  name.
- **A failure known to the layer**: a subscriber's failure is recorded, so
  that the runtime, at the end, does not report the finish it caused as
  an unexpected one.

### Reusing users' modport structs

A `#[modport]` struct a testbench declares for its monitor (all members
`#[input]`) is used as it is in an assertion block, so an interface's
signals are declared once (`#[modport]` itself is described in
[modport-clocking-design.md](modport-clocking-design.md#typed-modports)):

```rust
#[modport]
struct BusMon {
    #[input] valid: bool,
    #[input] ready: bool,
    #[input] data: u32,
}

#[assertions]
struct BusAssertions {
    #[clock]   clk: Clock,
    #[reset]   rst: Reset,
    #[modport] bus: BusMon,     // tb.set_modport("bus", bus.monitor)
    outstanding: u32,           // auxiliary state
}

#[assertions]
impl BusAssertions {
    #[assert]
    fn hold_while_stalled(&self) {
        if bus.valid && !bus.ready {
            seq!(delay(1) -> bus.valid && stable(bus.data));
        }
    }
}
```

Assertions only read. A signal the testbench drives is an output of the
modport it registered for driving (`bus.master`) and cannot be read as an
input, and the value it last wrote is not reliable where something else
drives the signal. So assertion blocks bind to a modport registered for
monitoring, all inputs, and a `#[modport]` struct with an `#[output]` field
is rejected in an assertion block.

### Not shared

- **The macros' code generation.** `#[assertions]` generates its own code
  on the shared types; sharing `#[clocking]`'s internals would tie the two
  macros together.
- **Change notifications** from the simulator, instead of values: SVA
  evaluates at every edge whether or not a signal changed (new attempts,
  counting delays, `stable`, `past`); reading a staged buffer costs about
  what checking a changed flag does; and assertions keep the previous
  samples anyway (for `stable` and `past`), so skipping work when nothing
  relevant changed is done within the component. Flags would pay for many
  wide signals (`BigUint`, `Value`) or for a transport that cannot share
  memory; the simulator already compares outputs before writing them back
  (`apply_outputs`), so flags for inputs could be added cheaply when needed.

## Evaluation in the simulator

What exists, in Veryl's simulator (`crates/simulator/src/component`):

- A component's inputs are staged before the flip-flops commit and its
  hooks run after (`stage_inputs`, `fire`): the values before the edge, as
  SVA samples.
- An input's source is either a direct pointer to the variable's storage
  or an expression.
- `on_clock` / `on_reset` run for the events a component listens to;
  `fired_clock` tells which.

One component holds every assertion block and evaluates all of them in one
hook call per event, rather than one call per bound instance, so the cost
across the C ABI is per edge, not per instance. The properties are state
machines stepped synchronously at each edge.

From the proof of concept on, the assertions are a subscriber of the host
layer's hooks (see [host-layer-design.md](host-layer-design.md#hooks)), not
tasks of the runtime: at `on_clock`, each block whose clock fired reads the
sampled values, steps its properties and runs `#[on_clock]`; at `on_reset`,
a block whose reset fired drops its attempts and re-initializes its
auxiliary state; at `on_finish`, incomplete attempts and cover counts are
reported. Bound instances on module instances, later, are evaluated the
same way.

### Attempts as bit sets

Each sequence is lowered to an NFA (states that may be occupied at once,
one per possibility still open). At run time:

- **An attempt is allocated at each start** (each cycle its condition
  starts it), holding a bit set of the NFA states its branches occupy,
  its start time and its `let` variables. The attempts of a property are
  kept in a `Vec` reused from edge to edge.
- **Branches within an attempt are advanced by bit operations**: at each
  edge, its bits and the sampled values give the next bits, a fork being
  several bits set. No allocation, and branches in the same state merge by
  themselves.
- **An attempt ends** when one branch matches (an expectation needs one
  match: the attempt succeeds and is dropped), or when its bit set is
  empty (it fails, reported with its start time).
- **Attempts with equal bit sets and `let` values merge**: their futures
  are the same. The merged attempt keeps the earliest start time and
  counts the attempts merged, for the report. So without `let`, the
  attempts of a property are bounded by the number of bit sets reached,
  even with unbounded ranges (`delay(1..)`).
- **A `let` after a fork** (`delay(1..=3) -> let d: u32 = data -> ..`) takes a
  different value on each branch: such branches are split into attempts
  of their own when the `let` is taken.
- Ranges with large bounds (`delay(m..=n)`) keep `n` states, moved on by
  a one-bit shift of the bit set's words: an edge costs `n / 64` words.
  A counter in the attempt was considered and set aside: a thread may
  enter one delay at several cycles (after a range before it), so one
  counter per delay does not do, and counters do not merge as bits do.
- The condition side of an implication starts an expectation at each of
  its matches (every match is checked); `never(s)`, covers and `ended(s)`
  only need whether a match happened.

Rejected: one bit set per property shared by all its attempts (the
bit-parallel matching of regular expressions). It is the lightest, but
loses which attempt a branch belongs to: no start time in reports, no
`let` values, and failures missed, since an attempt whose last branch dies
cannot be told from one whose branches live on in the same bits. Also
rejected: branches kept one by one in a list as the only form, which
needs a copy per fork and a merge by comparison where bit operations do.

Possible optimizations:

- Staging only the inputs bound to the event that fired (today every input
  is staged before every hook).
- Reading, while no attempt of a property is running, only what the first
  cycle of its condition needs, as the wait macros read only what their
  condition names.

## Failures and reporting

- Failures are reported through the existing services (`fail`, `log`), as
  Verald's errors: the property, the block, the simulation time and the
  attempt's start time.
- **A failure ends the test.** A limit on the number of failures is
  deferred; it would use the reporting's `max_errors`.
- At the end: incomplete attempts and cover counts.

## Changes to Veryl

None for the proof of concept beyond veryl-lang/veryl#3701 and #3717. For
assertions on module instances:

- **Analysis metadata**: per module definition, its ports, top-level
  variables and interface members, their types and widths, and the kinds of
  its clocks and resets. `veryl dump --ir` already prints this as debug
  text; what is needed is a structured, stable form (serde). A hash of the
  sources lets a component check, when it connects, that the metadata
  matches the simulation.
- **Host API**:
  - enumerating the instances of a module definition after elaboration;
  - probes: an input added at `create`, whose source is a variable of an
    instance (the existing direct inputs);
  - subscribing to the events of an instance's clocks and resets;
  - keeping observed internal signals from being optimized away: declared
    statically (the component manifest) before the simulator optimizes, or
    top-level variables always kept in storage.

Veryl's `bind` is not involved: the component reads the signals itself.

## Proof of concept

Scope:

- one component, with the testbench or without it, through `export!`
  over subscribers;
- assertion blocks on the test module's signals, bound at instance paths
  among the registrations, which the host layer keeps, with
  `bind_to_instance` (`Verald.toml` comes later);
- the temporal syntax above, compiled through the property IR into state
  machines;
- auxiliary state with `#[on_clock]` and `check!`;
- failures ending the test.

Implemented so far (`crates/assertion`, `examples/regblock`):

- `#[assertions]` on the struct and the impl. The struct's signal fields
  move to a generated signals struct, sampled at each edge; the fields left
  are the auxiliary state (`Default`, re-initialized at a reset). The
  struct's expansion defines a `macro_rules!` carrying the signals' names,
  through which the impl's goes, so that bare names are rewritten into the
  sampled values: **the struct comes before its impl, in the same module**.
- Properties: `if` conditions without `else`, `let`s, and expectations,
  every one of which must hold; a failure names the expectation as
  written, and the attempt's start when it started at an earlier edge.
- `seq!` with booleans, `delay(n)`, `delay(m..=n)`, `delay(m..)`,
  `let name: T = x`, `wait(b)`, `happen(n, b)` (and ranges), `repeat(n, s)`
  (and ranges), `throughout(a, s)`, and `ended(s)` as a boolean anywhere in
  a property; as an expectation ending a body and as an `if`'s condition
  (every match evaluates the body). A statement holding a `seq!` ends its
  block. `repeat`'s and `happen`'s ranges may start at 0: none at all
  skips the element (see [An empty repetition is skipped](#an-empty-repetition-is-skipped));
  a count alone is 1 or more. `until(a, b)` is an element as well as a
  property ending a body. `#[cover]` is an error (deferred).
- `#[sequence]` and `#[property]` (see
  [Named sequences and properties](#named-sequences-and-properties)),
  expanded where used, before the NFA is built: a sequence's call is an
  element (in `seq!`, or as a sequence argument), a property's call a
  statement of a body. Arguments are substituted for the parameters as
  tokens (in parentheses unless an operand of their own, such as a name, a
  field or a call), not where a name is a field or a path's segment; the
  body's `let`s are renamed per expansion; recursion, direct or not, is an
  error, as is a named item used outside a property. The types are checked
  by binding each argument, by reference, to a variable of its
  parameter's type, in `if false` (checked by rustc, never evaluated: an
  argument is evaluated where the body refers to it, not at the call): a
  type error points at the argument and notes the parameter's type. A named
  property that takes time (directly or through another) ends its body as
  a `seq!` does.
- `eventually(x)` and `never(s)` end a body. `eventually` is an
  expectation `delay(0..) -> x`, strong; `never` a condition sequence of
  its own whose match fails the property, reported with the attempt's
  start.
- At the end of the test (`on_finish`, not on a poisoned component), each
  expectation's attempts still running are logged as incomplete (how
  many, and the earliest start); a strong one's (`eventually`) fail the
  test instead, unless it failed already: a test ended by a failure cuts
  them short, so they are logged as incomplete too.
- The engine as in [Attempts as bit sets](#attempts-as-bit-sets): each
  `seq!` is built into an NFA whose states are points within a cycle,
  linked within the cycle (a condition, a `let`), to the next edge (a
  tick), or by a delay's chain of states. The macro generates a method per
  state, visiting it at an edge, and one per `seq!` visiting the states a
  branch occupies; the states are a bit set of `ceil(states / 64)` words.
  A delay's range takes one state per cycle, moved on by an edge as a
  one-bit shift of the words, so a long range costs its words, not its
  states (no counter: a thread may enter one delay at several cycles).
  `wait` and `happen` loop on a state until their condition holds;
  `repeat` copies its sequence up to its count (an unbounded one loops on
  its last copy), a `let` in it shared by the copies; `throughout` adds its
  condition to every state and delay inside. Condition threads with equal
  states and `let`s merge, keeping the earliest start; an expectation's
  attempt holds one bit set per value of its `let`s, and attempts with
  equal ones merge; a branch reaching a match by several paths counts once.
  At an edge the `ended` matchers are stepped first, then the `seq!`s from
  the last to the first, then the body, so what starts at an edge is not
  stepped again there.
- `ended(s)`: a matcher of its own per use, started at every edge and
  stepped as a condition sequence; its result at the edge is passed to
  every generated method. Its sequence runs apart from the attempts, so it
  reads no `let` and holds no `ended`; elsewhere than in a property
  (hooks, helpers) it is an error.
- Errors point at what the user wrote: the code the macros write in
  place of a name (a signal's read, a `let`'s, a check's reference) has
  spans in the macro's context, so that its names resolve and its
  parentheses are not linted, located at the user's name
  (`Span::located_at`), so that rustc points a type error there. Which
  span rustc reports is not guaranteed; if it changes, errors fall back to
  the `#[assertions]` attribute, with nothing else affected.
- `past(x)`, `past(x, n)`, `rose`, `fell`, `stable`, `changed` over the
  samples of earlier edges (a ring of `n + 1` signals structs, moved on
  at each edge by `on_clock` and, in reset, `on_reset`); `onehot`, `onehot0`, `countones` as functions on
  `verald_assertion::Bits` (in `verald::prelude`).
- `#[on_clock]` and `#[on_reset]` hooks, run after the properties;
  `check!` in them, an error elsewhere.
- Methods of the impl may call each other; the samples (and a hook's
  report) are passed on as hidden arguments.
- `bind_to_instance(path, block)`, wildcards included;
  `bind_to_module` is an error saying it is not supported yet.
- `verald::export!` with `assertions = [..]`, with the testbench or alone;
  the subscriber is dispatched before the testbench.
- `examples/regblock`: the same block (`regblock_assertions::BusAssertions`,
  in a library crate shared by two components, over the VIP crate's
  monitor modport) beside the testbench, and alone in a test
  module driving the bus itself. The protocol is written with `seq!` and
  `past`/`stable`; the registers' contents with auxiliary state and
  `check!`. Bugs injected for a check (a master not holding its request,
  wrong read data, `rvalid` after a write or never) were caught.

Checked while building it:

- `#[test]` modules' signals stay readable: the block reads the bus through
  the monitor modport the test module registers;
- a failure is reported as the assertion's (the property, the bind, the
  time and the expectation), and the testbench does not report the finish
  it causes as unexpected;
- when `on_reset` runs: at each edge of a reset the testbench holds
  (`$tb::reset_gen`), synchronous or asynchronous, in place of `on_clock`
  (see [host-layer-design.md](host-layer-design.md#the-state-of-a-reset));
  not checked yet: a reset the design drives.

## Formal verification: a later study

Not planned yet; recorded so that the simulation design leaves room for it.

### Approach: existing engines

Model checking engines (IC3/PDR, k-induction, BMC, and the SAT and SMT
solvers under them) are refined over years of the Hardware Model Checking
Competition (HWMCC); writing one to match them is not realistic. Veryl's
side builds the problem and reports the result in Veryl's terms:

```text
Veryl IR (design)  ─┐
property IR        ─┼─▶ transition system ─▶ BTOR2 / AIGER ─▶ existing engine
formal aux logic   ─┤    (init, constraints,
assume             ─┘     bad states)
                                   counterexample ─▶ report and waveform in Veryl names
```

- **A transition system**: the design, the monitors (the state machines the
  property IR already gives for simulation), the formal auxiliary logic,
  the initial state (the reset sequence), the constraints (`assume`), a bad
  state per assert, a target per cover.
- **Output formats**: BTOR2 (word level; close to Veryl's IR, better for
  wide data paths) first, AIGER (bit level; the most engines read it)
  later. Veryl's synthesizer may help with the bit level (not checked).
- **A driver**, as SymbiYosys: engines, depths, timeouts, a portfolio.
- **Counterexamples mapped back** to Veryl's names. Replayed as stimulus
  in Veryl's simulator, a counterexample gets its waveform and its failure
  report from what exists.
- **Another route**: monitors lowered to RTL with immediate assertions
  (`assert` in `always`), emitted as SV and checked with Yosys
  (`read_verilog -formal`) and SymbiYosys. Quickest to try, and immediate
  assertions need no SVA support from open-source Yosys; but it depends on
  Yosys and its counterexamples come in the SV's names.

Lean is not an engine for this: it is for proofs over parameters (any
width `N`), for trusted results (`bv_decide` checks a SAT solver's
certificate), or for proving the semantics and the translation themselves.

### What simulation-only design does not cover

1. **Auxiliary logic in Rust** cannot be translated. Simulation and formal
   auxiliary logic differ anyway: a scoreboard keeps the whole queue, while
   formal tracks one arbitrary datum (symbolic tracking, with an
   `$anyconst`-like value) to keep the state space small. So:
   - by default, `#[on_clock]` stays unrestricted Rust (simulation only),
     as UVM's scoreboards are;
   - an opt-in block (`#[assertions(formal)]`) restricts `#[on_clock]` to
     an RTL-like subset (fixed-width integers and `bool`, fixed arrays,
     `if` / `match`, assignments, explicit wrapping arithmetic), with the
     formal-only features (symbolic values); the macros reject the rest.
     Such a block runs in simulation too, as an SVA checker with RTL
     auxiliary logic does;
   - large auxiliary logic, or logic shared with the design, can be written
     in Veryl and registered as any signal.

   Properties are shared by both. Reusing a property as an assert in one
   context and an assume in another (assume-guarantee) is worth providing.
2. **`assume`** is needed: without input constraints, impossible inputs
   give false counterexamples.
3. **Initial state and X**: formal leaves uninitialized flip-flops free,
   unlike simulation's X read as 0; the reset sequence is to be modeled.
4. **Liveness**: strong properties (`eventually`) are not safety
   properties; they need liveness checking or a liveness-to-safety
   translation.
5. **Scale**: memories, wide data paths and multipliers are hard for SAT;
   word-level formats and engines fare better.

### Engines in Rust

Checked in November 2026; licenses and maintenance to be checked again
before adopting any.

- **rIC3** ([paper](https://arxiv.org/abs/2502.13605),
  [crate](https://lib.rs/crates/ric3)): a bit-level hardware model checker
  in Rust. IC3/PDR as its main engine, with BMC and k-induction, alone or
  as a portfolio. Reads AIGER; BTOR2 is bit-blasted to AIGER with
  btor2aiger. AIGER is simplified with ABC, then encoded to CNF for its SAT
  solvers (GipSAT, CaDiCaL, Kissat). Evaluated the best tool at HWMCC 2024,
  in the bit-level track and, through bit-blasting, in the word-level
  bit-vector track. Needs a nightly toolchain. **The license is unclear**:
  crates.io lists GPL-3.0, while lib.rs says it may not be used for
  commercial purposes without authorization. Even under GPL, it would be
  run as a separate process, not linked into Veryl (MIT / Apache-2.0).
- **patronus** ([GitHub](https://github.com/ekiwi/patronus),
  [docs.rs](https://docs.rs/crate/patronus/0.35.0/source/Readme.md)): a
  hardware bug-finding toolkit in Rust by Kevin Laeufer, around BTOR2
  (a parser, transition systems). BSD-3-Clause, so it can be linked; it
  may spare writing a BTOR2 writer. How far its model checking goes is to
  be checked in the repository.
- **batsat**: the SAT solver Verald's constrained random already uses
  behind its `SatSolver` trait (see
  [random-design.md](random-design.md#sat-solver)); enough for a BMC of
  its own.

A likely split: BTOR2 out of Veryl (with patronus's representation if it
fits), rIC3 as an external prover if its license allows, a BMC on batsat
for quick checks.

## Deferred items

- Formal verification (see [above](#formal-verification-a-later-study)).
- The subset of temporal operators.
- **Reusing assertion blocks** (next after the first version), as SVA's
  checkers bound in several places: an assertion block is a Rust type, so
  the same block is instantiated for several interfaces, from a shared
  crate too (a VIP's). Binding one block several times, and maps of its
  members, come with the first version (see
  [Binding to the design](#binding-to-the-design)). Needed for
  interfaces of other widths: generic blocks and modport structs, which
  `#[modport]` rejects today (`check_plain` in the macros' codegen), or
  fields of `BigUint` at a cost in speed.
- Named sequences and properties across impls and crates (a library of
  protocol sequences): a proc macro sees its own tokens only, so either
  definitions expanded into `macro_rules!` that pass their tokens on
  (compile time kept, complex), or properties built as IR at run time
  (simple, slower). Not before block reuse shows the need.
- **`#[cover]`**, with functional and code coverage: a cover means
  something only over a regression, its counts written per test and
  merged into one report, and that report is wanted for functional
  coverage (covergroups) and code coverage (the simulator's) first. So
  covers wait for that machinery (per-test results, merging, reports),
  and are one more source in it; `#[cover]` is an error until then (the
  user's decision, 2026-11-28).
- A limit on failures (`max_errors`).
- Assertions on module instances, with the host API and metadata above
  (`bind_to_module`, `[[modules]]` and `connections` in
  [config-design.md](config-design.md) are reserved for them).
- Structs generated from metadata.
- Emitting SVA from the property IR.

## Decisions and rejected alternatives

### Outside the Veryl language

Rejected: assertion syntax in Veryl, `assert property` in modules. Veryl is
kept an RTL language with a small grammar.

### Not on the generated SystemVerilog

Rejected: assertions written against the emitted SV. Its names follow the
emitter's rules (prefixes, mangled generics, clock and reset suffixes).

### Not `embed`

Rejected: an assertion language inside `embed (inline) sva {{{ .. }}}`.
Editors give no syntax highlighting inside `embed`.

### Not a language of its own

Rejected: a separate assertion language in its own files (`.vas`, with
`assertions for Fifo (i_clk, i_rst) { .. }`). Its parser and its tracking of
Veryl's grammar (expressions, scoping) would have to be maintained, and
editors and formatters would need support for it. A Rust DSL gets parsing
from `syn` and tooling from Rust, as constrained random does.

### One component, with or without the testbench

The assertions are a subscriber in the same component as the testbench,
not a component of their own, nor tasks of the runtime. One component
means one registration of each signal, one staging of the inputs, and a
failure the testbench knows of (not reported again as an unexpected
finish). A component of their own was found feasible (the host layer has
the hooks' entry and the services) but would register every signal twice
and stage it twice; tasks of the runtime would need the testbench, and
would be thrown away when the assertions bind to module instances.

The registrations move to the host layer so that the assertions do not
need the testbench: a DUT test without one exports the assertions alone.

### Not Veryl's `bind`

Rejected: emitting checkers and `bind` (which Veryl's simulator does not
process today: `bind` is only checked). The component evaluates the
properties and reads the signals itself, so nothing is instantiated in the
design.

### Explicit observation, not all internal signals

Considered: ports only, as a property is a specification of a module's
inputs and outputs; and every internal signal, as SVA in a module sees.
Ports only rules out white-box invariants (one-hot states, pointer
consistency), which catch bugs near their source. Every internal signal
hides the dependency on the implementation. Listing internal signals
(`#[observe]`) keeps both.

### Keywords, not SVA's symbols

SVA's `|->`, `|=>`, `##[m:n]`, `[*m:$]` and `[->n]` are hard to read.
Rust's expressions have no `->`-like operator: `>>` binds tighter than `&&`
and `==` (`a && b >> c` is `a && (b >> c)`) and a block cannot start a
statement before it, so it was rejected as an implication.

`->` is used only within `seq!`, between elements, for a sequence to read
as one continuing expression. It means `&&` extended over time (SVA's
`##0`), which explains every case with one rule: `a -> b` is `a && b`, and
`wait(x) -> y` checks `y` at the cycle `x` matches.

Considered first: reading `->` as "then" and rejecting two booleans chained
directly, since "a then b" suggests b after a while nothing takes time
between them, and since SystemVerilog's `->` is an implication. But read in
the flow of a condition ("if .., then ..") rather than of time, `->`
suggests no time passing; what remains is the implication reading, which
an error on `a -> b` alone would not remove from `wait(x) -> y` and the
like. Reading `->` as `&&` needs no special case: `a -> b` means what it
reads, and the documentation states that `->` is not SV's implication,
which is written with `if`. SVA's sequence `and` (two sequences starting
at one cycle, both matching) is a different operator; it is not in the
first version, and its name is to be kept apart from `->` if it comes.

### `repeat` and `happen`: one word for each kind of repetition

SVA has separate operators for consecutive (`[*n]`) and goto (`[->n]`)
repetition. Written as `repeat(n, s)` and `repeat(n, wait(b))`, the two
differed only by a `wait` easily overlooked, and `repeat(3, rose(a))` (never
matching) was easily written for `repeat(3, wait(rose(a)))`. So each kind
has its own word, a verb as `repeat` is:

- `repeat(n, s)`, consecutive, for sequences and booleans, as SVA's
  `[*n]` (SVA's own term for which is "consecutive repetition");
- `happen(n, b)`, intermittent, for booleans only, as SVA's `[->n]`; a
  sequence through `ended(s)` (SVA's `s.triggered`).

Considered for the intermittent one: `wait(b, n)` (a count on `wait`,
dropped); `recur` (pairs with `repeat`), `occur`, `collect`, `gather`,
`count` (says nothing of where it ends), `catch`, `reach`, `goto` (SVA's
term, opaque to others), `appear` and `arise`. `appear` and `arise` were
rejected for suggesting an edge: a signal held high for three cycles
"appears" once, and `arise` recalls `rose`, while the count is of cycles
where the condition holds. `happen` is neutral about duration.

Also considered: a word for a continuing state in place of `repeat`
(`stay`; `hold`, which may read as `stable`; `keep`; `last`; `continue`, a
Rust keyword). It reads best for booleans, but not for repeating a
sequence, so `repeat` is kept.

### `ended(s)`, and `happen` on booleans only

Considered first: `happen(n, s)` taking a sequence, counting the end points
of its matches that start at or after `happen` starts. It fits "after
`req`, three handshakes" more closely, but departs from SVA and tracks the
matches in flight for every attempt. Chosen instead: `happen` on booleans,
as `[->n]` is, and `ended(s)` for a sequence's end points, matching SVA's
`s.triggered[->n]` one to one and evaluated once per cycle for all
attempts. The name follows `rose`, `fell` and `changed`, predicates on the
current cycle in the past tense, and SVA's former `.ended`; rejected:
`triggered` (reads as causing something), a method `s.triggered()` (the
sampled-value functions are functions), `matched` (SVA's `.matched` is a
different, multi-clock feature), `done` and `finished` (`done` is a common
signal name).

### An empty repetition is skipped

`repeat(0..=3, a) -> delay(1) -> b` with no `a` reads as `delay(1) -> b`:
none at all is nothing, and what follows goes on at the same cycle, as
`->` (`&&` extended over time) suggests. SVA's empty match is not: there
`a[*0:3] ##1 b` with no `a` is `b` at the start (`empty ##1 b` is `b`,
the repetition taking a cycle of the delay after it), and `empty ##0 b`
does not match. Both were weighed; the reading of the text was preferred
(the user's decision), at the cost of SVA's `a[*0:n] ##1 b` not carrying
over as written. `repeat(0..=1, a) -> b` is meaningful here (`b` at the
start if there is no `a`). A count of 0 alone (`repeat(0, a)`) is an
error.

### `repeat`, not `*`

Considered: `busy * (1..)` and `wait(a) * 3` within `seq!`, after SVA's
`[*n]`. It parses without ambiguity (an element is a boolean, and a boolean
times an integer means nothing in Rust, so a top-level `*` can only be a
repetition), but it looks like ordinary multiplication next to arithmetic
in the same expressions (`count * 2 == x`), and ranges need parentheses
(`busy * 1..` is `(busy * 1)..` in Rust). `repeat(n, s)` stays the one way
to write a repetition.

### `implies`, not `then`

Considered for the separator (in the forms that have one): `then` suggests
"afterwards", inviting `a then b` to be read as `|=>`, and a sequence
`a then b` cannot be told from an implication.

### Blocks rather than closures

(Superseded by `seq!`: sequence arguments are now one element or a
`seq!`. Kept for the reasoning against closures.)

A closure (`repeat(1.., || { busy; })`) matches Rust's intuition of code
evaluated later and repeatedly, and is a value that could be passed around.
But a proc macro sees only its own tokens, so a closure defined elsewhere
could not be compiled into a state machine; the benefit would come only
with an embedded combinator library evaluated at run time, which is slower.
Blocks add no symbols.

Also rejected: properties as `async fn` with `delay(1).await`, which needs
no macro rewriting but allocates a future per attempt and needs attempts
forked for every match of a ranged delay.

### Notation: Rust functions, with sequences in `seq!`

Compared on twelve properties, then refined:

- **(A) Rust statements**: sequences as `;`-separated statements
  (`if { a; delay(1..=3); b; } { c; }`). rustfmt and rust-analyzer work
  throughout; but properties take six to ten lines each, a sequence has no
  feeling of one continuing expression, and a condition sequence needs the
  unusual `if { .. } {` form. `->` cannot replace `;` there: rustc parses
  an attributed function's body before the attribute sees it, and rejects
  `a -> b` ("`->` is not valid syntax for field accesses and method
  calls").
- **(B) a function-like macro with a syntax of its own**
  (`a delay(1..=3) b implies c`, or SVA-like `property!` declarations used
  by asserts): reads most naturally and frees the notation entirely (named,
  parameterized properties included). But rustfmt does not format it,
  rust-analyzer barely helps inside it, and once the whole property is a
  language of its own the question becomes whether to define a separate
  language after all. Its strongest argument there would be expressions:
  Rust's (`0xf`, no `a[3:0]`, Rust's integer rules) are not Veryl's
  (`4'hf`, `a[3:0]`, `{a, b}`, Veryl's widths); a separate language would
  also have its auxiliary logic in Veryl RTL rather than Rust, which suits
  formal verification. Not taken now; the property IR keeps the way open.
- **Also considered**: a comma-separated macro,
  `prop!(a, delay(1..=3), b, implies, c)`, formatted by rustfmt, but with
  the separator one argument among the others and no `let`.
- **Adopted**: (A)'s Rust functions, with sequences only in `seq!` chained
  by `->`: the structure (conditions in `if` headers, bodies, `let`s,
  helpers, hooks) stays Rust that the tools handle, and only the sequences,
  short as most are, are a token stream of the macro's.

Since every notation lowers to the same property IR, it can still be
changed, or another added, without touching the evaluation; best done
before many properties are written.

### Typed fields, not run-time-width values

Rejected: binding signals by names collected from the property bodies and
holding their values as bit vectors of a width known only at run time,
which is slower. Fields declared with Rust types, as in clocking blocks,
are checked when bound and later generated from metadata.

### Types written on `let`s kept across cycles

A `let` whose value an attempt keeps is stored in a struct generated per
property, and a proc macro cannot name an expression's type. Rejected:
values kept as `Box<dyn Any>` (an allocation and a downcast per value);
the type inferred when the `let` is a bare `#[input]` field (the
macro knows those types, but not a modport member's or an expression's,
so the rule would depend on the expression). Written types are checked
by rustc, and a `let` used within its cycle keeps Rust's inference.

### `past` over the samples of earlier edges

`past(x, n)` keeps the signals' samples of `n` edges: the block's signals
struct is bound `n + 1` times, as a ring, and each edge reads into the
oldest. `x` is evaluated with its names read from the sample `n` edges
back, so the macro needs no type of `x` and nothing is cloned. Rejected:
a history per `past` call of the value of `x` (needs the type of `x`).
The cost is that `x` reads signals only.

### X and Z read as 0

Considered: evaluating properties in four states, to match SV where X
makes a condition false, or failing on a sample with X or Z. Chosen: the
two-state conversion of clocking blocks; explicit checks for X and Z come
with four-state fields, deferred.

### Clock and reset as fields, as in clocking blocks

The clock and the reset are fields marked `#[clock]` and `#[reset]`, bound
by name with the other members, in assertion blocks and clocking blocks
alike. The reasons and the alternatives rejected are in
[modport-clocking-design.md](modport-clocking-design.md#clock-and-reset-as-fields).

### `check!`, not `assert!`

With failures ending the test, `assert!` would do (a panic poisons the
component, which is not used afterwards). `check!` is introduced from the
start so that a limit on failures can be added without changing what users
write.
