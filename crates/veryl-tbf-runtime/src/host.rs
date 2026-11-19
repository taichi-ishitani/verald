//! Veryl's simulator adapter: the hooks and methods of the exported
//! testbench component, and what they do to the runtime.
//!
//! The component holds nothing; the runtime's state is thread-local (see
//! `state`). The test module registers clocks, resets and interfaces, gives
//! options and settings, then calls `start`, as UVM's `run_test`:
//!
//! ```veryl
//! inst tb: $comp::tb;
//! initial {
//!     tb.set_clock("clk", clk);
//!     tb.set_reset("rst", rst);
//!     tb.set_interface("bus", bus.tb);
//!     tb.start("my_test");
//!     clk.start();
//! }
//! ```
//!
//! Runtime arguments (`veryl test --runtime-arg=..`) under the `TBF_`
//! prefix override the test module, as UVM's `+UVM_` plusargs:
//! `TBF_TESTNAME:<name>` the test given to `start`, and `TBF_<KEY>:<value>`
//! the option `<key>` (in lower case), applied at `start` after the test
//! module's.

use std::future::Future;
use std::rc::Rc;

use veryl_component::{
    ClockPort, InputPort, MemberPort, ModportRef, OutputPort, ResetPort, SimCtx, Value, bail,
};

use crate::TestError;
use crate::args::{self, ArgValue};
use crate::executor::{self, Polled};
use crate::signals::{Clock, ClockState, Reset, ResetState};
use crate::sim::{self, HookGuard};
use crate::state::{self, EndHandler, Phase, RunEnd, Setting, with_state};

type HookResult = veryl_component::Result<()>;

/// What a testbench gives the runtime: how to take options, and how to start
/// a test. `veryl-tbf`'s methodology implements it; a testbench on the
/// runtime alone may too.
pub trait Testbench: 'static {
    /// Takes an option the runtime does not know (`tb.option(key, value)`,
    /// or the runtime argument `TBF_<KEY>:<value>`). Returns `Ok(false)` for
    /// a key it does not know either. `testname` is not an option: its
    /// runtime argument names the test.
    fn option(_key: &str, _value: &Value) -> Result<bool, TestError> {
        Ok(false)
    }

    /// The options `option` takes, each with what it is, for the error on
    /// an unknown one.
    fn options() -> Vec<String> {
        Vec::new()
    }

    /// Starts the test named by the test module (`tb.start(name)`): spawns
    /// its tasks and sets the main task, whose end ends the run phase.
    fn start(start: &mut Start) -> Result<(), TestError>;

    /// Called at the start of each simulation (`on_init`), before any
    /// option: a fresh state for the testbench's own thread-local data.
    fn init() {}
}

/// The options the runtime takes itself.
const RUNTIME_OPTIONS: &[(&str, &str)] = &[(
    "timeout",
    "u64: the cycles of the master clock the test must end within; 0 for no limit; 10000000 by default",
)];

/// What `Testbench::start` builds the test with.
pub struct Start {
    test: String,
    main: bool,
}

impl Start {
    /// The test's name, as given to `start`.
    pub fn test_name(&self) -> &str {
        &self.test
    }

    /// The clock registered as `name`.
    pub fn clock(&self, name: &str) -> Result<Clock, TestError> {
        with_state(|s| s.clocks.iter().position(|c| c.name == name))
            .map(Clock::new)
            .ok_or_else(|| TestError::new(format!("no clock is registered as `{name}`")))
    }

    /// The reset registered as `name`.
    pub fn reset(&self, name: &str) -> Result<Reset, TestError> {
        with_state(|s| s.resets.iter().position(|r| r.name == name))
            .map(Reset::new)
            .ok_or_else(|| TestError::new(format!("no reset is registered as `{name}`")))
    }

    /// The interface registered as `name`.
    pub fn interface(&self, name: &str) -> Result<ModportRef, TestError> {
        registered_interface(name)
    }

