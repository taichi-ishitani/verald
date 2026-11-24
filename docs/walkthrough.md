# Walkthrough: `examples/regblock`

This walkthrough reads a whole testbench, from the design under test to
the output of a run: `examples/regblock`, a register block on a simple
bus. It shows how the parts of Verald fit together; the reasons behind
them are in [design.md](design.md), and constrained random in
[random.md](random.md).

The example is a Veryl project:

```text
examples/regblock/
├── Veryl.toml              the project, with the testbench as a component
├── Makefile                runs every test, one after another
├── src/regblock.veryl      the DUT, its bus, and the #[test] module
└── component/              the testbench: a Rust crate
    ├── Cargo.toml
    └── src/lib.rs
```

Run it with `veryl +local test` in `examples/regblock`; the test and the
options are given as runtime arguments
(`--runtime-arg=VERALD_TESTNAME:ReverseTest`, see section 2). `make` there
runs every test, one after another (`make ReverseTest` one, `make SEED=5`
with a seed, `make ARGS=..` with more arguments).

## Contents

1. [The design under test](#1-the-design-under-test)
2. [The test module](#2-the-test-module)
3. [The testbench crate](#3-the-testbench-crate)
4. [The bus VIP](#4-the-bus-vip)
5. [Sequences](#5-sequences)
6. [The testbench: scoreboard and env](#6-the-testbench-scoreboard-and-env)
7. [The base test](#7-the-base-test)
8. [Tests made from the base](#8-tests-made-from-the-base)
9. [Constrained random](#9-constrained-random)
10. [Running and reading the output](#10-running-and-reading-the-output)

## 1. The design under test

`bus_if` is a valid/ready bus. A request is accepted at an edge where
`valid` and `ready` are both high; a read's data comes back at the next
edge, with `rvalid`. The interface has a modport for each side, and one
for watching:

```veryl
interface bus_if {
    var valid : logic    ;
    var ready : logic    ;
    var write : logic    ;
    var addr  : logic<3> ;
    var wdata : logic<16>;
    var rvalid: logic    ;
    var rdata : logic<16>;

    modport master  { valid: output, ready: input , write: output, addr: output, wdata: output, rvalid: input , rdata: input  }
    modport slave   { valid: input , ready: output, write: input , addr: input , wdata: input , rvalid: output, rdata: output }
    modport monitor { valid: input , ready: input , write: input , addr: input , wdata: input , rvalid: input , rdata: input  }
}
```

`regblock` holds eight 16-bit registers behind the `slave` modport. To give
the testbench some back-pressure to deal with, `ready` drops one cycle in
three.

## 2. The test module

A test module instantiates the DUT and the testbench, `$comp::verald_regblock`,
the name the crate exports. Then, in `initial`, it tells the testbench
what it needs and starts a test:

```veryl
#[test(regblock_test)]
module regblock_test {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    inst bus: bus_if;
    inst dut: regblock (
        i_clk: clk       ,
        i_rst: rst       ,
        bus  : bus.slave ,
    );
    inst tb : $comp::verald_regblock;

    initial {
        tb.set_clock("clk", clk);               // registered under a name
        tb.set_reset("rst", rst);
        tb.set_interface("bus", bus.master);    // the driver's side
        tb.set_interface("bus_mon", bus.monitor);
        tb.start("BasicTest");                  // the test, by its registered name
        clk.start();                            // runs until the test ends
        rst.assert(clk);
    }
}
```

- The testbench finds signals by the names registered here, never by HDL
  paths: a component asks for `"clk"` or `"bus"`.
- `start` builds the named test and starts its run. `clk.start()` keeps
  the clock running until the test ends: the testbench ends the simulation
  when the test is done, so the test module sets no number of cycles. A
  hung test is ended by the framework's timeout (10,000,000 cycles of the
  first clock registered, by default; `VERALD_TIMEOUT` below). A `$finish`
  in the test module reached while the test still runs is reported as an
  unexpected finish.

The module is the harness every test shares; which test runs, and the
framework's options, are given when it runs, as runtime arguments, as
UVM's `+UVM_*` plusargs:

```text
veryl +local test --runtime-arg=VERALD_TESTNAME:ReverseTest --runtime-arg=VERALD_TIMEOUT:100000
```

- `VERALD_TESTNAME` names the test over the one given to `start`: the
  testbench has `BasicTest`, `ReverseTest`, `RandomTest` and `DirectTest`;
  `BasicTest` runs when none is named.
- `VERALD_<OPTION>` sets an option of the framework: `VERALD_TIMEOUT` (the
  cycles the test must end within; `0` for no limit), `VERALD_LOG_LEVEL` (`error` .. `trace`, `info` by
  default), `VERALD_MAX_ERRORS`. A test module may also give options with
  `tb.option(key, value)`; a runtime argument overrides it.
- A misspelt one (`VERALD_LOG_LEVL`) fails the test, listing the options.
- Arguments without the prefix are the tests' own: `ROUNDS:4` sets how
  many times `BasicTest` writes and reads every register (2 by default;
  see [section 7](#7-the-base-test)).

## 3. The testbench crate

`Veryl.toml` names the crate as a component:

```toml
[[components]]
path = "component"
```

The crate is a `cdylib` depending on `verald` and on `veryl-component`,
which the derives behind `export!` name:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
verald = { path = "../../../crates/verald" }
veryl-component = { git = "https://github.com/taichi-ishitani/veryl", branch = "tbf" }
```

`src/lib.rs` imports the prelude, and at its end registers the tests and
exports the testbench once:

```rust
use verald::prelude::*;

// .. the VIP, the testbench and the tests ..

register_test!(BasicTest);
register_test!(ReverseTest);
register_test!(RandomTest);
// DirectTest is a function marked `#[testcase]`, which registers itself

verald::export!("verald_regblock");   // $comp::verald_regblock
```

## 4. The bus VIP

The first half of `lib.rs` is what a VIP for the bus would hold: the item,
the configuration, the agent's parts and the agent, and a base for
sequences (in [section 5](#5-sequences)).

### The item

A transfer is a plain struct. It is random (see
[section 9](#9-constrained-random)): `#[rand]` fields are drawn by
`randomize`, and a constraint keeps the address within the registers, a
number the configuration gives:

```rust
#[randomize(context = BusConfig)]
#[sequence_item]
#[derive(Clone, Debug, Default)]
pub struct BusItem {
    #[rand] pub write: bool,
    #[rand] pub addr: u8,
    #[rand] pub data: u16,
}

#[constraints]
impl BusItem {
    fn in_range() {
        addr < context.regs;
    }
}
```

`#[randomize]` adds a hidden field, so the struct is made with `Default`
or a constructor of its own (`BusItem::new(write, addr, data)`), not with
a literal.

`#[sequence_item]` adds the alias of the item's handle,
`BusItemHandle` (`ItemHandle<BusItem>`): items are shared between the
sequence, the driver and the monitor's subscribers as handles, and the
ports and subscribers below name it.

### The configuration

The agent's configuration says whether it drives (active) or only watches
(passive), how many registers there are, and which registered interfaces
its driver and monitor use:

```rust
pub struct BusConfig {
    pub active: bool,
    pub regs: u8,
    pub driver_if: &'static str,
    pub monitor_if: &'static str,
}

impl AgentConfig for BusConfig {
    fn active(&self) -> bool {
        self.active
    }
}

pub type BusContext = InstanceContext<BusConfig>;
```

An `InstanceContext` holds a configuration (fixed once made, shared by its
clones) and a status. A parent hands clones of it to its children as their
`Context`, which is how settings travel down the hierarchy: typed, with
no string-keyed database. A configuration, once made, is put in one with
`into_context()` (`BusConfig { .. }.into_context()`, see
[section 7](#7-the-base-test)).

### Modports and clocking blocks

The driver and the monitor see the bus through clocking blocks. A
`#[modport]` struct lists the members of a modport as fields: `#[input]`
fields are sampled, `#[output]` fields are driven. A `#[clocking]` struct
ties modports to a clock:

```rust
#[modport]
pub struct BusMaster {
    #[output(init = false)] valid: bool,   // driven to false by `cb.init()`
    #[output] write: bool,
    #[output] addr: u8,
    #[output] wdata: u16,
    #[input]  ready: bool,
    #[input]  rvalid: bool,
    #[input]  rdata: u16,
}

#[clocking]
pub struct DriverCb {
    #[modport] bus: BusMaster,
}
```

`tick!(cb)` drives the outputs changed since the last tick, waits for the
next edge of the clock, and samples the inputs, as SystemVerilog's
`@(cb)`. `tick_until!(cb.bus.rvalid)` ticks until the condition holds.
The monitor has its own pair, `BusWatch` (every member an input) and
`MonitorCb`.

### The driver

A component is a struct and an `impl Component`, both marked with its kind:

```rust
#[component(driver)]
pub struct BusDriver {
    #[context]
    context: BusContext,
    #[seq_item_port]
    port: SeqItemPort<BusItem>,
    cb: DriverCb,
    rst: Reset,
}

#[component(driver)]
impl Component for BusDriver {
    type Context = BusContext;

    fn build(scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        let cb = scope.clocking_with(
            "clk",
            DriverCbPaths {
                bus: Some(context.config().driver_if),
                ..Default::default()
            },
        )?;
        Ok(BusDriver {
            rst: scope.reset("rst")?,
            context,
            port: SeqItemPort::new(),
            cb,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let rst = self.rst;
        let never: Result<Infallible, TestError> = run_with_reset(
            rst,
            self,
            |driver| driver.cb.init(),
            async |driver| loop { driver.drive_one().await? },
        )
        .await;
        never.map(|_| ())
    }
}
```

- `build` makes the component from what its parent gives (its `Context`).
  `scope.clocking_with` binds the clocking block to the clock registered
  as `"clk"`, and its `bus` modport to the interface the configuration
  names; `scope.clocking("clk")` binds every field at its own name.
- The field markers say what the agent connects: `#[context]` gives the
  component `context()`, `#[seq_item_port]` the port to its sequencer.
- `run` is the component's work during the run phase; every component's
  `run` runs concurrently. `run_with_reset` runs the body until a reset,
  calls the reset handler (here `cb.init()`, driving `valid` low), waits
  for the release, and starts the body again.

Driving one item is plain sequential code:

```rust
async fn drive_one(&mut self) -> Result<(), TestError> {
    let item = self.port.get_next_item().await?;
    let request = item.borrow().clone();
    let cb = &mut self.cb;
    cb.bus.valid = true;
    cb.bus.write = request.write;
    cb.bus.addr = request.addr;
    cb.bus.wdata = request.data;
    item.mark_begin();
    tick!(cb);                         // accepted where `ready` is high with `valid`
    while !cb.bus.ready {
        tick!(cb);
    }
    cb.bus.valid = false;
    if !request.write {
        tick_until!(cb.bus.rvalid);
        item.borrow_mut().data = cb.bus.rdata;   // the sequence sees the data
    }
    item.mark_end();
    self.port.item_done()
}
```

`get_next_item` returns the item as an `ItemHandle`, shared with the
sequence that sent it: what the driver writes into it, the sequence reads
back after `finish_item`.

### The monitor

The monitor watches the bus and publishes each transfer on an analysis
port, `#[analysis_port]`:

```rust
#[component(monitor)]
pub struct BusMonitor {
    #[context]
    context: BusContext,
    #[analysis_port]
    port: AnalysisPort<BusItemHandle>,
    cb: MonitorCb,
}
```

Its `run` follows the protocol with the wait macros: it waits for
`valid`, then for `ready` (the request is accepted at an edge where both
are high), and publishes a write at once, a read when its data comes back
with `rvalid`:

```rust
async fn run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
    let cb = &mut self.cb;
    loop {
        tick_until!(cb.bus.valid);
        if !cb.bus.ready {
            tick_until!(cb.bus.ready);
        }
        let (write, addr, wdata) = (cb.bus.write, cb.bus.addr, cb.bus.wdata);
        let item = if write {
            run_context.trace(format!("write {addr} = {wdata:#06x}"));
            BusItem::new(true, addr, wdata).into_handle()
        } else {
            tick_until!(cb.bus.rvalid);
            let rdata = cb.bus.rdata;
            run_context.trace(format!("read {addr} = {rdata:#06x}"));
            BusItem::new(false, addr, rdata).into_handle()
        };
        self.port.write(&item);
    }
}
```

- `tick_until!` always waits for at least one edge, so `ready` is checked
  first at the edge where `valid` was seen: it may already be high there.
- A wait macro takes the clocking block from the start of its path, so
  the block is bound to a local, `cb`, as in the driver.
- One request is outstanding at a time, as this master sends them, so the
  monitor waits for a read's data before looking for the next request.
- `into_handle()` puts the item it made in a handle, the type the port
  carries; subscribers share it.

`run_context` gives the component's path, logging and errors.

### The sequencer and the agent

A sequencer and an agent are generated whole from attributes:

```rust
#[component(sequencer(item = BusItem))]
pub struct BusSequencer {
    #[context]
    context: BusContext,
}

#[component(agent(monitor = BusMonitor, sequencer = BusSequencer, driver = BusDriver))]
pub struct BusAgent {}
```

The agent hands its context to its parts: each of the monitor, the
sequencer and the driver gets a clone of it. So the parts share one
`Context` type, which the agent takes as its own (from the monitor's). It
always makes the monitor; it makes the sequencer and the driver only when
the agent is active (as its configuration's `active()` says), and connects
the driver's port to the sequencer. It gets the public fields
`monitor`, `sequencer` and `driver` (the last two optional), and the
accessors `sequencer_handle()`, `analysis_port()` and `context()`.

## 5. Sequences

A sequence is a struct with `#[sequence(sequencer = ..)]` and an
`impl Sequence` with its `body`: what it does, sending items to the
driver through its sequencer. The attribute supplies the rest, the
sequence core (`SequenceCore`, UVM's `uvm_sequence_base`). The bus VIP
adds a layer between the two: what every bus sequence can do, its API,
which it also publishes for tests through a proxy.

### The protocol's base

What every bus sequence can do is part of the VIP: a plain struct (for
fields the protocol's sequences share; empty here) and a trait over the
sequence core, with a blanket impl:

```rust
#[derive(Default)]
pub struct BusSeqBase {}

pub trait BusSequence: SequenceCore<Sequencer = BusSequencer> + HasBase<BusSeqBase> {
    async fn write(&mut self, addr: u8, data: u16) -> Result<(), TestError> {
        let mut item = BusItem::new(true, addr, data);
        self.start_item(&mut item).await?;
        self.finish_item(item).await?;
        Ok(())
    }

    async fn read(&mut self, addr: u8) -> Result<u16, TestError> {
        let mut item = BusItem::new(false, addr, 0);
        self.start_item(&mut item).await?;
        let item = self.finish_item(item).await?;
        let data = item.borrow().data;   // filled in by the driver
        Ok(data)
    }
}

impl<S> BusSequence for S where S: SequenceCore<Sequencer = BusSequencer> + HasBase<BusSeqBase> {}
```

`start_item` waits until the sequencer grants the sequence the driver;
`finish_item` hands the item over and waits until the driver ends it
(`item_done`), returning its handle, through which the sequence reads what
the driver wrote. A sequence declared with `base = BusSeqBase` on
`BusSequencer` gets `write` and `read` with no more code.
(`#![allow(async_fn_in_trait)]` at the top of the crate allows the
`async fn`s in a public trait.)

A VIP is controlled through sequences; such an API is what a VIP adds
where it is useful, made of items (as here) or of its own sequences.

### The proxy

For a test to call `write` and `read` as tasks, the VIP publishes a proxy:
a sequence with no body, declared with `proxy`:

```rust
#[sequence(sequencer = BusSequencer, base = BusSeqBase, proxy)]
pub struct BusBfm {}
```

It has no `impl Sequence`, so it is never started; its `new` takes the
sequencer and starts it there (`BusBfm::new(&sequencer)`), and the test
calls the API on it (in [section 8](#a-test-as-a-function)). It has
`write` and `read` through the blanket impl, as any sequence with
`base = BusSeqBase` does.

### A sequence

`WriteReadAll` writes every register, then reads every one back, with
the protocol's `write` and `read`:

```rust
#[sequence(sequencer = BusSequencer, base = BusSeqBase)]
pub struct WriteReadAll {
    rounds: u16,
}

impl Sequence for WriteReadAll {
    async fn body(&mut self) -> Result<(), TestError> {
        for round in 0..self.rounds {
            for addr in 0..8u8 {
                let data = (0x1111 * u16::from(addr)) ^ (round << 8) ^ 0x00a5;
                self.write(addr, data).await?;
            }
            for addr in 0..8u8 {
                self.read(addr).await?;
            }
        }
        Ok(())
    }
}
```

The attribute generates `new` from the struct's own fields
(`WriteReadAll::new(2)`), and the sequence core: `start`, `start_item`,
`finish_item`, `sequencer()`, priorities. Starting it on another type of
sequencer does not compile. `BasicTest` starts it on the agent's
sequencer ([section 8](#8-tests-made-from-the-base)):
`WriteReadAll::new(self.rounds).start(&self.sequencer)`.

## 6. The testbench: scoreboard and env

### The scoreboard

The scoreboard keeps a model of the registers. Writes update it; reads are
checked against it:

```rust
#[component(scoreboard)]
pub struct RegScoreboard {
    model: [u16; 8],
    reads: usize,
    writes: usize,
    mismatches: Vec<String>,
}

#[component(scoreboard, passive)]
impl Component for RegScoreboard {
    type Context = ();

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> { .. }

    fn check(&mut self, check_context: &mut CheckContext) {
        for mismatch in &self.mismatches {
            check_context.error(mismatch);
        }
        if self.reads == 0 {
            check_context.error("no read was checked");
        }
    }

    fn report(&self, report_context: &ReportContext) {
        report_context.info(format!("{} writes, {} reads checked, {} mismatches", ..));
    }
}

impl Subscriber<BusItemHandle> for RegScoreboard {
    type Id = ();

    fn write(&mut self, (): (), item: &BusItemHandle) {
        // update the model, or check a read against it
    }
}
```

- It has no `run`, so it is `passive`: the attribute requires that, and a
  passive component is never held by a run task, so an analysis port can
  write to it during the run phase.
- `Subscriber` receives the items of the ports it is connected to; `Id`
  tells ports of one item type apart (an enum), or is `()` for one.
- `check` runs after the run phase, children before parents; errors it
  reports fail the test. `report` follows, for summaries.

### The env

The env makes the agent and the scoreboard and connects them:

```rust
#[component(env)]
pub struct RegEnv {
    pub bus: Instance<BusAgent>,
    pub scoreboard: Instance<RegScoreboard>,
}

#[component(env, passive)]
impl Component for RegEnv {
    type Context = BusContext;

    fn build(scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        let bus = scope.create_instance::<BusAgent>("bus", context)?;
        let scoreboard = scope.create_instance::<RegScoreboard>("scoreboard", ())?;
        bus.analysis_port().connect(&scoreboard, ());
        Ok(RegEnv { bus, scoreboard })
    }
}
```

`create_instance` calls the child's `build` there and then, so the parent
has the child, with the child's own children, right after making it, and
connects them in the same place: there is no separate connect phase. An
`Instance<T>` reads as the component (`bus.analysis_port()`) outside the
run phase; during it, `borrow()` reaches a passive one.

## 7. The base test

The tests are written as in UVM: a base test builds the testbench and does
what every test does before and after its scenario; each test is made
from it with a scenario of its own.

A test is the root of the hierarchy. It is made in two steps, its
configuration (`setup`, from the settings and the runtime arguments) and
its hierarchy (`build`, from that configuration), and runs in three,
`pre_run`, `run` and `post_run`. The base test `RegTest` writes all but `run`:

```rust
pub struct RegTestConfig {
    pub rounds: u16,
}

#[component(test, base)]
pub struct RegTest {
    env: Instance<RegEnv>,
    sequencer: SequencerHandle<BusSequencer>,
    rst: Reset,
    rounds: u16,
}

#[component(test, base)]
impl Component for RegTest {
    type Context = RegTestConfig;

    fn setup(_setup_context: &mut SetupContext) -> Result<RegTestConfig, TestError> {
        Ok(RegTestConfig { rounds: runtime_arg_value("ROUNDS")?.unwrap_or(2) })   // --runtime-arg=ROUNDS:4
    }

    fn build(scope: &Scope, config: RegTestConfig) -> Result<Self, TestError> {
        let context = BusConfig {
            active: true,
            regs: 8,
            driver_if: "bus",
            monitor_if: "bus_mon",
        }
        .into_context();
        let env = scope.create_instance::<RegEnv>("env", context)?;
        let sequencer = env.bus.sequencer_handle();
        Ok(RegTest { env, sequencer, rst: scope.reset("rst")?, rounds: config.rounds })
    }

    async fn pre_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.rst.asserted().await;
        self.rst.released().await;
        Ok(())
    }

    async fn post_run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
        run_context.info(format!(
            "done; the scoreboard has seen {} reads",
            self.env.borrow().scoreboard.borrow().reads
        ));
        Ok(())
    }
}
```

- `base` marks a test meant only as a base, as UVM's `base_test`: it
  needs no `run`, and it cannot be registered itself (`register_test!`
  on it does not compile).
- The whole test is one impl, in the order it runs. `setup`, `pre_run`
  and `post_run` belong to the `Test` trait; `#[component(test)]` moves
  them to an `impl Test` it generates.
- The run phase ends when `post_run` returns: the other components'
  `run`s, endless loops, are dropped, and `check` and `report` follow.
  Nothing needs an objection.
- The sequencer handle is taken from the agent right after it is made, and
  kept typed. A sequence started on a handle left unconnected stops the
  test with a message naming the sequence.
- `runtime_arg_value("ROUNDS")` reads the runtime argument as the field's
  type, `None` if it was not given; a value that does not convert (`ROUNDS:x`,
  or one beyond `u16`) fails the start, naming the argument. A test module
  could pass the value instead (`tb.set("rounds", 2)`, read with
  `setup_context.get("rounds")?`); this one leaves all to run time.

A simple test may instead write its `run` too, without `base`, and be
registered as it is.

## 8. Tests made from the base

A test made from the base gives the step the base leaves out, its `run`,
in one of three ways: as a method of its own, as a scenario, or as a
function. All three are shown here.

### A `run` of its own

`BasicTest` writes its `run` itself, as a UVM test derived from
`base_test` writes its `run_phase`:

```rust
#[component(test(base = RegTest, run))]
pub struct BasicTest {}

impl BasicTest {
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        WriteReadAll::new(self.rounds).start(&self.sequencer).await
    }
}

register_test!(BasicTest);
```

`BasicTest` is generated: it holds a `RegTest` (its field `base`) and uses
its `setup`, `build`, `pre_run`, `post_run`, `check` and `report`; `run`
in the attribute says the test writes that step itself, as a method in a
plain `impl`. The method reaches `RegTest`'s fields through `Deref`
(`self.rounds`, `self.sequencer`), and a step of the base through
`self.base`. `pre_run` and `post_run` may be written the same way.

### A scenario

A scenario is a type that implements `Scenario`. Its `body` is given the
base test, so it reaches every sequencer the test holds, as a UVM virtual
sequence reaches them through its virtual sequencer:

```rust
#[derive(Default)]
pub struct ReverseScenario {}

impl Scenario for ReverseScenario {
    type Test = RegTest;

    async fn body(&mut self, test: &mut RegTest, run_context: &mut RunContext)
        -> Result<(), TestError>
    {
        run_context.info("reading back in reverse");
        ReverseReadBack::new().start(&test.sequencer).await
    }
}

#[component(test(base = RegTest, run = ReverseScenario))]
pub struct ReverseTest {}

register_test!(ReverseTest);
```

`ReverseReadBack` writes every register, then reads them back from the
last, rewriting each. `RandomTest` is made the same way from
`RandomScenario` (see [section 9](#9-constrained-random)).

A method suits a step one test needs; a scenario is a type of its own, so
it can serve several tests, or be given for any step (`pre_run = ..`,
`post_run = ..`) of tests made from different bases of one type.

- `register_test!(BasicTest)` registers the test under its own name, the
  one the test module starts. `register_test!(Name, Test, spec)` registers
  a test with a spec, a variation applied before and after the build (see
  [design.md](design.md#tests)); a spec written for `RegTest` serves every
  test made from it.

### A test as a function

`DirectTest` is a scenario written straight through, as a testbench
before UVM wrote one in an `initial` block calling a BFM's tasks: it
writes a few registers, reads them back, and checks each read where it is
made. It is a function marked `#[testcase]`:

```rust
#[testcase(base = RegTest)]
async fn direct_test(test: &mut RegTest, run_context: &mut RunContext) -> Result<(), TestError> {
    let mut bus = BusBfm::new(&test.sequencer);
    for addr in 0..4u8 {
        bus.write(addr, 0x0f00 | u16::from(addr)).await?;
    }
    for addr in 0..4u8 {
        let data = bus.read(addr).await?;
        if data != 0x0f00 | u16::from(addr) {
            run_context.error(format!("read {addr}: got {data:#06x}"));
        }
    }
    Ok(())
}
```

- `#[testcase]` makes the test from the base with the function as its
  `run`, as the scenario above does, and registers it under the
  function's name in CamelCase, `DirectTest`. `name = ..` gives another
  name, `spec = ..` a spec. The function takes the base test and the run
  context, as a scenario's `body` does.
- `BusBfm::new(&test.sequencer)` makes the VIP's proxy (in
  [section 5](#the-proxy)), started on the sequencer: `write` and `read`
  are `BusSequence`'s, called as a BFM's tasks.
- Its items go through the sequencer and the driver as any sequence's, so
  the monitor and the scoreboard see them, and the scoreboard checks the
  reads too. Tasks forked to drive the bus together each make their own
  proxy, and the sequencer arbitrates between them.
- What its methods randomize draws from the test's seed source; a
  sequence with a stream of its own is written as a sequence.

A function suits a scenario of calls to the VIP's tasks, one test each; a
sequence suits traffic to reuse, and a scenario type a `run` shared by
tests.

## 9. Constrained random

`RandomTest` is made the same way, with a scenario that runs random
transfers:

```rust
#[sequence(sequencer = BusSequencer, base = BusSeqBase)]
pub struct RandomAccess {
    count: u16,
}

impl Sequence for RandomAccess {
    async fn body(&mut self) -> Result<(), TestError> {
        for _ in 0..self.count {
            let mut item = BusItem::default();
            self.start_item(&mut item).await?;
            self.randomize_item(&mut item)?;
            self.finish_item(item).await?;
        }
        Ok(())
    }
}
```

- `BusItem`'s constraints read the agent's configuration
  (`addr < context.regs`). `self.randomize_item(&mut item)` randomizes the
  item under the sequencer's configuration, the one its `#[context]`
  holds; outside a sequence it is `item.randomize(&config)`.
- `randomize_item_with(&mut item, Set)` adds a constraint set for the
  call, and `randomize_with!(item, &self.config(), { addr == 3; })` inline
  constraints.
- A failure to solve is a `RandError`, which `?` turns into the test's
  fatal error, naming the conflicting constraints.

Results are reproducible from Veryl's seed. Every component and every
started sequence has its own seed source, derived from the test's seed and
its names (its type and path), as UVM seeds them, so adding a component
or a sequence elsewhere changes no other numbers. The seed is printed in
the report summary; `veryl +local test --seed 5` gives the same run every
time.

## 10. Running and reading the output

`veryl +local test` builds the crate, then runs the `#[test]` module. For
`BasicTest`:

```text
[INFO ]    Executing test (regblock_test)
[tb] cycle 71: INFO @ 142 BasicTest: done; the scoreboard has seen 16 reads
[tb] cycle 71: INFO @ 142 env.scoreboard: 16 writes, 16 reads checked, 0 mismatches
[tb] cycle 71: --- report summary: test BasicTest, seed 6120866807781676934, ended at 142 ---
[tb] cycle 71: 0 error, 0 warning, 2 info, 1 debug, 32 trace
[INFO ]    Succeeded test (regblock_test)
```

- A message names its severity, the time and the component's path (the
  test's name for the test itself).
- The report summary gives the test, its seed, the end time, and the
  count of messages of each severity, including those not printed.
- The summary also lists the runtime arguments given, when there are
  some, so a log shows how its run was made.
- With `--runtime-arg=VERALD_LOG_LEVEL:debug`, the hierarchy is printed when
  the test starts:

  ```text
  [tb] cycle 0: DEBUG @ 0 BasicTest: hierarchy:
  BasicTest (test)
    env (env, passive)
      bus (agent, passive)
        monitor (monitor)
        sequencer (sequencer, passive)
        driver (driver)
      scoreboard (scoreboard, passive)
  ```

  `trace` also prints the monitor's transfers.

A broken DUT fails the test. With the register at address 5 storing
`wdata ^ 1`:

```text
[tb] cycle 71: ERROR @ 142 env.scoreboard: read 5: expected 0x55f0, got 0x55f1
[tb] cycle 71: ERROR @ 142 env.scoreboard: read 5: expected 0x54f0, got 0x54f1
[tb] cycle 71: INFO @ 142 env.scoreboard: 16 writes, 16 reads checked, 2 mismatches
[tb] cycle 71: --- report summary: test BasicTest, seed 6120866807781676934, ended at 142 ---
[tb] cycle 71: 2 error, 0 warning, 2 info, 1 debug, 32 trace
[ERROR]       Failed test (regblock_test): [tb] cycle 71: 2 errors; the first: env.scoreboard: read 5: expected 0x55f0, got 0x55f1
```

A test fails when it reports an error (`error`, in `run` or `check`) or
ends with a fatal one (an `Err` from a `run`, a timeout, an unexpected
finish); Veryl shows the count and the first error as the test's result.
