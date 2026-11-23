# Assertions: design study

> **Status:** a design study, not implemented. It records what has been
> decided about SVA-like assertions for Veryl's simulator, and why, so that
> the proof of concept and later changes can refer back to it. Names of
> crates, attributes and functions are tentative.

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

Assertions are a crate of Verald, as constrained random is
(`verald-random`): tentatively `verald-assert`, re-exported by
`verald`.

```text
#[assertions] struct / impl                (compile time, proc macros)
  -> property IR
  -> a state machine per property (Rust code)
test module's initial                      (run time)
  -> signals registered (tb.set_clock, tb.set_interface, ..)
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
[the macro alternative](#notation-a-rust-statements-and-b-a-function-like-macro))
and SVA can be emitted later from the same IR.

## Writing assertions

An assertion block is a struct, as a clocking block is: its fields declare
the signals it reads, with their Rust types, and its other fields are
auxiliary state. The impl holds the properties and the hooks.

```rust
use verald::prelude::*;

#[assertions(clock = "clk", reset = "rst")]
#[derive(Default)]
struct BridgeSva {
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
impl BridgeSva {
    // A combinational helper: a pure method
    fn req_fire(&self) -> bool {
        i_valid && i_ready
    }

    // req |-> ##[1:4] ack
    #[assert]
    fn req_ack(&self) {
        if req {
            delay(1..=4);
            ack;
        }
    }

