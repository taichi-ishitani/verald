# Verald design notes

A UVM-style verification framework in Rust for Veryl. This file records the
decisions made so far, the reasons behind them, and what is still open.

Status: design in progress. Implemented: the runtime's core (step 1),
clocking blocks with `run_with_reset` (step 2), the methodology's skeleton
(step 3: components, phases, tests, specs and the runner), and TLM,
sequences, sequencers and agents (step 4), test scenarios, and constrained
random with seeds per component and sequence (step 5; `examples/regblock`
puts them together), and tests written as functions calling a VIP's
methods as tasks (`#[testcase]`, proxies). Notes
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
- [The name](#the-name)
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
  receiver's `Subscriber<T>` by item type. How far to check at compile
  time is a matter of degree: a check is made there when what it costs
  (more types, a more involved API, more for users to write) is small,
  and left to run time, with a clear error, when it is not.
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
  [veryl-proposals.md](veryl-proposals.md)) rather than duplicated in Rust.

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
- The code on veryl-component's side of this boundary is a layer of its
  own, shared with the assertions: see
  [host-layer-design.md](host-layer-design.md).

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
  | `run` | every component, concurrently | `async fn run(&mut self, run_context: &mut RunContext) -> Result<(), TestError>` |
  | `check` | checks what the run left | `fn check(&mut self, check_context: &mut CheckContext)`, errors reported to `check_context` |
  | `report` | summarizes | `fn report(&self, report_context: &ReportContext)` |

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

- Any component (a sequencer too) may hold its context, as tue gives every
  component one: the field marked `#[context]`, of type
  `InstanceContext<C, S>`, for which `#[component(..)]` on the struct
  implement `HasContext` (`Config`, `Status`, `context()`), so it is reached
  the same way everywhere: `driver.context().config()`,
  `self.sequencer().context().config()` in a sequence. A component with no
  such field has none (a scoreboard, a virtual sequencer). A sequencer
  holding only its context is built from it, its `build` generated:
  `scope.create_instance::<ApbSequencer>("sequencer", context.clone())?`;
  one with other fields names its `Context` in the attribute.
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
      fn pre_drive(&self, _item: &mut ApbItem, _callback_context: &mut CallbackContext) -> Action { Action::Go }
      /// Cycles to wait before driving.
      fn delay(&self, _item: &ApbItem, _callback_context: &mut CallbackContext) -> u32 { 0 }
      /// After the response.
      fn post_drive(&self, _item: &ApbItem, _rsp: &ApbResponse, _callback_context: &mut CallbackContext) {}
  }

  pub struct ApbConfig {
      pub driver_callbacks: Callbacks<dyn ApbDriverCallbacks>,
      ..
  }

  // the driver's run, at each point
  let delay = callbacks.sum(|c| c.delay(&item, &mut callback_context));

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
    fn setup(setup_context: &mut SetupContext) -> Result<Self::Context, TestError>;
}

pub trait Spec<T: Test> {
    fn pre_build(&self, _config: &mut T::Context) {}  // after setup, before build: settings, callbacks
    fn post_build(&self, _test: &mut T) {}            // after build, before run: typed adjustments
}
impl<T: Test> Spec<T> for () {}                                   // no spec
pub struct BundledSpecs<T: Test> { specs: Vec<Box<dyn Spec<T>>> } // applied in the order given
impl<T: Test> Spec<T> for BundledSpecs<T> { .. }

// the framework's new for a test, at start
let mut config = T::setup(&mut setup_context)?;
spec.pre_build(&mut config);
let mut test = scope.create_root::<T>(config)?;          // T::build
spec.post_build(&mut test);