    /// The signal the testbench reads at `path`: one registered with
    /// `set_input`, or a member of an interface (`"bus.done"`).
    pub fn input(&self, path: &str) -> Result<InputPort, TestError> {
        registered_input(path)
    }

    /// The signal the testbench drives at `path`: one registered with
    /// `set_output`, or a member of an interface (`"bus.start"`).
    pub fn output(&self, path: &str) -> Result<OutputPort, TestError> {
        registered_output(path)
    }

    /// The setting the test module gave as `key` (`tb.set(key, value)`),
    /// if any. A setting no one reads fails the start.
    pub fn setting(&self, key: &str) -> Option<Value> {
        with_state(|s| {
            let setting = s.settings.iter_mut().find(|setting| setting.key == key)?;
            setting.read = true;
            Some(setting.value.clone())
        })
    }

    /// Spawns a task for the run phase; `owner` names it in messages. It is
    /// dropped when the run phase ends, if still going.
    pub fn spawn(
        &mut self,
        owner: &str,
        task: impl Future<Output = Result<(), TestError>> + 'static,
    ) {
        executor::spawn(Rc::from(owner), Box::pin(task));
    }

    /// Spawns the main task: the run phase ends when it returns.
    pub fn main(
        &mut self,
        owner: &str,
        task: impl Future<Output = Result<(), TestError>> + 'static,
    ) {
        let id = executor::spawn(Rc::from(owner), Box::pin(task));
        with_state(|s| s.main = Some(id));
        self.main = true;
    }

    /// Sets what runs when the run phase has ended and every task is
    /// dropped (the methodology's `check` and `report`). An `Err` fails the
    /// test.
    pub fn on_end(&mut self, handler: impl FnOnce(&RunEnd) -> Result<(), TestError> + 'static) {
        let handler: EndHandler = Box::new(handler);
        with_state(|s| s.on_end = Some(handler));
    }
}

impl crate::clocking::Signals for Start {
    fn clock(&self, name: &str) -> Result<Clock, TestError> {
        Start::clock(self, name)
    }
    fn interface(&self, name: &str) -> Result<ModportRef, TestError> {
        registered_interface(name)
    }
    fn input(&self, path: &str) -> Result<InputPort, TestError> {
        registered_input(path)
    }
    fn output(&self, path: &str) -> Result<OutputPort, TestError> {
        registered_output(path)
    }
}

/// The interface registered as `name`.
pub fn registered_interface(name: &str) -> Result<ModportRef, TestError> {
    with_state(|s| {
        s.interfaces
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, bus)| bus.clone())
    })
    .ok_or_else(|| TestError::new(format!("no interface is registered as `{name}`")))
}

/// The interface and member a dotted path names: `bus.done`.
fn member_path(path: &str) -> Option<(ModportRef, &str)> {
    let (interface, member) = path.rsplit_once('.')?;
    Some((registered_interface(interface).ok()?, member))
}

/// The signal the testbench reads at `path`: one registered with
/// `set_input`, or a member of an interface (`"bus.done"`).
pub fn registered_input(path: &str) -> Result<InputPort, TestError> {
    if let Some(port) = with_state(|s| s.inputs.iter().find(|(n, _)| n == path).map(|(_, p)| *p)) {
        return Ok(port);
    }
    match member_path(path) {
        Some((bus, member)) => Ok(bus.input(member)?),
        None => Err(TestError::new(format!(
            "no input is registered as `{path}`"
        ))),
    }
}

/// The signal the testbench drives at `path`: one registered with
/// `set_output`, or a member of an interface (`"bus.start"`).
pub fn registered_output(path: &str) -> Result<OutputPort, TestError> {
    if let Some(port) = with_state(|s| s.outputs.iter().find(|(n, _)| n == path).map(|(_, p)| *p)) {
        return Ok(port);
    }
    match member_path(path) {
        Some((bus, member)) => Ok(bus.output(member)?),
        None => Err(TestError::new(format!(
            "no output is registered as `{path}`"
        ))),
    }
}

