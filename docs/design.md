# veryl-tbf design notes

A UVM-style verification framework in Rust for Veryl. This file records the
decisions made so far, the reasons behind them, and what is still open.

Status: design in progress. Implemented: the runtime's core (step 1),
clocking blocks with `run_with_reset` (step 2), and the methodology's
skeleton (step 3: components, phases, tests, specs and the runner). Notes
marked "for now" record where the implementation stops short of the design.

- [Principles](#principles)
- [Simulator interface](#simulator-interface)
- [Replacing inheritance](#replacing-inheritance)
- [Components](#components)
- [Configuration and callbacks](#configuration-and-callbacks)
- [Tests](#tests)
- [Sequences](#sequences)
- [TLM and the sequencer](#tlm-and-the-sequencer)
- [Executor](#executor)
- [Runner](#runner)
- [Crate layout](#crate-layout)
- [Implementation notes](#implementation-notes)
- [Taken from UVM](#taken-from-uvm)
- [Open questions](#open-questions)
- [TODO](#todo)

## Principles

The decisions below come back to these:

- **Typed.** Components, handles, configurations, callbacks and sequencers
  are reached through their types, so that a wrong connection is a compile
  error, not a run-time lookup failure: a sequence's `start` takes its own
  sequencer type, a child is an `Instance<T>`, an analysis port picks its
  receiver's `Subscriber<T>` by item type.
- **No settings addressed by strings.** No config_db-like store: settings
  travel down in typed configurations, and handles are taken from the
  children a level has just made. Strings remain only where Veryl hands
  things over (interface paths, test names, `option` / `set`).
- **A light run.** Where run-time cost and convenience pull apart, the
  lighter run wins: a single-threaded executor, a per-edge path that wakes
  only the tasks waiting, static dispatch, synchronous callbacks, no
  deadlock detection.
- **No code shaped by Rust's constraints in users' hands.** What Rust needs
  (type erasure, borrow splitting, boxing, lifetimes) stays inside the
  framework and its macros, so that users write what a UVM user would: a
  plain `async fn run`, `create_instance`, `fork_scope` without `'static`,
  `Instance<T>` read as a field.
- **UVM where the above allow.** UVM's terms and behaviour are followed
  wherever they do not conflict with the principles above, so that a UVM
  user finds what they expect: the phases' order, fatal and error, random
  stability, the sequencer's arbitration, default sequences, the report
  summary, and the run phase ending by killing what is left.
- **Veryl as the source.** The design's values and types are defined in
  Veryl, and the testbench follows them: values are assigned in the test
  module and reach the testbench through `set` alone, so they come from one
  place, and what Veryl cannot yet hand over is proposed to Veryl (see
  [TODO](#todo)) rather than duplicated in Rust.

## Simulator interface

The framework is built around Veryl's simulator, behind an interface of its
own, so that VPI or DPI adapters can be added when a testbench has to run
elsewhere.

The interface covers only what a testbench needs, not VPI's general
name-lookup-and-callback model:

| Area | Contents |
|---|---|
| Time | clock and reset events; the current time |
| Signals | typed handles bound at elaboration (as a `ModportRef`'s members), never looked up by name |
| Timing | inputs read the value before the edge; outputs written as non-blocking assignments (clocking-block semantics), specified as part of the interface |
| Control | finishing, reporting the result, logging |

Decided: the boundary is shaped for Veryl's simulator, since its shape
holds for the others too.

- The adapter calls the executor; the executor never registers callbacks
  with the simulator. Its entry points are `start`, `clock_edge(clock)`
  (once per edge of a clock), `reset_edge(reset)` and `finish`. The wait
  lists are the executor's own: what a task can wait for starts only at
  clock and reset events, so a general callback mechanism (condition and
  action per registration, scanned at every event, as VPI has) is not
  needed. Waiting for a signal is sampling it at edges
  (`tick_until!`); delays are counted in edges.
- Signals are read and written only within a hook, with clocking-block
  semantics: a read returns the value before the edge, a write reaches the
  design at the edge. The boundary promises no more, so an adapter may read
  on demand (Veryl's `ctx.read`) or otherwise.
- Besides signals, the boundary has the current time, finishing, failing
  and logging.
- Static dispatch (a type parameter or a feature), not a trait object: reads
  and writes are on the per-edge path. Until a second adapter exists, the
  boundary is a module split inside the framework, not a public trait.

How other simulators would fit (not implemented):

- Veryl simulator: veryl-component's hooks call the entry points;
  `ctx.read` / `ctx.write` are the reads and writes.
- DPI: Veryl generates the SV side from its interfaces: a module per clock
  whose `always @(posedge clk)` calls the edge entry point through an
  imported function (one hook per clock and edge, not per modport, so the
  executor runs once all inputs are in), and a bridge per modport with a
  clocking block whose sampled inputs and drives are reached through
  exported functions called from Rust within the hook. The SV language then
  gives the timing. To be verified when built: clocking drives from an
  exported function, and the clocking block having sampled before the
  `always` calls in.
- VPI (for simulators without DPI): the adapter binds signals by name at
  start, registers a value-change callback on each clock that calls the
  edge entry point, reads in that callback and writes with a zero inertial
  delay; the regions must be handled precisely to be race-free, as cocotb
  does.
- Verilator: DPI, or a Rust main loop over the C++ model that calls the
  edge entry point after each `eval`, as Veryl's simulator does.

## Replacing inheritance

Rust has no inheritance. What UVM does with it maps as follows:

| UVM uses inheritance to | Rust uses | Example here |
|---|---|---|
| keep a skeleton and vary a policy or settings | a type parameter or a value implementing a trait with default methods | test specs, driver callbacks |
| reuse methods over shared state | trait default methods; the state reached through an accessor trait | `Sequence::send`, `ApbSequence::write` |
| substitute an implementation | trait objects | callbacks as `Rc<dyn ..>` in a `Callbacks` list |
| add new state or behaviour | composition and delegation | rare |

Delegation is kept for the last case only: forwarding every public method by
hand does not scale. `Deref` to fake inheritance is not used.

A class splits in two: its properties become a struct, its methods a trait,
and an accessor trait connects the two (traits have no fields).

## Components

Decided:

- No factory. Replacing a component means rewriting it (no inheritance);
  components expose callbacks for the differences instead.
- A component's author writes its `build`; the framework makes it, by
  calling `build` from `create_instance` (and, for the test, from its own
  `new`, see [Tests](#tests)). `build` is not a phase: `create_instance`
  calls the child's `build` there and then (depth first), so a parent has
  its child, with the child's own children, right after making it, and
  takes its handles and connects them there; no connect phase is needed.
  What a parent hands a child is the child's `Context`, an associated type
  (an `InstanceContext`, handles, or a tuple of them):

  ```rust
  pub trait Component: Sized + 'static {
      type Context;
      fn build(scope: &Scope, context: Self::Context) -> Result<Self, TestError>;
      // run, check, report: see below
  }
  ```

  Since every component is made through `create_instance`, the framework
  names it, stores it (see [Executor](#executor)) and adds its path to a
  build error without fail; nothing calls `build` directly.
- A child is named where it is made, with `create_instance`, which takes its
  type and its context, and held typed in `Instance<T>`:

  ```rust
  let driver: Instance<ApbDriver> =
      scope.create_instance::<ApbDriver>("driver", context.clone())?;
  ```

  The instance name is used for logging, error messages and the hierarchy
  printout. During the run phase the component's own `run` borrows it for
  the whole phase (see [Executor](#executor)), so that every `run` proceeds
  concurrently; before and after the run the parent reaches it typed.
- An array of children is made with `create_instances`, which names them
  as UVM does (`apb[0]`, `apb[1]`, ...), from one context each:

  ```rust
  let apb: Vec<Instance<ApbAgent>> =
      scope.create_instances::<ApbAgent>("apb", contexts)?;
  ```
- A child that may be absent (a passive agent's driver) is an
  `OptInstance<T>`, an alias of `Option<Instance<T>>`: the types still tell a
  child that is always there from one that may not be, and the usual `Option`
  API applies. `create_instance_if` makes one conditionally:

  ```rust
  pub type OptInstance<T> = Option<Instance<T>>;

  let driver: OptInstance<ApbDriver> =
      scope.create_instance_if::<ApbDriver>(active, "driver", context.clone())?;
  ```
- There is no build phase walked by the framework top-down (it would need a
  connect phase after it). The test's hierarchy is built when the testbench
  starts (after the test module has handed over its interfaces), so `build`
  can already bind interface handles.

- Phases after construction, each with a default that does nothing:

  | Phase | Does | Shape |
  |---|---|---|
  | `run` | every component, concurrently | `async fn run(&mut self, context: &mut RunContext) -> Result<(), TestError>` |
  | `check` | checks what the run left | `fn check(&mut self, context: &mut CheckContext)`, errors reported to `context` |
  | `report` | summarizes | `fn report(&self, context: &ReportContext)` |

  UVM's extract folds into `check`. end_of_elaboration,
  start_of_simulation and final are added when needed. There are no runtime
  sub-phases (reset, main, ...): a component handles resets inside `run`.
- The run phase ends when the test's `run` returns. The test scenario is
  started from there: the test's `run` starts the sequences and waits for
  what it needs, so its end is the scenario's end. There are no objections.
  The `run`s still going (drivers' and monitors' loops, default sequences)
  are then dropped, as UVM kills its run_phase processes.
  - A test's `run` is required (other components' `run` defaults to doing
    nothing), so a test cannot end at time 0 by forgetting it: the
    `#[component(test)]` attribute fails to compile on a test with no `run`
    of its own.
  - What UVM does with objections raised elsewhere is done in the test's
    `run`: waiting for a scoreboard to empty, for example. Waiting for the
    last transfers after the scenario is the drain time (see [TODO](#todo)).
- Two kinds of errors:

  | Path | Meaning | Effect |
  |---|---|---|
  | `context.error(..)` | an error the test can go on after (a mismatch; `uvm_error`) | recorded; the test fails; the run goes on |
  | `run` returns `Err` (`?`) | a fatal error (a sequence failing to start, randomization failing, a wait timing out; `uvm_fatal`) | the test fails and the run phase ends at once: every `run` is dropped |

  `run` returns `Result` so that fatal errors can be propagated with `?`.
  There is one error type, `TestError`, for `build`, `run` and a
  sequence's `body` alike, so `?` carries an error across them without
  conversions.
- After a fatal error, `check` and `report` are skipped, as with
  `uvm_fatal`: a run cut short leaves transfers half done, and the other
  components' findings would mostly be noise. The framework reports the
  fatal error (the component's path and the message) and the test's result.

- `check` and `report` go bottom-up (children first), as in UVM, where they
  are bottom-up phases. A component whose `run` was dropped is no longer
  borrowed by then, so a scoreboard's `check` always runs.

- Every component's `run` is concurrent; a parent never waits for its
  children.

- A component's configuration and status travel together as an
  `InstanceContext<C, S>`: the configuration `C`, fixed after construction
  and shared with `Rc`, and the status `S`, written by the components
  (observations) and shared with `RefCell`. Agents and envs get one as their
  `Context` and hand clones to their children; a test, the root, makes the
  ones it hands down from its configuration. (Named apart from the phase
  contexts, `RunContext` and the others.)
- Settings that may change while the test runs are `Knob<T>` fields of the
  configuration (an `Rc<RefCell<T>>` with `get`, which returns a clone, and
  `set`; clones share the value). The types then tell what may change at run
  time from what may not (widths, modes). A component reads a knob where it
  uses the value, every time; copying it at construction would miss changes.
  `get` returning a value means no borrow is held across an await.

  ```rust
  pub struct AxiSlaveConfig {
      pub data_width: u32,                    // fixed after construction
      pub ready_delay: Knob<DelayRange>,      // tunable at run time
  }

  // the driver, each transfer
  let delay = self.context.config().ready_delay.get().pick(&mut rng);
  // the test, during run
  self.context.config().ready_delay.set(DelayRange::new(0..=8));
  ```

  Behaviour a value cannot express stays with callbacks.
- Interfaces are found by path: the Veryl test module
  registers each under a path (`tb.set_interface("apb", apb.master)`), the
  configuration holds that path as a string, and the component that drives or
  watches the bus (the driver, the monitor) looks the interface up by it in
  the interface registry, typed, failing at construction if the path is
  missing or the type differs. `build` returns `Result`, so such failures
  propagate with `?`; `create_instance` passes them up, and `start` reports
  the first with the path of the component being built and fails the test:

  ```rust
  pub struct ApbConfig {
      pub driver_if: String,                  // the path the test module registered
      pub monitor_if: String,
      ..
  }

  // the driver's build
  let bus: ApbMasterMP = scope.interface(&context.config().driver_if)?;
  ```

- Random stability follows UVM. Every component has its own random
  generator, seeded from the test's seed, its type name and its full instance
  path (UVM's `uvm_create_random_seed(type_name, full_name)`), so changing
  another part of the hierarchy does not change its numbers. A sequence is
  seeded the same way from its sequencer's path and its own name; as in UVM,
  a per-string count makes repeated creations under the same name differ
  while staying reproducible. Constrained random draws its seeds from these
  generators, so nothing calls a `set_seed` by hand.
- What each stage is given:

  | | Gives |
  |---|---|
  | `Scope` (construction) | the path, creating children, interfaces, random, logging |
  | `RunContext` (`run`) | the path, logging, `error`, random, the current time |
  | `CheckContext` (`check`) | the path, logging, `error` |
  | `ReportContext` (`report`) | the path, logging |

  Logging has the levels of Rust's `log` crate: `error`, `warning`,
  `info`, `debug`, `trace`; there is no `fatal` (a fatal error is an `Err`
  from `run`). What is printed is a threshold, as `log`'s `LevelFilter`, in
  place of UVM's verbosity: `tb.option("log_level", "info")`, `info` by
  default. Errors are counted, and decide the result, whatever is printed.
  Per-component levels and message IDs are added when needed.

- Handles (a sequencer, an analysis port) are reached directly after
  `create_instance`, typed. When a level above needs one, that level holds
  it: it takes the handle from the child it has just made and keeps a
  reference to it. Each level passes on only what the level above it uses;
  there is no lookup.

  ```rust
  // the env's build (its Context is its configuration, EnvConfig)
  let agent = scope.create_instance::<ApbAgent>("agent", InstanceContext::new(config.apb))?;
  let apb = agent.sequencer();            // kept by the env, for the test
  let scoreboard = scope.create_instance::<Scoreboard>("scoreboard", ())?;
  agent.analysis_port().connect(&scoreboard, ());
  Ok(Env { agent, scoreboard, apb })

  // the test's build
  let env = scope.create_instance::<Env>("env", config.env)?;
  let apb = env.apb.clone();
  ```

## Configuration and callbacks

Decided:

- Settings travel down typed, in the configuration: as UVM's configuration
  objects, a level's configuration nests its children's (an env's holds its
  agents'), and each level hands its children theirs as their `Context`.
  There is no config_db-like store until a need appears: what it would
  carry (settings, callbacks, a default sequence, an arbiter) goes in the
  configuration or through a typed handle.
- Callbacks (UVM's `uvm_callback`) are part of the configuration. A VIP
  defines a trait per component with a no-op default at each point; the
  component's configuration holds a `Callbacks<dyn Trait>`, a shared list
  like a `Knob` (cloning shares it, so it can be added to after the
  configuration is frozen in an `InstanceContext`); the component calls
  every callback in the order added, and decides how their results combine.

  ```rust
  // the VIP
  pub trait ApbDriverCallbacks: 'static {
      /// Before driving: may change the item (error injection) or skip it.
      fn pre_drive(&self, _item: &mut ApbItem, _cx: &mut CallbackContext) -> Action { Action::Go }
      /// Cycles to wait before driving.
      fn delay(&self, _item: &ApbItem, _cx: &mut CallbackContext) -> u32 { 0 }
      /// After the response.
      fn post_drive(&self, _item: &ApbItem, _rsp: &ApbResponse, _cx: &mut CallbackContext) {}
  }

  pub struct ApbConfig {
      pub driver_callbacks: Callbacks<dyn ApbDriverCallbacks>,
      ..
  }

  // the driver's run, at each point
  let delay = callbacks.sum(|c| c.delay(&item, &mut cx));

  // a spec's pre_build
  fn pre_build(&self, config: &mut RegTestConfig) {
      config.env.apb.driver_callbacks.add(Rc::new(SlowWriteCb));
      for axi in &mut config.env.axi { axi.driver_callbacks.add(Rc::new(ErrorInj { rate: 5 })); }
  }
  ```

  - Callbacks are synchronous. One that needs time returns what to do (a
    number of cycles to wait) and the component does it: an `async`
    callback behind a trait object would need erasure and a boxed future at
    every call; a synchronous one is one virtual call.
  - They take `&self` (they are shared); one with state keeps it in a
    `Cell` / `RefCell`. One that should turn on and off during the run reads
    a `Knob` of its own.
  - `CallbackContext` gives the caller's path, logging, the current time and
    the caller's random generator, so random stability holds.
  - With none added, a call point walks an empty list.
  - The configuration is the main way in, before the build; the component
    may also expose the list (`agent.driver_callbacks()`) for adding after
    the build or during the run.
- pre_build / post_build are plain code around a base's `build`: the
  component that composes the base calls the base's `build`, so what it
  writes before is pre_build and what it writes after is post_build. A test
  has them as spec hooks (see [Tests](#tests)).

## Tests

Decided: a test is made in two steps, its configuration and then its
hierarchy, and tests that share a hierarchy and differ in settings follow
the idea of constraint sets in constrained random: one test
type, plus a spec applied between and after the two steps.

```rust
// framework
pub trait Test: Component {
    /// Makes the configuration (may randomize it): the test's `Context`.
    fn setup(cx: &mut SetupContext) -> Result<Self::Context, TestError>;
}

pub trait Spec<T: Test> {
    fn pre_build(&self, _config: &mut T::Context) {}  // after setup, before build: settings, callbacks
    fn post_build(&self, _test: &mut T) {}            // after build, before run: typed adjustments
}
impl<T: Test> Spec<T> for () {}                                   // no spec
pub struct BundledSpecs<T: Test> { specs: Vec<Box<dyn Spec<T>>> } // applied in the order given
impl<T: Test> Spec<T> for BundledSpecs<T> { .. }

// the framework's new for a test, at start
let mut config = T::setup(&mut cx)?;
spec.pre_build(&mut config);
let mut test = scope.create_root::<T>(config)?;          // T::build
spec.post_build(&mut test);

// a test: setup and build, no spec in sight
impl Test for RegTest {
    fn setup(cx: &mut SetupContext) -> Result<RegTestConfig, TestError> {
        Ok(RegTestConfig::new(cx))
    }
}
impl Component for RegTest {
    type Context = RegTestConfig;
    fn build(scope: &Scope, config: RegTestConfig) -> Result<Self, TestError> {
        let env = scope.create_instance::<Env>("env", config.env)?;
        Ok(RegTest { env })
    }
    async fn run(&mut self, context: &mut RunContext) -> Result<(), TestError> { .. }
}
```

A test type and a spec are paired when the test is registered (see
[Runner](#runner)).

This is how a test gets UVM's pre_build / post_build without a build phase
walked by the framework (which would bring a connect phase along, since a
parent could no longer take its children's handles right after making
them). The framework applies the spec, so a test's author writes `setup`
and `build` only and cannot forget or misorder the hooks. A test takes one
spec; specs are layered by `BundledSpecs`, a spec that applies the specs it
holds in the order given. It is made with the macro
`bundled_specs![A, B, ..]`, or step by step with
`BundledSpecs::new().with(A).with(B)` when what goes in depends on a
condition; either boxes each spec, once, while the test is built. It is the
one way to bundle (no tuples), so there is no limit on the count. A user may
also write a named bundle of their own. A spec implements only the methods
it needs and is used up while the test is made, so the test keeps neither
it nor a type parameter for it; the build itself (how the env is made) is
never rewritten. `SetupContext` gives the setup the settings the test
module passed with `set` (`cx.get`, see [Runner](#runner)), random numbers
(from the test's seed) and logging.

The hierarchy is built in one place, so it is the same for every test.

## Sequences

Decided: three layers, as `uvm_sequence_base`, a protocol's sequence base,
and the sequences that generate items.

| Layer | Fields declared by | Methods |
|---|---|---|
| 1. sequence base | the framework (`SeqBase`: the sequencer, the parent sequence, the current item, ids, rng, logger, priority) | `Sequence` trait: required `body`; provided `start_item`, `finish_item`, `send`, `start`, `sequencer` |
| 2. protocol base | the VIP, in a struct marked `#[sequence_base(item = ..)]`, which embeds `SeqBase` (`parent = ..` for more levels) | a protocol trait with default methods, e.g. `ApbSequence: Sequence<..> + HasBase<ApbSeqBase>`, given to every such sequence by a blanket impl |
| 3. sequence | its author | `body` only |

```rust
#[sequence(base = ApbSeqBase)]       // inserts the base as a hidden field
struct RandomWrites { count: usize }

impl Sequence for RandomWrites {
    async fn body(&mut self) -> Result<(), TestError> {
        for _ in 0..self.count {
            let mut item = ApbItem::new(self.context().config());
            self.start_item(&mut item).await?;
            randomize_with!(item, { address < 0x20; })?;
            self.finish_item(item).await?;
        }
        self.write(0x00, 0x1234).await?;          // layer 2
        Ok(())
    }
}
```

- The base is composed into the sequence by a proc macro (`#[sequence]`),
  which also implements the accessor traits (`HasSeqBase`, `HasBase<T>`).
  The accessor trait is separate from `Sequence` so that a macro on the
  struct can implement it without touching the author's `impl Sequence`.
- `start` belongs to the framework, so it sets the sequencer (UVM's
  `m_sequencer`) before `body`; every layer sees the same sequencer.
- Every base struct implements `Default`. The macro generates `new(..)` over
  the author's fields, which then need not be `Default`; a struct literal with
  `..Default::default()` works when they are.
- VIP authors write no proc macros; the macros are the framework's.
- A protocol context (configuration) is reached through the sequencer
  (see below).

### Items

Decided: items are shared once sent, as UVM passes one object by handle
between a sequence, its driver and the monitors' subscribers:

- A sequence builds and randomizes an item as a plain value (`&mut`), and
  `finish_item` turns it into an `ItemHandle<T>` (an `Rc` underneath),
  which it returns: the sequence keeps the item and reads what the driver
  filled in, as a UVM sequence reads `req` after `finish_item`. The driver
  gets the same handle from `get_next_item`; a monitor publishes the items
  it makes as handles, so subscribers and a reactive sequencer share them
  rather than copies. One `Rc` per item, about what UVM's object costs.

  ```rust
  // the sequence
  let item: ItemHandle<ApbItem> = self.finish_item(item).await?;
  let data = item.borrow().rdata;          // filled in by the driver
  item.end_event().wait().await;           // or wait for the transfer's end

  // the driver
  {
      let mut it = item.borrow_mut();
      it.rdata = sampled;
      it.resp = Resp::Okay;
  }                                        // the borrow ends before any wait
  ```

- The contents are reached with `borrow()` / `borrow_mut()` (a `RefCell`
  inside), the names `Instance<T>` uses during the run, not `read` / `write`,
  which would read as bus operations. A borrow must not be held across an
  `.await`: another holder's borrow would then panic (found at run time,
  never undefined behaviour). Clippy's `await_holding_refcell_ref` lint,
  off by default, catches it at compile time and is recommended.

### Sequencers and virtual sequences

A sequencer is a component in the hierarchy, as in UVM, and its type is the
user's: `#[sequencer(item = ..)]` embeds the base sequencer (item queue,
arbitration) in a struct with fields of the user's own. A virtual sequencer
is the same thing with sub-sequencers among its fields; it is made in the env
like any other component.

```rust
#[sequencer(item = ApbItem)]
pub struct ApbSequencer {
    pub context: InstanceContext<ApbConfig>,        // what sequences on it may need
}

#[sequencer(item = ())]                             // virtual: no driver behind it
pub struct SocSequencer {
    pub apb: SequencerHandle<ApbSequencer>,
    pub axi: SequencerHandle<AxiSequencer>,
}

// the env's build (SocSequencer's Context is its two sub-sequencer handles)
let apb = scope.create_instance::<ApbAgent>("apb", apb_context)?;
let axi = scope.create_instance::<AxiAgent>("axi", axi_context)?;
let vseqr = scope.create_instance::<SocSequencer>("vseqr", (apb.sequencer(), axi.sequencer()))?;
```

A sequence names the sequencer type it runs on, and `start` takes exactly
that type:

```rust
#[sequence(sequencer = SocSequencer)]      // Req/Rsp come from the sequencer's item
struct InitThenTraffic { count: usize }

impl Sequence for InitThenTraffic {
    async fn body(&mut self) -> Result<(), TestError> {
        let soc = self.sequencer().clone();          // typed: UVM's p_sequencer, no cast
        ApbInitSeq::new().start(&soc.apb).await?;
        let (a, b) = join(
            RandomWrites::new(self.count).start(&soc.apb),
            AxiBurstSeq::new(self.count).start(&soc.axi),
        ).await;
        a?;
        b?;
        Ok(())
    }
}

// the test's run
InitThenTraffic::new(10).start(&self.vseqr).await?;
```

- A sequence holds a handle to the sequencer (`SequencerHandle<T>`, a cheap
  clone giving typed access), not the component, which its own `run`
  borrows during the run phase.
- `start(&mut self, sequencer: &SequencerHandle<Self::Sequencer>)` is typed,
  so starting a sequence on the wrong sequencer does not compile. With no
  factory there is no type erasure, so UVM's `p_sequencer` needs no `$cast`:
  `m_sequencer` and `p_sequencer` are one typed handle, `self.sequencer()`.
- A virtual sequence is an ordinary `Sequence`. Whether it sends items
  depends only on its sequencer's item type: on an `item = ()` sequencer it
  only starts other sequences; a sequencer with an item type lets it send
  items too. No separate trait.
- A sequence that names no sequencer (`#[sequence(item = ApbItem)]`) runs on
  the plain sequencer of that item.
- A sequencer's sub-sequencer handles may be given in its `Context` or set
  after it is made, while the parent still holds it typed (before the run phase), so the
  virtual sequencer may be made before the agents. A `SequencerHandle<T>`
  starts unbound (its `Default`); before the run phase starts, the framework
  checks every component for unbound handles and stops with the path of any
  it finds, so a missing connection never surfaces only when a sequence
  starts. A handle that may stay unconnected is an
  `Option<SequencerHandle<T>>`, which the check skips.
- Arrays of sub-sequencers are `Vec<SequencerHandle<T>>`, filled from the
  array of agents. An empty `Vec` passes the unbound check; a sequencer that
  needs a given count checks it itself.

  ```rust
  #[sequencer(item = ())]
  pub struct MultiApbSequencer {                // its Context is (): filled after it is made
      pub apb: Vec<SequencerHandle<ApbSequencer>>,
  }

  let mut vseqr = scope.create_instance::<MultiApbSequencer>("vseqr", ())?;
  for agent in &apb {
      vseqr.apb.push(agent.sequencer());
  }
  ```

### Default sequence

Decided: as in UVM and OVM, a sequencer runs a default sequence in the run
phase when one is set. It is set typed, in the sequencer's configuration
(before the build) or through its handle (after it):

```rust
// a spec's pre_build: the agent's configuration carries its sequencer's
config.env.apb.default_sequence.set(|| RandomWrites::new(10));
// or a spec's post_build, through the handle the env keeps
test.env.apb.set_default_sequence(|| RandomWrites::new(10));
```

- It is a `DefaultSequence<S>`, typed by the sequencer, so a sequence for
  another sequencer type does not compile. One value: the last set wins.
- The value is a function that builds the sequence; the sequencer calls it
  once at the start of its `run` and starts the sequence on itself. The
  sequencer's part of `run` is the framework's (from `#[sequencer]`), so a
  sequencer's author writes nothing for it.
- A default sequence runs in the background: the run phase ends with the
  test's `run`, not with it, and it is dropped with the other `run`s. It
  suits traffic that only accompanies the scenario (idle traffic, a
  responder). UVM 1.2's automatic phase objection is not needed: a sequence
  the test waits for is started from the test's `run`.
- A default sequence that returns `Err` is a fatal error, as any `run`.
- Sequences started explicitly (from the test's `run`, for example) run
  alongside it and share the sequencer's arbitration.

## TLM and the sequencer

Decided: both follow UVM. TLM is kept to the minimum:

- the sequence item port between a driver and its sequencer
  (`seq_item_port` / `seq_item_export`): `get_next_item`, `try_next_item`,
  `item_done`, `put_response`, as in UVM;
- the analysis port: one writer, any number of subscribers, broadcast.

Other UVM TLM ports (put/get/peek, FIFOs, ...) are added when needed.
Connections are made in `build`, between typed handles, so a mismatched item
type is a compile error.

### Receiving from an analysis port

A port's `write` runs synchronously inside the publisher's `run`. It cannot
call into a component that has a `run` of its own: that component is held
mutably by its running `run` for the whole run phase, so there is nothing to
borrow. Writes therefore go
to things without a `run`. Three ways to receive:

| Receiver | Connect | For |
|---|---|---|
| a passive component (`#[component(passive)]`, implements `PassiveComponent`: no `run`, still `check` / `report`) | `port.connect(&instance, id)` | scoreboards, coverage: components that only react |
| a plain shared object (no path, no phases) | `port.subscribe(closure)` | reference and expected-value models, ad-hoc logging |
| an `AnalysisFifo<T>` field of a component with a `run`, which reads it there; the component exposes the FIFO's `AnalysisExport<T>` | `port.connect_export(&export)` | receivers that must wait for time (a response due within N cycles) |

An `AnalysisFifo<T>` keeps its queue behind an `Rc`; its `AnalysisExport<T>`
is a cloneable handle that only pushes, so a port holds the export, never the
component, and can push while the component's `run` holds it (`get` waits
for the next item). This is UVM's `uvm_tlm_analysis_fifo` and its
`analysis_export`:

```rust
impl Scoreboard {
    pub fn output_export(&self) -> AnalysisExport<ItemB> { self.output.export() }
}
// the env's build
output_monitor.analysis_port().connect_export(&scoreboard.output_export());
```

A passive component, like every component, lives in an `Rc<RefCell<T>>`
(see [Executor](#executor)), and a port keeps a weak reference to it; with no `run` holding a borrow, the write can borrow
it. The two kinds are separate traits, so a passive component cannot grow a
`run` by mistake. Connecting to a passive component is a closure underneath,
so there is one mechanism. Writes are synchronous, so arrival order across
ports is kept; `write` is not `async` (as UVM's `write` is a function). A
chain of writes that loops back to the same component would panic on the
double borrow; one-way chains (predictor to scoreboard) do not.

A passive component receives through `Subscriber<T>`, one impl per item
type. Ports of the same item type are told apart by an ID given at connect
time; there are no `write_xxx` methods (UVM's `uvm_analysis_imp_decl`).

```rust
pub trait Subscriber<T> {
    type Id;                                   // () when one port of this type
    fn write(&mut self, id: Self::Id, item: &T);
}

// ports carry items as handles (see Items), so subscribers are typed by them
impl Subscriber<ItemHandle<ItemA>> for Scoreboard {    // input side: queue the prediction
    type Id = ();
    fn write(&mut self, _id: (), item: &ItemHandle<ItemA>) {
        self.expected.push_back(predict(&item.borrow()));
    }
}

impl Subscriber<ItemHandle<ItemB>> for Scoreboard {    // output side: compare at once
    type Id = ();
    fn write(&mut self, _id: (), item: &ItemHandle<ItemB>) { .. }
}

// the env's build: the impl is chosen by the port's item type
input_monitor.analysis_port().connect(&scoreboard, ());
output_monitor.analysis_port().connect(&scoreboard, ());
```

- The item type picks the impl at compile time; a port whose item type the
  receiver has no impl for does not compile. Items travel as
  `ItemHandle<T>` on every port (analysis ports, `AnalysisFifo`, the
  sequence item port), so a receiver may keep the handle and wait on the
  item's events.
- The ID is an enum of the user's, so a wrong ID does not compile and a
  `match` on it is checked for completeness. It may carry a value for ports
  made in a loop: `enum Lane { Port(usize) }`.
- The typical scoreboard (inputs queued, outputs compared on arrival) is a
  passive component with a `VecDeque`; leftovers are reported in `check`. If
  an output can be observed at the same edge as its input (zero latency),
  queue early outputs too and compare when both sides have one.

### Arbitration and responses

Decided:

- Responses are matched to requests by transaction id, as UVM's
  `get_response(id)`.
- The sequencer arbitrates in FIFO order by default (UVM's `SEQ_ARB_FIFO`),
  and the arbitration is replaceable: an `Arbiter` trait object picks one of
  the requests waiting when the driver asks for an item. Only `FifoArbiter`
  is implemented at first; the interface is fixed now so that the others
  (UVM's `WEIGHTED`, `RANDOM`, `STRICT_FIFO`, `STRICT_RANDOM`, a user's own,
  a round robin) can be added without changing it.

  ```rust
  pub trait Arbiter: 'static {
      /// Picks the request to grant: an index into `requests`.
      fn select(&mut self, requests: &[ArbRequest], rng: &mut Rng) -> usize;
  }

  pub struct ArbRequest {
      pub sequence_id: SequenceId,
      pub sequence_name: &'static str,   // the sequence's type, for arbiters that tell sequences apart
      pub priority: i32,                 // the sequence's priority (UVM's default 100)
      pub waiting_since: u64,            // the sequencer's count of requests, when this one began waiting
  }

  /// UVM's SEQ_ARB_FIFO: the oldest request.
  pub struct FifoArbiter;
  impl Arbiter for FifoArbiter {
      fn select(&mut self, _requests: &[ArbRequest], _rng: &mut Rng) -> usize { 0 }
  }
  ```

  What the sequencer guarantees an arbiter:
  - `select` is called only with at least one request, and the requests are
    in the order they began waiting (`waiting_since` rising), so the oldest
    is at index 0.
  - The requests are the eligible ones only: what `grab` / `lock` (later)
    and a sequence's relevance would exclude is filtered out by the
    sequencer before the call, so an arbiter never deals with them, and
    adding them later does not change the interface.
  - `&mut self` lets an arbiter keep state (a round robin's last grant);
    `rng` is the sequencer's generator, so a random arbiter keeps random
    stability.
  - An index out of range is a fatal error naming the sequencer.
  - The arbiter is called once per grant, on the per-item path, so it is a
    plain synchronous call with no allocation: `ArbRequest` is a small copy
    kept in the sequencer's queue.
- An arbiter is set on the sequencer, through its instance before the run
  or its handle at any time (UVM's `set_arbitration`):
  `apb.set_arbiter(MyArbiter::new())`. A sequencer deep in the hierarchy can
  be given one through its configuration, which holds a factory, as each
  sequencer needs an arbiter of its own.
- A sequence's priority follows UVM: it is set before the sequence starts
  (`set_priority`); one left unset takes its parent sequence's when started
  from one (UVM's `this_priority = -1`), else 100. The priority is fixed
  when the sequence starts, so an arbiter sees only the value in
  `ArbRequest::priority`.
- A sequence records its parent (the sequence it was started from, as UVM's
  `m_parent_sequence`), for the priority above and for `grab` / `lock`
  later, which, as in UVM, let the locking sequence and its descendants
  through. The sequencer uses it when filtering requests; arbiters do not
  see it, as UVM's standard arbitrations do not use it.
- `grab` and `lock` are added when needed.

## Executor

Where choices trade run-time cost against convenience, the lighter run
wins.

Decided:

- The executor is single-threaded and deterministic. A testbench runs only
  at the simulator's hooks, while the simulator waits, and does little per
  edge, so spreading it over threads would cost more in synchronization than
  it gains, would need `Send` and `Mutex` throughout, and would make the
  order of events (and with it the results for a seed) vary from run to run.
  Multiple cores are used by running tests in parallel, which `veryl test`
  already does (one test per worker thread); heavy computations (a reference
  model) can later be handed to other threads and awaited.
- Components are type-erased with a dyn-compatible twin of `Component` and a
  blanket impl, the usual pattern for async traits (as `async-trait` and
  `dynosaur` generate). Users implement `Component` with a plain `async fn`;
  the twin is the framework's:

  ```rust
  pub trait Component: Sized + 'static {
      type Context;
      fn build(scope: &Scope, context: Self::Context) -> Result<Self, TestError>;
      async fn run(&mut self, _context: &mut RunContext) -> Result<(), TestError> { Ok(()) }
      fn check(&mut self, _context: &mut CheckContext) {}
      fn report(&self, _context: &ReportContext) {}
  }

  trait ErasedComponent {
      fn run<'a>(&'a mut self, context: &'a mut RunContext)
          -> Pin<Box<dyn Future<Output = Result<(), TestError>> + 'a>>;
      fn check(&mut self, context: &mut CheckContext);
      fn report(&self, context: &ReportContext);
  }

  impl<T: Component> ErasedComponent for T { .. }   // forwards; boxes run's future
  ```

  - A component lives in an `Rc<RefCell<T>>`. `Instance<T>` holds it typed;
    the hierarchy holds the same `Rc` as `Rc<RefCell<dyn ErasedComponent>>`
    (an unsizing coercion), so a component stays one object that the
    framework can walk (the hierarchy printout, the unbound-handle check)
    and that ports can refer to weakly.
  - A component's run task owns the `Rc` and borrows the component inside
    its async block for the whole run phase. Dropping the task releases the
    borrow; nothing is moved out and put back, and no `unsafe` is needed.
    Reaching a component whose `run` is going panics on the borrow.
  - `Instance<T>` gives the component through `Deref` / `DerefMut` outside
    the run phase, where most access is (a parent's `build` taking handles and
    connecting ports, a spec's `post_build`, `check` and `report`), so it
    reads as a field: `agent.sequencer()`. During the run phase `Deref`
    panics with the child's path, and `borrow()` / `borrow_mut()` are used
    instead, checked by the `RefCell`: a passive component (a scoreboard
    the test's `run` waits on) is borrowed by port writes meanwhile, and a
    plain reference held across an `.await` could alias them.

    ```rust
    // the test's run
    while !self.env.borrow().scoreboard.borrow().is_empty() { .. }
    ```

    `Deref` checks the phase and the `RefCell`'s state and returns the
    reference unguarded; no reference from outside the run phase survives
    into it, since the code that took it has returned.
  - A passive component (`PassiveComponent`, no `run`) becomes an
    `ErasedComponent` through a blanket impl of its own, which gives it no
    run task: it is never borrowed for the run phase, so port writes can
    borrow it meanwhile.
  - The futures are not `Send`, as intended; the `async_fn_in_trait` lint,
    which warns about exactly that, is allowed.
  - The cost of erasure is one boxed future per component and one borrow per
    run phase; the code inside a `run` is one state machine of concrete
    types.
- The per-edge path is kept light, as every driver and monitor waits for
  every edge:
  - a clock wakes only the tasks waiting for it, kept per clock as task ids;
    a hook with no task waiting returns at once;
  - waiting registers a task id in a reused list, with no allocation per
    wait;
  - a waker carries only a task id: cloning is a copy, with no reference
    count; the ready queue is a `VecDeque` of task ids;
  - a clocking block keeps its members as plain fields:
    `tick` reads the inputs it declares, nothing else. A conditional wait
    (`tick_until!`, `tick_while!`, `posedge!`, ..) reads, while it waits,
    only the members its condition refers to, found in the condition by the
    macro, and all inputs at the edge it ends: a monitor idle until `psel`
    reads one signal per edge. Fields not read meanwhile are stale, but only
    the condition runs while waiting. A condition the macro cannot see into
    (a method on the block) reads all inputs at each edge. The gain is the
    reading and conversion on the Rust side: Veryl's simulator copies a
    component's inputs at every edge it hears regardless.

### Clocking blocks

Decided (implemented in the runtime, the macros in `veryl-tbf-macros`); what
users see follows rustdv-veryl's clocking blocks:

- A `#[clocking]` struct is a clocking block, as SystemVerilog's
  `clocking cb @(posedge clk)`. Its fields are `#[input]` (sampled) and
  `#[output]` (driven) signals, and `#[modport]` fields, structs declaring a
  modport's members the same way. A signal need not belong to an interface
  (an interrupt).
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
  against the fields' types at construction. Until a Veryl clocking
  declaration can say which clock a block follows (see [TODO](#todo)), the
  clock is given here.
- Field types implement `SignalValue`: `bool`, the unsigned integers up to
  `u128`, `BigUint` (any width), and `Value` (the bits as they are); every
  type but `Value` reads X and Z as 0, as SystemVerilog's conversion to
  two-state types.
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
  names while waiting, see above); the `_within` forms of the macros give
  up after a number of edges with a `ClockingTimeout`, which `?` turns into
  a fatal error naming the condition.
- The macros' generated code names the runtime through the crate the user
  depends on, `veryl-tbf` (as `veryl_tbf::runtime`) or `veryl-tbf-runtime`.
- `run_with_reset(reset, state, on_reset, body)` is in the runtime: it runs
  `on_reset`, waits for the release, runs `body` until the next assertion
  (dropping it, with what it forked), and starts over.

### Concurrency within a component

Decided:

- SystemVerilog's `fork` maps as follows:

  | SystemVerilog | here |
  |---|---|
  | `fork .. join` | `join(a, b).await` (and `try_join`) |
  | `fork .. join_any` | `first(a, b).await`; the others are dropped, not left running |
  | `fork .. join_none` | `scope.fork(..)` inside a `fork_scope` |
  | `disable fork` | dropping the scope (or the future that holds it) |

- A `fork_scope` owns the tasks forked in it and polls them along with its
  own body (as the `moro` crate does), so they may borrow what outlives the
  scope and need no `'static`, no join handle and no lifetime management.
  Dropping the scope drops them: a reset that drops a driver's work
  (`run_with_reset`) also drops what it forked, with no `disable fork`.
- There is no detached spawn: a task is needed apart from its starter only
  when the starter returns first, and the code that starts work (a
  component's `run`, the test's `run`) lasts as long as the work. Added if a
  need appears.
- A component has one `run`. Two `run`s would each take `&mut self` at once;
  several threads of one component are one `run` joining several loops.
- The framework's parts that tasks share (`Fifo`, `AnalysisFifo`, clock
  handles, events) work through `&self`, with interior mutability inside:
  `Fifo::get(&self)` changes the queue, as `std::sync::mpsc::Receiver::recv`
  does. The `RefCell` inside is borrowed only within one poll, never across
  an `.await`, and the executor is single-threaded, so the borrows never
  overlap. Users meet `Cell` / `RefCell` only when a forked task changes a
  field of their own.
- The recommended shape for a component with several threads: a struct per
  thread (a stage, a channel) holding what that thread owns, the parts the
  threads share as fields of the component, and `run` joining the
  threads' methods. Rust borrows fields written out directly
  (`s.delay`, `s.exec`) separately, so no destructuring is needed. A slave
  driver that delays each request and then responds:

  ```rust
  pub struct SlaveDriver {
      delay: DelayStage,        // the request FIFO (fed by the monitor), clock, rng, config
      exec: ExecStage,          // the clocking block
      ready: Fifo<Request>,     // the execution queue, shared by the stages
      reset: ResetSignal,
  }

  impl DelayStage {
      async fn run(&mut self, ready: &Fifo<Request>) -> Result<Infallible, TestError> {
          fork_scope(async |scope| loop {
              let req = self.requests.get().await;
              let delay = self.config().resp_delay.get().pick(&mut self.rng);
              scope.fork(delayed_push(&self.clk, delay, req, ready));   // fork .. join_none
          })
          .await
      }
  }

  /// The forked task: waits `delay` cycles, then queues the request.
  async fn delayed_push(clk: &Clock, delay: u32, req: Request, ready: &Fifo<Request>) {
      clk.wait(delay).await;
      ready.push(req);
  }

  impl ExecStage {
      async fn run(&mut self, ready: &Fifo<Request>) -> Result<Infallible, TestError> {
          loop {
              let req = ready.get().await;
              self.cb.respond(&req);
              tick!(self.cb);
          }
      }
  }

  // SlaveDriver::run: run_with_reset(.., on_reset, body) with
  // body = async |s| join(s.delay.run(&s.ready), s.exec.run(&s.ready)).await
  ```

  A forked task is an `async fn` called with what it needs (`fork` takes
  the future the call returns, as SystemVerilog's `fork` takes a task
  call): the arguments say exactly what the task borrows (`&self.clk`,
  `ready`) and what it owns (`req`), so it borrows no more of `self` than
  those, and the loop goes on using the rest (`self.requests`,
  `self.rng`). Requests whose delays end at the same edge are queued in the
  order they started waiting.
- A thread may also be a child component with its own `run`, when it is to
  be replaced or logged on its own.

### Veryl's simulator

What the executor builds on (veryl-component, as of the reference-arguments
work):

- A component runs only in its hooks: `on_init`, methods the test module
  calls (`inst.name(..)`, at zero time), `on_clock`, `on_reset`,
  `on_finish`. `on_clock` is called once per firing clock, at its active
  edge only, in a deterministic order when several fire at once, and at
  every edge: there is no way to skip one.
- In `on_clock` and `on_reset`, reads return the value before the edge, and
  writes reach the design after the edge's flip-flops commit.
- In a method, writes are held until the component's next hook (so they
  reach the design after the next edge), and reads return the value of the
  last edge the component saw (at time 0, the value when the port was bound).
- `on_clock` and `on_reset` are exclusive: while a reset is asserted, every
  edge calls `on_reset` and none calls `on_clock`, whatever drives the
  reset; the release calls no hook. (Whether a flip-flop resets
  synchronously or asynchronously is the flip-flop's matter, not the
  hooks'.)
- `finish` and `fail` take effect at the end of the current cycle (`fail`
  from a method at once). `on_finish` is always called, and does not say why
  the simulation ended.
- A test's hooks are all called on one thread; tests run in parallel on
  other threads.

Decided:

- `start`, the method the test module calls after registering the
  interfaces (UVM's `run_test`), builds the hierarchy, checks unbound
  handles and polls every task once at time 0.
- The limits on signals at time 0 are accepted, since signals are reached
  through clocking blocks, which drive and sample at
  edges: a drive before the first edge reaches the design after it, as a
  drive at the first edge would, and an input read before the first `tick`
  is undefined. Initial output values are the design's.
- `on_clock` wakes the tasks waiting for the clock that fired and polls the
  ready queue until it is empty. With no task waiting for the clock it
  returns at once, which matters since it is called at every edge. Every
  hook that polls (`start`, `on_clock`, `on_reset`) ends the run phase once
  its ready queue is empty if the test's `run` has returned.
- Resets are tracked from the hooks: an `on_reset` sets the reset
  asserted, and the next `on_clock` sets it released, since the two hooks
  are exclusive and every edge in reset calls `on_reset`. No reset port is
  read, so no polarity is needed. The tasks waiting for assertion or
  release are woken when the state changes. In reset no `on_clock` comes,
  so tasks waiting for clock edges do not wake: the clock stops in reset,
  as Veryl models it. (Confirmed against the simulator by
  `examples/hooks`: no `on_clock` comes in reset, and the release is seen
  at the first `on_clock` after it.)

  A driver initializes its outputs on a reset from its `run`: it waits for
  the assertion (`run_with_reset` runs its work until a reset, then an
  `on_reset` handler, then the work again) and drives the
  idle values at once with the clocking block's `drive` (not at the next
  edge); they reach the design when the hook ends, after the first edge in
  reset.
- At the end of the run phase, within the same hook: drop the remaining
  tasks, run `check` and `report`, then `finish` or `fail` the Veryl test
  (it stops at the end of the cycle).
  - Tasks are dropped in the reverse of the order they were made (as Rust
    drops locals), so a task made later, which may refer to what an earlier
    one holds, goes first.
  - While tasks are being dropped, no task is polled: a `Drop` may push to a
    queue or wake a task (`InFlight` answering its sequence with an abort
    response), but nothing goes on running. The ready queue is discarded
    afterwards.
  - `check` runs after every task is dropped, so it sees what the `Drop`s
    recorded.

  UVM does the same in effect: when the run phase ends, `uvm_phase` kills
  the process under which every `run_phase` was forked (`m_phase_proc.kill()`
  in `uvm_phase.svh`), which kills its descendants, and nothing runs in them
  afterwards. SystemVerilog has no destructors, so UVM has no drop order;
  the order here exists because a Rust `Drop` runs code.
- An `on_finish` before the run phase has ended is an error, "unexpected
  finish": the simulation was ended by something else (the test module's
  `$finish`, its `initial` block ending, another component) while the
  test's `run` was going; `on_finish` cannot tell which. It is fatal:
  `check` is skipped and the test fails.
- Panics are not caught by the framework. veryl-component wraps every hook
  (and the component's drop) in `catch_unwind` and fails the test with the
  panic's message, which already names the file and line. Catching them
  here would only add the component's path, and going on with `check` and
  `report` would work on state the panic left half-updated.
  - A hook left by a panic poisons the executor. Every hook sets a flag on
    entry and clears it on a normal return; a hook that finds it still set
    knows the previous one unwound. The executor is single-threaded, so a
    plain flag is enough: two writes per hook. It sits in the code that
    every hook runs anyway (publishing the `SimCtx` to thread-local storage
    for the clocking blocks, through a guard set on entry).
  - Once poisoned, later hooks (`on_finish` among them) only clear the
    thread-local state (dropping it, with nothing polled): no `check` runs,
    and no further error (an unexpected finish) is reported.
- The timeout is the framework's, as in UVM (`set_timeout`): when it runs
  out before the test's `run` returns, the run phase ends with a fatal
  error. The test module only keeps the clocks running, with a bound well
  beyond the timeout; reaching that bound is an unexpected finish.
- No deadlock detection (every task waiting on something only another task
  could give, none on a clock or a reset): such a test runs into the
  timeout, as in UVM, and the hooks it costs meanwhile return at once. The
  timeout reports the tasks still alive, by the path of the component that
  owns each; what each one waits for is not recorded, which would cost at
  every wait.
- The executor's state lives in thread-local storage, since a Veryl
  component must be `Send` and the testbench uses `Rc`.
  - One testbench root per simulation, as UVM's `uvm_root`: the testbench is
    one hierarchy, and a checker is a component in it, not a second root. A
    second `start` in the same simulation is an error.
  - `start` creates the state afresh and `on_finish` (and drop) clears it,
    since a worker thread runs the next test after this one.
  - Code reaches the executor only through the framework (contexts, clocking
    blocks), so moving the state into the component instance later (for
    several roots) would not change the API.

## Runner

Decided:

- One test per simulation, as in UVM. With Veryl's simulator the test is
  chosen in the Veryl test module, by name, as the argument of `start`:
  `tb.start("RegTest")`. Veryl has no way to run one `#[test]` module in
  several variants or to pass arguments from `veryl test` into the
  simulation (only `--seed` reaches it, and `$test$plusargs` is not
  supported), so each test has a `#[test]` module of its own, kept thin: a
  shared harness module holds the clocks, resets, DUT and testbench and
  their connections; the test module instantiates it and names the test.
  (heliodor's ISA tests are written the same way, one module per program.)
- The name is given as a method argument, not a component parameter, so
  that the harness is the same in every test: `veryl test` builds a
  component that recurs across tests once and reuses it in the others
  (`DutReuseCache`, `ChunkArtifactCache` in the simulator), and the more
  of the hierarchy is identical, the more is reused. To be measured once
  implemented.
- The runner takes the name from the adapter; another simulator's adapter
  may take it from elsewhere (a plusarg).
- Tests are registered with `register_test!`, collected across the crate
  (`inventory` or `linkme`), so there is no
  list to keep by hand:

  ```rust
  register_test!(RegTest);                                                    // the type's own name, no spec
  register_test!(SlowWriteTest, RegTest, SlowWrites);                         // a name, a test type, a spec
  register_test!(SlowErrTest,   RegTest, bundled_specs![SlowWrites, InjectErrors]);
  ```

  - It takes one argument or three. One: a test type, registered under its
    own name with no spec (`()`), for when the name and the type's name are
    the same. Three: the name, the test type and a spec (one spec, or a
    `BundledSpecs`). The framework makes either as described under
    [Tests](#tests).
  - The name is an identifier, and its text is the name `start` takes
    (`tb.start("SlowWriteTest")`). The macro also emits an item named from
    it (`__TBF_TEST_SlowWriteTest`, so as not to clash with the type in the
    one-argument form), so registering a name twice in one module does not
    compile. Names are CamelCase, as the one-argument form makes them type
    names.
  - A test type implements `Test`, which marks a component that can be the
    root and requires its `run`.
  - `start` fails on a name registered twice (across modules, which the
    compiler does not see) and on an unknown name, listing the registered
    names.
- The test's seed comes from Veryl: the component's `BuildCtx::seed()`,
  which Veryl derives from the run's seed (`veryl test --seed`, else
  `[test].seed`, else random; logged as "Test seed: .. (reproduce with
  --seed)"), the test's name and the instance's name. It is UVM's test seed:
  every component's and sequence's generator is seeded from it as described
  under random stability, so `--seed` reproduces a run and each test draws
  differently. The framework has no seed option of its own.

- The result follows UVM: the test passes when it has no errors
  (`context.error`) and no fatal error; warnings do not count. A pass
  calls `finish`; a fail calls `fail` with a summary (the counts and the
  first error's message), which Veryl shows as the test's result.
- After `report`, a summary is logged, as UVM's report summary: the test's
  name, the seed, the counts of infos, warnings, errors and fatal errors,
  the end time, and for a fatal error its component's path and message.
- An error limit, as UVM's `max_quit_count`: when the errors reach it, the
  run phase ends with a fatal error (`check` is skipped) naming the limit
  and the last error. Unlimited by default.
- The timeout (see [Executor](#executor)) is in Veryl's simulation time, the
  unit of `ctx.time()`, as UVM's is in time: it needs no choice of clock
  when there are several, and checking it is one comparison at each hook.
  A timeout in cycles is the test module's multiplication. Unlimited by
  default: the framework cannot know a design's scale, and UVM's default
  (9200 s) means unlimited too. Without one, the test module's bound on its
  clock ends the simulation as an unexpected finish, which, like a timeout,
  lists the tasks still alive by their components' paths.
- Settings are given where UVM gives plusargs, since Veryl passes no
  arguments into a simulation: the Veryl test module (the shared harness)
  calls methods before `start`, as often as needed. The framework's
  options and the testbench's own settings are apart, as UVM's `+UVM_*`
  plusargs are from a testbench's own:

  ```veryl
  initial {
      tb.option("timeout", 200000);        // the framework's: checked at once
      tb.option("max_errors", 10);
      tb.set("num_masters", N_MASTERS);    // the testbench's: read by the test's setup
      tb.start("RegTest");
      clk.next(1000000);
      $finish();
  }
  ```

  - A method argument reaches the component as a `Value`: a string literal
    as a string, any other expression (a number, `1'b1`, an enum value, a
    variable, a Veryl parameter) as bits of its width. Strings can only be
    literals.
  - `option(key, value)`: `core` declares each option with its type
    (`timeout`, `max_errors`) and converts the value: integers and `bool`
    from bits (failing if they do not fit), strings from strings, an enum
    from its name or its value. An unknown option or a mismatched value
    fails the call, which fails the Veryl test at once, listing the options
    and their types.
  - `set(key, value)`: the testbench's settings from the design, kept as
    given until the test's `setup` reads them typed:
    `cx.get::<usize>("num_masters")?` (failing if missing or mismatched) or
    `cx.get_or("num_masters", 1)?`. A key no one has read once `setup`
    returns is an error, so a misspelt key is not ignored. The two never
    share a key space, so a testbench may name a setting as it likes.
  - The runtime's exported component has the two generic methods and hands
    their keys and values to `core`, so the runtime needs no knowledge of
    either.
  - To check when implemented: the width an unsized literal (`200000`) is
    passed with; values beyond 32 bits may need a sized literal (`64'd..`).
  - Every value from the design, a bus's width included, comes through
    `set`, so that values have one source rather than several. An interface
    found with `scope.interface(..)` is not read for values.
  - Values are assigned on the Veryl side, so Veryl is their source; one
    value per `set` is the way until Veryl can hand its types to Rust (see
    [TODO](#todo)).

  Only this is implemented at first. Setting the options from the
  testbench's code (in a test's `setup` or a spec's `pre_build`, UVM's
  `set_timeout` and `set_report_max_quit_count`), overridden by the test
  module's, is added when needed.

## Crate layout

Decided: the event handling and the UVM-like methodology are separate crates
(as cocotb is apart from pyuvm). The methodology depends on the runtime,
never the other way, so the runtime's per-edge path is kept apart from the
methodology's needs, and the runtime alone serves small tests that need no
hierarchy.

| Crate | Contents |
|---|---|
| `veryl-tbf` | the entry point users depend on: re-exports the others, with a prelude; `core` can be left out by a feature |
| `veryl-tbf-runtime` | the executor, tasks and their concurrency (`fork_scope`, `join`, `first`), waits for clocks and resets (`run_with_reset`), the shared parts (`Fifo`, events), clocking blocks, the simulator boundary and Veryl's adapter, the timeout, unexpected finishes, panic detection |
| `veryl-tbf-core` | the UVM-like part: `Component`, `Test`, `Instance`, `Scope`, the phases, `Knob`, `Callbacks`, TLM, items (`ItemHandle`), sequences and sequencers, agents' and monitors' helpers, `ItemWaiter`, `Spec` / `BundledSpecs`, the runner (`register_test!`, test selection, reporting, the error limit) |
| `veryl-tbf-macros` | the proc macros of both (`#[clocking]`, `#[modport]`, the wait macros; `#[component(..)]`, `#[sequence_item]`, `#[sequence_base]`, `#[sequence]`, `#[sequencer]`, `register_test!`, `bundled_specs!`) |
| `veryl-tbf-random` | constrained random; depends on nothing else here, so it can be used on its own |

What each side holds where they meet:

| | `runtime` | `core` |
|---|---|---|
| End of the run phase | runs a main task; when it ends, drops the others and calls an end handler | gives the test's `run` as the main task; its end handler runs `check` and `report` |
| Fatal errors | stops everything on a task's `Err` | adds the component's path |
| Errors and the result | | counts them and reports the result |
| Seeds | passes on the seed from Veryl | derives every component's and sequence's |
| Logging | a sink | paths, severities, counts |
| The entry point | the macro that exports the Veryl component takes a function to call at `start`, and passes `option` and `set` on | gives the function that looks the test up by name and builds it, the options it accepts, and the settings for `setup` |

Static dispatch across the crates is kept with generics and `#[inline]`, so
the split costs nothing per edge.

## Implementation notes

What the implementation settled where the design left room, and where it
stops short for now:

- The runtime's fork scope is `ForkScope` (`fork_scope(async |scope| ..)`),
  apart from the methodology's `Scope`.
- A component's kind and passiveness are, for now, the constants
  `Component::KIND` and `Component::PASSIVE`; the `#[component(..)]`
  attribute (kinds, `passive`, the agent's generation, a test required to
  have a `run`) comes with step 4. A passive component's `run`, if written,
  is not run.
- The test is the root: its path is empty, its children's paths have no
  prefix (`env.driver`), and messages name it by the name it was registered
  under. The hierarchy is printed at the debug level when the test starts.
- `Instance<T>` is not `Clone`, so `DerefMut` through it is unique.
- Each component's run task borrows the component and yields once before
  running it, so every component is borrowed before any user code runs.
- Reaching the error limit asks the runtime to end the run phase
  (`host::fatal`); it ends once the task that reported the error yields.
- The report summary does not show the seed yet; seeds come with
  constrained random (step 5).

## Taken from UVM

Decided, beyond what the chapters above settle (the component tree, the
phases, TLM, sequences and sequencers, callbacks):

| UVM | Here |
|---|---|
| `uvm_object`'s field automation | Rust's own: `Clone` (copy), `Debug` (print), `PartialEq` (compare); bit packing by a derive. A compare that reports where items differ is added when needed |
| `uvm_sequence_item` | a trait for items, with the transaction id responses are matched by |
| `uvm_event` | the runtime's events (`trigger`, `wait`, carrying data) |
| `uvm_env`, `uvm_agent`, `uvm_driver`, `uvm_monitor`, `uvm_scoreboard`, .. | no types (in UVM they are nearly empty subclasses that name a role); a marker in the component attribute instead: `#[component(monitor)]`, `#[component(scoreboard, passive)]`, one attribute so that `test` does not clash with Rust's `#[test]`. Kinds: `test`, `env`, `agent`, `driver`, `monitor`, `sequencer`, `scoreboard`, `subscriber`. The kind shows in the hierarchy printout and in logs and can be queried; checks by kind (a monitor driving nothing) may come later |
| verbosity | log levels with a threshold (see [Components](#components)) |
| report summary, `max_quit_count` | as in UVM (see [Runner](#runner)) |

Not taken:

| UVM | Why |
|---|---|
| factory, `uvm_config_db` / `uvm_resource_db`, objections | settled above: callbacks and specs, typed configuration, the test's `run` |
| `uvm_cmdline_processor` | Veryl passes no arguments; `option` / `set` instead |
| `uvm_heartbeat` | the timeout, which lists the tasks still alive |
| `uvm_event_pool` | a global pool looked up by strings; events are handed over as handles |
| `uvm_pool`, `uvm_queue` | Rust's collections |
| `uvm_report_catcher` | an injected error is expected by the checker, not demoted after it is reported: a spec that injects an error through a driver's callback tells the scoreboard, through the scoreboard's callback, what to expect. Silencing parts that cannot be changed is looked at if a need appears |
| `uvm_push_driver` / `uvm_push_sequencer`, `uvm_random_stimulus` | hardly used |
| TLM2 (sockets, generic payload) | no use here (SystemC links) |

Later:

- `` `uvm_do ``-like macros (`start_item!`, `finish_item!`, `send_item!`,
  hiding `.await?`).
- The register model (RAL): a crate of its own (`veryl-tbf-ral`), designed
  separately; models could come from rggen.
- `uvm_barrier`; TLM1 put / get / peek ports and `uvm_tlm_fifo`;
  `grab` / `lock`; the response handler; `uvm_sequence_library`;
  `uvm_in_order_comparator` / `uvm_algorithmic_comparator` as scoreboard
  templates; severity actions; transaction recording (perhaps through
  Veryl's trace variables); end_of_elaboration and the other phases.

### Extensions from tue

tue (the UVM extension library that the tvip VIPs are built on) adds to UVM
what VIPs keep writing; veryl-tbf takes these in:

| tue | Here |
|---|---|
| configuration and status on every component, sequence and item (`set_context`), passed to items and sequences through a proxy on the sequencer | `InstanceContext<C, S>`; sequences and items reach it typed through the sequencer's handle, so no proxy |
| `tue_test::create_configuration` | `Test::setup` |
| `tue_param_agent` (an agent over its monitor, sequencer and driver types: makes them as the agent is active or passive, connects them, optionally keeps a sequencer when passive) | the component attribute takes the parts and generates the agent into the VIP's own struct: `#[component(agent(monitor = ApbMonitor, sequencer = ApbSequencer, driver = ApbDriver))] pub struct ApbAgent { .. }` (the parts nested under `agent`, so that `monitor` as a part is not read as the `monitor` kind). It generates the parts' fields, the `build` (making the driver when active, the sequencer when active or asked for when passive, and the connections) and the usual accessors (`sequencer()`, `analysis_port()`); the VIP may add fields (a coverage collector) and methods (`driver_callbacks()`) to the same struct. Active or passive comes from an `AgentConfig` trait on the configuration, which hand-written agents use too. A generic `Agent<M, S, D>` type was set aside: methods cannot be added to another crate's type, and a VIP would have had to rewrite the agent as soon as it left the standard shape |
| `tue_reactive_agent` / `_monitor` / `_sequencer` / `_sequence` (the monitor sends requests to the sequencer, a reactive sequence takes them with `get_request` and answers) | the same, for slaves and responders: `#[component(agent(reactive, ..))]` connects the monitor's request port to the sequencer's request FIFO, and a sequence on it waits with `self.sequencer().get_request().await`. A responding sequence suits being the default sequence |
| `tue_param_monitor` (`item_port`; `create_item` begins a transaction, `write_item` ends and publishes it) | helpers for monitors: the analysis port and the begin / end marking of an item |
| item begin / end events (`wait_for_beginning`, `wait_for_ending`) | items have begin and end events, so that a sequence can wait for the transfer to finish, apart from its response. An event is made when first asked for, as UVM's event pool makes one on its first `get("end")`, so only an item someone waits on allocates one; a driver or monitor marks begin and end on any item (`item.mark_end()`), a no-op when no one asked. Since items are shared (see [Items](#items)), a mark reaches every holder: the driver's reaches the sequence, and a monitor's reaches a reactive sequence that took the request |
| `tue_item_waiter` (waits for the next item, or one matching a key) | `ItemWaiter<T, K>`, a shared object subscribed to an analysis port: `next().await`, `by_key(k).await`, from a test's `run` |
| `tue_fifo` (a bounded mailbox: `put`, `get`, `peek`, `try_*`) | the runtime's `Fifo`, with a capacity, `peek` and the `try_` forms |
| `tue_do` / `tue_fork_do` macros | with the `` `uvm_do ``-like macros, later, the fork form included |
| the register model extensions (`tue_reg*`) | with the register model, later |
| plusarg macros | not taken: `option` / `set` |
| automatic phase objection for UVM before 1.2 | not taken: no objections |
| `tue_id_manager`, `tue_sequence_item_dispatcher`, `tue_address_allocator` | parts for VIPs, not the framework (see [TODO](#todo)) |

## Open questions

None at present; deferred work is under [TODO](#todo).

## TODO

### veryl-tbf

- Drain time: a wait between the test's `run` returning and the end of the
  run phase (UVM's `set_drain_time`), in time or in clock edges. Not needed
  yet. (The case it solves: a monitor publishing the last transfer at the
  edge after the sequence has its response.)

- Choosing the test at run time (`+UVM_TESTNAME`), once Veryl passes
  arguments into a simulation (see below): the runner would take the test's
  name, and the options, from them.

The UVM features left for later are listed under
[Taken from UVM](#taken-from-uvm).

### For the VIPs

Parts that belong to a common library for VIPs (as tvip's), not to the
framework:

- An ID manager (tue's `tue_id_manager`): hands out IDs and makes a caller
  wait while all are in use, until one is released (AXI's IDs).
- A sequence item dispatcher (`tue_sequence_item_dispatcher`): a sequence
  that sends each item to the sequencer it selects for it (by address, for
  example), among sequencers of one type (`Vec<SequencerHandle<T>>`).
- An address allocator (`tue_address_allocator`): draws address ranges that
  do not overlap those already allocated, on constrained random.

### Proposals to Veryl

- Arguments into a simulation, so that a test can be chosen at run time,
  like UVM's `+UVM_TESTNAME`, and one `#[test]` module can run any test.
  Veryl would need a `veryl test` option for them (or per-test arguments in
  `Veryl.toml`) and an API for a component to read them. A file the
  testbench reads is no substitute today: reads resolve against the project
  root, shared by every test (only writes go to a per-test directory,
  `target/veryl-components/out/<test>`), and a component cannot learn its
  test's name.

- Clocking blocks as a language feature. The timing is
  there already (hooks read values before the edge and writes reach the
  design at it); what is missing is the declaration. What veryl-tbf would
  need of it:
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
  - emitted as SystemVerilog `clocking` blocks, which a DPI bridge builds on;
  - usable in Veryl test modules too (`cb.next()`), not only by components.

  With it, veryl-tbf would no longer have users write `#[modport]` /
  `#[clocking]` structs by hand (they would be generated from, or checked
  against, the declaration), and `tick` would know its clock; the value
  conversions and the wait macros stay in Rust. Until then the Rust-side
  `#[clocking]` is used, its binding (where the members and the clock come
  from) kept apart so that it can be replaced.

- Rust bindings for Veryl's types. Settings are assigned
  in Veryl and the design's types (structs, enums, package parameters) are
  defined there, so Veryl is the source and Rust follows: `veryl build`
  would emit Rust types for them, with their bit layout, as it emits
  SystemVerilog. Then a test module could pass a whole configuration struct
  (`tb.set("config", cfg)`, decoded by the generated type, no layout kept
  in step by hand), and a clocking block could hold a struct-typed modport
  member (an AXI channel) as a Rust struct. Until then settings are set one
  value at a time. (Veryl already takes a component's interface from Rust,
  through its manifest of ports, parameters and methods; no types travel
  either way.)

- A hook on reset release (`on_reset_release`, the pair of `on_reset`).
  Today a release calls no hook and is seen only at the next `on_clock`.
  With the hook, a release would be known when it happens, values to drive
  at the release could be written then, rather than at the first edge
  after it.

- `%m`, the hierarchical name of the current scope, as a
  format specifier, as in SystemVerilog. A setting given per instance is
  best keyed by the HDL hierarchy (each module of the harness sets its own:
  `u_apb[0].data_width`). With `%m` among the format specifiers (`$display`
  and the like), a key can be written as `"%m.data_width"`. For it to reach
  a component method as a string, either the string argument of a method
  call is formatted before the call (`tb.set("%m.data_width", W)`), or a
  formatted string (an `$sformatf` equivalent) can be passed as an argument
  (today only string literals arrive as strings; other expressions arrive
  as bits).
