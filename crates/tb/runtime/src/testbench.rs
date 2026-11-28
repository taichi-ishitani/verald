//! Veryl's simulator adapter: the hooks and methods of the exported
//! testbench component, and what they do to the runtime.
//!
//! The component holds nothing; the runtime's state is thread-local (see
//! `state`). The test module registers clocks, resets and interfaces (with
//! the host layer, whose table the runtime builds its clocks and resets
//! over at `start`), gives options and settings, then calls `start`, as
//! UVM's `run_test`:
//!
//! ```veryl
//! inst tb: $comp::tb;
//! initial {
//!     tb.set_clock("clk", clk);
//!     tb.set_reset("rst", rst);
//!     tb.set_modport("bus", bus.tb);
//!     tb.start("my_test");
//!     clk.start();
//! }
//! ```
//!
//! Runtime arguments (`veryl test --runtime-arg=..`) under the `VERALD_`
//! prefix override the test module, as UVM's `+UVM_` plusargs:
//! `VERALD_TESTNAME=<name>` the test given to `start`, and `VERALD_<KEY>=<value>`
//! the option `<key>` (in lower case), applied at `start` after the test
//! module's.

use std::future::Future;
use std::marker::PhantomData;
use std::rc::Rc;

use veryl_component::{
    ClockPort, InputPort, ModportRef, OutputPort, ResetPort, SimCtx, Value, bail,
};

use crate::TestError;
use crate::args::{self, ArgValue};
use crate::clocking::Clocks;
use crate::executor::{self, Polled};
use crate::host::{self, HookSubscriber, HostError, Signals, registrations};
use crate::signals::{Clock, ClockState, Reset, ResetState};
use crate::state::{self, EndHandler, Phase, RunEnd, Setting, with_state};

type HookResult = veryl_component::Result<()>;