/// Enters a hook: publishes its context and checks for a poisoned runtime.
/// `None` when the hook has nothing to do.
fn enter(ctx: &mut SimCtx) -> Option<HookGuard> {
    if !state::exists() {
        return None;
    }
    let poisoned = with_state(|s| {
        if s.in_hook {
            // The previous hook unwound: a panic, reported by Veryl.
            s.poisoned = true;
        }
        s.in_hook = true;
        s.poisoned
    });
    if poisoned {
        return None;
    }
    Some(HookGuard::enter(ctx))
}

/// Leaves a hook normally.
fn leave() {
    if state::exists() {
        with_state(|s| s.in_hook = false);
    }
}

fn wake_all(wakers: Vec<std::task::Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

/// Polls the ready tasks, then ends the run phase if the main task has
/// returned or a task has failed.
fn run() {
    if with_state(|s| s.phase) != Phase::Running {
        return;
    }
    let polled = executor::run_ready(
        |id| {
            with_state(|s| {
                if s.main == Some(id) {
                    s.main_done = true;
                }
            })
        },
        || with_state(|s| s.pending_fatal.is_some()),
    );
    let pending = with_state(|s| s.pending_fatal.take());
    match (polled, pending) {
        (Polled::Fatal { owner, error }, _) => end(RunEnd::Fatal {
            owner: owner.to_string(),
            error,
        }),
        (_, Some((owner, error))) => end(RunEnd::Fatal { owner, error }),
        (Polled::Idle, None) => {
            if with_state(|s| s.main_done) {
                end(RunEnd::Finished);
            }
        }
        (Polled::Stopped, None) => unreachable!("stopped with no fatal error"),
    }
}

/// Ends the run phase: drops the remaining tasks, runs the end handler, and
/// finishes or fails the Veryl test (at the end of the cycle).
fn end(how: RunEnd) {
    with_state(|s| s.phase = Phase::Ended);
    executor::drop_all();
    let handler = with_state(|s| s.on_end.take());
    let handled = handler.map_or(Ok(()), |handler| handler(&how));
    match (&how, handled) {
        (RunEnd::Fatal { owner, error }, _) => sim::fail(&fatal_message(owner, error)),
        (RunEnd::Finished, Err(error)) => sim::fail(error.message()),
        (RunEnd::Finished, Ok(())) => sim::finish(),
    }
}

fn fatal_message(owner: &str, error: &TestError) -> String {
    if owner.is_empty() {
        format!("fatal: {error}")
    } else {
        format!("fatal: {owner}: {error}")
    }
}

/// The tasks still alive, for a timeout or an unexpected finish.
fn alive() -> String {
    let owners = executor::alive_owners();
    if owners.is_empty() {
        "no task alive".to_string()
    } else {
        let names: Vec<&str> = owners.iter().map(|o| &**o).collect();
        format!("tasks alive: {}", names.join(", "))
    }
}

/// Ends the run phase with a fatal error once the master clock has passed
/// the timeout's edges: the test may end at the last edge allowed, so the
/// edge after it is checked, before any task runs at it.
fn check_timeout() {
    let passed = with_state(|s| {
        let master = s.master_clock.or((!s.clocks.is_empty()).then_some(0))?;
        let clock = &s.clocks[master];
        (s.timeout > 0 && clock.edges > s.timeout).then(|| (clock.name.clone(), s.timeout))
    });
    if let Some((clock, timeout)) = passed {
        let error = TestError::new(format!(
            "timeout after {timeout} cycles of `{clock}`; {}",
            alive()
        ));
        end(RunEnd::Fatal {
            owner: String::new(),
            error,
        });
    }
}

thread_local! {
    /// The seed Veryl gives the testbench instance (`BuildCtx::seed`); kept
    /// apart from the state, which `on_init` resets after it is taken.
    static SEED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Takes the seed Veryl derives for the instance from the run's seed
/// (`veryl test --seed`), the test and the instance.
#[doc(hidden)]
pub fn on_build(ctx: &mut veryl_component::BuildCtx) -> HookResult {
    SEED.with(|s| s.set(ctx.seed()));
    Ok(())
}

/// The test's seed: the one Veryl gives the testbench instance, from which
/// every seed of the testbench is derived.
pub fn seed() -> u64 {
    SEED.with(std::cell::Cell::get)
}

/// `on_init`: a fresh runtime for this simulation.
pub fn on_init<T: Testbench>(_ctx: &mut SimCtx) -> HookResult {
    state::init();
    T::init();
    Ok(())
}

/// Ends the run phase with a fatal error raised by `owner` outside a task's
/// return (an error limit reached), once the running task yields.
pub fn fatal(owner: &str, error: TestError) {
    with_state(|s| {
        if s.pending_fatal.is_none() {
            s.pending_fatal = Some((owner.to_string(), error));
        }
    });
}

/// The clock registered as `name`.
pub fn registered_clock(name: &str) -> Result<Clock, TestError> {
    with_state(|s| s.clocks.iter().position(|c| c.name == name))
        .map(Clock::new)
        .ok_or_else(|| TestError::new(format!("no clock is registered as `{name}`")))
}

/// The reset registered as `name`.
pub fn registered_reset(name: &str) -> Result<Reset, TestError> {
    with_state(|s| s.resets.iter().position(|r| r.name == name))
        .map(Reset::new)
        .ok_or_else(|| TestError::new(format!("no reset is registered as `{name}`")))
}

/// `on_clock`: counts the edge of the clock that fired, wakes its waiters,
/// releases the resets (`on_clock` and `on_reset` are exclusive: a reset in
/// force calls `on_reset` at every edge), and runs the ready tasks.
pub fn on_clock(ctx: &mut SimCtx) -> HookResult {
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    if with_state(|s| s.phase) == Phase::Running {
        let fired: Vec<usize> = with_state(|s| {
            s.clocks
                .iter()
                .enumerate()
                .filter(|(_, c)| sim::fired(c.port))
                .map(|(i, _)| i)
                .collect()
        });
        for i in fired {
            let wakers = with_state(|s| s.clocks[i].edge());
            let mut list = wakers;
            for waker in list.drain(..) {
                waker.wake();
            }
            with_state(|s| s.clocks[i].recycle(list));
        }
        check_timeout();
        if with_state(|s| s.phase) != Phase::Running {
            leave();
            return Ok(());
        }
        let released: Vec<std::task::Waker> =
            with_state(|s| s.resets.iter_mut().flat_map(|r| r.set(false)).collect());
        wake_all(released);
        run();
    }
    leave();
    Ok(())
}

/// `on_reset`: asserts the reset that fired and runs the ready tasks.
pub fn on_reset(ctx: &mut SimCtx) -> HookResult {
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    if with_state(|s| s.phase) == Phase::Running {
        let asserted: Vec<std::task::Waker> = with_state(|s| {
            s.resets
                .iter_mut()
                .filter(|r| sim::fired(r.port))
                .flat_map(|r| r.set(true))
                .collect()
        });
        wake_all(asserted);
        run();
    }
    leave();
    Ok(())
}

/// `on_finish`: an end before the run phase's is an unexpected finish.
pub fn on_finish(ctx: &mut SimCtx) -> HookResult {
    if let Some(_guard) = enter(ctx) {
        match with_state(|s| s.phase) {
            Phase::Setup => ctx_fail("the testbench was not started: call `start`"),
            Phase::Running => ctx_fail(&format!(
                "unexpected finish: the simulation ended before the test did \
                 (the test module's `$finish` or cycle limit?); {}",
                alive()
            )),
            Phase::Ended => {}
        }
    }
    state::clear();
    Ok(())
}

fn ctx_fail(message: &str) {
    sim::fail(message);
}

fn in_setup(what: &str) -> veryl_component::Result<()> {
    if !state::exists() {
        bail!("the testbench got `{what}` before `on_init`");
    }
    if with_state(|s| s.phase) != Phase::Setup {
        bail!("`{what}` is only allowed before `start`");
    }
    Ok(())
}

fn check_name(name: &str) -> veryl_component::Result<()> {
    if name.is_empty() {
        bail!("an empty name cannot be registered");
    }
    if with_state(|s| s.has_name(name)) {
        bail!("`{name}` is already registered");
    }
    Ok(())
}

/// `set_clock(name, clk)`: registers a clock.
pub fn set_clock(_ctx: &mut SimCtx, name: String, clk: ClockPort) -> HookResult {
    in_setup("set_clock")?;
    check_name(&name)?;
    with_state(|s| s.clocks.push(ClockState::new(name, clk)));
    Ok(())
}

/// `set_master_clock(name)`: the clock the timeout counts, over the first
/// registered.
pub fn set_master_clock(_ctx: &mut SimCtx, name: String) -> HookResult {
    in_setup("set_master_clock")?;
    let Some(index) = with_state(|s| s.clocks.iter().position(|c| c.name == name)) else {
        bail!("set_master_clock: no clock is registered as `{name}`");
    };
    with_state(|s| s.master_clock = Some(index));
    Ok(())
}

/// `set_reset(name, rst)`: registers a reset.
pub fn set_reset(_ctx: &mut SimCtx, name: String, rst: ResetPort) -> HookResult {
    in_setup("set_reset")?;
    check_name(&name)?;
    with_state(|s| s.resets.push(ResetState::new(name, rst)));
    Ok(())
}

/// `set_interface(name, bus.modport)`: registers an interface; its clock and
/// reset members are registered as `<name>.<member>`.
pub fn set_interface(_ctx: &mut SimCtx, name: String, bus: ModportRef) -> HookResult {
    in_setup("set_interface")?;
    check_name(&name)?;
    for member in bus.members() {
        let path = format!("{name}.{}", member.name);
        match member.port {
            MemberPort::Clock(port) => {
                check_name(&path)?;
                with_state(|s| s.clocks.push(ClockState::new(path, port)));
            }
            MemberPort::Reset(port) => {
                check_name(&path)?;
                with_state(|s| s.resets.push(ResetState::new(path, port)));
            }
            MemberPort::Input(_) | MemberPort::Output(_) => {}
        }
    }
    with_state(|s| s.interfaces.push((name, bus)));
    Ok(())
}

/// `set_input(name, sig)`: registers a signal the testbench reads, outside
/// any interface (an interrupt).
pub fn set_input(_ctx: &mut SimCtx, name: String, sig: InputPort) -> HookResult {
    in_setup("set_input")?;
    check_name(&name)?;
    with_state(|s| s.inputs.push((name, sig)));
    Ok(())
}

/// `set_output(name, sig)`: registers a signal the testbench drives,
/// outside any interface.
pub fn set_output(_ctx: &mut SimCtx, name: String, sig: OutputPort) -> HookResult {
    in_setup("set_output")?;
    check_name(&name)?;
    with_state(|s| s.outputs.push((name, sig)));
    Ok(())
}

/// `option(key, value)`: an option of the runtime or the testbench, checked
/// at once.
pub fn option<T: Testbench>(_ctx: &mut SimCtx, key: String, value: Value) -> HookResult {
    in_setup("option")?;
    take_option::<T>(&key, &value).map_err(|e| veryl_component::anyhow!("{e}"))
}

/// Takes an option, from the test module or from a runtime argument.
fn take_option<T: Testbench>(key: &str, value: &Value) -> Result<(), TestError> {
    if key == "timeout" {
        let edges = value
            .as_u64()
            .map_err(|e| TestError::new(format!("option `timeout`: {e}")))?;
        with_state(|s| s.timeout = edges);
        return Ok(());
    }
    match T::option(key, value) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let known: Vec<String> = RUNTIME_OPTIONS
                .iter()
                .map(|(k, desc)| format!("`{k}` ({desc})"))
                .chain(T::options())
                .collect();
            Err(TestError::new(format!(
                "unknown option `{key}`; known: {}",
                known.join(", ")
            )))
        }
        Err(e) => Err(TestError::new(format!("option `{key}`: {e}"))),
    }
}

