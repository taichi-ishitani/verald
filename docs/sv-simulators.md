# Running a testbench on a SystemVerilog simulator

How a testbench built with veryl-tbf would run on a SystemVerilog simulator
as well as on Veryl's, with the testbench's code unchanged: the shape
agreed so far, the reasons behind it, and a prototype
(`crates/veryl-tbf-dpi`, `dpi/`, `examples/adder/verilator`) that runs the
adder's test on Verilator.

## Contents

- [Where the two meet](#where-the-two-meet)
- [Choosing the simulator](#choosing-the-simulator)
- [DPI, not VPI](#dpi-not-vpi)
- [Signals](#signals)
- [Hooks](#hooks)
- [Ending the test](#ending-the-test)
- [Logs, seeds, time, X](#logs-seeds-time-x)
- [The DPI interface](#the-dpi-interface)
- [A prototype](#a-prototype)

## Where the two meet

A testbench touches the simulator only through the component SDK
(`veryl-component`): its hooks (`on_init`, `on_clock`, `on_reset`,
`on_finish`), its methods (`set_clock`, `set_interface`, `set`, `start`,
..), and the host's services behind `SimCtx` (reading and writing ports
and modport members, time, logging, failing, finishing, the seed). Running
on another simulator is a second host behind the same API: the testbench's
code and veryl-tbf itself stay the same.

| Layer | Veryl's simulator | A SystemVerilog simulator |
|---|---|---|
| The testbench, veryl-tbf | the same | the same |
| The SDK's API (`SimCtx`, `ModportRef`, `Value`) | the same | the same |
| The host behind it | Veryl's (a vtable it passes) | a DPI host (buffers and a layout table) |
| The entry points | `export!`'s, which Veryl loads | DPI functions (`tbf_on_clock`, ..) |
| The test module | run by Veryl | a SystemVerilog wrapper, by hand for now |

## Choosing the simulator

At compile time, with a cargo feature (`dpi`) of the SDK that veryl-tbf
passes on (`veryl-tbf/dpi`). Without it, `export!` emits what Veryl loads,
as now; with it, it emits the DPI functions and the host is the DPI one.
The two builds are separate artifacts, so neither carries the other's
needs (the DPI build may use the simulator's DPI library freely), nothing
is decided at run time, and the unit tests (`MockSim`) run on the default
build as now.

## DPI, not VPI

The testbench exchanges values through DPI functions the wrapper calls,
not through VPI handles to the signals:

- A DPI import is a plain, typed C call; arguments are pointers. VPI goes
  through `vpi_get_value` / `vpi_put_value` per signal, resolving handles
  and converting value formats each time.
- One call per edge carries every signal; VPI would need a call per
  signal, and callbacks to learn of the edges.
- Signals reached through VPI need read and write access granted
  (`-debug_access+r+w` with VCS), which keeps them observable and holds
  back the simulator's optimizations. With DPI the wrapper reads and
  writes them as ordinary SystemVerilog.
- Sampling (`@(posedge ..)`) and driving (nonblocking assignments) are the
  wrapper's SystemVerilog, scheduled by the simulator itself.

What VPI would give, finding signals by name with nothing generated, is
what the layout table and the wrapper replace.

## Signals

Passing the clock and the reset (the hooks) is kept apart from moving the
signals. At each edge the wrapper hands the host each bus's inputs, calls
the hook, then takes each bus's outputs back and drives them with
nonblocking assignments. This is Veryl's own timing: hooks read the
values before the edge, and writes reach the design at it (a flip-flop
clocked by that edge sees the old value).

```systemverilog
always @(posedge i_clk, negedge i_rst_n) begin
    tbf_sample(0, {bus0.ready, bus0.rvalid, bus0.rdata});   // bus 0's inputs, before the edge
    tbf_sample(1, {bus1.ready, bus1.rvalid, bus1.rdata});   // bus 1's
    if (!i_rst_n) tbf_on_reset(0 /* clock */, $time);
    else          tbf_on_clock(0 /* clock */, $time);
    if (tbf_drive(0, out0)) {bus0.valid, bus0.addr, ..} <= out0[..];  // bus 0's outputs
    if (tbf_drive(1, out1)) {bus1.valid, bus1.addr, ..} <= out1[..];
end
```

- The inputs go in before the hook and the outputs come out after it:
  exchanging both after the hook would have the testbench read the
  previous edge's inputs, and both before it would drive the previous
  hook's writes a cycle late.
- In reset as well: the reset handler's writes (`cb.init()` driving
  `valid` low) are driven the same way.
- Buses on the same clock and reset are lines in the same `always`; the
  hooks take a clock's key and the time only. A call per bus each way and
  one for the hook, all light, non-`context` functions.
- The host keeps each entry's latest inputs and returns them when the
  testbench reads; which clock a bus follows is the wrapper's `always`,
  not the host's concern.

- A packed vector crosses DPI as an array of 32-bit words
  (`svBitVecVal`, word 0 the least significant), close to `Value`'s 64-bit
  words. An import declares one type for every call, while entries differ
  in width, so the vectors are of one fixed width, `TBF_MAX_W` (1024 bits
  unless `TBF_DPI_MAX_WIDTH` is defined), narrower entries zero-extended;
  the wrapper gives the width to `tbf_init`, so the host reads and writes
  as many words. (Open arrays would lift the bound; a fixed width is
  simpler and enough.)
- What crosses is a key and bits: the key names a `signal_map` entry (a
  modport or a single signal bound to the testbench); its layout and width
  come from a table, so no width is passed. Clocks and resets have keys
  too, for the hooks.
- The table has two levels:
  - layouts, one per modport kind: each member's name, direction and
    width, in order (a parameterized interface gives one layout per set of
    widths);
  - signal maps: a key, the signals' path, whether they are a modport or
    a single signal, and the layout they are packed with; and the clocks,
    each with its reset.
- The names the testbench looks up (`"bus"`) are bound to keys by the
  wrapper's registration calls (`tbf_set_interface("bus", 0)`), as the
  test module's `tb.set_interface("bus", bus.master)` binds them today;
  the table describes packing only.
- `tbf_drive` returns 1 when the testbench wrote any output of the entry
  since the last call; the wrapper then updates all of the entry's
  outputs, and otherwise leaves them as they were. One flag per entry
  keeps the wrapper to one assignment per bus. The outputs stay undriven
  until the testbench first writes one (an `#[output]` with no `init` is
  not driven, as on Veryl's simulator), but from then on the entry's
  outputs are driven together, the ones not yet written at 0.
- The table and the wrapper are written by hand for now. Generating them
  belongs to Veryl, which alone knows the interfaces, their parameters and
  the bindings of a test module (see
  [veryl-proposals.md](veryl-proposals.md)).

```toml
[layout.bus_master]
members = [
  { name = "valid",  dir = "out", width = 1  },
  { name = "addr",   dir = "out", width = 3  },
  { name = "ready",  dir = "in",  width = 1  },
  { name = "rdata",  dir = "in",  width = 16 },
  # ..
]

[[signal_map]]
key    = 0
path   = "regblock_test.bus"     # for messages
kind   = "modport"               # or "signal"
layout = "bus_master"

[[clock]]
key   = 0
reset = 0
```

## Hooks

`on_clock` and `on_reset` are exclusive on Veryl's simulator: while the
reset is asserted, every edge calls `on_reset`. The wrapper's `always`
above reproduces it, as a flip-flop with an asynchronous reset is written
(the buses' calls around the hook left out here):

| Event | Veryl's simulator | The `always` |
|---|---|---|
| reset asserted | `on_reset` | `negedge i_rst_n`: `on_reset` |
| a clock edge in reset | `on_reset` | `!i_rst_n`: `on_reset` |
| reset released | no hook (seen at the next `on_clock`) | no event waited on |
| a clock edge after | `on_clock` | `on_clock` |

- The form follows the reset's kind: `posedge` and `if (i_rst)` for an
  active-high one, `always @(posedge i_clk)` alone for a synchronous one.
- A clock edge and an assertion at the same time run the `always` once,
  and the reset wins, as on Veryl's simulator.
- Each clock has an `always` of its own, passing its key to the hook and
  holding the buses of its domain; the host knows which clock fired.
- `on_init` is called from an `initial` (`tbf_init`), `on_finish` from a
  `final` (`tbf_on_finish`).

## Ending the test

The test module starts the test and checks the host's status as often as
it likes; the hooks return nothing:

```systemverilog
initial begin
    if (tbf_start("BasicTest", $time) != 0) $fatal(1, "veryl-tbf: could not start");
    forever begin
        repeat (1000) @(posedge i_clk);
        status = tbf_status();          // 0: running, 1: passed, 2: failed
        if (status != 0) break;
    end
    if (status == 2) $fatal(1, "veryl-tbf: test failed");
    $finish;
end
```

- veryl-tbf ends as now, calling the host's `finish` (passed) or `fail`
  (failed, with its summary); the DPI host records the status.
- The status is a function of its own, so a test module checks it every
  1000 cycles, or every cycle, as it chooses. Until it sees the end, the
  hooks keep being called: the host does nothing on them (the testbench's
  tasks have ended, and the outputs stay as last driven), and the status,
  once ended, does not change. The summary's end time is the testbench's,
  so a coarse check costs only the extra cycles simulated.
- A failure ends with `$fatal`, so the simulator's exit code is not zero.
- A `$finish` from elsewhere while the test runs reaches `final`
  (`tbf_on_finish`), which reports it as on Veryl's simulator; its
  failure shows in the log only, since the exit code cannot be changed
  there. So the test's end is left to veryl-tbf (its timeout included).
- The waiting is SystemVerilog's either way. An imported task may consume
  time only through an exported task, and Rust cannot block: the
  simulator runs on one thread. A `tbf_run` task, starting the test and
  waiting until it ends, can be written in the wrapper's package.

## Logs, seeds, time, X

- Logs, and a failure's summary, go to Rust's standard output, in Veryl's
  form, `[<instance>] cycle <n>: <message>` (the instance's label given
  to `tbf_init`, the edges counted): the DPI library runs in the
  simulator's process. If their order against the
  simulator's own output (buffered when redirected) or their absence from
  its log file (VCS's `-l`) matters, `vpi_printf` can be added behind a
  feature.
- The seed is taken by `tbf_init`; how to get it (a plusarg, VCS's
  `+ntb_random_seed`, Xcelium's `-svseed`) is the wrapper's, since it
  differs per simulator. veryl-tbf derives its seeds from it as from
  Veryl's. Veryl's `--seed` is not the value the testbench receives (Veryl
  derives a per-test seed from it), so a run is reproduced on the other
  simulator with the seed the report summary shows.
- Time is passed with each hook (`$time`), and with `tbf_start` and
  `tbf_on_finish`; veryl-tbf reads it only there (log lines, the summary,
  the timeout, `RunContext::time`). Its unit is the simulator's timescale,
  so the `timeout` option is given in that unit.
- The DPI arguments are `bit`, so X and Z become 0 in SystemVerilog's
  conversion, as veryl-tbf reads them on Veryl's simulator; only a
  clocking-block field of type `Value` sees a difference (it keeps X on
  Veryl's four-state simulation). `logic` (`svLogicVecVal`, whose
  `aval` / `bval` match `Value`'s mask) would carry X if needed.

## The DPI interface

All are non-`context` functions that return at once.

| Function | |
|---|---|
| `tbf_init(label, table, seed, vector_width)` | loads the table, takes the seed and the vectors' width (`TBF_MAX_W`); creates the testbench and calls `on_init` |
| `tbf_set_clock(name, key)`, `tbf_set_reset(name, key)` | binds a name the testbench looks up to a clock or a reset |
| `tbf_set_interface(name, key)`, `tbf_set_input(name, key)`, `tbf_set_output(name, key)` | binds a name to a `signal_map` entry |
| `tbf_set(key, value)`, `tbf_option(key, value)` | the test's settings and the framework's options |
| `tbf_start(test, time)` | starts the test; not zero if it could not (no such test, a failed build) |
| `tbf_sample(key, in)` | a `signal_map` entry's inputs, before the hook |
| `tbf_on_clock(clock, time)`, `tbf_on_reset(clock, time)` | the hooks |
| `tbf_drive(key, out)` | a `signal_map` entry's outputs after the hook; 1 if any was written, when the wrapper updates them all |
| `tbf_status()` | 0 running, 1 passed, 2 failed |
| `tbf_on_finish(time)` | from `final` |

## A prototype

The prototype is a host of its own, not a layer over
`veryl_component::testing::MockSim`: the SDK's C ABI is public
(`veryl_component::export`'s entry points, which Veryl's simulator calls,
and `sys::VrlHostApi`, the table of host services), so the host implements
the services and calls the entry points as Veryl's simulator does. It
records the testbench's writes itself (for `tbf_drive`'s flag), and keeps
each port's words for direct access.

- `crates/veryl-tbf-dpi`: the host and `export!(Component)`, which a
  testbench's crate invokes in place of `export!` under a `dpi` feature,
  to generate the DPI functions.
- `dpi/tbf_dpi_pkg.sv`: the DPI functions' declarations, for any wrapper,
  and `tbf_wait_end(interval)`, which polls the status every `interval`
  (in ns) and reports the result (`$fatal` on a failure).
- `examples/adder/verilator`: the wrapper and the table, by hand, and a
  `Makefile` (`make`, `make run SEED=5`, `make clean`). The adder's test
  passes as on Veryl's simulator, and a broken adder fails it.

What the prototype leaves out:

- Settings and options are 64-bit integers only (no strings, nothing
  wider).
- The host services a testbench of veryl-tbf does not use are empty: files,
  traces and parameters.
- One testbench per simulation: one host, one `tbf_init`.
- A table's member is an input or an output; a clock or a reset inside a
  modport cannot be described.
- Methods' return values are dropped (veryl-tbf's methods return none).
- The cycle count in messages counts every hook, the reset's included,
  and follows the wrapper's reset, so it differs from Veryl's.
- Not tried yet: several clocks, an unexpected finish (`final`),
  `tbf_set_output`, a release build of the library.
- The table and the wrapper are written by hand.