// a test: setup and build, no spec in sight
#[component(test)]
impl Component for RegTest {
    type Context = RegTestConfig;
    fn setup(setup_context: &mut SetupContext) -> Result<RegTestConfig, TestError> {
        Ok(RegTestConfig::new(setup_context))
    }
    fn build(scope: &Scope, config: RegTestConfig) -> Result<Self, TestError> {
        let env = scope.create_instance::<Env>("env", config.env)?;
        Ok(RegTest { env })
    }
    async fn run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> { .. }
}
```

A test is written in one impl, in the order it runs: `#[component(test)]`
on `impl Component` moves the test's own steps (`setup`, and `pre_run` /
`post_run` below) to a generated `impl Test`, so `setup`, `build`,
`pre_run`, `run`, `post_run` and `check` read top to bottom. The macro
moves the items as they are (their spans kept, so errors point at the
user's code); a test whose impl has `pre_run` or `post_run` needs its
`setup` there too. Writing `impl Test` apart, by hand, still works.

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
module passed with `set` (`setup_context.get`, see [Runner](#runner)), random numbers
(from the test's seed) and logging.

The hierarchy is built in one place, so it is the same for every test.

### The run of a test, and scenarios

Decided: a test's run is three steps, the common preparation, the scenario
and the common wind-up, each of which may take time:

```rust
pub trait Test: Component {
    fn setup(setup_context: &mut SetupContext) -> Result<Self::Context, TestError>;
    /// Before `run`: wait for the reset, set the DUT up.
    async fn pre_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> { Ok(()) }
    /// After `run`: wait for the bus to go idle, read the status.
    async fn post_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> { Ok(()) }
}
```

The framework calls `pre_run`, `run` (`Component::run`) and `post_run` in
turn in the test's task, and the run phase ends when `post_run` returns; an
`Err` stops there (as a fatal error stops UVM's later phases). They are on
`Test`, not `Component`: only the test's run is split (written in the
test's one impl all the same, as above). Checks that take no
time stay in `check`. (`setup` was taken by `Test::setup`; the names follow
`Spec`'s `pre_build` / `post_build`.)

A simple test writes its `run` (and `pre_run` / `post_run` as it needs).
Tests that share their hierarchy and their preparation and differ in what
they do are made from a base test and scenarios: a scenario is a type that
implements `Scenario`, run on the base test, which plays the role of UVM's
virtual sequencer (a virtual sequence reaching every sequencer through
`p_sequencer`) with nothing to copy:

```rust
pub trait Scenario: Default + 'static {
    /// The test it runs on.
    type Test: Test;
    async fn body(&mut self, test: &mut Self::Test, run_context: &mut RunContext)
        -> Result<(), TestError>;
}

#[derive(Default)]
pub struct WriteReadAll {}

impl Scenario for WriteReadAll {
    type Test = RegTest;
    async fn body(&mut self, test: &mut RegTest, _run_context: &mut RunContext)
        -> Result<(), TestError> {
        WriteReadAllSeq::new(test.rounds).start(&test.sequencer).await
    }
}

// a test made of the base and a scenario for some of its steps
#[component(test(base = RegTest, run = WriteReadAll))]
pub struct WriteReadAllTest {}

register_test!(WriteReadAllTest);
```

`#[component(test(base = B, pre_run = P, run = R, post_run = Q))]` (each
step optional) generates the test: a field `base` holding the base test
(and `Deref` / `DerefMut` to it), and a `Component` and `Test` that forward
`setup`, `build`, `check` and `report` to the base, and for each step call
the scenario given for it (made with `Default`, given `&mut` the base) or
else the base's own. A step given alone (`run`, without `= ..`) is the
test's own method of that name, written in a plain `impl` (UVM's derived
test writing its `run_phase`); it reaches the base's fields through
`Deref`, and the base's step through `self.base`:

```rust
#[component(test(base = RegTest, run))]
pub struct BasicTest {}

impl BasicTest {
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        WriteReadAll::new(self.rounds).start(&self.sequencer).await
    }
}
```

| A step in the attribute | What runs |
|---|---|
| left out | the base's |
| `run = S` | the scenario `S` |
| `run` | the test's own `run` method |

The steps are named in the attribute, as an agent's `post_build`, since the
attribute generates the whole `Component` and `Test`: an `impl Component`
of the user's would clash with them. The base keeps its place in the hierarchy (its
children are the test's), and a generated test may itself be a base. The
struct's own fields start at their defaults.

A test meant only as a base, as UVM's `base_test` (a `virtual class`), is
marked `#[component(test, base)]` on its struct and its impl. It needs no
`run` (the tests made from it give one; one they do not runs nothing), and
it cannot be registered: `register_test!` on it does not compile, naming
it. (`virtual` and `abstract` are Rust keywords; `base` pairs with
`test(base = ..)`.)

```rust
#[component(test, base)]
impl Component for RegTest {
    type Context = RegTestConfig;
    fn setup(..) { .. }
    fn build(..) { .. }
    async fn pre_run(..) { .. }       // wait for the reset
    async fn post_run(..) { .. }      // wind up
}

#[component(test(base = RegTest, run = ReverseScenario))]
pub struct ReverseTest {}

register_test!(ReverseTest);
```

- A spec is written for the base test (`Spec<RegTest>`) and serves every
  test made from it: a generated test applies its spec to the base, and
  takes the base's configuration. What a test's specs are written for is
  told by a hidden trait that `#[component(test)]` on the struct
  implements, so a registered test needs the attribute on its struct (a
  test without it is refused at `register_test!`, by name): Rust has no way
  to give every test a default and let a generated test override it.
- A scenario is made with `Default` and reads its values from the test in
  `body`; given for several steps, it is made once for each.
- The trait is `Scenario`, its method `body`, as a sequence's: a scenario
  is not a `Sequence` (no sequencer arbitrates it), and `run` would repeat
  the step's name.

### Tests as functions

Decided: a test whose run is a scenario written straight through, as a
testbench before UVM wrote one in an `initial` block calling tasks, is a
function marked `#[testcase]`, made from a base test:

```rust
#[testcase(base = RegTest)]                // registered as `Basic`
async fn basic(test: &mut RegTest, run_context: &mut RunContext) -> Result<(), TestError> {
    let mut bus = BusBfm::new(&test.sequencer);  // the VIP's proxy (see Sequences)
    bus.write(0, 0x1234).await?;
    let data = bus.read(0).await?;
    if data != 0x1234 {
        run_context.error(format!("read {data:#06x}"));
    }
    WriteReadAll::new(2).start(&test.sequencer).await
}
```

- It stands for a `Scenario` calling the function, a test made from the
  base with that scenario as its `run` (`#[component(test(base = B, run =
  S))]`), and `register_test!`: nothing new runs underneath, and the
  hierarchy, the configuration, `pre_run` / `post_run`, `check` and
  `report` are the base's. Every part of the VIP (the monitor, the
  scoreboard, callbacks, the sequencer's arbitration) works as in any test.
- The test is registered under the function's name in CamelCase
  (`basic_write` as `BasicWrite`), the name of the struct generated for it;
  `name = ..` gives another. `spec = ..` registers it with a spec (one, or a
  `BundledSpecs`).
- The function takes the base test and the run context, as a scenario's
  `body`, and returns what `run` returns.
- An attribute of its own, not `#[component(..)]`: a function is not a
  component's struct or impl, whatever it is made into. `testcase`, not
  `test`, which is Rust's own.
- Why: a VIP built on Verald is to serve testbenches written in the
  manner before UVM, a scenario of task calls, as well. Such a scenario
  written in Rust on the VIP's whole agent keeps everything the VIP does,
  and needs nothing of Veryl; one written in Veryl (task calls from an
  `initial` block, through methods of the component) would need tickets
  polled each cycle, a root per component instance and blocking methods
  from Veryl, and is not taken up.

## Sequences

Decided: a VIP is controlled through sequences, as in UVM. Where it is
useful, a VIP also gives an API, methods made of items and sequences,
which its sequences use and which it publishes for tests to call (see
[A protocol's methods from a test](#a-protocols-methods-from-a-test)).
Sequences have three layers, as UVM's `uvm_sequence_base`, a protocol's
sequence base (where the API is), and the sequences that generate items.

| Layer | Fields | Methods | Written by |
|---|---|---|---|
| 1. sequence core | the framework's, inserted by `#[sequence]`: the sequencer, the parent sequence, ids, priority, the random generator | `SequenceCore`: `start` (a sequence with a body only), `start_item`, `finish_item`, `sequencer` | the framework (`#[sequence]` implements it) |
| 2. protocol base (optional) | a plain struct of the VIP's (`Default`), inserted by `#[sequence(base = ..)]`, reached through `HasBase<T>` | the VIP's API: a trait of the VIP's over `SequenceCore + HasBase<T>`, with a blanket impl | the VIP's author, with no macro |
| 3. sequence | its author's | `Sequence`: `body` | the sequence's author |

```rust
// the VIP: fields every APB sequence has, and methods over them
#[derive(Default)]
pub struct ApbSeqBase {
    pub prot: u8,
}

pub trait ApbSequence: SequenceCore<Sequencer = ApbSequencer> + HasBase<ApbSeqBase> {
    async fn write(&mut self, addr: u32, data: u32) -> Result<ItemHandle<ApbItem>, TestError> {
        let mut item = ApbItem { write: true, addr, data, prot: self.base().prot };
        self.start_item(&mut item).await?;
        self.finish_item(item).await
    }
}
impl<S> ApbSequence for S where S: SequenceCore<Sequencer = ApbSequencer> + HasBase<ApbSeqBase> {}

// a sequence
#[sequence(sequencer = ApbSequencer, base = ApbSeqBase)]
pub struct RandomWrites { count: usize }

impl Sequence for RandomWrites {
    async fn body(&mut self) -> Result<(), TestError> {
        for i in 0..self.count {
            let mut item = ApbItem::default();
            self.start_item(&mut item).await?;
            self.randomize_item(&mut item)?;        // under the sequencer's configuration
            self.finish_item(item).await?;
        }
        self.write(0x00, 0x1234).await?;          // layer 2
        Ok(())
    }
}
```

- The user writes `Sequence` (`body`) only; the core, `SequenceCore`, is the
  framework's and `#[sequence]` implements it, so the author's `impl
  Sequence` and the macro's impl do not meet. The names follow UVM's
  `uvm_sequence` and `uvm_sequence_base`.
- The core is what a sequence is to its sequencer (an id, a priority) and
  sending items; it does not require a body. `start` runs the body, so it
  is bounded by `where Self: Sequence`: a sequence with no body (a proxy,
  see [A protocol's methods from a test](#a-protocols-methods-from-a-test))
  has the core and the protocol's methods written over it, and starting
  it does not compile. (A trait of its own for `start` was tried first;
  the bound on the method does the same with one trait fewer. A check at
  run time, a body the macro gives a proxy that fails, would need no
  change to the core at all; the check is made at compile time since it
  costs only a bound on three methods, as [Principles](#principles) weigh
  it.)
- The protocol layer is a plain struct and a trait: VIP authors write no
  macro. A method of it may send items (`start_item` / `finish_item`) or
  start the VIP's sequences (`start_from(self.sequencer_handle(), self)`,
  passing on the priority). More layers nest: a base struct holds the one below it, and the
  trait above reaches it through it.
- `start` belongs to the framework, so it sets the sequencer (UVM's
  `m_sequencer`) before `body`; every layer sees the same sequencer.
- The macro generates `new(..)` over the author's fields, which then need
  not be `Default`; the inserted fields start at their defaults.
- A protocol's configuration is reached through the sequencer's context
  (see below).

### Items

Decided: items are shared once sent, as UVM passes one object by handle
between a sequence, its driver and the monitors' subscribers:

- An item is a plain struct of the user's (`#[derive(Clone, Debug)]`); what
  the framework records about it (its sequence and transaction ids, its
  begin and end) lives in the handle, so items need no macro.
  `#[sequence_item]` only adds the alias of its handle,
  `type BusItemHandle = ItemHandle<BusItem>` (`<Name>Handle`, or
  `#[sequence_item(handle = Name)]`), as visible as the item, for the
  ports and subscribers that name it (`AnalysisPort<BusItemHandle>`),
  leaving the struct as it is. It is an attribute, not a derive, as the
  framework's other marks are (`#[component]`, `#[sequence]`,
  `#[clocking]`, `#[randomize]`), though a derive is Rust's usual form
  for what changes nothing: one form for the framework's marks reads
  better beside them. It is named for what it marks, as UVM's `uvm_sequence_item` covers both a
  sequence's items and a monitor's transactions, and does that only. UVM's
  `uvm_object_utils` (printing, comparing, copying) is Rust's `Debug`,
  `PartialEq` and `Clone`; something all objects share (configurations
  and sequences too), if it comes, is a derive of its own, not this one.
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

- An item's default constraints (declared on its type, applied at every
  randomization) may refer to a configuration without the item holding it:
  the type declares the context its constraints read
  (`#[randomize(context = ApbConfig)]`), the constraints read it as
  `context.x` (a constant, as a set's `self.x`), and the context is an
  argument of the randomization (`item.randomize(&cfg)?`,
  `item.randomize_with(Set, &cfg)?`, `randomize_with!(item, &cfg, { .. })?`).
  Within a sequence the framework passes it: `self.randomize_item(&mut
  item)?` and `self.randomize_item_with(&mut item, Set)?` take the
  sequencer's configuration (its `#[context]`'s), matched by type at
  compile time, as tue's `set_context` gives an item its configuration,
  but explicit: the call shows which configuration it was randomized
  under, and an item never keeps a stale one; `self.config()` gives it for
  an inline randomization (`randomize_with!(item, &self.config(), { ..
  })`). The methods are `randomize_item`, not `randomize`, so that a
  sequence that is itself random (`#[randomize]` on it, `seq.randomize()`)
  keeps both. An item with no context randomizes with no argument (and is
  randomized directly, `item.randomize()?`). A nested random object with a
  context gets its parent's; one whose parent has none fails the
  randomization (`Invalid`). The constraint syntax is the random crate's
  ([random.md](random.md)).
- The contents are reached with `borrow()` / `borrow_mut()` (a `RefCell`
  inside), the names `Instance<T>` uses during the run, not `read` / `write`,
  which would read as bus operations. A borrow must not be held across an
  `.await`: another holder's borrow would then panic (found at run time,
  never undefined behaviour). Clippy's `await_holding_refcell_ref` lint,
  off by default, catches it at compile time and is recommended.

### Sequencers and virtual sequences

A sequencer is a component in the hierarchy, as in UVM, and its type is the
user's: `#[component(sequencer(item = ..))]` on a struct generates a whole
sequencer around fields of the user's own: the core (item queue,
arbitration, default sequence), `Sequencer`, and its `Component`, built from
its fields (one field: that field's value; several: a tuple, in order).
`response = ..` gives a response type other than the item's. A virtual
sequencer is the same thing with sub-sequencers among its fields; it is made
in the env like any other component. A sequencer is passive (no run task),
so its fields stay reachable during the run phase; `instance.handle()` gives
the `SequencerHandle` sequences start on and a driver's port connects to.

```rust
#[component(sequencer(item = ApbItem))]
pub struct ApbSequencer {
    #[context]
    context: InstanceContext<ApbConfig>,            // what sequences on it may need
}

#[component(sequencer(item = ()))]                  // virtual: no driver behind it
pub struct SocSequencer {
    pub apb: SequencerHandle<ApbSequencer>,
    pub axi: SequencerHandle<AxiSequencer>,
}

// the env's build (SocSequencer's Context is its two sub-sequencer handles)
let apb = scope.create_instance::<ApbAgent>("apb", apb_context)?;
let axi = scope.create_instance::<AxiAgent>("axi", axi_context)?;
let vseqr = scope.create_instance::<SocSequencer>("vseqr", (apb.sequencer(), axi.sequencer()))?;
// the agent's build: the driver's port to its sequencer
driver.port.connect(&sequencer.handle());
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
  #[component(sequencer(item = ()))]
  pub struct MultiApbSequencer {                // its Context is its one field: filled after it is made
      pub apb: Vec<SequencerHandle<ApbSequencer>>,
  }

  let mut vseqr = scope.create_instance::<MultiApbSequencer>("vseqr", Vec::new())?;
  for agent in &apb {
      vseqr.apb.push(agent.sequencer());
  }
  ```

### Default sequence

Decided: as in UVM and OVM, a sequencer runs a default sequence in the run
phase when one is set, through its configuration or through its handle:

```rust
// the VIP: the configuration carries one, the sequencer names the field
pub struct ApbConfig {
    pub default_sequence: DefaultSequence<ApbSequencer>,
    ..
}

#[component(sequencer(item = ApbItem, default_sequence = default_sequence))]
pub struct ApbSequencer {
    #[context]
    context: ApbContext,
}

// a test's setup or a spec's pre_build, before the build
config.env.apb.default_sequence = DefaultSequence::new(|| RandomWrites::new(10));

// or a spec's post_build, through the handle the env keeps
test.env.apb.set_default_sequence(|| RandomWrites::new(10));
```

- Both are typed: the sequence must run on that sequencer's type, so
  another does not compile.
- `DefaultSequence<S>` holds how to make the sequence, or nothing
  (`Default`). The sequencer takes it from its configuration (through its
  `#[context]`) when it is made; the handle's `set_default_sequence` after
  that replaces it. So the later wins: the VIP's default (in its
  configuration's `Default`, as a slave's responder), then the test's
  configuration, then a handle in `post_build`. One sequence runs, as in
  UVM, where the last setting of `default_sequence` wins.
- Sequencers sharing a configuration each make their own sequence: the
  function is `Fn` and is called per sequencer.
- The framework calls the function when the run phase starts and starts
  the sequence on the sequencer, as a task of its own; a sequencer's author
  writes the field and the attribute's `default_sequence = field` only.
- A default sequence runs in the background: the run phase ends with the
  test's `run` (its `post_run`), not with it, and it is dropped with the
  other `run`s. It suits traffic that only accompanies the scenario (idle
  traffic, a responder). UVM 1.2's automatic phase objection is not
  needed: a sequence the test waits for is started from the test's `run`.
- There is one default sequence per sequencer, for the run phase: UVM's
  per-phase default sequences (`main_phase`, ..) have no counterpart, since
  the test's run is split into `pre_run` / `run` / `post_run` with
  scenarios instead.
- A default sequence that returns `Err` is a fatal error, as any `run`.
- Sequences started explicitly (from the test's `run`, for example) run
  alongside it and share the sequencer's arbitration.

### A protocol's methods from a test

Decided: the VIP's API (the methods of its protocol layer,
`ApbSequence::write`) is called from a test as tasks through a proxy, a
sequence with no body that the VIP declares and publishes with
`#[sequence(.., proxy)]`:

```rust
// the VIP: the proxy it publishes
#[sequence(sequencer = ApbSequencer, base = ApbSeqBase, proxy)]
pub struct ApbBfm {}

// a test's run
let mut apb = ApbBfm::new(&self.sequencer); // started on the sequencer
apb.write(0x10, 0x1234).await?;             // ApbSequence's, through its blanket impl
```

- A proxy is a sequence: to the sequencer, which arbitrates sequences and
  matches responses to them, it is one like any other (as UVM's
  `execute_item` makes a sequence of its own for an item). Its items go
  through the sequencer, so arbitration, the driver's callbacks and the
  monitors work as for any sequence. It has no body (no `impl Sequence`),
  since its steps are called from outside, so it is not started: `new`
  takes the sequencer first and starts it there, giving it its sequence
  id. `new` on an unbound handle panics, as `borrow()` does.
- `#[sequence]` implements `SequenceCore` and `HasBase` on it as on any
  sequence, so the protocol trait's blanket impl covers it; the VIP writes
  the struct and the attribute only. It is the VIP's own type, so it may
  have fields (after the sequencer in `new`) and methods of its own, and
  methods only a test needs go there or in a trait of the VIP's for it.
- `set_priority` takes effect at once (on any sequence it sets the
  priority it runs with, which `start` reckons again).
- It has no seed source of its own: what its methods randomize draws from
  the task calling them (the test's), which keeps runs reproducible; a
  sequence that needs its own stream is written as a sequence.
- One proxy is used by one task at a time (its methods take `&mut self`);
  tasks forked to drive one bus together each make their own, and the
  sequencer arbitrates between them.
- The driver's transfer itself is best kept in plain `async` methods over
  its clocking block (the driver's loop calling them), so the protocol's
  behaviour is in one place; nothing here depends on it.
- Considered: a generic proxy type the framework gives (`BfmProxy<S, B>`),
  its base named on the sequencer or as a type argument. A type of the
  VIP's own names the BFM for its users, takes methods of its own, and
  needs nothing on the sequencer.

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
| a passive component (`#[component(scoreboard, passive)]` on its impl, which then cannot have a `run`; still `check` / `report`) | `port.connect(&instance, id)` | scoreboards, coverage: components that only react |
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
      fn select(&mut self, requests: &[ArbRequest], rng: &SeedSource) -> usize;
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
      fn select(&mut self, _requests: &[ArbRequest], _rng: &SeedSource) -> usize { 0 }
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
    `rng` is the sequencer's seed source, so a random arbiter keeps random
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
      async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> { Ok(()) }
      fn check(&mut self, _check_context: &mut CheckContext) {}
      fn report(&self, _report_context: &ReportContext) {}
  }

  trait ErasedComponent {
      fn run<'a>(&'a mut self, run_context: &'a mut RunContext)
          -> Pin<Box<dyn Future<Output = Result<(), TestError>> + 'a>>;
      fn check(&mut self, check_context: &mut CheckContext);
      fn report(&self, report_context: &ReportContext);
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
  - A passive component (`#[component(.., passive)]`, no `run`), and any
    component without a `run` of its own, gets no run task: it is never
    borrowed for the run phase, so port writes, and `borrow()` from other
    tasks, reach it meanwhile.
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

Decided (implemented in the runtime, the macros in `verald-tb-macros`); what
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
  against the fields' types at construction. Clocking stays on the
  component's side, not in Veryl (see
  [veryl-proposals.md](veryl-proposals.md#clocking-blocks-as-a-language-feature-a-study)),
  so the clock is given here.
- Field types implement `SignalValue`: `bool`, the unsigned integers up to
  `u128` and `BigUint` (any width). Signals are two-state: X and Z read as
  0, as SystemVerilog's conversion to two-state types, and each type reads
  and writes itself through the host layer's direct paths (see
  [host-layer-design.md](host-layer-design.md#two-state-signals-first)).
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
  depends on, `verald` (as `verald::tb::runtime`) or `verald-tb-runtime`.
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
- A `Semaphore`, as SystemVerilog's `semaphore`, for a limit on what is
  outstanding (AXI's outstanding transactions) or a resource shared by
  tasks: `acquire(n).await` takes keys (`get`), `try_acquire(n)` does not
  wait (`try_get`), `release(permit)` gives them back (`put`), `add(n)`
  adds keys (`put` beyond those taken).
  - Taking keys gives a `Permit`, which `release` takes back. A permit
    dropped instead gives its keys back too, so a task dropped while it
    holds keys (at the end of the run phase, the losing side of `first`)
    does not keep them. `forget` keeps them taken. `release` panics on a
    permit of another semaphore.
  - Requests are served in the order made, one that cannot be served yet
    holding back those after it, so the order is the same in every run (the
    LRM leaves it open). A wait dropped before it is served leaves the
    queue; served but not yet taken, it gives the keys back.
  - `acquire` is an `async fn`, as `Fifo::get` / `put` are, awaited as the
    other waits are, not a macro: the `.await` marks where a task yields,
    and a future is needed to combine waits
    (`first(semaphore.acquire(1), clk.wait(100))`).
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
  error. The test module only starts the clocks (`clk.start()`, which keeps
  a clock running until the test ends) and leaves the end to the
  testbench; a test module ending the simulation itself (`$finish`, a
  bound on its clock) while the test runs is an unexpected finish.
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
  named in the Veryl test module, as the argument of `start`
  (`tb.start("RegTest")`), and the runtime argument `VERALD_TESTNAME`
  overrides it, as UVM's `+UVM_TESTNAME` does `run_test`'s (see "Runtime
  arguments" below). So one `#[test]` module, the harness every test
  shares, runs any test: `examples/regblock` has one, its tests and
  options given at run time. A test module of its own per test (a thin
  module naming the test, over a shared harness module) remains possible,
  for a fixed list that `veryl test` runs at once.
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
    it (`__VERALD_TEST_SlowWriteTest`, so as not to clash with the type in the
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
- The timeout (see [Executor](#executor)) is in cycles of the master
  clock, as Veryl is cycle-based: tests are measured in cycles, and a
  clock's period in time is no concern of theirs. The master clock is the
  first registered; the test module names another with
  `tb.set_master_clock(name)` when that one is not the clock to count.
  Checking it is one comparison at the master clock's edges: the test may
  end at the last cycle allowed, and the cycle after it ends the run phase
  with a fatal error, before any task runs at it, listing the tasks still
  alive by their components' paths.
  - A default as UVM's (`UVM_DEFAULT_TIMEOUT`): 10,000,000 cycles, beyond
    any usual test, so that a hung one still ends now that the test module
    keeps its clocks running with no bound of its own (`clk.start()`). The
    `timeout` option (`VERALD_TIMEOUT`) sets a test's; `0` is no limit.
- Settings are given by the Veryl test module (the shared harness), which
  calls methods before `start`, as often as needed; runtime arguments
  override them where UVM's plusargs would (below). The framework's
  options and the testbench's own settings are apart, as UVM's `+UVM_*`
  plusargs are from a testbench's own:

  ```veryl
  initial {
      tb.option("timeout", 200000);        // the framework's: checked at once
      tb.option("max_errors", 10);
      tb.set("num_masters", N_MASTERS);    // the testbench's: read by the test's setup
      tb.start("RegTest");
      clk.start();                         // runs until the testbench finishes the test
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
    `setup_context.get::<usize>("num_masters")?` (failing if missing or mismatched) or
    `setup_context.get_or("num_masters", 1)?`. A key no one has read once `setup`
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

- Runtime arguments (`veryl test --runtime-arg=..`, Veryl's plusargs;
  see [veryl-proposals.md](veryl-proposals.md#arguments-into-a-simulation))
  override the test module, as UVM's `+UVM_*` plusargs do `run_test` and
  the testbench's own settings:

  ```
  veryl +local test -t regblock_test --runtime-arg=VERALD_TESTNAME:ReverseTest --runtime-arg=VERALD_LOG_LEVEL:debug
  ```

  - The runtime takes a copy of them at `start`; they do not change during
    the run.
  - Its own are under the `VERALD_` prefix, in upper case as UVM's:
    `VERALD_TESTNAME:<name>` names the test over the name given to `start`
    (UVM's `+UVM_TESTNAME`), and any other `VERALD_<KEY>:<value>` is the option
    `<key>` in lower case (`VERALD_TIMEOUT`, `VERALD_LOG_LEVEL`, a testbench's
    own), taken in the order given after the test module's, so the command
    line wins and the last of a name wins. An unknown key, or one given as
    a flag, fails the start as `tb.option` would: a misspelt one is not
    ignored. A value reads as an integer if it is one, otherwise as a
    string. `testname` is not an option's name.
  - No prefix overrides settings (`tb.set`): a test reads its own arguments
    instead, typed, from anywhere and without a context, as UVM's command
    line processor is reached: `runtime_arg_value::<u32>("ROUNDS")?`
    (`None` if not given; an error naming the argument if it is a flag or
    does not convert), `runtime_arg_exists`, `runtime_arg` (the value, flag
    or text) and `runtime_args` (all, as given). The conversion is the
    SDK's `TryFrom<&ArgValue>`, so a testbench can read its own types.
  - The report's summary lists the arguments given, beside the seed, so a
    log shows how its run was made.

  Only this is implemented at first. Setting the options from the
  testbench's code (in a test's `setup` or a spec's `pre_build`, UVM's
  `set_timeout` and `set_report_max_quit_count`), overridden by the test
  module's, is added when needed.

## The name

Decided: **Verald** (Verification + Emerald, after Veryl's Verilog +
Beryl, the emerald being a variety of beryl), in place of veryl-tbf. With assertions planned (see
[assertion-design.md](assertion-design.md)) beside the testbench and
constrained random, and coverage likely later, "testbench framework" no
longer covered what the project is; the name covers verification as a
whole and serves as a short prefix for the crates and in paths
(`verald::prelude`). Others considered: `veryl-verify` (plain, longer),
`veryl-dv` (short, but close to rustdv and rustdv-veryl), `veryl-vf`
(short, but unclear). The name was free on crates.io (`verald` and
`verald-*`, checked on 2026-11-20). The runtime arguments and the DPI
functions are prefixed `VERALD_` and `verald_`.

## Crate layout

Decided: the event handling and the UVM-like methodology are separate crates
(as cocotb is apart from pyuvm). The methodology depends on the runtime,
never the other way, so the runtime's per-edge path is kept apart from the
methodology's needs, and the runtime alone serves small tests that need no
hierarchy.

| Crate (directory) | Contents |
|---|---|
| `verald` (`crates/verald`) | the entry point users depend on: re-exports the others (`verald::host`, `verald::tb::{runtime, core}`, `verald::random`) with one prelude and `export!`; the parts are chosen by features (`tb` and `random` by default, `assert` later) |
| `verald-host` (`crates/host`) | the host layer: the hook entry and its dispatch, the hook's context and services, panic detection, value conversion, typed signals and modports, `Signals` (see [host-layer-design.md](host-layer-design.md)) |
| `verald-macros` (`crates/macros`) | the proc macros shared by the parts: `#[modport]` |
| `verald-macros-codegen` (`crates/macros/codegen`) | a library: `#[modport]`'s code generation and the parts `#[clocking]` shares with it, as functions both macro crates call |
| `verald-random` (`crates/random`) | constrained random; depends on nothing else here, so it can be used on its own |
| `verald-random-macros` (`crates/random/macros`) | its proc macros (`#[randomize]`, `#[constraints]`, ..) |
| `verald-tb-runtime` (`crates/tb/runtime`) | the executor, tasks and their concurrency (`fork_scope`, `join`, `first`), waits for clocks and resets (`run_with_reset`), the shared parts (`Fifo`, events, `Semaphore`), clocking blocks, the registrations and Veryl's adapter as a subscriber of the host layer (`testbench`), the timeout, unexpected finishes |
| `verald-tb-core` (`crates/tb/core`) | the UVM-like part: `Component`, `Test`, `Instance`, `Scope`, the phases, `Knob`, `Callbacks`, TLM, items (`ItemHandle`), sequences and sequencers, agents' and monitors' helpers, `ItemWaiter`, `Spec` / `BundledSpecs`, the runner (`register_test!`, test selection, reporting, the error limit) |
| `verald-tb-macros` (`crates/tb/macros`) | the testbench's proc macros (`#[clocking]`, the wait macros; `#[component(..)]`, `#[sequence]`, `#[testcase]`, `register_test!`; `bundled_specs!` is a `macro_rules!` of the core) |
| `verald-tb-dpi` (`crates/tb/dpi`) | a DPI host (a prototype) running a testbench on a SystemVerilog simulator; its functions call the testbench component's methods (`set_clock`, `start`, ..), so it belongs to the testbench |

Decided: the directories group the crates by part. What the parts share is
at the top (`host`, `macros`); a part's crates are under its directory
(`tb/`, `random/`, later `assert/`), each with its own macros. A crate's
name joins its path (`crates/tb/runtime` is `verald-tb-runtime`). The
entry point is `verald` alone, with features, rather than an entry point
per part as well (`verald-tb`): one `export!` covers a component holding
several parts.

`#[modport]` is shared (the assertions bind modports too), and kept apart
from `#[clocking]`: structs for an interface's modports may come to be
generated from Veryl's description (veryl-lang/veryl#3712, for packages so
far), with `verald-macros-codegen` moved to Veryl's component SDK as the
functions it is.

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

The host layer (`verald-host`) is below the runtime: what talks to
Veryl's host API, which the runtime and the assertions both build on. See
[host-layer-design.md](host-layer-design.md).

## Implementation notes

What the implementation settled where the design left room, and where it
stops short for now:

- The runtime's fork scope is `ForkScope` (`fork_scope(async |scope| ..)`),
  apart from the methodology's `Scope`.
- `#[component(kind)]` goes on both a component's struct and its
  `impl Component`, as a marker on each: on the struct it records the kind
  (and implements `HasContext` for a `#[context]` field); on the impl it
  sets `Component::KIND` / `PASSIVE`, fails to compile if the struct's kind
  differs or the struct has no attribute, and checks `run`: a component
  with a `run` is not `passive`, one without is (`#[component(env,
  passive)]`), and a test has one. A passive component gets no run task, so
  it is never borrowed during the run phase (an env or agent stays
  reachable with `borrow()` from the test's `run`). The generated
  components (a sequencer, an agent) are passive by the macro. An impl
  without the attribute is given a task, to be safe.
- For now the unbound-handle check covers the `SequencerHandle` fields of
  generated sequencers only. Handles in other components (a test's, an
  env's), the elements of a `Vec<SequencerHandle<T>>`, and a driver's
  `SeqItemPort` are not checked before the run phase; a missing connection
  still stops the test, with a message, when it is used (a sequence
  started on an unbound handle, `get_next_item` on an unconnected port).
  Widening the check (the attribute on every component's struct generating
  it from the fields' types) is left until it is wanted.
- Seeds are kept by `SeedSource` of the random crate: a SplitMix64
  generator shared by its clones. One is made for every component (in
  `Scope::create_instance`, before its `build`) and for every sequence (in
  `start`), and is entered (made the thread's seed source) while its owner
  builds or runs: `build` runs entered; a run task and a sequence's `body`
  are wrapped in `seeded(source, future)`, which enters the source for the
  span of each poll, so tasks interleaving on one thread never draw from
  each other's sources. `randomize()` (an object's first seed),
  `urandom()`, `randcase!` and `std_randomize!` take from the entered
  source, else from the thread's own one (the random crate used on its
  own). The test's source is entered for `setup` as well. A sequencer's
  arbiter gets the sequencer's source, taken while it builds. The seed of
  a source is FNV-1a over the names (type, path, and for a sequence its
  count), mixed with the test's seed by SplitMix64.
- The sequence item port follows UVM's sequencer: `item_done` removes the
  item at once (UVM's `m_req_fifo.try_get`), so `get_next_item` never sees
  it again; before choosing the next request, `get_next_item` and
  `try_next_item` yield once, as UVM's `wait_for_sequences` waits for the
  NBA region, so that a sequence woken in the same hook (the one whose item
  just ended) asks before the choice and priorities hold.
- An agent generated by `#[component(agent(monitor = M, sequencer = S,
  driver = D))]` takes its monitor's `Context` as its own (an
  `InstanceContext` whose configuration implements `AgentConfig`), hands
  clones of it to its parts, and adds public fields `monitor`,
  `sequencer` (optional) and `driver` (optional). The struct's own fields
  start at their defaults; `post_build` calls the agent's
  `post_build(&mut self, scope: &Scope) -> Result<(), TestError>` after the
  parts are made and connected, for more children or connections (named
  after `Spec::post_build`, which also runs after `build`). It finds its parts'
  ports by field markers that `#[component(..)]` on the parts' structs reads:
  `#[analysis_port]` (`MonitorPort`), `#[seq_item_port]` (`DriverPort`),
  `#[request_port]` (`RequestPort`, for `reactive`). A `#[context]` field may
  be written with a type alias of an `InstanceContext`.
- `SeqItemPort::put_response(&request, response)` takes the request's
  handle, whose transaction id matches the response; a sequence waits with
  `self.get_response(&request)`. A child sequence started with
  `start_from(&sequencer, parent)` inherits the parent's priority.
- The test is the root: its path is empty, its children's paths have no
  prefix (`env.driver`), and messages name it by the name it was registered
  under. The hierarchy is printed at the debug level when the test starts.
- `Instance<T>` is not `Clone`, so `DerefMut` through it is unique.
- Each component's run task borrows the component and yields once before
  running it, so every component is borrowed before any user code runs.
- Reaching the error limit asks the runtime to end the run phase
  (`host::fatal`); it ends once the task that reported the error yields.

## Taken from UVM

Decided, beyond what the chapters above settle (the component tree, the
phases, TLM, sequences and sequencers, callbacks):

| UVM | Here |
|---|---|
| `uvm_object`'s field automation | Rust's own: `Clone` (copy), `Debug` (print), `PartialEq` (compare); bit packing by a derive. A compare that reports where items differ is added when needed |
| `uvm_sequence_item` | a plain struct shared as `ItemHandle<T>`, which holds the transaction id responses are matched by (see [Items](#items)) |
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
- The register model (RAL): a crate of its own (`verald-ral`), designed
  separately; models could come from rggen.
- `uvm_barrier`; TLM1 put / get / peek ports and `uvm_tlm_fifo`;
  `grab` / `lock`; the response handler; `uvm_sequence_library`;
  `uvm_in_order_comparator` / `uvm_algorithmic_comparator` as scoreboard
  templates; severity actions; transaction recording (perhaps through
  Veryl's trace variables); end_of_elaboration and the other phases.

### Extensions from tue

tue (the UVM extension library that the tvip VIPs are built on) adds to UVM
what VIPs keep writing; Verald takes these in:

| tue | Here |
|---|---|
| configuration and status on every component, sequence and item (`set_context`), passed to items and sequences through a proxy on the sequencer | `InstanceContext<C, S>`; sequences and items reach it typed through the sequencer's handle, so no proxy |
| `tue_test::create_configuration` | `Test::setup` |
| `tue_param_agent` (an agent over its monitor, sequencer and driver types: makes them as the agent is active or passive, connects them, optionally keeps a sequencer when passive) | the component attribute takes the parts and generates the agent into the VIP's own struct: `#[component(agent(monitor = ApbMonitor, sequencer = ApbSequencer, driver = ApbDriver))] pub struct ApbAgent { .. }` (the parts nested under `agent`, so that `monitor` as a part is not read as the `monitor` kind). It generates the parts' fields, the `build` (making the driver when active, the sequencer when active or asked for when passive, and the connections) and the usual accessors (`sequencer_handle()`, `analysis_port()`, `context()`); the VIP may add fields (a coverage collector, made in the agent's `post_build` hook) and methods (`driver_callbacks()`) to the same struct. Active or passive comes from an `AgentConfig` trait on the configuration, which hand-written agents use too. A generic `Agent<M, S, D>` type was set aside: methods cannot be added to another crate's type, and a VIP would have had to rewrite the agent as soon as it left the standard shape |
| `tue_reactive_agent` / `_monitor` / `_sequencer` / `_sequence` (the monitor sends requests to the sequencer, a reactive sequence takes them with `get_request` and answers) | the same, for slaves and responders: `#[component(agent(reactive, ..))]` connects the monitor's request port to the sequencer's request FIFO when the agent is active (as tue: a passive agent's kept sequencer has no driver to answer through), and a sequence on it waits with `self.get_request().await` (`ReactiveSequence`, for any sequence on a sequencer made with `request = R`). A responding sequence suits being the default sequence |
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

### Verald

- Drain time: a wait between the test's `run` returning and the end of the
  run phase (UVM's `set_drain_time`), in time or in clock edges. Not needed
  yet. (The case it solves: a monitor publishing the last transfer at the
  edge after the sequence has its response.)

- Ports put in by the attribute: a driver's `#[seq_item_port]` and a
  monitor's `#[analysis_port]` fields are always the same shape, but the
  attribute cannot add them, since the user's `build` writes the struct
  literal and would still have to initialise a field it does not see
  declared. Revisit when Rust's default field values (`default_field_values`,
  nightly only) are stable: the attribute would add
  `seq_item_port: SeqItemPort<T, R> = SeqItemPort::new()` and `build` would
  end its literal with `..`.

- Utility macros for objects, as UVM's `uvm_object_utils` and its field
  macros (`uvm_field_*`): a derive for every kind of object
  (configurations, sequences, items), apart from `#[sequence_item]`, which
  only marks items. Rust's `Debug`, `PartialEq` and `Clone` cover
  printing, comparing and copying today; what such a derive would add is
  what they cannot: per-field policies (a field left out of a comparison
  or a printout, as `UVM_NOCOMPARE` / `UVM_NOPRINT`), a printout in a
  fixed, readable form (UVM's table printer, for logs), and recording
  for a waveform or a transaction database. Packing to bits is the
  component SDK's (see
  [veryl-proposals.md](veryl-proposals.md#rust-types-generated-from-veryl-packages)).
  Not needed yet.

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

What Verald would want of Veryl is kept apart, in
[veryl-proposals.md](veryl-proposals.md).
