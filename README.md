# Verald

Verification for [Veryl](https://github.com/veryl-lang/veryl) in Rust, on
Veryl's built-in simulator. Verald brings what SystemVerilog offers for
verification to Veryl, while Veryl itself stays an RTL language: its parts
are Rust crates that Veryl loads as verification components.

- **A UVM-style testbench**: components, phases, sequences, TLM and
  clocking blocks; its tests are started from a Veryl `#[test]` module.
- **Constrained random**: SystemVerilog's `rand`, `constraint` and
  `randomize()` on Rust structs, usable on its own.
- **Assertions** (in progress): SVA-like properties over the design's
  signals, evaluated by a component, beside the testbench or alone.

The name is Verification + Emerald, after Veryl's Verilog + Beryl: the
emerald is a variety of beryl.

> **Status:** in development and not released. It depends on unreleased
> Veryl: [veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)
> (signal and modport references passed to component methods) and
> [veryl-lang/veryl#3717](https://github.com/veryl-lang/veryl/pull/3717)
> (runtime arguments), merged in Veryl's master, run as `veryl +local`.

## Testbench

A testbench is a Rust crate (a `cdylib`) exporting one component, which a
Veryl `#[test]` module instantiates: the module registers the clocks,
resets and interfaces with it and starts a test by name, as UVM's
`run_test`.

- **Components and phases**: a typed hierarchy built in `build`, a
  concurrent `run` phase on a single-threaded executor, then `check` and
  `report`; kinds (`test`, `env`, `agent`, `driver`, `monitor`, ..) marked
  with `#[component(..)]`.
- **Tests, specs and scenarios**: a test's configuration (`setup`) and
  hierarchy (`build`), variations as specs applied before and after the
  build, a run split into `pre_run` / `run` / `post_run`, and tests made from
  a base test and scenarios (`#[component(test(base = .., run = ..))]`).
- **TLM**: analysis ports, subscribers, analysis FIFOs, item waiters.
- **Sequences**: sequences, sequencers with replaceable arbitration,
  responses, priorities, default sequences (from the configuration or a
  handle), and agents generated from their parts, active or passive,
  reactive for slaves.
- **Clocking blocks**: `#[modport]` / `#[clocking]` structs that sample
  inputs and drive outputs at a clock edge, with `tick!` and wait macros.
  Signals are two-state and read and written without a call to the host.
- **Reporting**: log levels with a threshold, errors, an error limit, a
  timeout, and a summary with the seed and the runtime arguments.
- **Runtime arguments**: `veryl test --runtime-arg=..` chooses the test
  (`VERALD_TESTNAME`, as UVM's `+UVM_TESTNAME`) and the options
  (`VERALD_TIMEOUT`, `VERALD_LOG_LEVEL`, ..), and a test reads its own
  (`runtime_arg_value::<u32>("ROUNDS")`).
- **SystemVerilog simulators** (a prototype): the same testbench, built
  with a `dpi` feature, runs on a SystemVerilog simulator through DPI,
  with a wrapper that passes it the signals at each edge. The adder's test
  runs on Verilator; see [docs/sv-simulators.md](docs/sv-simulators.md).

Configuration is typed and passed down the hierarchy; there is no
string-keyed configuration database.

```rust
use verald::prelude::*;

/// Drives each item the sequencer hands it.
#[component(driver)]
pub struct BusDriver {
    #[seq_item_port]
    port: SeqItemPort<BusItem>,
    cb: DriverCb,
}

#[component(driver)]
impl Component for BusDriver {
    type Context = BusContext;

    fn build(scope: &Scope, _context: BusContext) -> Result<Self, TestError> {
        Ok(BusDriver {
            port: SeqItemPort::new(),
            cb: scope.clocking()?,          // sampled and driven at its `#[clock]`
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            let item = self.port.get_next_item().await?;
            // drive it through `self.cb`, then
            self.port.item_done()?;
        }
    }
}