/// What a testbench gives the runtime: how to take options, and how to start
/// a test. `verald`'s methodology implements it; a testbench on the
/// runtime alone may too.
pub trait Testbench: 'static {
    /// Takes an option the runtime does not know (`tb.option(key, value)`,
    /// or the runtime argument `VERALD_<KEY>=<value>`). Returns `Ok(false)` for
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

    /// The modport registered as `name`.
    pub fn modport(&self, name: &str) -> Result<ModportRef, TestError> {
        registered_modport(name)
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

/// The test module's registrations, as the host layer's `Signals` and the
/// runtime's `Clocks`: what `Start` and the methodology's `Scope` bind
/// through, and what code outside both (a test's `setup`) may use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Registered;

impl Signals for Registered {
    type Members = ModportRef;
    fn modport(&self, name: &str) -> Result<ModportRef, HostError> {
        registrations::modport(name)
    }
    fn input(&self, path: &str) -> Result<InputPort, HostError> {
        registrations::input(path)
    }
    fn output(&self, path: &str) -> Result<OutputPort, HostError> {
        registrations::output(path)
    }
    fn clock_port(&self, name: &str) -> Result<ClockPort, HostError> {
        registrations::clock(name)
    }
    fn reset_port(&self, name: &str) -> Result<ResetPort, HostError> {
        registrations::reset(name)
    }
}

impl Clocks for Registered {
    fn clock(&self, name: &str) -> Result<Clock, TestError> {
        registered_clock(name)
    }
    fn reset(&self, name: &str) -> Result<Reset, TestError> {
        registered_reset(name)
    }
}

/// Implements the host layer's `Signals` and the runtime's `Clocks` for
/// `$ty` over the test module's registrations ([`testbench::Registered`]).
#[macro_export]
#[doc(hidden)]
macro_rules! __registered_signals {
    ($ty:ty) => {
        impl $crate::Signals for $ty {
            type Members = $crate::host::ModportRef;
            fn modport(
                &self,
                name: &str,
            ) -> ::core::result::Result<Self::Members, $crate::HostError> {
                $crate::Signals::modport(&$crate::testbench::Registered, name)
            }
            fn input(
                &self,
                path: &str,
            ) -> ::core::result::Result<$crate::host::InputPort, $crate::HostError> {
                $crate::Signals::input(&$crate::testbench::Registered, path)
            }
            fn output(
                &self,
                path: &str,
            ) -> ::core::result::Result<$crate::host::OutputPort, $crate::HostError> {
                $crate::Signals::output(&$crate::testbench::Registered, path)
            }
            fn clock_port(
                &self,
                name: &str,
            ) -> ::core::result::Result<$crate::host::ClockPort, $crate::HostError> {
                $crate::Signals::clock_port(&$crate::testbench::Registered, name)
            }
            fn reset_port(
                &self,
                name: &str,
            ) -> ::core::result::Result<$crate::host::ResetPort, $crate::HostError> {
                $crate::Signals::reset_port(&$crate::testbench::Registered, name)
            }
        }

        impl $crate::Clocks for $ty {
            fn clock(
                &self,
                name: &str,
            ) -> ::core::result::Result<$crate::Clock, $crate::TestError> {
                $crate::Clocks::clock(&$crate::testbench::Registered, name)
            }
            fn reset(
                &self,
                name: &str,
            ) -> ::core::result::Result<$crate::Reset, $crate::TestError> {
                $crate::Clocks::reset(&$crate::testbench::Registered, name)
            }
        }
    };
}

crate::__registered_signals!(Start);

/// The modport registered as `name`.
pub fn registered_modport(name: &str) -> Result<ModportRef, TestError> {
    Ok(registrations::modport(name)?)
}

/// The signal the testbench reads at `path`: one registered with
/// `set_input`, or a member of an interface (`"bus.done"`).
pub fn registered_input(path: &str) -> Result<InputPort, TestError> {
    Ok(registrations::input(path)?)
}

/// The signal the testbench drives at `path`: one registered with
/// `set_output`, or a member of an interface (`"bus.start"`).
pub fn registered_output(path: &str) -> Result<OutputPort, TestError> {
    Ok(registrations::output(path)?)
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
        (RunEnd::Fatal { owner, error }, _) => host::fail(&fatal_message(owner, error)),
        (RunEnd::Finished, Err(error)) => host::fail(error.message()),
        (RunEnd::Finished, Ok(())) => host::finish(),
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

/// The test's seed: the one Veryl gives the testbench instance, from which
/// every seed of the testbench is derived.
pub fn seed() -> u64 {
    crate::host::seed()
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

/// The port of the clock registered as `name`.
pub fn registered_clock_port(name: &str) -> Result<ClockPort, TestError> {
    Ok(registrations::clock(name)?)
}

/// The port of the reset registered as `name`.
pub fn registered_reset_port(name: &str) -> Result<ResetPort, TestError> {
    Ok(registrations::reset(name)?)
}

/// The runtime as a subscriber of the host layer's hooks, over the
/// testbench `T`.
pub struct Runtime<T> {
    testbench: PhantomData<fn() -> T>,
}

impl<T> Default for Runtime<T> {
    fn default() -> Self {
        Runtime {
            testbench: PhantomData,
        }
    }
}

impl<T: Testbench> HookSubscriber for Runtime<T> {
    /// A fresh runtime for this simulation.
    fn on_init(&mut self) -> Result<(), HostError> {
        state::init();
        T::init();
        Ok(())
    }

    /// Starts the test (the test module's `start`), as UVM's `run_test`, and
    /// runs its tasks up to their first waits (time 0). The runtime
    /// arguments are taken first: `VERALD_TESTNAME` names the test over
    /// `test`, and `VERALD_<KEY>` sets options.
    fn on_start(&mut self, test: Option<&str>) -> Result<(), HostError> {
        if !state::exists() {
            return Ok(());
        }
        start::<T>(test.unwrap_or_default()).map_err(|e| HostError::new(e.message()))
    }

    /// Counts the edge of the clock that fired, wakes its waiters, releases
    /// the resets (`on_clock` and `on_reset` are exclusive: a reset in force
    /// calls `on_reset` at every edge), and runs the ready tasks.
    fn on_clock(&mut self) -> Result<(), HostError> {
        if !state::exists() || with_state(|s| s.phase) != Phase::Running {
            return Ok(());
        }
        let fired: Vec<usize> = with_state(|s| {
            s.clocks
                .iter()
                .enumerate()
                .filter(|(_, c)| host::fired(c.port))
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
            return Ok(());
        }
        let released: Vec<std::task::Waker> = with_state(|s| {
            s.resets
                .iter_mut()
                .flat_map(|r| r.set(host::reset_asserted(r.port)))
                .collect()
        });
        wake_all(released);
        run();
        Ok(())
    }

    /// Asserts the reset that fired and runs the ready tasks.
    fn on_reset(&mut self) -> Result<(), HostError> {
        if !state::exists() || with_state(|s| s.phase) != Phase::Running {
            return Ok(());
        }
        let asserted: Vec<std::task::Waker> = with_state(|s| {
            s.resets
                .iter_mut()
                .filter(|r| host::fired(r.port))
                .flat_map(|r| r.set(host::reset_asserted(r.port)))
                .collect()
        });
        wake_all(asserted);
        run();
        Ok(())
    }

    /// An end before the run phase's is an unexpected finish. The tasks
    /// and the state are dropped, poisoned or not.
    fn on_finish(&mut self) -> Result<(), HostError> {
        if state::exists() && !host::poisoned() {
            match with_state(|s| s.phase) {
                Phase::Setup => ctx_fail("the testbench was not started: call `start`"),
                // A finish a failure caused is expected.
                Phase::Running if host::failed() => {}
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
}

fn ctx_fail(message: &str) {
    host::fail(message);
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

/// `set_master_clock(name)`: the clock the timeout counts, over the first
/// registered.
pub fn set_master_clock(_ctx: &mut SimCtx, name: String) -> HookResult {
    in_setup("set_master_clock")?;
    if let Err(error) = registrations::clock(&name) {
        bail!("set_master_clock: {error}");
    }
    with_state(|s| s.master_clock_name = Some(name));
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

/// Takes the runtime arguments, and the options among them (`VERALD_<KEY>`),
/// in the order given, so the last of a name wins. Returns the test's name
/// from `VERALD_TESTNAME`, if given.
fn take_runtime_args<T: Testbench>() -> Result<Option<String>, TestError> {
    let runtime_args = host::runtime_args();
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
    match host::find_arg(&runtime_args, args::TESTNAME) {
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

/// Starts the test `test`, at the test module's `start`: builds the clocks
/// and resets over the registrations, takes the runtime arguments, and lets
/// the testbench spawn its tasks.
fn start<T: Testbench>(test: &str) -> Result<(), TestError> {
    with_state(|s| {
        s.clocks = registrations::clocks()
            .into_iter()
            .map(|(name, port)| ClockState::new(name, port))
            .collect();
        s.resets = registrations::resets()
            .into_iter()
            .map(|(name, port)| ResetState::new(name, port))
            .collect();
        s.master_clock = match &s.master_clock_name {
            Some(name) => s.clocks.iter().position(|c| &c.name == name),
            None => None,
        };
    });
    let test = take_runtime_args::<T>()?.unwrap_or_else(|| test.to_string());
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
            Ok(())
        }
        Err(error) => {
            with_state(|s| s.phase = Phase::Ended);
            executor::drop_all();
            Err(TestError::new(format!("test `{}`: {error}", start.test)))
        }
    }
}