    // (i_valid && i_ready, d = i_data) |-> (o_valid && o_ready)[->1] ##0 o_data == d
    #[assert]
    fn echo_data(&self) {
        if self.req_fire() {
            let d = i_data;
            wait(o_valid && o_ready);
            o_data == d;
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
        full;
        delay(1..);
        empty;
    }
}
```

- `#[assert]` and `#[cover]` mark properties; SVA's `assume` is for formal
  tools and is not provided.
- A property takes `&self` and a hook `&mut self`; the macros reject a
  property that changes state.
- Signal fields are named in property bodies by their bare names; auxiliary
  state through `self`.
- Field types are those of clocking blocks (`SignalValue`): `bool`, the
  unsigned integers up to `u128` and `BigUint`. Signals are two-state for
  now (see
  [host-layer-design.md](host-layer-design.md#two-state-signals-first)).

## Temporal syntax

Property bodies are written in Rust syntax, so that rustfmt and
rust-analyzer work on them, and with keywords rather than SVA's symbols
(`|->`, `##`, `[*]`, `[->]`), which are hard to read.

### The time model

A property body reads as procedural code: statements are evaluated in
order, and **time advances only at `delay` and `wait`**. Statements with no
delay between them are evaluated in the same cycle (`##0`). This is how
time passes in testbench code (`tick!`).

### Implication

The condition (antecedent) and the expectation (consequent) are kept apart
syntactically: the condition goes in the `if` header, the expectation in
the body.

| SVA | here |
|---|---|
| `a \|-> p` | `if a { p }` |
| `a \|=> p` | `if a { delay(1); p }` |
| `s \|-> p`, `s` a sequence | `if { s } { p }`, e.g. `if { a; delay(1..=3); b; } { c; }` |

The two sides differ in how multiple matches count. Every match of the
condition is checked (universal); the expectation holds if one of its
matches does (existential). `a ##[1:3] b |-> c` requires `c` at every cycle
where `b` matches, so it is written with the whole sequence in the header:

```rust
if { a; delay(1..=3); b; } {
    c;
}
```

Written with a nested `if` instead, `if a { delay(1..=3); if b { c; } }`,
the inner condition would fall on the expectation's side, where one match
suffices: the property would hold whenever `b` is false at any cycle of the
window, which is almost always. **An `if` in a body after time has advanced
(`delay`, `wait`, `repeat`) is therefore an error**, pointing to the header
form. An `if`/`else` before any delay is a same-cycle condition and is
allowed.

### Operators

| SVA | here | notes |
|---|---|---|
| `##n`, `##[m:n]`, `##[m:$]` | `delay(n);`, `delay(m..=n);`, `delay(m..);` | Veryl's range notation |
| `b[->1]` | `wait(b);` | matches at the current cycle if `b` holds |
| `s[*n]`, `s[*m:$]` | `repeat(n, { s });`, `repeat(m.., { s });` | one cycle between repetitions, as in SVA |
| `a throughout s` | `throughout(a, { s });` | |
| `a until b` | `until(a, b);` | weak |
| `s_eventually p` | `eventually({ p });` | strong |
| `not s` | `never({ s });` | |
| `$past`, `$rose`, `$fell`, `$stable`, `$changed` | `past(x)`, `past(x, n)`, `rose(x)`, `fell(x)`, `stable(x)`, `changed(x)` | |
| `$onehot`, `$onehot0`, `$countones` | `onehot(x)`, `onehot0(x)`, `countones(x)` | |

Sequence arguments are block expressions (`{ ... }`), not closures; see
[blocks rather than closures](#blocks-rather-than-closures). Whether a bare
boolean is accepted where a block is expected (`repeat(1.., busy)`) is to be
decided; it would keep rustfmt from spreading a one-signal block over three
lines.

### Examples

```rust
// o_valid && !i_ready |=> o_valid && $stable(o_data)
#[assert]
fn hold_while_stalled(&self) {
    if o_valid && !i_ready {
        delay(1);
        o_valid && stable(o_data);
    }
}

// start |=> busy[*1:$] ##1 done
#[assert]
fn burst(&self) {
    if start {
        delay(1);
        repeat(1.., {
            busy;
        });
        delay(1);
        done;
    }
}

// start |=> busy throughout done[->1]
#[assert]
fn busy_until_done(&self) {
    if start {
        delay(1);
        throughout(busy, {
            wait(done);
        });
    }
}

// not (grant ##1 (grant && !release))
#[assert]
fn no_double_grant(&self) {
    never({
        grant;
        delay(1);
        grant && !release;
    });
}

// req |-> s_eventually ack
#[assert]
fn req_eventually_ack(&self) {
    if req {
        eventually({
            ack;
        });
    }
}
```

### Not in the first version

Property `and` / `or` (two asserts do for `and`; `or` is rare), SVA's
`if`/`else` property, `intersect`, `within`, `first_match`, non-consecutive
repetition (`[=n]`), `[->n]` for `n > 1`, and parameterized properties. The
subset is to be confirmed.

## Semantics

The semantics are SVA's (IEEE 1800, chapter 16) except where noted.

- **Sampling**: properties read the values before the clock edge, as SVA's
  sampled values and as clocking blocks do.
- **Attempts**: every property starts an attempt at every edge of its
  clock; attempts run concurrently, each with its own copies of `let`
  variables.
- **Clock and reset**: given per assertion block (`clock = ..`,
  `reset = ..`). The edge and the reset's polarity come from the Veryl types
  (`clock_posedge`, `reset_async_low`, ..), so they are never written. A
  block without a reset is never disabled. Properties of another clock
  domain go in another block; a property cannot override its block's clock.
- **Reset** (`disable iff`): while the reset is asserted, attempts are
  dropped and no attempts start. Whether a synchronous reset reaches the
  component at its assertion as an asynchronous one does (`on_reset`) is to
  be checked.
- **Strength**: sequences in an expectation are weak, as in SVA:
  `if req { wait(ack); }` does not fail if `ack` never comes. `eventually`
  is strong. At the end of the simulation, attempts still running are
  reported as incomplete, not as failures, as commercial simulators do.
- **`wait`** matches at the current cycle if its condition holds there, as
  `b[->1]` does; "from the next cycle" is `delay(1); wait(b);`.
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

### Proof of concept: the test module's signals

Signals are bound as clocking blocks bind them: by field name among what the
test module registered, with the widths checked against the field types when
bound.

```veryl
#[test(fifo_test)]
module fifo_test {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    inst bus: fifo_if;
    inst dut: fifo (i_clk: clk, i_rst: rst, bus: bus.slave);
    inst tb : $comp::verald_fifo;      // testbench and assertions in one component

    initial {
        tb.set_clock("clk", clk);
        tb.set_reset("rst", rst);
        tb.set_interface("bus", bus.master);
        tb.start("FifoTest");
        clk.start();
        rst.assert(clk);
    }
}
```

- The component is instantiated explicitly in the test module. A
  testbench that already registers its signals needs no extra wiring for
  its assertions.
- Signals reached this way are the test module's, which connect to the
  DUT's ports: enough for assertions on a DUT's inputs and outputs.
- It relies on references passed to component methods
  ([veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)),
  so it runs on the `tbf` branch with `veryl +local`.
- How an assertion block is registered and when it is bound (with
  `tb.start`, or by a method of its own) is to be decided.

### Later: module instances

The aim is assertions written for a module and applied to all its instances,
wherever they are in the hierarchy:

```rust
#[assertions(target = "Fifo", clock = "i_clk", reset = "i_rst")]
struct FifoSva {
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
  proof of concept, through the runtime's `Start` and `Scope`) and later
  to an instance's probes, unchanged;
- `InputSignal<T>`, `SignalValue` and the direct reads: assertions read
  every field at every edge, through the staged buffers' pointers, without
  a host call;
- `ModportRead` and `InputOnly`, generated by `#[modport]`;
- the services (`fail`, `log`, `time`, `fired`, the reset state);
- in the full implementation, `HookSubscriber`: the assertions are a
  subscriber of the hooks, evaluating synchronously, without the runtime.

### Reusing users' modport structs

A `#[modport]` struct a testbench declares for its monitor (all members
`#[input]`) is used as it is in an assertion block, so an interface's
signals are declared once:

```rust
#[modport]
struct BusMon {
    #[input] valid: bool,
    #[input] ready: bool,
    #[input] data: u32,
}

#[assertions(clock = "clk", reset = "rst")]
struct BusSva {
    #[modport] bus: BusMon,     // tb.set_interface("bus", bus.monitor)
    outstanding: u32,           // auxiliary state
}

#[assertions]
impl BusSva {
    #[assert]
    fn hold_while_stalled(&self) {
        if bus.valid && !bus.ready {
            delay(1);
            bus.valid && stable(bus.data);
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

For the proof of concept, each assertion block runs as a task of the
runtime, under `run_with_reset`: wait for the clock's edge, read the
sampled values, step the properties, run `#[on_clock]`. The reset handling
(attempts dropped, auxiliary state re-initialized) comes from
`run_with_reset`, and a failure returned as the task's `Err` ends the test,
so the runtime needs no change. On module instances the assertions are a
subscriber of the host layer's hooks instead, evaluating every bound
instance synchronously (see
[host-layer-design.md](host-layer-design.md#hooks)).

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

- assertion blocks on the test module's signals, bound through the
  registrations;
- the temporal syntax above, compiled through the property IR into state
  machines;
- auxiliary state with `#[on_clock]` and `check!`;
- failures ending the test.

To be checked while building it:

- `#[test]` modules are not optimized, so their signals stay readable;
- how a failure is reported (as an assertion failure with the time, not as
  a crashed component);
- when `on_reset` runs for a synchronous reset.

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
- A limit on failures (`max_errors`).
- Assertions on module instances, with the host API and metadata above.
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
statement before it, so it was rejected.

### `implies`, not `then`

Considered for the separator (in the forms that have one): `then` suggests
"afterwards", inviting `a then b` to be read as `|=>`, and a sequence
`a then b` cannot be told from an implication.

### Blocks rather than closures

A closure (`repeat(1.., || { busy; })`) matches Rust's intuition of code
evaluated later and repeatedly, and is a value that could be passed around.
But a proc macro sees only its own tokens, so a closure defined elsewhere
could not be compiled into a state machine; the benefit would come only
with an embedded combinator library evaluated at run time, which is slower.
Blocks add no symbols.

Also rejected: properties as `async fn` with `delay(1).await`, which needs
no macro rewriting but allocates a future per attempt and needs attempts
forked for every match of a ranged delay.

### Notation: (A) Rust statements, and (B) a function-like macro

Compared on twelve properties:

- (A), adopted for now: Rust statements, as above. rustfmt and
  rust-analyzer work; properties take six to ten lines each and have no
  feeling of one continuing expression.
- (B), kept as an alternative: a function-like macro with a syntax of its
  own:

  ```rust
  properties! {
      for BridgeSva {
          assert req_ack:
              req implies delay(1..=4) ack;
          assert b_then_c:
              a delay(1..=3) b implies c;
          cover fill_drain:
              full delay(1..) empty;
      }
  }
  ```

  It reads most naturally (one or two lines a property, as SVA), and only
  the temporal syntax is parsed by the macro (tokens from Rust, booleans by
  `syn`). But rustfmt does not format it, rust-analyzer barely helps inside
  it, the keywords become reserved, and the extent of an operand (after
  `delay(1)`, up to which token?) and the boundary between same-cycle
  elements need rules.

- Also considered: a comma-separated macro,
  `prop!(a, delay(1..=3), b, implies, c)`, whose arguments are Rust
  expressions, so rustfmt formats it; the separator is one argument among
  the others, and `let` cannot be written.

Since both lower to the same property IR, (A) can be replaced by (B), or
both kept, without touching the evaluation; best done before many
properties are written. The rule against an `if` after a delay is specific
to (A).

### Typed fields, not run-time-width values

Rejected: binding signals by names collected from the property bodies and
holding their values as bit vectors of a width known only at run time,
which is slower. Fields declared with Rust types, as in clocking blocks,
are checked when bound and later generated from metadata.

### X and Z read as 0

Considered: evaluating properties in four states, to match SV where X
makes a condition false, or failing on a sample with X or Z. Chosen: the
two-state conversion of clocking blocks; explicit checks for X and Z come
with four-state fields, deferred.

### `check!`, not `assert!`

With failures ending the test, `assert!` would do (a panic poisons the
runtime, which is not used afterwards). `check!` is introduced from the
start so that a limit on failures can be added without changing what users
write.