/// The agent: monitor, sequencer and driver, made by mode and connected.
#[component(agent(monitor = BusMonitor, sequencer = BusSequencer, driver = BusDriver))]
pub struct BusAgent {}

/// A test starts a sequence on the agent's sequencer.
#[component(test)]
impl Component for RegTest {
    // ..
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        WriteReadAll::new(self.rounds).start(&self.sequencer).await
    }
}

register_test!(RegTest);
verald::export!("verald_regblock");
```

```veryl
#[test(regblock_test)]
module regblock_test {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    inst bus: bus_if;
    inst dut: regblock (i_clk: clk, i_rst: rst, bus: bus.slave);
    inst tb : $comp::verald_regblock;

    initial {
        tb.set_clock("clk", clk);
        tb.set_reset("rst", rst);
        tb.set_modport("bus", bus.master);
        tb.start("RegTest");
        clk.start();                    // runs until the test ends
        rst.assert(clk);
    }
}
```

The [walkthrough](docs/walkthrough.md) reads a whole testbench,
`examples/regblock`, from the DUT to the output.

## Constrained random

SystemVerilog's random model on plain Rust structs, solved with a SAT
solver: `#[rand]` fields, constraints written as functions in Rust syntax,
`randomize()` and `randomize_with!`, constraint sets, `randcase!` and
`std_randomize!`. Constraints may read a configuration
(`#[randomize(context = ..)]`). Within a testbench, seeds follow UVM's
random stability: every component and sequence has its own, derived from
Veryl's seed (`veryl test --seed`).

```rust
use verald::random::{constraints, randomize, randomize_with};

#[randomize]
#[derive(Clone, Default)]
struct AluCmd {
    #[rand] a: u8,
    #[rand] b: u8,
    limit: u8,                        // state: a constant while solving
}

#[constraints]
impl AluCmd {
    fn ranges() {
        a != 0;
        b < limit;
    }
}

let mut cmd = AluCmd { limit: 8, ..Default::default() };
cmd.randomize()?;
randomize_with!(cmd, { a < 16; })?;
```

See [docs/random.md](docs/random.md).

## Assertions (in progress)

SVA-like concurrent assertions over the design's signals, written in Rust
against the Veryl source and evaluated by a component at each clock edge,
with auxiliary state and immediate checks; on the test module's signals,
later on every instance of a module. Sequences (delays, `let`s, `wait`,
`happen`, `repeat`, `throughout`, `until`, `ended`), implication,
`eventually`, `never` and `past` are implemented, with named sequences
and properties; covers will come with coverage. See [docs/assertion-design.md](docs/assertion-design.md)
and `examples/regblock`.

## Getting started

1. Build Veryl from the master of veryl-lang/veryl and install it as
   `local` (`verylup install local` in the Veryl repository).