/// Takes the runtime arguments, and the options among them (`TBF_<KEY>`),
/// in the order given, so the last of a name wins. Returns the test's name
/// from `TBF_TESTNAME`, if given.
fn take_runtime_args<T: Testbench>(ctx: &SimCtx) -> Result<Option<String>, TestError> {
    let runtime_args = ctx.runtime_args().to_vec();
    with_state(|s| s.runtime_args = runtime_args.clone());
    for arg in &runtime_args {
        let Some(key) = arg.name.strip_prefix(args::PREFIX) else {
            continue;
        };
        if arg.name == args::TESTNAME {
            continue;
        }
        let value = match &arg.value {
            ArgValue::Flag => return Err(args::error(&arg.name, &"an option takes a value")),
            ArgValue::Text(text) => match u64::try_from(&arg.value) {
                Ok(n) => Value::from(n),
                Err(_) => Value::from(text.as_str()),
            },
        };
        take_option::<T>(&key.to_lowercase(), &value).map_err(|e| args::error(&arg.name, &e))?;
    }
    match args::find(&runtime_args, args::TESTNAME) {
        None => Ok(None),
        Some(value) => String::try_from(value)
            .map(Some)
            .map_err(|e| args::error(args::TESTNAME, &e)),
    }
}

/// `set(key, value)`: a setting for the test, read when it starts.
pub fn set(_ctx: &mut SimCtx, key: String, value: Value) -> HookResult {
    in_setup("set")?;
    if with_state(|s| s.settings.iter().any(|setting| setting.key == key)) {
        bail!("setting `{key}` is given twice");
    }
    with_state(|s| {
        s.settings.push(Setting {
            key,
            value,
            read: false,
        })
    });
    Ok(())
}

