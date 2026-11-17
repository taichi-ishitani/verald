# veryl-tbf

A UVM-style verification framework for [Veryl](https://github.com/veryl-lang/veryl)'s
built-in simulator, written in Rust. A testbench is a Rust crate that Veryl
loads as a verification component; its tests are started from a Veryl
`#[test]` module.

> **Status:** in development and not released. It depends on unreleased
> Veryl: [veryl-lang/veryl#3701](https://github.com/veryl-lang/veryl/pull/3701)
> (signal and modport references passed to component methods), run as
> `veryl +local`.

## Features

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
- **Constrained random**: SystemVerilog's `rand`, `constraint` and
  `randomize()` on Rust structs, solved with a SAT solver; constraints may
  read a configuration (`#[randomize(context = ..)]`). Seeds follow UVM's
  random stability: every component and sequence has its own, derived from
  Veryl's seed (`veryl test --seed`).
- **Reporting**: log levels with a threshold, errors, an error limit, a
  timeout, and a summary with the seed.

Configuration is typed and passed down the hierarchy; there is no
string-keyed configuration database.

## A first look

```rust
use veryl_tbf::prelude::*;

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
            cb: scope.clocking("clk")?,     // sampled and driven at `clk`
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
veryl_tbf::export!("tbf_regblock");
```

```veryl
#[test(regblock_test)]
module regblock_test {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    inst bus: bus_if;
    inst dut: regblock (i_clk: clk, i_rst: rst, bus: bus.slave);
    inst tb : $comp::tbf_regblock;

    initial {
        tb.set_clock("clk", clk);
        tb.set_reset("rst", rst);
        tb.set_interface("bus", bus.master);
        tb.set("rounds", 2);
        tb.start("RegTest");
        rst.assert(clk);
        clk.next(100000);
        $finish();
    }
}
```

The [walkthrough](docs/walkthrough.md) reads a whole testbench,
`examples/regblock`, from the DUT to the output.

## Getting started

1. Build Veryl from PR #3701 and install it as `local`
   (`verylup install local` in the Veryl repository).
2. Add a Rust crate for the testbench to the Veryl project, a `cdylib`
   depending on `veryl-tbf` and on `veryl-component` (the derives name
   it), from the same revision of Veryl:

   ```toml
   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   veryl-tbf = { git = "https://github.com/taichi-ishitani/veryl-tbf" }
   veryl-component = { git = "https://github.com/taichi-ishitani/veryl", branch = "feat/component-method-ref-args" }
   ```

   Fetching the git dependencies may need `CARGO_NET_GIT_FETCH_WITH_CLI=true`.
3. Register it in `Veryl.toml`:

   ```toml
   [[components]]
   path = "component"
   ```

4. In the crate, write the components and tests, register each test with
   `register_test!`, and export the testbench once with
   `veryl_tbf::export!("name")`; it is instantiated as `$comp::name`.
5. In a `#[test]` module, register the clocks, resets and interfaces, give
   options (`tb.option`) and settings (`tb.set`), and start a test by its
   registered name (`tb.start`).
6. Run `veryl +local test`.

## Examples

Each is a Veryl project run with `veryl +local test` in its directory.

| Example | Shows |
|---|---|
| [`examples/regblock`](examples/regblock) | the methodology: a register block on a valid/ready bus, a generated agent, a scoreboard, sequences, tests made from scenarios, constrained random. See the [walkthrough](docs/walkthrough.md) |
| [`examples/adder`](examples/adder) | the runtime alone: a clocking block driving an adder and checking its sums |
| [`examples/hooks`](examples/hooks) | the runtime alone: how the simulator calls the testbench around a reset |

## Crates

| Crate | Contents |
|---|---|
| `veryl-tbf` | the entry point: re-exports the others, with a prelude and `export!` |
| `veryl-tbf-runtime` | the executor, tasks and their concurrency, clocks and resets, clocking blocks, the simulator boundary |
| `veryl-tbf-core` | the UVM-like methodology: components, phases, tests, TLM, sequences, sequencers, agents, reporting |
| `veryl-tbf-macros` | the procedural macros of the others |
| `veryl-tbf-random` | constrained random; usable on its own |

## Documents

- [docs/walkthrough.md](docs/walkthrough.md): a testbench read from end to end.
- [docs/design.md](docs/design.md): the design, with the reasons behind it
  and what was taken from UVM.
- [docs/random.md](docs/random.md): constrained random, as users write it;
  [docs/random-design.md](docs/random-design.md): its implementation.

## License

MIT; see [LICENSE.txt](LICENSE.txt).