2. Add a Rust crate for the components to the Veryl project, a `cdylib`
   depending on `verald` and on `veryl-component` (the derives name it),
   from the same revision of Veryl (the component's ABI follows Veryl's,
   so the `veryl +local` build and the crate's lock name one commit):

   ```toml
   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   verald = { git = "https://github.com/taichi-ishitani/verald" }
   veryl-component = { git = "https://github.com/veryl-lang/veryl", branch = "master" }
   ```

   `verald`'s features choose the parts: `tb` and `random` by default.
   Fetching the git dependencies may need `CARGO_NET_GIT_FETCH_WITH_CLI=true`.
3. Register it in `Veryl.toml`:

   ```toml
   [[components]]
   path = "component"
   ```

4. In the crate, write the components and tests, register each test with
   `register_test!`, and export the testbench once with
   `verald::export!("name")`; it is instantiated as `$comp::name`.
5. In a `#[test]` module, register the clocks, resets and interfaces, give
   options (`tb.option`) and settings (`tb.set`), and start a test by its
   registered name (`tb.start`).
6. Run `veryl +local test`. Runtime arguments override the test module:
   `--runtime-arg=VERALD_TESTNAME=<test>` starts another test, and
   `--runtime-arg=VERALD_<OPTION>:<value>` sets an option
   (`VERALD_LOG_LEVEL=debug`); a test reads its own with `runtime_arg_value`.

## Examples

Each is a Veryl project run with `veryl +local test` in its directory;
in `examples/regblock`, `make` runs its tests one after another, each named
by a runtime argument.

| Example | Shows |
|---|---|
| [`examples/regblock`](examples/regblock) | the testbench's methodology: a register block on a valid/ready bus, a generated agent, a scoreboard, sequences, tests made from scenarios, constrained random. See the [walkthrough](docs/walkthrough.md). The bus's VIP (`vip`) and its assertions (`assertions`) are library crates; the assertions run beside the testbench, and alone in a second test module (`regblock_assertions_test`, its component in `assertions_component`) |
| [`examples/adder`](examples/adder) | the testbench's runtime alone: a clocking block driving an adder and checking its sums |
| [`examples/hooks`](examples/hooks) | the testbench's runtime alone: how the simulator calls the testbench around a reset |

`examples/adder/verilator` runs the adder's test on Verilator instead,
through the DPI host (`make` there; `make run SEED=5`, `make clean`): a
hand-written wrapper and table, and the testbench built with its `dpi`
feature.

## Crates

The directories group the crates by part: what the parts share is at the
top, a part's crates are under its directory.

| Crate | Directory | Contents |
|---|---|---|
| `verald` | `crates/verald` | the entry point: re-exports the others (`verald::host`, `verald::tb`, `verald::random`, `verald::assertion`) with a prelude and `export!`; parts chosen by features |
| `verald-host` | `crates/host` | the host layer: what talks to Veryl's host API (hooks, services, typed signals), shared by the parts |
| `verald-macros` | `crates/macros` | the shared proc macros: `#[modport]` |
| `verald-macros-codegen` | `crates/macros/codegen` | `#[modport]`'s code generation and the parts `#[clocking]` shares with it |
| `verald-assertion` | `crates/assertion` | assertions: SVA-like properties evaluated by the component (sequences, implication, `eventually`, `never`, `past`) |
| `verald-assertion-macros` | `crates/assertion/macros` | its proc macros (`#[assertions]`) |
| `verald-random` | `crates/random` | constrained random, usable on its own |
| `verald-random-macros` | `crates/random/macros` | its proc macros |
| `verald-tb-runtime` | `crates/tb/runtime` | the testbench's runtime: the executor, tasks and their concurrency, clocks and resets, clocking blocks, the registrations |
| `verald-tb-core` | `crates/tb/core` | the testbench's UVM-like methodology: components, phases, tests, TLM, sequences, sequencers, agents, reporting |
| `verald-tb-macros` | `crates/tb/macros` | the testbench's proc macros (`#[clocking]`, waits, `#[component]`, `#[sequence]`, `register_test!`) |
| `verald-tb-dpi` | `crates/tb/dpi` | a DPI host (a prototype): runs a testbench on a SystemVerilog simulator; its SystemVerilog side is [`verald_dpi_pkg.sv`](crates/tb/dpi/verald_dpi_pkg.sv) |

## Documents

- [docs/design.md](docs/design.md): the design, with the reasons behind it
  and what was taken from UVM.
- [docs/host-layer-design.md](docs/host-layer-design.md): the layer over
  Veryl's host API, shared by the parts.
- Testbench:
  - [docs/walkthrough.md](docs/walkthrough.md): a testbench read from end
    to end.
  - [docs/sv-simulators.md](docs/sv-simulators.md): running a testbench on
    a SystemVerilog simulator, through DPI.
- Constrained random: [docs/random.md](docs/random.md), as users write it;
  [docs/random-design.md](docs/random-design.md), its implementation.
- Assertions: [docs/assertion-design.md](docs/assertion-design.md), a
  design study.
- [docs/veryl-proposals.md](docs/veryl-proposals.md): what Verald would
  want of Veryl.
- [docs/todo.md](docs/todo.md): what is left to do, gathered from the
  documents above.

## License

MIT; see [LICENSE.txt](LICENSE.txt).