/// `start(test)`: starts the test, as UVM's `run_test`, and runs its tasks
/// up to their first waits (time 0). The runtime arguments are taken first:
/// `TBF_TESTNAME` names the test over `test`, and `TBF_<KEY>` sets options.
pub fn start<T: Testbench>(ctx: &mut SimCtx, test: String) -> HookResult {
    in_setup("start")?;
    let test = match take_runtime_args::<T>(ctx) {
        Ok(name) => name.unwrap_or(test),
        Err(error) => bail!("{error}"),
    };
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    let mut start = Start { test, main: false };
    let started = T::start(&mut start);
    let result = started.and_then(|()| {
        if !start.main {
            return Err(TestError::new("the testbench set no main task"));
        }
        let unread: Vec<String> = with_state(|s| {
            s.settings
                .iter()
                .filter(|setting| !setting.read)
                .map(|setting| format!("`{}`", setting.key))
                .collect()
        });
        if unread.is_empty() {
            Ok(())
        } else {
            Err(TestError::new(format!(
                "settings no one read (misspelt?): {}",
                unread.join(", ")
            )))
        }
    });
    match result {
        Ok(()) => {
            with_state(|s| s.phase = Phase::Running);
            run();
            leave();
            Ok(())
        }
        Err(error) => {
            with_state(|s| s.phase = Phase::Ended);
            executor::drop_all();
            leave();
            bail!("test `{}`: {error}", start.test)
        }
    }
}

/// The component the testbench exports to Veryl; see [`crate::export!`].
#[derive(Default)]
pub struct Host;
